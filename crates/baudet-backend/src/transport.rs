//! The connection: turn a request into a live stream of [`ThreadEvent`]s.
//!
//! Everything that decides *what to send* and *what a payload means* lives beside this file, in
//! [`crate::openai`] and [`crate::protocol`], precisely so that this file can be about the one thing
//! a socket actually forces on you: bytes arriving in pieces that do not line up with frames, with a
//! silence between some of them, and an end that has to be distinguished from a failure.
//!
//! The stack is `reqwest::blocking` read through `std::io::Read`, and the reasons are measured rather
//! than stylistic (all 2026-11-21, in `tests/slow_server.rs`):
//!
//!  * **A deadline bounds one read, not the answer.** The blocking client hands `timeout` to
//!    `wait::timeout`, which computes `Instant::now() + d` on every call, so a reasoning model that
//!    is quiet for two minutes before its first token keeps its turn. Same source as the async
//!    stack's `read_timeout` -- neither stack dies on a slow stream -- so the choice here is not
//!    about survival.
//!  * **Blocking is chosen for what happens after the last byte.** `Response::chunk()` in the async
//!    stack clears its timeout `Sleep` only on a chunk, never on the end of the stream, so a finished
//!    stream leaves an armed timer on the pooled connection that the *next* stream inherits; 64 KiB
//!    of body, which is ordinary for an SSE stream, sits inside that buffer. The blocking read path
//!    parks a thread instead of arming a timer, so nothing outlives the turn. It also means no
//!    runtime: the app already drives turns on its own threads.
//!  * **A read returns whatever has arrived, not one frame.** So the buffer is sized for a whole
//!    delta; a 1-byte buffer measured 12 bytes of the first delta and then nothing. Bytes go to the
//!    framer, which is the only thing that knows where a frame ends -- the same [`Framer`] the
//!    scripted player runs through, so a wire and a script cannot be framed by two sets of rules.
//!  * **An end of stream is an ordinary ending.** A server that closes its chunked body properly ends
//!    the read with `Ok(0)`, measured; the earlier note in `slow_server.rs` claiming the terminator
//!    costs a decoder error was measuring its own fixture hanging up mid-read. So `Ok(0)` is not a
//!    failure, and a turn the server finished answering is never marked failed because it said
//!    goodbye correctly.

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

/// The ceiling on one read, and deliberately not a ceiling on a turn.
///
/// A live stream re-arms it (see the module docs), so what it can end is a server that accepted the
/// request and then stopped talking without hanging up. The reference app uses thirty minutes for the
/// same purpose (`Http.kt:30`), and that is the number kept: short enough that a wedged local server
/// does not hang a turn for an hour, long enough that a model thinking at two tokens a second on a
/// laptop is never mistaken for one.
pub const READ_TIMEOUT: Duration = Duration::from_secs(1_800);

/// How much of the body to ask for per read.
///
/// Sized to hold a delta and its neighbour, not one frame: the measured shape of a read is "however
/// much has arrived", so a buffer that cannot hold what has arrived is the only way to split a frame
/// by accident. Frames are split by the framer, not by read boundaries.
const READ_BYTES: usize = 64 * 1024;

/// The credential, kept as a name plus an optional value so a line of log can say where it came from
/// without ever carrying the secret itself.
#[derive(Debug, Clone, Default)]
pub struct Credential {
    /// The value to send, or `None` for "this server takes no key" -- which is a statement about the
    /// server, not an absence of care: several local servers answer without one and a header they do
    /// not expect is a behaviour change nobody asked for.
    pub value: Option<String>,
    /// Where the value came from: the settings field, or the environment variable named after it.
    /// Shown in the scan's notes so a 401 can be traced to a variable without printing its contents.
    pub source: String,
}

impl Credential {
    pub fn none() -> Credential {
        Credential { value: None, source: "none".into() }
    }

    /// A key the person typed.
    pub fn typed(value: impl Into<String>) -> Credential {
        let value = value.into();
        Credential { value: Some(value), source: "settings".into() }
    }

    /// A key read from the environment variable `name`. The name is what travels into notes.
    pub fn from_env(name: &str, value: Option<String>) -> Credential {
        match value.filter(|v| !v.trim().is_empty()) {
            None => Credential { value: None, source: format!("{name} (unset)") },
            Some(value) => Credential { value: Some(value), source: name.to_string() },
        }
    }
}

