//! Which dialect a server speaks, decided by what it answers rather than by what was asked.
//!
//! The rule this file exists to enforce is a measured one (2026-11-21, against the two servers this
//! project actually runs against): **a status code cannot name a protocol.** LM Studio answers
//! `POST /responses` with `HTTP 200` and the body `{"error":"Unexpected endpoint or method. (POST
//! /responses)"}`; oMLX answers the same request with `404 Not Found` and `{"detail":"Not Found"}`.
//! One calls a wrong endpoint a success, the other calls an unsupported protocol a client error, and
//! both are lying about which protocol it speaks. Reading the *body* is the only way to tell, and it
//! is a guess either way -- which is why the guess is kept here, in one place, named, rather than
//! distributed through a transport as a chain of status-code checks.
//!
//! The measurement also settled which dialect is worth building first. No server on this machine
//! implements `/responses`; every one of them implements `/v1/chat/completions`. So Responses stays
//! the shape this app prefers -- `request_body` already builds it, and it is what the reference app
//! speaks to gateways -- and chat-completions is the dialect the local servers actually answer.
//! `TurnAccumulator` has read `chat.completion.chunk` from the start, so what is new here is the
//! request shape and the decision of when to use it, not a second parser.

use crate::openai::REASONING_NONE;
use baudet_core::Msg;
use serde_json::{Map, Value};

/// The chat-completions path, relative to a base URL that ends in `/v1` -- the same base the
/// listing in `baudet-settings` already uses, so one typed endpoint serves both.
pub const CHAT_COMPLETIONS_PATH: &str = "/chat/completions";

/// A server's dialect. `#[non_exhaustive]` because the honest list is longer than this machine: the
/// whole point of a scan is that a laptop can also hold Jan, Ollama and vLLM, and a dialect found
/// there must be added without breaking whoever matches on this today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Protocol {
    /// `POST {base}/responses`, what [`crate::openai::request_body`] builds.
    Responses,
    /// `POST {base}/chat/completions`, what every local server measured here answers.
    ChatCompletions,
}

impl Protocol {
    /// The path this dialect's turn request goes to.
    pub fn path(&self) -> &'static str {
        match self {
            Protocol::Responses => crate::openai::RESPONSES_PATH,
            Protocol::ChatCompletions => CHAT_COMPLETIONS_PATH,
        }
    }

    /// The words a person reads when the app says which dialect it settled on.
    /// The word a settings file uses for this dialect. Lowercase with an underscore, which is how
    /// the dialects are named in prose everywhere in this crate, so the file and the code agree.
    pub fn word(&self) -> &'static str {
        match self {
            Protocol::Responses => "responses",
            Protocol::ChatCompletions => "chat_completions",
        }
    }

    /// The dialect a settings word names, or `None` for a word this build has no path for.
    ///
    /// An unknown word is not guessed at. Guessing would send a turn built for one dialect to a
    /// server chosen for another, which is a wrong answer rather than a missing one.
    pub fn from_word(word: &str) -> Option<Protocol> {
        match word.trim() {
            "responses" => Some(Protocol::Responses),
            "chat_completions" => Some(Protocol::ChatCompletions),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Protocol::Responses => "responses",
            Protocol::ChatCompletions => "chat/completions",
        }
    }
}

