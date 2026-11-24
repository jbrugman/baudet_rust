
use crate::openai::REASONING_NONE;
use baudet_core::Msg;
use serde_json::{Map, Value};

pub const CHAT_COMPLETIONS_PATH: &str = "/chat/completions";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Protocol {
    Responses,
    ChatCompletions,
}

impl Protocol {
    pub fn path(&self) -> &'static str {
        match self {
            Protocol::Responses => crate::openai::RESPONSES_PATH,
            Protocol::ChatCompletions => CHAT_COMPLETIONS_PATH,
        }
    }

    pub fn word(&self) -> &'static str {
        match self {
            Protocol::Responses => "responses",
            Protocol::ChatCompletions => "chat_completions",
        }
    }

    pub fn from_word(word: &str) -> Option<Protocol> {
        match word.trim() {
            "responses" => Some(Protocol::Responses),
            "chat_completions" => Some(Protocol::ChatCompletions),
            _ => None,
        }
    }

    pub fn preferred(confirmed: &[Self], use_responses: bool) -> Option<Self> {
        let preferred = if use_responses { Self::Responses } else { Self::ChatCompletions };
        confirmed.iter().copied().find(|protocol| *protocol == preferred)
            .or_else(|| confirmed.first().copied())
    }

    pub fn label(&self) -> &'static str {
        match self {
            Protocol::Responses => "responses",
            Protocol::ChatCompletions => "chat/completions",
        }
    }
}

// Chat uses message content parts and a different token-limit field from Responses.
pub fn chat_request_body(shape: &crate::openai::RequestShape<'_>) -> Map<String, Value> {
    let mut body = Map::new();
    crate::openai::insert_sampling(&mut body, shape);
    crate::openai::insert_thinking_template(&mut body, shape.enable_thinking);
    body.insert("model".into(), shape.model.into());
    body.insert("stream".into(), Value::Bool(true));
    let mut stream_options = Map::new();
    stream_options.insert("include_usage".into(), Value::Bool(true));
    body.insert("stream_options".into(), Value::Object(stream_options));
    if let Some(tokens) = shape.max_output_tokens.filter(|t| *t > 0) {
        body.insert("max_tokens".into(), Value::from(tokens));
    }
    if let Some(effort) = shape.reasoning.filter(|effort| !effort.is_empty()) {
        body.insert("reasoning_effort".into(), effort.into());
    }
    if let Some(Ok(Value::Array(definitions))) = shape.tools.map(serde_json::from_str::<Value>) {
        let tools = definitions
            .into_iter()
            .filter_map(|definition| {
                let Value::Object(mut function) = definition else { return None };
                if function.remove("type").as_ref().and_then(Value::as_str) != Some("function") {
                    return None;
                }
                Some(serde_json::json!({ "type": "function", "function": function }))
            })
            .collect::<Vec<_>>();
        if !tools.is_empty() {
            body.insert("tools".into(), Value::Array(tools));
        }
    }

    let mut messages = Vec::new();
    if !shape.system.is_empty() {
        messages.push(chat_message("system", shape.system));
    }
    for message in shape.history {
        if message.role != "system" {
            messages.push(chat_message(&message.role, &message.content));
        }
    }
    messages.push(match shape.image {
        None => chat_message("user", shape.user_text),
        Some(image) => {
            let mut parts = Vec::new();
            if !shape.user_text.is_empty() {
                let mut part = Map::new();
                part.insert("type".into(), Value::String("text".into()));
                part.insert("text".into(), shape.user_text.into());
                parts.push(Value::Object(part));
            }
            let mut part = Map::new();
            part.insert("type".into(), Value::String("image_url".into()));
            let mut url = Map::new();
            url.insert("url".into(), image.into());
            part.insert("image_url".into(), Value::Object(url));
            parts.push(Value::Object(part));
            let mut message = Map::new();
            message.insert("role".into(), Value::String("user".into()));
            message.insert("content".into(), Value::Array(parts));
            Value::Object(message)
        }
    });
    body.insert("messages".into(), Value::Array(messages));
    body
}

fn chat_message(role: &str, content: &str) -> Value {
    let mut message = Map::new();
    message.insert("role".into(), role.into());
    message.insert("content".into(), content.into());
    Value::Object(message)
}

pub fn chat_summary_body(model: &str, system: &str, user_text: &str, max_tokens: u32) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("model".into(), model.into());
    body.insert("max_tokens".into(), Value::from(max_tokens.max(1)));
    body.insert("reasoning_effort".into(), REASONING_NONE.into());
    let mut messages = Vec::new();
    if !system.is_empty() {
        messages.push(chat_message("system", system));
    }
    messages.push(chat_message("user", user_text));
    body.insert("messages".into(), Value::Array(messages));
    body
}