/// Where a turn goes.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// The normalised base, which must already end in `/v1`; [`crate::protocol::scan`] returns one,
    /// and `baudet_settings::Provider::base_url` is the other producer of the same shape.
    pub base_url: String,
    pub credential: Credential,
    pub protocol: Protocol,
    /// The model to name in a request. Required, because a probe and a turn both have to name one and
    /// inventing a default here would ask a server for a model this machine may have to load.
    pub model: String,
    /// Whether the server keeps responses for `previous_response_id`. Only meaningful for
    /// [`Protocol::Responses`]; a chat-completions server holds nothing between turns.
    pub store: bool,
}

impl Endpoint {
    /// The URL a turn of `dialect` goes to, without consulting a backend.
    pub fn url_for(&self, dialect: Protocol) -> String {
        format!("{}{}", self.base_url, dialect.path())
    }
}

/// What a turn thread reads. One of these per configured endpoint.
///
/// `std::thread::spawn` demands `'static`, so a `&self` cannot travel into a turn: that is the
/// compiler saying out loud that a window can close while its turn is still reading a socket. The
/// answer is not a lifetime argument but a shared owner — the endpoint a person configured is one
/// object, the turns in flight are however many there are. `ScriptedBackend` gets this for free
/// because a script owns its steps outright; a socket does not.
struct Shared {
    endpoint: Endpoint,
    client: reqwest::blocking::Client,
    /// The reasoning level from settings, applied to a turn unless the turn overrides it.
    reasoning: Option<String>,
    /// The last turn's terminal event, for diagnostics; see [`ResponsesBackend::last_final_event`].
    last: Mutex<Option<String>>,
    /// Names turns `tu_1`, `tu_2`, … exactly as `ScriptedBackend` does, so the id a row is keyed on
    /// looks the same whether it came from a wire or from a script. Shared because ids have to be
    /// unique across every clone of an endpoint, not merely within one.
    turn_counter: std::sync::atomic::AtomicUsize,
}

/// Read one turn to its end, on its own thread.
fn spawn_turn(
    shared: Arc<Shared>,
    request: SendRequest,
    thread_id: String,
    turn_id: String,
    tx: tokio::sync::mpsc::UnboundedSender<ThreadEvent>,
    cancelled: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        // The turn publishes its own terminal event, whether it answered, was stopped, or failed;
        // the sender dropping when this closure ends is what closes the stream.
        shared.run(&request, &thread_id, &turn_id, &tx, &cancelled);
    });
}

/// The live transport.
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

    /// The dialect this backend sends to.
    pub fn protocol(&self) -> Protocol {
        self.inner.endpoint.protocol
    }

    /// The name of the last turn's terminal event, or `None` while one is still running.
    ///
    /// Kept because the honest summary of a turn that produced no text is a fact about the server --
    /// which event it ended on -- and a caller that renders "empty answer" without it cannot say
    /// whether the ceiling was reached or the model simply had nothing to add.
    pub fn last_final_event(&self) -> Option<String> {
        self.inner.last.lock().unwrap().clone()
    }
}

