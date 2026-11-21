//! The wire shape of an OpenAI-compatible server, as data rather than as a connection.
//!
//! Translated from `dev.baudet.api.ResponsesClient.buildRequest` and `dev.baudet.api.TurnAccumulator`.
//! Splitting the shape from the socket is what makes the interesting part testable: which event name
//! carries a delta is precisely where local servers deviate from OpenAI's text, and a transport that
//! also decides that would hide its guesses behind a server.
//!
//! Two rules carried over from the original, both earned against real backends:
//!
//!  * an unknown event is counted, never fatal -- at worst one delta is missed, and the census of
//!    what did arrive still says so (see [`TurnAccumulator::events`]);
//!  * a truncation is named. "No answer" and "budget exhausted before the answer started" look
//!    identical on screen and only one of them is fixed by raising the ceiling.

use baudet_core::{FunctionCall, ItemKind, ThreadEvent, Turn, Usage};
use serde_json::{Map, Value};

/// The Responses API path, relative to a base URL that ends in `/v1`.
pub const RESPONSES_PATH: &str = "/responses";

/// What the panel's "reasoning: default" means for a request: send nothing, so a server that
/// reasons by default keeps doing that and one that does not stays quiet.
pub const REASONING_DEFAULT: &str = "";

/// The reasoning ask of a one-off summarising call. The compactor's fold deletes text, so it must
/// not spend its ceiling on hidden thinking -- and the ask is unconditional, because a backend that
/// reasons by default would otherwise eat the whole budget before the summary started.
pub const REASONING_NONE: &str = "none";

/// How much of a first payload is kept as the sample of an event type. Long enough to show the
/// shape behind a name, short enough to print.
pub const SAMPLE_LIMIT: usize = 700;

/// Everything a request may name. Kept flat, so a caller sets only what it knows and the builder
/// decides what the wire carries: `None` and an empty string both mean "the server decides", and
/// neither is sent -- sending a zero ceiling is not the same as sending no ceiling, it caps the
/// answer at nothing.
#[derive(Debug, Clone, Default)]
pub struct RequestShape<'a> {
    pub model: &'a str,
    pub system: &'a str,
    /// Newest last, as `SendRequest::history` holds it. Skipped entirely when
    /// `previous_response_id` is set, because then the server already has it.
    pub history: &'a [baudet_core::Msg],
    pub user_text: &'a str,
    pub previous_response_id: Option<&'a str>,
    /// Enabled function definitions, already a JSON array as text.
    pub tools: Option<&'a str>,
    /// A base64 data URL riding the first request of a turn.
    pub image: Option<&'a str>,
    pub max_output_tokens: Option<u32>,
    /// One of `REASONING_*` or an effort the server names (`low`, `minimal`, ...).
    pub reasoning: Option<&'a str>,
    /// Whether the server keeps the response for `previous_response_id` chaining.
    pub store: bool,
}

/// The whole body of a streaming `/responses` request, in one of its two shapes: a chained turn
/// names the server's own session, an unchained one replays the transcript itself.
///
/// `stream` is always on. A server that cannot stream is a protocol the protocol-detection crate
/// will name, not a field to flip here.
pub fn request_body(shape: &RequestShape<'_>) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("model".into(), shape.model.into());
    if !shape.system.is_empty() {
        body.insert("instructions".into(), shape.system.into());
    }
    body.insert("stream".into(), Value::Bool(true));
    body.insert("store".into(), Value::Bool(shape.store));
    if let Some(tokens) = shape.max_output_tokens.filter(|t| *t > 0) {
        body.insert("max_output_tokens".into(), Value::from(tokens));
    }
    if let Some(effort) = shape.reasoning.filter(|e| !e.is_empty()) {
        let mut reasoning = Map::new();
        reasoning.insert("effort".into(), effort.into());
        body.insert("reasoning".into(), Value::Object(reasoning));
    }
    // A tools string that will not parse as an array is skipped rather than sent half-formed: a
    // server rejecting the whole body over one malformed definition loses the turn as well.
    if let Some(tools) = shape.tools {
        if let Ok(Value::Array(_)) = serde_json::from_str::<Value>(tools) {
            // Reached only by the parse that just proved it is an array.
            let parsed = serde_json::from_str::<Value>(tools).expect("an array just parsed above");
            body.insert("tools".into(), parsed);
        }
    }
    let chained = shape.previous_response_id.is_some();
    if let Some(previous) = shape.previous_response_id {
        body.insert("previous_response_id".into(), previous.into());
    }
    let mut input = Vec::new();
    if !chained {
        // `system` rides as `instructions`, so replaying it per turn would state it twice.
        for message in shape.history {
            if message.role != "system" {
                input.push(text_message(&message.role, &message.content));
            }
        }
    }
    input.push(match shape.image {
        None => text_message("user", shape.user_text),
        Some(image) => {
            let mut parts = Vec::new();
            if !shape.user_text.is_empty() {
                let mut part = Map::new();
                part.insert("type".into(), Value::String("input_text".into()));
                part.insert("text".into(), shape.user_text.into());
                parts.push(Value::Object(part));
            }
            let mut part = Map::new();
            part.insert("type".into(), Value::String("input_image".into()));
            part.insert("image_url".into(), image.into());
            parts.push(Value::Object(part));
            let mut message = Map::new();
            message.insert("role".into(), Value::String("user".into()));
            message.insert("content".into(), Value::Array(parts));
            Value::Object(message)
        }
    });
    body.insert("input".into(), Value::Array(input));
    body
}

