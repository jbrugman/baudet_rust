//! Everything that happens in a conversation, as a stream of facts rather than as text the
//! interface has to reassemble.
//!
//! Translated from `dev.baudet.thread.ThreadEvent`. Two decisions make this the seam a real
//! app-server can be built on later, and both come from what a machine with room to spare
//! actually needs:
//!
//!  * every event carries its [`ThreadEvent::thread_id`], because one conversation's events may
//!    not be assumed to be the only ones in the process -- a studio that runs three tasks at
//!    once produces three interleaved streams, and a listener must be able to tell them apart
//!    without guessing from the text;
//!  * every event carries its [`ThreadEvent::turn_id`], so a listener that started watching
//!    mid-turn, or one that still holds an older turn's card, can ignore what it is not looking
//!    at.
//!
//! [`ThreadEvent::wire_method`] is the app-server method each event corresponds to, using that
//! protocol's own spelling. It is documentation that is checked by a test rather than a comment:
//! when this process eventually answers a JSON-RPC request over stdio, it publishes these
//! methods.

use crate::item::{ApprovalDecision, FileChangeItem, Item, ItemKind, PlanEntry};
use crate::usage::Usage;
use serde::Deserialize;
use serde::Serialize;

/// Everything a conversation emits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "camelCase")]
pub enum ThreadEvent {
    /// A turn began, so the interface can open its card and time it.
    #[serde(rename = "turn/started")]
    TurnStarted { thread_id: String, turn_id: String },

    /// A new row. The item is the row's initial state, usually in progress with no content.
    #[serde(rename = "item/started")]
    ItemStarted { thread_id: String, turn_id: String, item: Item },

    /// Part of an item's content. The kind decides the method, exactly as the protocol splits
    /// one stream per content type: an answer delta and a command's output delta are different
    /// notifications even though both are "more text".
    ItemDelta { thread_id: String, turn_id: String, item_id: String, kind: ItemKind, delta: String },

    /// An item reached its final state. The item carries it, so a renderer never keeps its own.
    #[serde(rename = "item/completed")]
    ItemCompleted { thread_id: String, turn_id: String, item: Item },

    /// The plan checklist changed. Reserved with the plan tool.
    #[serde(rename = "turn/plan/updated")]
    PlanUpdated { thread_id: String, turn_id: String, entries: Vec<PlanEntry> },

    /// The whole turn's accumulated file changes. This is what lets an interface offer one
    /// "review this turn" action instead of asking the reader to remember which cards wrote what.
    #[serde(rename = "turn/diff/updated")]
    TurnDiffUpdated {
        thread_id: String,
        turn_id: String,
        files: Vec<FileChangeItem>,
        diff: String,
        additions: i32,
        deletions: i32,
    },

    /// The turn's token accounting, emitted as its own event so a footer can update before the
    /// turn closes.
    #[serde(rename = "thread/tokenUsage/updated")]
    TokenUsageUpdated { thread_id: String, turn_id: String, usage: Usage },

    /// A turn finished, cleanly or not. `final_event` and `incomplete_reason` are the backend's
    /// own signals, kept verbatim: "no answer" and "budget exhausted before the answer started"
    /// have to stay distinguishable here, because nowhere downstream can they be recovered.
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

    /// The turn is blocked on `request`; the answer arrives as [`ThreadEvent::ApprovalResolved`].
    ApprovalRequested { thread_id: String, turn_id: String, item_id: String, request: ApprovalRequest },

    /// A pending approval was answered, by the human or by a stored grant.
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

    /// The Codex app-server method this event maps to.
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

/// What the interface must ask the human before the turn may continue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ApprovalRequest {
    /// A shell command waiting for a decision, with the folder it would run in.
    RunCommand {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_seconds: Option<i32>,
    },
    /// A patch waiting for a decision.
    ApplyPatch {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<String>,
    },
    /// An external MCP tool waiting for a decision: `server_id` names the server, `tool` the
    /// tool on it, `arguments` the raw JSON it would run with. The human grants one call or the
    /// whole server; the method is Baudet's own, not an app-server one, because no wire item the
    /// backend defined covers an out-of-band third-party tool.
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

/// The method a content delta of `kind` travels under. An item kind with no delta method of its
/// own -- a plan line, an error -- reports its state through `ItemCompleted` instead, so the
/// fallback is spelled as ours rather than borrowed.
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
