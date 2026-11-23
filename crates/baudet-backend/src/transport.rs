
use crate::openai::{events_for_payload, request_body, Delta, StreamState, TurnAccumulator};
use crate::protocol::{chat_request_body, chat_summary_body, wire_history, Protocol};
use crate::sse::Framer;
use crate::{Backend, BackendError, SendRequest, TurnHandle};
use baudet_core::{CoreError, ItemStatus, ThreadEvent, Turn};
use serde_json::Value;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::Notify;
use std::time::Duration;

// The streaming read timeout re-arms whenever another chunk arrives.
pub const READ_TIMEOUT: Duration = Duration::from_secs(1_800);

fn http_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("HTTP runtime")
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Credential {
    pub value: Option<String>,
    pub source: String,
}

impl Credential {
    pub fn none() -> Credential {
        Credential { value: None, source: "none".into() }
    }

    pub fn typed(value: impl Into<String>) -> Credential {
        let value = value.into();
        Credential { value: Some(value), source: "settings".into() }
    }

    pub fn from_env(name: &str, value: Option<String>) -> Credential {
        match value.filter(|v| !v.trim().is_empty()) {
            None => Credential { value: None, source: format!("{name} (unset)") },
            Some(value) => Credential { value: Some(value), source: name.to_string() },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub base_url: String,
    pub credential: Credential,
    pub protocol: Protocol,
    pub model: String,
    pub store: bool,
    pub thinking_template: bool,
}

impl Endpoint {
    pub fn url_for(&self, dialect: Protocol) -> String {
        format!("{}{}", self.base_url, dialect.path())
    }
}

struct Shared {
    endpoint: Endpoint,
    client: reqwest::Client,
    reasoning: Option<String>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    last: Mutex<Option<String>>,
    turn_counter: std::sync::atomic::AtomicUsize,
}

pub struct ResponsesBackend {
    inner: Arc<Shared>,
}

impl ResponsesBackend {
    pub fn new(endpoint: Endpoint, reasoning: Option<String>) -> Result<ResponsesBackend, BackendError> {
        Self::with_sampling(endpoint, reasoning, None, None)
    }

    pub fn with_sampling(
        endpoint: Endpoint,
        reasoning: Option<String>,
        temperature: Option<f64>,
        top_p: Option<f64>,
    ) -> Result<ResponsesBackend, BackendError> {
        let client = reqwest::Client::builder()
            .read_timeout(READ_TIMEOUT)
            .build()
            .map_err(|error| BackendError::Core(CoreError::Http { status: 0, detail: error.to_string() }))?;
        Ok(ResponsesBackend {
            inner: Arc::new(Shared {
                endpoint,
                client,
                reasoning,
                temperature,
                top_p,
                last: Mutex::new(None),
                turn_counter: std::sync::atomic::AtomicUsize::new(0),
            }),
        })
    }

    pub fn protocol(&self) -> Protocol {
        self.inner.endpoint.protocol
    }

    pub fn last_final_event(&self) -> Option<String> {
        self.inner.last.lock().unwrap().clone()
    }
}

impl Shared {
    async fn run(
        &self,
        request: &SendRequest,
        thread_id: &str,
        turn_id: &str,
        tx: &tokio::sync::mpsc::UnboundedSender<ThreadEvent>,
        cancelled: &Notify,
        result: &Mutex<Option<Turn>>,
    ) {
        let _ = tx.send(ThreadEvent::TurnStarted { thread_id: thread_id.to_string(), turn_id: turn_id.to_string() });
        let mut state = StreamState::new(thread_id);
        let outcome = tokio::select! {
            biased;
            _ = cancelled.notified() => None,
            outcome = self.stream(request, turn_id, tx, &mut state) => Some(outcome),
        };
        let Some(outcome) = outcome else {
            for event in state.close_open_rows(turn_id, ItemStatus::Interrupted) {
                let _ = tx.send(event);
            }
            return;
        };
        match outcome {
            Ok(mut turn) => {
                turn.incomplete_reason = turn.incomplete_reason.map(|detail| redact(&detail, &self.endpoint.credential));
                *result.lock().unwrap() = Some(turn.clone());
                *self.last.lock().unwrap() = turn.final_event.clone();
                let _ = tx.send(ThreadEvent::TurnCompleted {
                    thread_id: thread_id.to_string(),
                    turn_id: turn_id.to_string(),
                    response_id: turn.response_id,
                    usage: turn.usage,
                    final_event: turn.final_event,
                    incomplete_reason: turn.incomplete_reason,
                });
            }
            Err(error) => {
                let detail = error.to_string();
                *self.last.lock().unwrap() = Some(format!("error: {detail}"));
                let _ = tx.send(ThreadEvent::TurnCompleted {
                    thread_id: thread_id.to_string(),
                    turn_id: turn_id.to_string(),
                    response_id: None,
                    usage: None,
                    final_event: Some(format!("error: {detail}")),
                    incomplete_reason: Some(detail),
                });
            }
        }
    }

    async fn stream(
        &self,
        request: &SendRequest,
        turn_id: &str,
        tx: &tokio::sync::mpsc::UnboundedSender<ThreadEvent>,
        state: &mut StreamState,
    ) -> Result<Turn, BackendError> {
        let reasoning = request.reasoning.as_deref().or(self.reasoning.as_deref());
        let enable_thinking = reasoning.filter(|effort| self.endpoint.thinking_template && !effort.is_empty())
            .map(|effort| effort != crate::openai::REASONING_NONE);
        let model = request.model.as_deref().filter(|m| !m.trim().is_empty()).unwrap_or(&self.endpoint.model);
        if model.trim().is_empty() {
            return Err(BackendError::Core(CoreError::Unanswered { request: "no model chosen; pick one in settings" }));
        }
        let history = wire_history(&request.history);
        let shape = crate::openai::RequestShape {
            model,
            system: &request.system,
            history: &history,
            user_text: &request.user_text,
            previous_response_id: request.previous_response_id.as_deref(),
            tools: request.tools.as_deref(),
            image: request.image.as_deref(),
            max_output_tokens: request.max_output_tokens,
            reasoning,
            enable_thinking,
            temperature: self.temperature,
            top_p: self.top_p,
            store: self.endpoint.store,
        };
        let body = match self.endpoint.protocol {
            Protocol::Responses => Value::Object(request_body(&shape)),
            Protocol::ChatCompletions => Value::Object(chat_request_body(&shape)),
        };
        let mut call = self.client.post(self.endpoint.url_for(self.endpoint.protocol)).json(&body);
        if let Some(key) = &self.endpoint.credential.value {
            call = call.bearer_auth(key);
        }
        let mut response = call
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await
            .map_err(|error| BackendError::Core(CoreError::Http { status: 0, detail: error.to_string() }))?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(BackendError::Core(CoreError::Http {
                status: status.as_u16(),
                detail: redact(detail.trim(), &self.endpoint.credential),
            }));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if !content_type.contains("text/event-stream") {
            let detail = response.text().await.unwrap_or_default();
            let detail = redact(detail.trim(), &self.endpoint.credential);
            let looks_like_json = detail.starts_with('{') || detail.starts_with('[');
            let said = if looks_like_json {
                detail.chars().take(400).collect::<String>()
            } else {
                format!(
                    "a {} body of {} bytes",
                    content_type.split(';').next().unwrap_or("empty"),
                    detail.chars().count()
                )
            };
            return Err(BackendError::Core(CoreError::Http { status: status.as_u16(), detail: said }));
        }

        let mut accumulator = TurnAccumulator::new();
        let mut framer = Framer::new();
        let mut pending: Vec<u8> = Vec::new();
        let mut terminal_seen = false;

        loop {
            match response.chunk().await {
                Ok(None) => break,
                Ok(Some(chunk)) => {
                    pending.extend_from_slice(&chunk);
                    while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                        let line = pending.drain(..=newline).collect::<Vec<u8>>();
                        let line = String::from_utf8_lossy(&line);
                        let line = line.trim_end_matches(['\r', '\n']);
                        if let Some(event) = framer.push_line(line) {
                            terminal_seen |=
                                self.publish(&event.event, &event.data, &mut accumulator, state, turn_id, tx);
                            if terminal_seen {
                                break;
                            }
                        }
                    }
                    if terminal_seen {
                        break;
                    }
                }
                Err(error) => {
                    if terminal_seen {
                        break;
                    }
                    return Err(BackendError::Core(CoreError::Incomplete {
                        detail: redact(&error.to_string(), &self.endpoint.credential),
                    }));
                }
            }
        }
        if let Some(event) = framer.finish() {
            self.publish(&event.event, &event.data, &mut accumulator, state, turn_id, tx);
        }
        let turn = accumulator.finish();
        for closing in state.close_open_rows(turn_id, ItemStatus::Completed) {
            let _ = tx.send(closing);
        }
        Ok(turn)
    }

    fn publish(
        &self,
        event: &Option<String>,
        data: &str,
        accumulator: &mut TurnAccumulator,
        state: &mut StreamState,
        turn_id: &str,
        tx: &tokio::sync::mpsc::UnboundedSender<ThreadEvent>,
    ) -> bool {
        let terminal = event.as_deref() == Some("response.completed")
            || event.as_deref() == Some("response.incomplete")
            || event.as_deref() == Some("response.failed");
        let delta: Option<Delta> = accumulator.feed(event.as_deref(), data);
        if let Some(delta) = delta {
            for produced in events_for_payload(state, turn_id, &delta) {
                let _ = tx.send(produced);
            }
        }
        terminal
    }
}

impl Shared {
    async fn complete_once(&self, system: &str, user_text: &str, max_tokens: u32) -> Result<Turn, BackendError> {
        let mut body = chat_summary_body(&self.endpoint.model, system, user_text, max_tokens);
        if let Protocol::Responses = self.endpoint.protocol {
            let shape = crate::openai::RequestShape {
                model: &self.endpoint.model,
                system,
                history: &[],
                user_text,
                previous_response_id: None,
                tools: None,
                image: None,
                max_output_tokens: Some(max_tokens.max(1)),
                reasoning: Some(crate::openai::REASONING_NONE),
                store: false,
                ..Default::default()
            };
            body = request_body(&shape);
            body.insert("stream".into(), Value::Bool(false));
        }
        crate::openai::insert_thinking_template(&mut body, self.endpoint.thinking_template.then_some(false));
        let mut call = self
            .client
            .post(self.endpoint.url_for(self.endpoint.protocol))
            .json(&Value::Object(body));
        if let Some(key) = &self.endpoint.credential.value {
            call = call.bearer_auth(key);
        }
        let response = call
            .send()
            .await
            .map_err(|error| BackendError::Core(CoreError::Http { status: 0, detail: error.to_string() }))?;
        let status = response.status();
        let text =
            response.text().await.map_err(|error| BackendError::Core(CoreError::Incomplete { detail: error.to_string() }))?;
        if !status.is_success() {
            return Err(BackendError::Core(CoreError::Http {
                status: status.as_u16(),
                detail: redact(text.trim(), &self.endpoint.credential),
            }));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| BackendError::Core(CoreError::Incomplete { detail: error.to_string() }))?;
        let answer = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                let output = value.get("output")?.as_array()?;
                let texts: Vec<&str> = output
                    .iter()
                    .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
                    .flat_map(|item| item.get("content").and_then(Value::as_array).into_iter().flatten())
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect();
                if texts.is_empty() {
                    return None;
                }
                Some(texts.join(""))
            })
            .ok_or(BackendError::Core(CoreError::Unanswered { request: "completeOnce" }))?;
        let usage = value.get("usage").and_then(Value::as_object).map(crate::openai::parse_usage);
        let mut turn = Turn::text(answer);
        let finish = match self.endpoint.protocol {
            Protocol::Responses => value.get("status").and_then(Value::as_str).map(|status| (status, "completed")),
            Protocol::ChatCompletions => value.pointer("/choices/0/finish_reason").and_then(Value::as_str)
                .map(|reason| (reason, "stop")),
        };
        if finish.is_none_or(|(actual, expected)| actual != expected) {
            turn.final_event = None;
            turn.incomplete_reason = value.pointer("/incomplete_details/reason").and_then(Value::as_str)
                .or(finish.map(|(actual, _)| actual)).map(str::to_owned);
        }
        turn.response_id = value.get("id").and_then(Value::as_str).map(str::to_string);
        turn.usage = usage;
        Ok(turn)
    }

}