fn text_message(role: &str, content: &str) -> Value {
    let mut message = Map::new();
    message.insert("role".into(), role.into());
    message.insert("content".into(), content.into());
    Value::Object(message)
}

/// What one payload carried, if anything. Kept separate from [`TurnAccumulator`] so a caller can
/// append to a card while the rest of the payload is bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    pub text: Option<String>,
    pub reasoning: Option<String>,
}

/// The census entry for a payload that was not JSON at all.
pub const NOT_JSON: &str = "(not-json)";
/// The census entry for a payload that carried no name anywhere.
pub const NO_TYPE: &str = "(no-type)";

/// Reduce framed payloads into one turn.
///
/// The tolerance is the feature. `data.type` wins because that is what the Responses API specifies,
/// then the `event:` field because some servers carry the name nowhere else, then `object` because
/// that is all a `chat.completion.chunk` brings. Anything else is counted and sampled, so the
/// census of a turn shows what a server really sent even when none of it was understood.
#[derive(Debug, Default)]
pub struct TurnAccumulator {
    text: String,
    reasoning: String,
    calls: Vec<FunctionCall>,
    events: Vec<(String, u32)>,
    samples: Vec<(String, String)>,
    response_id: Option<String>,
    usage: Option<Usage>,
    final_event: Option<String>,
    incomplete_reason: Option<String>,
    stream_error: Option<String>,
    event_count: u32,
}

impl TurnAccumulator {
    pub fn new() -> TurnAccumulator {
        TurnAccumulator::default()
    }

    /// Fold one framed payload. `event` is the framed `event:` name, if any.
    pub fn feed(&mut self, event: Option<&str>, data: &str) -> Option<Delta> {
        self.event_count += 1;
        let Ok(parsed) = serde_json::from_str::<Value>(data) else {
            self.record(NOT_JSON, data);
            return None;
        };
        let object = match &parsed {
            Value::Object(object) => object.clone(),
            // A bare array or string is a payload too; it just has no fields to read.
            other => {
                self.record(NO_TYPE, data);
                let _ = other;
                return None;
            }
        };
        // Kept for the one message that must quote the whole payload back: a server's error means
        // whatever sits beside its name, and a Map has no JSON spelling of its own.
        let payload = parsed.clone();
        let name = string_field(&object, "type")
            .or_else(|| event.map(str::to_string))
            .or_else(|| string_field(&object, "object"))
            .unwrap_or_else(|| NO_TYPE.to_string());
        self.record(&name, data);
        let delta = self.extract(&name, &object, &payload);
        if let Some(delta) = &delta {
            if let Some(text) = &delta.text {
                self.text.push_str(text);
            }
            if let Some(reasoning) = &delta.reasoning {
                self.reasoning.push_str(reasoning);
            }
        }
        delta
    }

    /// The finished turn, exactly as the stream produced it -- including the census, so a caller can
    /// show that a server spoke a dialect rather than merely that an answer looked short.
    pub fn finish(self) -> Turn {
        let event_count = self.event_count;
        Turn {
            text: self.text,
            response_id: self.response_id,
            usage: self.usage,
            final_event: Some(self.final_event.unwrap_or_else(|| format!("stream_end_after_{event_count} events"))),
            events: self.events,
            samples: self.samples,
            reasoning: self.reasoning,
            incomplete_reason: self.incomplete_reason.or_else(|| self.stream_error),
            function_calls: self.calls,
        }
    }

    /// How many payloads have been folded, so a caller can tell "no events" from "events, no text".
    pub fn event_count(&self) -> u32 {
        self.event_count
    }

    /// The name the server finished with, once a terminal event has been seen.
    pub fn final_event(&self) -> Option<&str> {
        self.final_event.as_deref()
    }

