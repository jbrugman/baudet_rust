
use crate::openai::{events_for_payload, request_body, Delta, StreamState, TurnAccumulator};
use crate::protocol::{chat_request_body, chat_summary_body, wire_history, Protocol};
use crate::sse::Framer;
use crate::{Backend, BackendError, SendRequest, TurnHandle};
use baudet_core::{CoreError, ItemStatus, ThreadEvent, Turn};
use serde_json::Value;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// The streaming read timeout re-arms whenever another chunk arrives.
pub const READ_TIMEOUT: Duration = Duration::from_secs(1_800);

const READ_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Default)]
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

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub base_url: String,
    pub credential: Credential,
    pub protocol: Protocol,
    pub model: String,
    pub store: bool,
}

impl Endpoint {
    pub fn url_for(&self, dialect: Protocol) -> String {
        format!("{}{}", self.base_url, dialect.path())
    }
}

struct Shared {
    endpoint: Endpoint,
    client: reqwest::blocking::Client,
    reasoning: Option<String>,
    last: Mutex<Option<String>>,
    turn_counter: std::sync::atomic::AtomicUsize,
}

// Blocking HTTP runs outside the application worker.
fn spawn_turn(
    shared: Arc<Shared>,
    request: SendRequest,
    thread_id: String,
    turn_id: String,
    tx: tokio::sync::mpsc::UnboundedSender<ThreadEvent>,
    cancelled: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Turn>>>,
) {
    std::thread::spawn(move || {
        shared.run(&request, &thread_id, &turn_id, &tx, &cancelled, &result);
    });
}

pub struct ResponsesBackend {
    inner: Arc<Shared>,
}

impl ResponsesBackend {
    pub fn new(endpoint: Endpoint, reasoning: Option<String>) -> Result<ResponsesBackend, BackendError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(READ_TIMEOUT)
            .build()
            .map_err(|error| BackendError::Core(CoreError::Http { status: 0, detail: error.to_string() }))?;
        Ok(ResponsesBackend {
            inner: Arc::new(Shared {
                endpoint,
                client,
                reasoning,
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
    fn run(
        &self,
        request: &SendRequest,
        thread_id: &str,
        turn_id: &str,
        tx: &tokio::sync::mpsc::UnboundedSender<ThreadEvent>,
        cancelled: &AtomicBool,
        result: &Mutex<Option<Turn>>,
    ) {
        let _ = tx.send(ThreadEvent::TurnStarted { thread_id: thread_id.to_string(), turn_id: turn_id.to_string() });
        let outcome = self.stream(request, thread_id, turn_id, tx, cancelled);
        if cancelled.load(Ordering::SeqCst) {
            return;
        }
        match outcome {
            Ok(turn) => {
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

    fn stream(
        &self,
        request: &SendRequest,
        thread_id: &str,
        turn_id: &str,
        tx: &tokio::sync::mpsc::UnboundedSender<ThreadEvent>,
        cancelled: &AtomicBool,
    ) -> Result<Turn, BackendError> {
        let reasoning = request.reasoning.as_deref().or(self.reasoning.as_deref());
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
            .map_err(|error| BackendError::Core(CoreError::Http { status: 0, detail: error.to_string() }))?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().unwrap_or_default();
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
            let detail = response.text().unwrap_or_default();
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
        let mut state = StreamState::new(thread_id);
        let mut framer = Framer::new();
        let mut pending: Vec<u8> = Vec::new();
        let mut buffer = vec![0u8; READ_BYTES];
        let mut terminal_seen = false;

        loop {
            if cancelled.load(Ordering::SeqCst) {
                return Err(BackendError::Abandoned);
            }
            match response.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    pending.extend_from_slice(&buffer[..read]);
                    while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                        let line = pending.drain(..=newline).collect::<Vec<u8>>();
                        let line = String::from_utf8_lossy(&line);
                        let line = line.trim_end_matches(['\r', '\n']);
                        if let Some(event) = framer.push_line(line) {
                            terminal_seen |=
                                self.publish(&event.event, &event.data, &mut accumulator, &mut state, turn_id, tx);
                            if terminal_seen || cancelled.load(Ordering::SeqCst) {
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
            self.publish(&event.event, &event.data, &mut accumulator, &mut state, turn_id, tx);
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

impl Backend for ResponsesBackend {
    fn complete_once(&self, system: &str, user_text: &str, max_tokens: u32) -> Result<Turn, BackendError> {
        let mut body = chat_summary_body(&self.inner.endpoint.model, system, user_text, max_tokens);
        if let Protocol::Responses = self.inner.endpoint.protocol {
            let shape = crate::openai::RequestShape {
                model: &self.inner.endpoint.model,
                system,
                history: &[],
                user_text,
                previous_response_id: None,
                tools: None,
                image: None,
                max_output_tokens: Some(max_tokens.max(1)),
                reasoning: Some(crate::openai::REASONING_NONE),
                store: false,
            };
            body = request_body(&shape);
            body.insert("stream".into(), Value::Bool(false));
        }
        let mut call = self
            .inner
            .client
            .post(self.inner.endpoint.url_for(self.inner.endpoint.protocol))
            .json(&Value::Object(body));
        if let Some(key) = &self.inner.endpoint.credential.value {
            call = call.bearer_auth(key);
        }
        let response = call
            .send()
            .map_err(|error| BackendError::Core(CoreError::Http { status: 0, detail: error.to_string() }))?;
        let status = response.status();
        let text =
            response.text().map_err(|error| BackendError::Core(CoreError::Incomplete { detail: error.to_string() }))?;
        if !status.is_success() {
            return Err(BackendError::Core(CoreError::Http {
                status: status.as_u16(),
                detail: redact(text.trim(), &self.inner.endpoint.credential),
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
        let finish = match self.inner.endpoint.protocol {
            Protocol::Responses => value.get("status").and_then(Value::as_str).map(|status| (status, "completed")),
            Protocol::ChatCompletions => value.pointer("/choices/0/finish_reason").and_then(Value::as_str)
                .map(|reason| (reason, "stop")),
        };
        if !finish.is_some_and(|(actual, expected)| actual == expected) {
            turn.final_event = None;
            turn.incomplete_reason = value.pointer("/incomplete_details/reason").and_then(Value::as_str)
                .or(finish.map(|(actual, _)| actual)).map(str::to_owned);
        }
        turn.response_id = value.get("id").and_then(Value::as_str).map(str::to_string);
        turn.usage = usage;
        Ok(turn)
    }

    fn send(&self, request: SendRequest) -> Result<TurnHandle, BackendError> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let turn_id = format!("tu_{}", self.inner.turn_counter.fetch_add(1, Ordering::SeqCst) + 1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let thread_id = "th_1".to_string();
        // A handle cancels only its own request.
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_turn_id = turn_id.clone();
        let result = Arc::new(Mutex::new(None));
        spawn_turn(Arc::clone(&self.inner), request, thread_id, worker_turn_id, tx, worker_cancelled, Arc::clone(&result));
        let cancel_state = Arc::clone(&cancelled);
        Ok(TurnHandle::new(
            turn_id.clone(),
            rx,
            move |_| {
            },
            move || cancel_state.store(true, Ordering::SeqCst),
        ).with_result(result))
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