// The orchestrator supplies newest-first events. The wire uses oldest-first messages.
pub fn wire_history(history: &[baudet_core::ThreadEvent]) -> Vec<Msg> {
    let mut messages: Vec<Msg> = history
        .iter()
        .filter_map(|event| match event {
            baudet_core::ThreadEvent::ItemCompleted { item, .. } => match item {
                baudet_core::Item::UserMessage(message) if !message.text.trim().is_empty() => {
                    Some(Msg::user(message.text.clone()))
                }
                baudet_core::Item::AgentMessage(message) if !message.text.trim().is_empty() => {
                    Some(Msg::assistant(message.text.clone()))
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    messages.reverse();
    messages
}

pub fn is_error_body(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object.contains_key("error") || object.contains_key("detail") || object.contains_key("message")
        }
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanOutcome {
    pub endpoint: String,
    pub protocols: Vec<Protocol>,
    pub models: Vec<String>,
    pub notes: Vec<String>,
}

impl ScanOutcome {
    pub fn usable(&self) -> bool {
        !self.protocols.is_empty()
    }
}

pub const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub fn scan(
    endpoint: &str,
    list: impl FnOnce() -> Result<Vec<String>, String>,
    probe: impl Fn(Protocol, &str, &Map<String, Value>) -> (Option<Protocol>, String),
) -> ScanOutcome {
    let endpoint = endpoint.to_string();
    let mut notes = Vec::new();
    let models = match list() {
        Ok(models) => models,
        Err(detail) => {
            notes.push(format!("no model listing: {detail}"));
            return ScanOutcome { endpoint, protocols: Vec::new(), models: Vec::new(), notes };
        }
    };
    let Some(model) = probeable_model(&models).map(str::to_string) else {
        notes.push(if models.is_empty() {
            "the server lists no models; nothing to ask it".into()
        } else {
            "the server lists no model that can answer a turn".into()
        });
        return ScanOutcome { endpoint, protocols: Vec::new(), models, notes };
    };
    let mut protocols = Vec::new();
    for (dialect, body) in [
        (Protocol::Responses, responses_probe_body(&model)),
        (Protocol::ChatCompletions, chat_probe_body(&model)),
    ] {
        let (found, note) = probe(dialect, &model, &body);
        notes.push(format!("{}: {note}", dialect.label()));
        if found == Some(dialect) {
            protocols.push(dialect);
        }
    }
    ScanOutcome { endpoint, protocols, models, notes }
}

pub fn likely_chat_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    !["embed", "bge", "tts", "whisper", "chatterbox", "rerank", "clip", "tokenizer", "vision-", "-ocr", "llava"]
        .iter()
        .any(|marker| id.contains(marker))
}

fn probeable_model(models: &[String]) -> Option<&str> {
    models.iter().map(String::as_str).find(|id| likely_chat_model(id))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    pub found: Option<ScanOutcome>,
    pub tried: Vec<ScanOutcome>,
}

impl Discovery {
    pub fn usable(&self) -> bool {
        self.found.is_some()
    }
}

pub fn discover(
    candidates: &[String],
    reachable: impl Fn(&str) -> bool,
    mut scan_one: impl FnMut(&str) -> ScanOutcome,
) -> Discovery {
    let mut tried = Vec::new();
    for candidate in candidates {
        if !reachable(candidate) {
            tried.push(ScanOutcome {
                endpoint: candidate.clone(),
                protocols: Vec::new(),
                models: Vec::new(),
                notes: vec!["nothing is listening on it".into()],
            });
            continue;
        }
        let outcome = scan_one(candidate);
        if outcome.usable() {
            tried.push(outcome.clone());
            return Discovery { found: Some(outcome), tried };
        }
        tried.push(outcome);
    }
    Discovery { found: None, tried }
}

fn responses_probe_body(model: &str) -> Map<String, Value> {
    Map::from_iter([
        ("model".into(), model.into()),
        ("input".into(), "ping".into()),
        ("max_output_tokens".into(), 16.into()),
        ("store".into(), false.into()),
    ])
}

fn chat_probe_body(model: &str) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("model".into(), model.into());
    body.insert("max_tokens".into(), Value::from(1u32));
    body.insert("messages".into(), Value::Array(vec![chat_message("user", "ping")]));
    body
}

pub fn judge_probe(dialect: Protocol, status: u16, body: &Value) -> (Option<Protocol>, String) {
    let identified = match dialect {
        Protocol::ChatCompletions => body
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(Value::as_object)
            .and_then(|choice| choice.get("message"))
            .is_some(),
        Protocol::Responses => body.get("output").and_then(Value::as_array).is_some()
            || (body.get("object").and_then(Value::as_str) == Some("response")
                && body.get("id").and_then(Value::as_str).is_some()),
    };
    if (200..300).contains(&status) && identified {
        return (Some(dialect), format!("answered {status} with a {} body", dialect.label()));
    }
    let said = match body {
        Value::Object(object) => ["error", "detail", "message"]
            .iter()
            .find_map(|key| object.get(*key))
            .map(|value| match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .unwrap_or_else(|| format!("a {status} body with no answer in it")),
        other => format!(
            "a {status} {} body",
            match other {
                Value::Null => "empty",
                _ => "non-object",
            }
        ),
    };
    (None, format!("did not answer as a turn request: {said}"))
}
