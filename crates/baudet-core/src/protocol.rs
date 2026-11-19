//! The turn's result and the backend's own terminal signals.
//!
//! Translated from `dev.baudet.api.Turn`. The three diagnostic fields (`final_event`, `events`,
//! `samples`) are kept because they are the reason this app exists: a local backend that closes
//! a stream without saying why is indistinguishable from one that ran out of budget, and only
//! the wire evidence separates them.

use crate::usage::Usage;
use serde::Deserialize;
use serde::Serialize;

/// A native function call the backend returned inside a response. `arguments` is kept as the raw
/// JSON string the backend produced rather than pre-parsed: a malformed or only half-streamed
/// argument object then degrades to "no usable call" at the extractor, not a transport failure.
/// `call_id` pairs this call with the observation returned for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    pub name: String,
    pub arguments: String,
}

/// What one streamed turn produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// The finish signal the backend actually sent, for diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_event: Option<String>,
    /// Payloads seen per event type, in first-seen order.
    #[serde(default)]
    pub events: Vec<(String, u32)>,
    /// First raw payload per event type, truncated: what the wire really carried.
    #[serde(default)]
    pub samples: Vec<(String, String)>,
    /// Reasoning summary streamed alongside the answer, kept out of the prompt.
    #[serde(default)]
    pub reasoning: String,
    /// Why the backend stopped early, e.g. `max_output_tokens`; `None` when it finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete_reason: Option<String>,
    /// Native function calls the backend returned; empty when the answer is prose only.
    #[serde(default)]
    pub function_calls: Vec<FunctionCall>,
}

impl Turn {
    /// True only when the backend sent an explicit successful terminal event. A stream that
    /// simply closed, or one that reported a reason, is not a completion, so callers that depend
    /// on a finished answer -- compaction above all -- can demand one.
    pub fn completed(&self) -> bool {
        self.final_event.as_deref() == Some("response.completed")
    }

    /// A minimal turn, for backends and tests that only produce prose.
    pub fn text(text: impl Into<String>) -> Turn {
        Turn {
            text: text.into(),
            response_id: None,
            usage: None,
            final_event: Some("response.completed".to_string()),
            events: Vec::new(),
            samples: Vec::new(),
            reasoning: String::new(),
            incomplete_reason: None,
            function_calls: Vec::new(),
        }
    }
}
