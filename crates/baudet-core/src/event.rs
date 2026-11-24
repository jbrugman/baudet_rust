
use crate::item::{ApprovalDecision, FileChangeItem, Item, ItemKind, PlanEntry};
use crate::usage::Usage;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "camelCase")]
pub enum ThreadEvent {
    #[serde(rename = "turn/started")]
    TurnStarted { thread_id: String, turn_id: String },

    #[serde(rename = "item/started")]
    ItemStarted { thread_id: String, turn_id: String, item: Item },

    ItemDelta { thread_id: String, turn_id: String, item_id: String, kind: ItemKind, delta: String },

    #[serde(rename = "item/completed")]
    ItemCompleted { thread_id: String, turn_id: String, item: Item },

    #[serde(rename = "turn/plan/updated")]
    PlanUpdated { thread_id: String, turn_id: String, entries: Vec<PlanEntry> },

    #[serde(rename = "turn/diff/updated")]
    TurnDiffUpdated {
        thread_id: String,
        turn_id: String,
        files: Vec<FileChangeItem>,
        diff: String,
        additions: i32,
        deletions: i32,
    },

    #[serde(rename = "thread/tokenUsage/updated")]
    TokenUsageUpdated { thread_id: String, turn_id: String, usage: Usage },

    #[serde(rename = "turn/completed")]
    TurnCompleted {
        thread_id: String,
        turn_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_event: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        incomplete_reason: Option<String>,
    },

    ApprovalRequested { thread_id: String, turn_id: String, item_id: String, request: ApprovalRequest },

    #[serde(rename = "serverRequest/resolved")]
    ApprovalResolved { thread_id: String, turn_id: String, item_id: String, decision: ApprovalDecision },
}

impl ThreadEvent {
    pub fn thread_id(&self) -> &str {
        match self {
            ThreadEvent::TurnStarted { thread_id, .. }
            | ThreadEvent::ItemStarted { thread_id, .. }
            | ThreadEvent::ItemDelta { thread_id, .. }
            | ThreadEvent::ItemCompleted { thread_id, .. }
            | ThreadEvent::PlanUpdated { thread_id, .. }
            | ThreadEvent::TurnDiffUpdated { thread_id, .. }
            | ThreadEvent::TokenUsageUpdated { thread_id, .. }
            | ThreadEvent::TurnCompleted { thread_id, .. }
            | ThreadEvent::ApprovalRequested { thread_id, .. }
            | ThreadEvent::ApprovalResolved { thread_id, .. } => thread_id,
        }
    }

    pub fn turn_id(&self) -> &str {
        match self {
            ThreadEvent::TurnStarted { turn_id, .. }
            | ThreadEvent::ItemStarted { turn_id, .. }
            | ThreadEvent::ItemDelta { turn_id, .. }
            | ThreadEvent::ItemCompleted { turn_id, .. }
            | ThreadEvent::PlanUpdated { turn_id, .. }
            | ThreadEvent::TurnDiffUpdated { turn_id, .. }
            | ThreadEvent::TokenUsageUpdated { turn_id, .. }
            | ThreadEvent::TurnCompleted { turn_id, .. }
            | ThreadEvent::ApprovalRequested { turn_id, .. }
            | ThreadEvent::ApprovalResolved { turn_id, .. } => turn_id,
        }
    }

    pub fn wire_method(&self) -> &'static str {
        match self {
            ThreadEvent::TurnStarted { .. } => "turn/started",
            ThreadEvent::ItemStarted { .. } => "item/started",
            ThreadEvent::ItemDelta { kind, .. } => delta_method(*kind),
            ThreadEvent::ItemCompleted { .. } => "item/completed",
            ThreadEvent::PlanUpdated { .. } => "turn/plan/updated",
            ThreadEvent::TurnDiffUpdated { .. } => "turn/diff/updated",
            ThreadEvent::TokenUsageUpdated { .. } => "thread/tokenUsage/updated",
            ThreadEvent::TurnCompleted { .. } => "turn/completed",
            ThreadEvent::ApprovalRequested { request, .. } => request.wire_method(),
            ThreadEvent::ApprovalResolved { .. } => "serverRequest/resolved",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ApprovalRequest {
    RunCommand {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_seconds: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        always_key: Option<String>,
    },
    ApplyPatch {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<String>,
    },
    McpTool { server_id: String, tool: String, arguments: String },
}

impl ApprovalRequest {
    pub fn wire_method(&self) -> &'static str {
        match self {
            ApprovalRequest::RunCommand { .. } => "item/commandExecution/requestApproval",
            ApprovalRequest::ApplyPatch { .. } => "item/fileChange/requestApproval",
            ApprovalRequest::McpTool { .. } => "item/mcpTool/requestApproval",
        }
    }
}

pub fn delta_method(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::AgentMessage | ItemKind::UserMessage => "item/agentMessage/delta",
        ItemKind::Reasoning => "item/reasoning/textDelta",
        ItemKind::CommandExecution => "item/commandExecution/outputDelta",
        ItemKind::FileChange => "item/fileChange/outputDelta",
        ItemKind::Plan => "item/plan/delta",
        _ => "baudet/item/delta",
    }
}