impl Shared {
    /// Send a turn and read its stream to the end. Runs on the turn's own thread; [`spawn_turn`]
    /// owns the sharing that makes that legal.
    fn run(&self, request: &SendRequest, thread_id: &str, turn_id: &str, tx: &tokio::sync::mpsc::UnboundedSender<ThreadEvent>, cancelled: &AtomicBool) {
        let _ = tx.send(ThreadEvent::TurnStarted { thread_id: thread_id.to_string(), turn_id: turn_id.to_string() });
        let outcome = self.stream(request, thread_id, turn_id, tx, cancelled);
        if cancelled.load(Ordering::SeqCst) {
            // Deliberate Stop. `AppState::cancel_turn` already closes the rows it can see as
            // interrupted and the scripted player publishes nothing here either, so the transport
            // stays silent rather than racing the orchestrator over who owns the interrupted close.
            return;
        }
        match outcome {
            Ok(turn) => {
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
                // A failed turn still has to close, and its rows still have to stop spinning: a
                // listener cleaning up cards cannot wait for a backend that has already given up.
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

    /// One request, one stream, folded into a turn.
    fn stream(&self, request: &SendRequest, thread_id: &str, turn_id: &str, tx: &tokio::sync::mpsc::UnboundedSender<ThreadEvent>, cancelled: &AtomicBool) -> Result<Turn, BackendError> {
        let reasoning = request.reasoning.as_deref().or(self.reasoning.as_deref());
        let model = request.model.as_deref().filter(|m| !m.trim().is_empty()).unwrap_or(&self.endpoint.model);
        if model.trim().is_empty() {
            // Both dialects require a model name, and leaving it out lets a server answer with
            // whatever it happens to have loaded — which is the one thing this machine's resident
            // model rule exists to prevent. So an unnamed turn is refused here, in the terminal
            // event the reducer already renders as an error row, rather than sent as a guess.
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
        // An SSE body must be read as it arrives, so a per-request whole-response timeout would be
        // the wrong instrument even though the blocking client's `timeout` re-arms per read.
        // `mut` is not tidiness: `Read::read` takes `&mut self`, and dropping it is the only way to
        // learn that by compiler rather than by habit.
        let mut response = call
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .map_err(|error| BackendError::Core(CoreError::Http { status: 0, detail: error.to_string() }))?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().unwrap_or_default();
            return Err(BackendError::Core(CoreError::Http { status: status.as_u16(), detail: redact(detail.trim(), &self.endpoint.credential) }));
        }
        // A success status is not a success: measured, LM Studio answers `POST /responses` with 200
        // and an error body, so a status test alone would read that as a turn and report a turn that
        // produced nothing. A non-streaming error body is named here, once, before the byte loop.
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if !content_type.contains("text/event-stream") {
            let detail = response.text().unwrap_or_default();
            let detail = redact(detail.trim(), &self.endpoint.credential);
            // Measured: a refusal's words are the only evidence there is, and a JSON body fits in a
            // row. A whole HTML page is not evidence, it is noise, so only a body that parses as JSON
            // is carried — the rest is named by its type and length.
            let looks_like_json = detail.starts_with('{') || detail.starts_with('[');
            let said = if looks_like_json {
                detail.chars().take(400).collect::<String>()
            } else {
                format!("a {} body of {} bytes", content_type.split(';').next().unwrap_or("empty"), detail.chars().count())
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
                // Dropping the response here is what closes the connection; the caller owns the
                // interrupted close, so nothing is published from this branch.
                return Err(BackendError::Abandoned);
            }
            match response.read(&mut buffer) {
                Ok(0) => break, // the server ended its body: an ordinary ending, measured
                Ok(read) => {
                    pending.extend_from_slice(&buffer[..read]);
                    // Only whole lines are framed; a newline inside a UTF-8 character has no byte
                    // that looks like one, so cutting on `\n` cannot split a character.
                    while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                        let line = pending.drain(..=newline).collect::<Vec<u8>>();
                        let line = String::from_utf8_lossy(&line);
                        let line = line.trim_end_matches(['\r', '\n']);
                        // `data: [DONE]` never reaches `publish`: the framer drops it as a terminator
                        // rather than a payload (sse.rs, and `sse_framing.rs` protects that). So
                        // nothing here stops at it, which is what the measurement below is about.
                        if let Some(event) = framer.push_line(line) {
                            terminal_seen |= self.publish(&event.event, &event.data, &mut accumulator, &mut state, turn_id, tx);
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
                    // A read error after a terminal event is the server hanging up while we already
                    // have its answer; that is an ending, not a failure.
                    if terminal_seen {
                        break;
                    }
                    return Err(BackendError::Core(CoreError::Incomplete { detail: redact(&error.to_string(), &self.endpoint.credential) }));
                }
            }
        }
        if let Some(event) = framer.finish() {
            self.publish(&event.event, &event.data, &mut accumulator, &mut state, turn_id, tx);
        }
        // Measured against LM Studio (2026-11-21), which is the server this app actually runs
        // against: a chat-completions stream sends its usage chunk, then `data: [DONE]`, then closes
        // the body. The framer drops `[DONE]`, so the only terminal this dialect ever reaches is the
        // clean EOF above — and by then the usage chunk has been folded. Stopping at `[DONE]` would
        // have thrown away the turn's token counts and named the turn
        // `stream_end_after_N events` for a server that said exactly what it spent. Stopping at a
        // Responses terminal still short-circuits, because there the terminal *is* the last payload.
        let turn = accumulator.finish();
        // The answer existed and the rows are still open, so they close here. An empty answer with a
        // terminal event (a model that refused, a `content_filter` truncation) must not leave a
        // spinner, and `AppState::cancel_turn`'s sweep covers only the turn the user stopped.
        let _ = state.close_open_rows(turn_id, ItemStatus::Completed);
        Ok(turn)
    }

    /// Fold one framed payload and publish the rows it opened or extended. Returns whether the
    /// payload was a Responses terminal, which is the only payload that ends a stream early; the
    /// chat dialect ends at the body's end, for the reason documented in [`Self::stream`].
    fn publish(&self, event: &Option<String>, data: &str, accumulator: &mut TurnAccumulator, state: &mut StreamState, turn_id: &str, tx: &tokio::sync::mpsc::UnboundedSender<ThreadEvent>) -> bool {
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
    /// One whole answer, no stream: what the compactor asks for when a folded summary must either
    /// exist in full or not at all.
    ///
    /// Non-streaming is the request, not a stream read to the end, because a fold has nothing to
    /// show while it runs. `chat_summary_body` forces reasoning off for the same reason: a model
    /// that spends its ceiling on hidden thinking before the summary starts leaves no summary.
    fn complete_once(&self, system: &str, user_text: &str, max_tokens: u32) -> Result<Turn, BackendError> {
        let mut body = chat_summary_body(&self.inner.endpoint.model, system, user_text, max_tokens);
        if let Protocol::Responses = self.inner.endpoint.protocol {
            // The Responses dialect has its own shape and its own word for the ceiling; sending a
            // chat body to it would be answered the way `/responses` answers a chat server.
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
        let mut call = self.inner.client.post(self.inner.endpoint.url_for(self.inner.endpoint.protocol)).json(&Value::Object(body));
        if let Some(key) = &self.inner.endpoint.credential.value {
            call = call.bearer_auth(key);
        }
        let response = call
            .send()
            .map_err(|error| BackendError::Core(CoreError::Http { status: 0, detail: error.to_string() }))?;
        let status = response.status();
        let text = response
            .text()
            .map_err(|error| BackendError::Core(CoreError::Incomplete { detail: error.to_string() }))?;
        if !status.is_success() {
            return Err(BackendError::Core(CoreError::Http {
                status: status.as_u16(),
                detail: redact(text.trim(), &self.inner.endpoint.credential),
            }));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| BackendError::Core(CoreError::Incomplete { detail: error.to_string() }))?;
        // The same body rule as everywhere else in this crate: a status is not an answer. Measured,
        // this server's non-streaming reply carries the prose in `choices[0].message.content`, while
        // a Responses gateway carries it in an `output` array; both are read, in that order.
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
        turn.response_id = value.get("id").and_then(Value::as_str).map(str::to_string);
        turn.usage = usage;
        Ok(turn)
    }

    /// Nothing pending to answer: neither dialect measured here asks the client's permission in
    /// mid-turn, so no `approval/requested` ever arrives over this transport. Empty is complete
    /// here, not unfinished -- the scripted backend, which does replay such an event, owns that path.
    fn send(&self, request: SendRequest) -> Result<TurnHandle, BackendError> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let turn_id = format!("tu_{}", self.inner.turn_counter.fetch_add(1, Ordering::SeqCst) + 1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let thread_id = "th_1".to_string();
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_turn_id = turn_id.clone();
        spawn_turn(Arc::clone(&self.inner), request, thread_id, worker_turn_id, tx, worker_cancelled);
        let cancel_state = Arc::clone(&cancelled);
        Ok(TurnHandle::new(
            // One name for two readers: the handle's `turn_id` is what a caller shows, while the
            // events carry the id the reducer keys its rows on. They agree because both are built
            // here, once, from the same counter, and `thread_id` is the same constant `AppState`
            // uses for its own rows -- a second value here would open rows nobody closes.
            turn_id.clone(),
            rx,
            move |_| {
                // Steering is not part of either dialect measured here: a turn in flight is one
                // HTTP request, and neither server offers a way to hand it new text. Saying nothing
                // is therefore the honest answer, and Stop is the control that does exist.
            },
            move || cancel_state.store(true, Ordering::SeqCst),
        ))
    }

    fn resolve_approval(&self, _item_id: &str, _decision: baudet_core::ApprovalDecision) {}
}

/// Cut the credential out of a server's own words before they reach a row or a log. A gateway that
/// echoes request context in its error would otherwise put the key on screen; the listing does the
/// same before it truncates, for the same reason.
fn redact(detail: &str, credential: &Credential) -> String {
    match credential.value.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        // An empty needle is guarded on purpose: `str::replace` would insert the marker between
        // every character of the text.
        None => detail.to_string(),
        Some(key) => detail.replace(key, "[redacted]"),
    }
}
