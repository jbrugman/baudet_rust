//! The turn's token accounting.
//!
//! Translated from `dev.baudet.api.Usage`. Every field is optional on purpose: a backend that
//! reports no usage must not be reported as having reported zero, because an honest `None` is
//! what tells the reader the number is missing rather than small.

use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,
}

impl Usage {
    pub fn new(prompt: Option<i64>, completion: Option<i64>, total: Option<i64>) -> Usage {
        Usage { prompt, completion, total }
    }
}
