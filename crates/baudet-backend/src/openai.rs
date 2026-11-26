
use baudet_core::{FunctionCall, ItemKind, ThreadEvent, Turn, Usage};
use serde_json::{Map, Value};

pub const RESPONSES_PATH: &str = "/responses";

pub const REASONING_DEFAULT: &str = "";

pub const REASONING_NONE: &str = "none";

pub const SAMPLE_LIMIT: usize = 700;

#[derive(Debug, Clone, Default)]
pub struct RequestShape<'a> {
    pub model: &'a str,
    pub system: &'a str,
    pub history: &'a [baudet_core::Msg],
    pub user_text: &'a str,
    pub previous_response_id: Option<&'a str>,
    pub tools: Option<&'a str>,
    pub image: Option<&'a str>,
    pub max_output_tokens: Option<u32>,
    pub reasoning: Option<&'a str>,
    pub enable_thinking: Option<bool>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub store: bool,
}

// Responses tools and images use a different shape from the chat API.
pub fn request_body(shape: &RequestShape<'_>) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("model".into(), shape.model.into());
    if !shape.system.is_empty() {
        body.insert("instructions".into(), shape.system.into());
    }
    body.insert("stream".into(), Value::Bool(true));
    body.insert("store".into(), Value::Bool(shape.store));
    insert_sampling(&mut body, shape);
    insert_thinking_template(&mut body, shape.enable_thinking);
    if let Some(tokens) = shape.max_output_tokens.filter(|t| *t > 0) {
        body.insert("max_output_tokens".into(), Value::from(tokens));
    }
    if let Some(effort) = shape.reasoning.filter(|e| !e.is_empty()) {
        let mut reasoning = Map::new();
        reasoning.insert("effort".into(), effort.into());
        if effort != REASONING_NONE {
            reasoning.insert("summary".into(), "auto".into());
        }
        body.insert("reasoning".into(), Value::Object(reasoning));
    }
    if let Some(tools) = shape.tools {
        if let Ok(Value::Array(_)) = serde_json::from_str::<Value>(tools) {
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

pub(crate) fn insert_thinking_template(body: &mut Map<String, Value>, enabled: Option<bool>) {
    if let Some(enabled) = enabled {
        body.insert("chat_template_kwargs".into(), serde_json::json!({ "enable_thinking": enabled }));
    }
}

pub(crate) fn insert_sampling(body: &mut Map<String, Value>, shape: &RequestShape<'_>) {
    for (name, setting) in [("temperature", shape.temperature), ("top_p", shape.top_p)] {
        if let Some(value) = setting.and_then(serde_json::Number::from_f64) {
            body.insert(name.into(), Value::Number(value));
        }
    }
}

fn text_message(role: &str, content: &str) -> Value {
    let mut message = Map::new();
    message.insert("role".into(), role.into());
    message.insert("content".into(), content.into());
    Value::Object(message)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    pub text: Option<String>,
    pub reasoning: Option<String>,
}

pub const NOT_JSON: &str = "(not-json)";
pub const NO_TYPE: &str = "(no-type)";

#[derive(Debug, Default)]
pub struct TurnAccumulator {
    text: String,
    reasoning: String,
    calls: Vec<FunctionCall>,
    chat_calls: std::collections::BTreeMap<u64, FunctionCall>,
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

    pub fn feed(&mut self, event: Option<&str>, data: &str) -> Option<Delta> {
        self.event_count += 1;
        let Ok(parsed) = serde_json::from_str::<Value>(data) else {
            self.record(NOT_JSON, data);
            return None;
        };
        let object = match &parsed {
            Value::Object(object) => object.clone(),
            other => {
                self.record(NO_TYPE, data);
                let _ = other;
                return None;
            }
        };
        let payload = parsed.clone();
        let name = string_field(&object, "type")
            .or_else(|| event.map(str::to_string))
            .or_else(|| string_field(&object, "object"))
            .or_else(|| object.contains_key("error").then(|| "error".to_owned()))
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
            incomplete_reason: self.incomplete_reason.or(self.stream_error),
            function_calls: self.calls,
        }
    }

    pub fn event_count(&self) -> u32 {
        self.event_count
    }

    pub fn final_event(&self) -> Option<&str> {
        self.final_event.as_deref()
    }

    pub fn stream_error(&self) -> Option<&str> {
        self.stream_error.as_deref()
    }

    fn extract(&mut self, name: &str, object: &Map<String, Value>, payload: &Value) -> Option<Delta> {
        let delta = string_field(object, "delta");
        if name.ends_with("output_text.delta") {
            return delta.map(|text| Delta { text: Some(text), reasoning: None });
        }
        if name.ends_with("reasoning_summary_text.delta") || name.ends_with("reasoning_text.delta") {
            return delta.map(|reasoning| Delta { text: None, reasoning: Some(reasoning) });
        }
        // Read usage before the choice. A final usage chunk may have no choices.
        if name == "chat.completion.chunk" {
            self.absorb(name, object);
            let choice =
                object.get("choices").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_object);
            let delta = choice.and_then(|c| c.get("delta").or_else(|| c.get("message"))).and_then(Value::as_object);
            if let Some(calls) = delta.and_then(|d| d.get("tool_calls")).and_then(Value::as_array) {
                for (position, call) in calls.iter().enumerate() {
                    let index = call.get("index").and_then(Value::as_u64).unwrap_or(position as u64);
                    let pending = self.chat_calls.entry(index).or_insert_with(|| FunctionCall {
                        call_id: None,
                        name: String::new(),
                        arguments: String::new(),
                    });
                    if let Some(id) = call.get("id").and_then(Value::as_str) {
                        pending.call_id = Some(id.to_owned());
                    }
                    if let Some(function) = call.get("function") {
                        if let Some(name) = function.get("name").and_then(Value::as_str) {
                            pending.name.push_str(name);
                        }
                        if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                            pending.arguments.push_str(arguments);
                        }
                    }
                }
            }
            if let Some(reason) = choice.and_then(|choice| string_field(choice, "finish_reason")) {
                if reason == "stop" {
                    self.final_event = Some("response.completed".into());
                }
                if reason == "tool_calls" {
                    self.calls.extend(std::mem::take(&mut self.chat_calls).into_values().filter(|call| {
                        !call.name.is_empty()
                            && serde_json::from_str::<Value>(&call.arguments).is_ok_and(|args| args.is_object())
                    }));
                }
                if !matches!(reason.as_str(), "stop" | "tool_calls" | "function_call") {
                    self.incomplete_reason = Some(reason);
                }
            }
            let delta = delta?;
            let text = string_field(delta, "content");
            let reasoning = string_field(delta, "reasoning_content").or_else(|| string_field(delta, "reasoning"));
            return (text.is_some() || reasoning.is_some()).then_some(Delta { text, reasoning });
        }
        if name == "error" {
            self.stream_error = Some(payload.to_string());
            return None;
        }
        if name.starts_with("response.") {
            self.absorb(name, object);
            let text = (name == "response.completed" && self.text.is_empty())
                .then(|| completed_output_text(object))
                .flatten();
            let reasoning = if self.reasoning.is_empty() {
                if name.ends_with("reasoning_summary_text.done") || name.ends_with("reasoning_text.done") {
                    string_field(object, "text")
                } else if matches!(name, "response.completed" | "response.incomplete") {
                    reasoning_summary(object)
                } else {
                    None
                }
            } else {
                None
            };
            return (text.is_some() || reasoning.is_some()).then_some(Delta { text, reasoning });
        }
        None
    }

    fn absorb(&mut self, name: &str, object: &Map<String, Value>) {
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
                            .and_then(|e| {
                                let code = string_field(e, "code");
                                let message = string_field(e, "message");
                                if let (Some(code), Some(message)) = (&code, &message) {
                                    let detail = format!("{code}: {message}");
                                    if baudet_core::error::looks_like_context_overflow(Some(&detail)) {
                                        return Some(detail);
                                    }
                                }
                                code.or(message)
                            })
                    })
                    .or_else(|| Some(name.to_string()));
            }
        }
        if let Some(Value::Array(output)) = response.get("output") {
            for item in output {
                self.collect_call(item.as_object());
            }
        }
        if name.ends_with("output_item.done") {
            self.collect_call(object.get("item").and_then(Value::as_object));
        }
        if name.ends_with("output_text.done") && self.text.is_empty() {
            if let Some(text) = string_field(object, "text") {
                self.text.push_str(&text);
            }
        }
    }

    // A function call may appear in both item and terminal events.
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