    /// A stream-level error the server reported, which is not the same as an empty answer.
    pub fn stream_error(&self) -> Option<&str> {
        self.stream_error.as_deref()
    }

    fn extract(&mut self, name: &str, object: &Map<String, Value>, payload: &Value) -> Option<Delta> {
        let delta = string_field(object, "delta");
        if name.ends_with("output_text.delta") {
            return delta.map(|text| Delta { text: Some(text), reasoning: None });
        }
        // Reasoning is the bulk of what a local reasoning model spends its ceiling on. Discarding it
        // leaves an empty answer that only reports tokens burned, so it is captured and shown.
        if name.ends_with("reasoning_summary_text.delta") || name.ends_with("reasoning_text.delta") {
            return delta.map(|reasoning| Delta { text: None, reasoning: Some(reasoning) });
        }
        if name == "chat.completion.chunk" {
            let choice =
                object.get("choices").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_object);
            let content = choice
                .and_then(|c| c.get("delta").or_else(|| c.get("message")))
                .and_then(Value::as_object)
                .and_then(|d| string_field(d, "content"));
            return content.map(|text| Delta { text: Some(text), reasoning: None });
        }
        if name == "error" {
            // The whole payload, as the original does: an error's meaning is usually in the fields
            // beside the name, and a map has no JSON spelling of its own.
            // The whole payload, as the original does: the meaning of an error is usually in the
            // fields beside its name, so quoting only the name would quote nothing.
            self.stream_error = Some(payload.to_string());
            return None;
        }
        if name.starts_with("response.") {
            self.absorb(name, object);
        }
        None
    }

    /// Response-level bookkeeping: the id, the usage, why the run ended, any function calls.
    fn absorb(&mut self, name: &str, object: &Map<String, Value>) {
        // Some servers wrap the response, some hand it over bare.
        let response = object.get("response").and_then(Value::as_object).cloned().unwrap_or_else(|| object.clone());
        if let Some(id) = string_field(&response, "id") {
            self.response_id = Some(id);
        }
        if let Some(usage) = response.get("usage").and_then(Value::as_object) {
            self.usage = Some(parse_usage(usage));
        }
        if name == "response.completed" || name == "response.incomplete" || name == "response.failed" {
            self.final_event = Some(name.to_string());
            if name != "response.completed" {
                self.incomplete_reason = response
                    .get("incomplete_details")
                    .and_then(Value::as_object)
                    .and_then(|d| string_field(d, "reason"))
                    .or_else(|| {
                        response
                            .get("error")
                            .and_then(Value::as_object)
                            .and_then(|e| string_field(e, "code").or_else(|| string_field(e, "message")))
                    })
                    .or_else(|| Some(name.to_string()));
            }
        }
        // Native calls ride two shapes: the finished response carries them in its output array, and
        // some servers surface each item on its own done event. Both are read; the dedup in
        // `collect_call` keeps that from counting one call twice.
        if let Some(Value::Array(output)) = response.get("output") {
            for item in output {
                self.collect_call(item.as_object());
            }
        }
        if name.ends_with("output_item.done") {
            self.collect_call(object.get("item").and_then(Value::as_object));
        }
        // Tolerant fallback: a server may skip deltas and deliver the finished text only. Adopted
        // just when nothing streamed, so nothing is counted twice.
        if name.ends_with("output_text.done") && self.text.is_empty() {
            if let Some(text) = string_field(object, "text") {
                self.text.push_str(&text);
            }
        }
        if name.ends_with("reasoning_summary_text.done") && self.reasoning.is_empty() {
            if let Some(text) = string_field(object, "text") {
                self.reasoning.push_str(&text);
            }
        }
    }

    fn collect_call(&mut self, item: Option<&Map<String, Value>>) {
        let Some(item) = item.filter(|i| string_field(i, "type").as_deref() == Some("function_call")) else {
            return;
        };
        let Some(name) = string_field(item, "name") else { return };
        let call_id = string_field(item, "call_id").or_else(|| string_field(item, "id"));
        let arguments =
            string_field(item, "arguments").or_else(|| item.get("arguments").map(Value::to_string)).unwrap_or_default();
        let call = FunctionCall { call_id, name, arguments };
        let seen = self.calls.iter().any(|existing| match (&call.call_id, &existing.call_id) {
            (Some(id), Some(seen)) => id == seen,
            _ => existing.name == call.name && existing.arguments == call.arguments,
        });
        if !seen {
            self.calls.push(call);
        }
    }

    fn record(&mut self, name: &str, payload: &str) {
        match self.events.iter_mut().find(|(seen, _)| seen == name) {
            Some((_, count)) => *count += 1,
            None => {
                self.events.push((name.to_string(), 1));
                self.samples.push((name.to_string(), payload.chars().take(SAMPLE_LIMIT).collect()));
            }
        }
    }
}