/// A request body in chat-completions shape.
///
/// Deliberately parallel to [`crate::openai::request_body`] rather than a translation of it: the two
/// differ in more than field names, and pretending otherwise is how a shared builder ends up with an
/// `if is_chat` in six places. Three differences matter and are each a measured or specified fact:
///
///  * **the system message rides in `messages`.** The Responses shape sends it as `instructions`;
///    chat-completions has no such field, so leaving it out here would silently drop the persona.
///  * **the ceiling is `max_tokens` and means *output* tokens** -- the same meaning as Responses'
///    `max_output_tokens`, under a name that reads as a whole-request budget. A ceiling of `0` or
///    `None` is not sent, exactly as there: sending a zero ceiling is not "no opinion", it caps the
///    answer at nothing.
///  * **there is no `store` and no `previous_response_id`.** A chat-completions server has no
///    server-side session to chain, so the transcript always rides in `messages`. That is also why
///    reasoning effort is dropped rather than guessed at: `reasoning_effort` is a name a reasoning
///    model's endpoint may or may not read, and sending an unread field to get a silent answer is
///    worse than not sending it.
///
/// Streaming is unconditional, as in the Responses shape: a server that cannot stream is a protocol
/// decision, not a field to flip.
pub fn chat_request_body(shape: &crate::openai::RequestShape<'_>) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("model".into(), shape.model.into());
    body.insert("stream".into(), Value::Bool(true));
    // `stream_options.include_usage` is what makes a local server send its token counts on the last
    // chunk at all. Without it a streamed answer ends with no accounting, and the one number a
    // person can check against their own GPU is missing; a server that ignores the field simply
    // omits usage, which the accumulator already reads as "the server did not say".
    let mut stream_options = Map::new();
    stream_options.insert("include_usage".into(), Value::Bool(true));
    body.insert("stream_options".into(), Value::Object(stream_options));
    if let Some(tokens) = shape.max_output_tokens.filter(|t| *t > 0) {
        body.insert("max_tokens".into(), Value::from(tokens));
    }

    let mut messages = Vec::new();
    if !shape.system.is_empty() {
        messages.push(chat_message("system", shape.system));
    }
    // No chaining to skip: a chat-completions server holds nothing between turns, so the transcript
    // always travels.
    for message in shape.history {
        if message.role != "system" {
            messages.push(chat_message(&message.role, &message.content));
        }
    }
    messages.push(match shape.image {
        None => chat_message("user", shape.user_text),
        Some(image) => {
            // The OpenAI vision shape, because that is the shape local servers implement: an array
            // of typed parts. A server without vision rejects the whole body rather than ignoring
            // the part, which is why the caller decides whether an image may ride at all.
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

/// The whole body of a non-streaming chat-completions call: what the compactor asks for when it
/// wants the entire summary or nothing.
///
/// `reasoning` is forced to [`REASONING_NONE`] where the server accepts the field, because a fold
/// deletes text: a model that spends its ceiling on hidden thinking before the summary starts leaves
/// no summary at all. `max_tokens` is required, so `None` is answered with a real ceiling rather
/// than with an absent field.
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

/// A transcript in wire order: oldest first, without the turn now being asked.
///
/// The caller appends its own question afterwards, which is why there is no question parameter:
/// folding it in would make it ambiguous whether the caller's `user_text` still has to be appended,
/// and a transcript that carries the question twice reads to the model as a person repeating
/// themselves.
///
/// Lives here because both request builders need it and only here can its two rules be stated once:
/// the newest message last (which is how [`baudet_core::Msg`] is stored, so a reversal has to happen
/// somewhere), and no empty content -- an empty turn reaches the model as a blank line it is
/// expected to continue.
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
    // Stored newest-last, sent oldest-first.
    messages.reverse();
    messages
}

/// Is this body a refusal rather than an answer?
///
/// The shape both servers measured here use is a top-level `error` member: LM Studio's
/// `{"error":"Unexpected endpoint or method. (POST /responses)"}` is one, and oMLX's 404
/// `{"detail":"Not Found"}` is another. An error body is only an error when it carries one: checking
/// the status instead was measured to be the wrong instrument on the first server.
pub fn is_error_body(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object.contains_key("error") || object.contains_key("detail") || object.contains_key("message")
        }
        _ => false,
    }
}

/// Everything a scan may say about one endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanOutcome {
    /// Where the probe was sent, after normalising, so the person can see which server answered.
    pub endpoint: String,
    /// The dialect to send turns to, or `None` when nothing identified an answerable server.
    pub protocol: Option<Protocol>,
    /// What the models are called, so choosing a model is not a second request.
    pub models: Vec<String>,
    /// What was seen, in the order it was seen, including an attempt that identified nothing. A scan
    /// that says only "no server" throws away the evidence a person needs to fix the endpoint.
    pub notes: Vec<String>,
}

impl ScanOutcome {
    /// Whether the scan found a server worth sending a turn to.
    pub fn usable(&self) -> bool {
        self.protocol.is_some()
    }
}