impl Backend for ResponsesBackend {
    fn context_endpoint(&self) -> Option<(&str, &str)> {
        Some((&self.inner.endpoint.base_url, &self.inner.endpoint.model))
    }

    fn complete_once(&self, system: &str, user_text: &str, max_tokens: u32) -> Result<Turn, BackendError> {
        http_runtime().block_on(self.inner.complete_once(system, user_text, max_tokens))
    }

    fn send(&self, request: SendRequest) -> Result<TurnHandle, BackendError> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let turn_id = request.turn_id.clone().unwrap_or_else(|| {
            format!("tu_{}", self.inner.turn_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1)
        });
        let cancelled = Arc::new(Notify::new());
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_turn_id = turn_id.clone();
        let result = Arc::new(Mutex::new(None));
        let worker_result = Arc::clone(&result);
        let shared = Arc::clone(&self.inner);
        http_runtime().spawn(async move {
            shared.run(&request, "th_1", &worker_turn_id, &tx, &worker_cancelled, &worker_result).await;
        });
        Ok(TurnHandle::new(turn_id, rx, |_| {}, move || cancelled.notify_one()).with_result(result))
    }

    fn resolve_approval(&self, _item_id: &str, _decision: baudet_core::ApprovalDecision) {}
}

// A server can echo the credential in an error body.
fn redact(detail: &str, credential: &Credential) -> String {
    match credential.value.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        None => detail.to_string(),
        Some(key) => detail.replace(key, "[redacted]"),
    }
}