fn string_field(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Token accounting, reading both vocabularies: `input_tokens` is what the Responses API calls it,
/// `prompt_tokens` what a chat-completions server does. A number a server omits stays omitted.
fn parse_usage(usage: &Map<String, Value>) -> Usage {
    let number = |keys: [&str; 2]| keys.into_iter().filter_map(|key| usage.get(key)).find_map(Value::as_i64);
    Usage::new(
        number(["input_tokens", "prompt_tokens"]),
        number(["output_tokens", "completion_tokens"]),
        usage.get("total_tokens").and_then(Value::as_i64),
    )
}

/// The events one payload produced, in the order a listener should apply them.
///
/// Lives apart from [`TurnAccumulator`] because the two see different things: the accumulator needs
/// the whole census, the turn's events need only open and close the rows a payload touched. The
/// item id is derived from the turn id rather than the payload's, so answer and reasoning each own
/// exactly one row however a server chooses to split or name its chunks.
pub fn events_for_payload(state: &mut StreamState, turn_id: &str, delta: &Delta) -> Vec<ThreadEvent> {
    let mut events = Vec::new();
    for (kind, text) in [(ItemKind::AgentMessage, &delta.text), (ItemKind::Reasoning, &delta.reasoning)] {
        let Some(text) = text else { continue };
        let item_id = state.item_id(turn_id, kind);
        if !state.opened(kind) {
            events.push(ThreadEvent::ItemStarted {
                thread_id: state.thread_id.clone(),
                turn_id: turn_id.to_string(),
                item: match kind {
                    ItemKind::Reasoning => baudet_core::Item::Reasoning(baudet_core::ReasoningItem {
                        item_id: item_id.clone(),
                        text: String::new(),
                        status: baudet_core::ItemStatus::InProgress,
                    }),
                    _ => baudet_core::Item::AgentMessage(baudet_core::AgentMessageItem {
                        item_id: item_id.clone(),
                        text: String::new(),
                        status: baudet_core::ItemStatus::InProgress,
                    }),
                },
            });
        }
        events.push(ThreadEvent::ItemDelta {
            thread_id: state.thread_id.clone(),
            turn_id: turn_id.to_string(),
            item_id,
            kind,
            delta: text.clone(),
        });
    }
    events
}

/// Which rows a turn has opened, so [`events_for_payload`] announces each exactly once.
#[derive(Debug, Clone)]
pub struct StreamState {
    pub thread_id: String,
    answer_open: bool,
    reasoning_open: bool,
}

impl StreamState {
    pub fn new(thread_id: impl Into<String>) -> StreamState {
        StreamState { thread_id: thread_id.into(), answer_open: false, reasoning_open: false }
    }

    pub fn item_id(&self, turn_id: &str, kind: ItemKind) -> String {
        match kind {
            ItemKind::Reasoning => format!("{turn_id}/reasoning"),
            _ => format!("{turn_id}/msg"),
        }
    }

    fn opened(&mut self, kind: ItemKind) -> bool {
        match kind {
            ItemKind::Reasoning => std::mem::replace(&mut self.reasoning_open, true),
            _ => std::mem::replace(&mut self.answer_open, true),
        }
    }

    /// Close every open row, completed or interrupted, which is what a listener needs to stop
    /// showing a spinner. Returns the events; the caller owns which terminal status applies.
    pub fn close_open_rows(&mut self, turn_id: &str, status: baudet_core::ItemStatus) -> Vec<ThreadEvent> {
        let mut events = Vec::new();
        for (kind, open) in [(ItemKind::Reasoning, self.reasoning_open), (ItemKind::AgentMessage, self.answer_open)] {
            if !open {
                continue;
            }
            let item_id = self.item_id(turn_id, kind);
            events.push(ThreadEvent::ItemCompleted {
                thread_id: self.thread_id.clone(),
                turn_id: turn_id.to_string(),
                item: match kind {
                    ItemKind::Reasoning => baudet_core::Item::Reasoning(baudet_core::ReasoningItem {
                        item_id,
                        text: String::new(),
                        status,
                    }),
                    _ => baudet_core::Item::AgentMessage(baudet_core::AgentMessageItem {
                        item_id,
                        text: String::new(),
                        status,
                    }),
                },
            });
        }
        self.answer_open = false;
        self.reasoning_open = false;
        events
    }

    /// Whether the turn ever opened a row, so "the server streamed nothing" can be told apart from
    /// "the server streamed nothing *useful*".
    pub fn any_open(&self) -> bool {
        self.answer_open || self.reasoning_open
    }
}
