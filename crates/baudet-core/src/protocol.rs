
use crate::usage::Usage;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_event: Option<String>,
    #[serde(default)]
    pub events: Vec<(String, u32)>,
    #[serde(default)]
    pub samples: Vec<(String, String)>,
    #[serde(default)]
    pub reasoning: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete_reason: Option<String>,
    #[serde(default)]
    pub function_calls: Vec<FunctionCall>,
}

impl Turn {
    // An explicit successful terminal event is required for compaction.
    pub fn completed(&self) -> bool {
        self.final_event.as_deref() == Some("response.completed")
    }

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