/// What the probe costs.
///
/// Kept this short for the reason the listing keeps 20 s: a machine with several local servers answers
/// instantly on the running ones, so a long timeout only measures the ones that are not there.
pub const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Does this endpoint answer chat-completions, and with which models?
///
/// One listing plus one probe. Three rules, each earned by measurement or by the user's own
/// constraint, and all three are why this is a function rather than a guess:
///
///  1. **`GET /v1/models` first, and never a turn endpoint on a model this machine has to load.**
///     A probe always names a model, and naming a model the server has to page in costs a load that
///     displaces the resident one. The listing says whether the server is there *and* names the
///     models worth naming, so no probe is ever sent at a server that has nothing loaded. (`list` is
///     injected rather than called here so this crate stays free of a network dependency it does not
///     need, and so a scan is testable without a socket.)
///  2. **The dialect is settled by the answer body, not by the status.** `200` with a
///     `chat.completion` body is a dialect; `200` with `{"error": ...}` is a refusal -- exactly what
///     LM Studio returns for a path it does not serve. A status-code test would call that a success.
///     (Measured 2026-11-21.)
///  3. **One probe, because a second one buys nothing.** `Responses` is deliberately not probed. A
///     gateway that refuses with `400 {"error":"unknown field"}` and one that refuses with
///     `404 Not Found` carry the same information -- "this is not a chat answer" -- and neither
///     establishes that the server *does* speak Responses. Sending that request would therefore spend
///     a model load to produce a guess, so the refusal is reported in `notes` and an explicit choice
///     belongs to the person, in settings.
///
/// `protocol: None` means "not a chat-completions server", not "no server": the notes say which of
/// the two it was, because those two need different fixes.
pub fn scan(
    endpoint: &str,
    list: impl FnOnce() -> Result<Vec<String>, String>,
    probe: impl Fn(&str, &str, &Map<String, Value>) -> (Option<Protocol>, String),
) -> ScanOutcome {
    let endpoint = endpoint.to_string();
    let mut notes = Vec::new();
    let models = match list() {
        Ok(models) => models,
        Err(detail) => {
            // No listing means no server, or no readable one. A probe cannot improve on that: it
            // names a model, and a server that cannot list has nothing to name.
            notes.push(format!("no model listing: {detail}"));
            return ScanOutcome { endpoint, protocol: None, models: Vec::new(), notes };
        }
    };
    let Some(model) = models.first().cloned() else {
        notes.push("the server lists no models; nothing to ask it".into());
        return ScanOutcome { endpoint, protocol: None, models, notes };
    };
    let (found, note) = probe(CHAT_COMPLETIONS_PATH, &model, &chat_probe_body(&model));
    notes.push(format!("chat/completions: {note}"));
    let protocol = match found {
        Some(Protocol::ChatCompletions) => Some(Protocol::ChatCompletions),
        // Proved to answer a listing and not to answer a chat turn. The one thing that can still be
        // true -- that it speaks Responses -- is not knowable from a refusal, so it is said aloud.
        _ => {
            notes.push("the responses dialect is not probed; set it in settings if this server uses it".into());
            None
        }
    };
    ScanOutcome { endpoint, protocol, models, notes }
}

/// The chat sibling of [`probe_body`], in the shape [`chat_request_body`] produces.
fn chat_probe_body(model: &str) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("model".into(), model.into());
    body.insert("max_tokens".into(), Value::from(1u32));
    body.insert("messages".into(), Value::Array(vec![chat_message("user", "ping")]));
    body
}

/// Judge one probe answer: is that body this dialect's answer, and if not, what does it say?
///
/// Returns the dialect and the sentence for the scan's notes. Recognising a chat answer is a shape
/// test (`choices` with a `message`), not a status test, for the reason stated on [`scan`]: the
/// measured servers disagree about which status a wrong path deserves and agree about neither body.
pub fn judge_probe(dialect: Protocol, status: u16, body: &Value) -> (Option<Protocol>, String) {
    let identified = match dialect {
        Protocol::ChatCompletions => body
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(Value::as_object)
            .and_then(|choice| choice.get("message"))
            .is_some(),
        Protocol::Responses => body.get("output").and_then(Value::as_array).is_some() || body.get("id").is_some(),
    };
    if identified {
        return (Some(dialect), format!("answered {status} with a {} body", dialect.label()));
    }
    // The refusal, in the server's own words, cut short: a gateway's HTML error page is not evidence
    // and would bury the scan's three lines of notes.
    let said = match body {
        Value::Object(object) => ["error", "detail", "message"]
            .iter()
            .find_map(|key| object.get(*key))
            .map(|value| match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .unwrap_or_else(|| format!("a {status} body with no answer in it")),
        other => format!("a {status} {} body", match other {
            Value::Null => "empty",
            _ => "non-object",
        }),
    };
    (None, format!("did not answer as a turn request: {said}"))
}
