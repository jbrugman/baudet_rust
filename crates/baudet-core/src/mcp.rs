use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn enabled() -> bool {
    true
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct McpServerConfig {
    pub id: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default, rename = "timeoutSeconds", alias = "timeout_seconds")]
    pub timeout_seconds: Option<u64>,
}

impl McpServerConfig {
    pub fn valid(&self) -> bool {
        !self.id.trim().is_empty() && !self.command.trim().is_empty()
    }
}

// Command arguments and environment values may contain credentials.
impl std::fmt::Debug for McpServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerConfig").field("id", &self.id).field("enabled", &self.enabled).finish_non_exhaustive()
    }
}