fn completed_output_text(event: &Map<String, Value>) -> Option<String> {
    let response = event.get("response").and_then(Value::as_object).unwrap_or(event);
    let text: String = response.get("output")?.as_array()?.iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .filter(|item| item.get("role").and_then(Value::as_str) == Some("assistant"))
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("output_text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();
    (!text.is_empty()).then_some(text)
}

fn reasoning_summary(event: &Map<String, Value>) -> Option<String> {
    let response = event.get("response").and_then(Value::as_object).unwrap_or(event);
    let blocks = response.get("output")?.as_array()?.iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        .filter_map(|item| item.get("summary").and_then(Value::as_array))
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("summary_text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    (!blocks.is_empty()).then(|| blocks.join("\n\n"))
}

pub(crate) fn parse_usage(usage: &Map<String, Value>) -> Usage {
    let number = |keys: [&str; 2]| keys.into_iter().filter_map(|key| usage.get(key)).find_map(Value::as_i64);
    Usage::new(
        number(["input_tokens", "prompt_tokens"]),
        number(["output_tokens", "completion_tokens"]),
        usage.get("total_tokens").and_then(Value::as_i64),
    )
}

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
                        created_at_ms: None,
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
                        created_at_ms: None,
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

    pub fn any_open(&self) -> bool {
        self.answer_open || self.reasoning_open
    }
}
