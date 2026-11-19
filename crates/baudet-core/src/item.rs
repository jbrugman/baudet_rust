//! The lifecycle and vocabulary of one transcript row.
//!
//! Translated from `dev.baudet.thread.Item`. The `wire_name` spellings are the ones the Codex
//! app-server uses for the same states, and that match is the point: the item vocabulary is
//! Baudet's internal seam, so a later phase that speaks the protocol out loud over stdio
//! serializes these names instead of translating them. Read the tag off [`Item::wire_name`];
//! never invent a second spelling at a call site.

use serde::Deserialize;
use serde::Serialize;

/// The lifecycle of one transcript item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemStatus {
    #[serde(rename = "inProgress")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "interrupted")]
    Interrupted,
}

impl ItemStatus {
    pub fn wire_name(self) -> &'static str {
        match self {
            ItemStatus::InProgress => "inProgress",
            ItemStatus::Completed => "completed",
            ItemStatus::Failed => "failed",
            ItemStatus::Interrupted => "interrupted",
        }
    }
}

/// What an item is.
///
/// Every tag except [`ItemKind::ToolCall`] and [`ItemKind::Image`] is a real app-server item
/// tag. Those two are named as ours on purpose: Baudet's `read_file` and its read-only `git` are
/// actions the model really took and deserve a row of their own, but dressing them up in a Codex
/// tag would make the seam lie about what the wire would carry, and honest accounting is the
/// reason this app exists. The `baudet/` prefix marks them as an extension a protocol mapper
/// must map or drop, never pass through as if the backend had invented it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemKind {
    #[serde(rename = "userMessage")]
    UserMessage,
    #[serde(rename = "agentMessage")]
    AgentMessage,
    #[serde(rename = "reasoning")]
    Reasoning,
    #[serde(rename = "commandExecution")]
    CommandExecution,
    #[serde(rename = "fileChange")]
    FileChange,
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "baudet/toolCall")]
    ToolCall,
    #[serde(rename = "baudet/image")]
    Image,
}

impl ItemKind {
    pub fn wire_name(self) -> &'static str {
        match self {
            ItemKind::UserMessage => "userMessage",
            ItemKind::AgentMessage => "agentMessage",
            ItemKind::Reasoning => "reasoning",
            ItemKind::CommandExecution => "commandExecution",
            ItemKind::FileChange => "fileChange",
            ItemKind::Plan => "plan",
            ItemKind::Error => "error",
            ItemKind::ToolCall => "baudet/toolCall",
            ItemKind::Image => "baudet/image",
        }
    }
}

/// What the human decided about a pending approval.
///
/// The four spellings are the app-server's own decision tags for a command and a patch request.
/// [`ApprovalDecision::AcceptForSession`] is what Baudet's "Always" pin already is in spirit -- a
/// grant that outlives the turn -- so the decision a card records and the pin the store keeps can
/// be reconciled in one place later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalDecision {
    #[serde(rename = "accept")]
    Accept,
    #[serde(rename = "acceptForSession")]
    AcceptForSession,
    #[serde(rename = "decline")]
    Decline,
    #[serde(rename = "cancel")]
    Cancel,
}

impl ApprovalDecision {
    pub fn wire_name(self) -> &'static str {
        match self {
            ApprovalDecision::Accept => "accept",
            ApprovalDecision::AcceptForSession => "acceptForSession",
            ApprovalDecision::Decline => "decline",
            ApprovalDecision::Cancel => "cancel",
        }
    }
}

/// The kind of write a file-change item stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FileChangeKind {
    #[serde(rename = "add")]
    Add,
    #[serde(rename = "update")]
    #[default]
    Update,
    #[serde(rename = "delete")]
    Delete,
}

impl FileChangeKind {
    pub fn wire_name(self) -> &'static str {
        match self {
            FileChangeKind::Add => "add",
            FileChangeKind::Update => "update",
            FileChangeKind::Delete => "delete",
        }
    }
}

/// One checklist line of a plan item. The plan tool does not exist yet; the vocabulary does, so a
/// card can.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanEntryStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "inProgress")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
}

impl PlanEntryStatus {
    pub fn wire_name(self) -> &'static str {
        match self {
            PlanEntryStatus::Pending => "pending",
            PlanEntryStatus::InProgress => "inProgress",
            PlanEntryStatus::Completed => "completed",
        }
    }
}

/// One checklist line of a plan item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanEntry {
    pub content: String,
    pub status: PlanEntryStatus,
}

impl PlanEntry {
    pub fn new(content: impl Into<String>, status: PlanEntryStatus) -> Self {
        PlanEntry { content: content.into(), status }
    }
}

/// The add and delete counts of a unified patch, counted the one way every consumer may use.
///
/// A diff header line starts with `---` or `+++` and a hunk header with `@@`, so those are
/// skipped before the sign is read; counting them would turn two file paths into two adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStats {
    pub additions: i32,
    pub deletions: i32,
}

impl DiffStats {
    pub fn of(diff: Option<&str>) -> DiffStats {
        let mut additions = 0;
        let mut deletions = 0;
        for line in diff.unwrap_or("").lines() {
            if line.starts_with("---") || line.starts_with("+++") || line.starts_with("@@") {
                continue;
            } else if line.starts_with('+') {
                additions += 1;
            } else if line.starts_with('-') {
                deletions += 1;
            }
        }
        DiffStats { additions, deletions }
    }
}

/// A shell command the model asked for and the human let run.
///
/// `exit_code` stays `None` for a run that never produced one: a timeout and a command that was
/// never started are not exit code 0, and reporting them as one is the exact dishonesty this app
/// was written against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandExecutionItem {
    pub item_id: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<i32>,
    pub status: ItemStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
    #[serde(default)]
    pub duration_ms: i64,
    #[serde(default)]
    pub output: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalDecision>,
}

/// A write to a project file. `diff` is the human-facing unified preview already produced by the
/// edit tool, and `additions`/`deletions` are counted from it via [`DiffStats`] so a card header
/// and a turn's diff summary agree on one number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChangeItem {
    pub item_id: String,
    pub path: String,
    #[serde(default)]
    pub change: FileChangeKind,
    pub status: ItemStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    #[serde(default)]
    pub additions: i32,
    #[serde(default)]
    pub deletions: i32,
}

/// A tool the interface has no dedicated card for yet: `read_file`, and the read-only `git`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallItem {
    pub item_id: String,
    pub tool: String,
    #[serde(default)]
    pub detail: String,
    pub status: ItemStatus,
    #[serde(default = "defaults::yes")]
    pub ok: bool,
    #[serde(default)]
    pub output: String,
}

/// An image a tool produced, shown inline the way the app-server shows a returned image. The
/// bytes live here and nowhere else: `base64` is never folded into a model-facing observation,
/// so a transcript that carries a picture does not spend the context window on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageItem {
    pub item_id: String,
    pub tool: String,
    pub mime_type: String,
    pub base64: String,
    pub status: ItemStatus,
}

/// One model reply. A tool turn produces one of these per iteration, which is why the text is
/// not simply "the answer": the row the model wrote before it asked to run a command is its own
/// item, exactly as the app-server shows a step before a command card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessageItem {
    pub item_id: String,
    #[serde(default)]
    pub text: String,
    pub status: ItemStatus,
}

/// Reasoning streamed with a reply. Display-only: it is never replayed into a prompt, so an item
/// here records what was shown, not what was sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningItem {
    pub item_id: String,
    #[serde(default)]
    pub text: String,
    pub status: ItemStatus,
}

/// The instruction as the human sent it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserMessageItem {
    pub item_id: String,
    pub text: String,
    pub status: ItemStatus,
}

/// The plan checklist. Reserved: nothing emits one until Baudet has a plan tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub item_id: String,
    #[serde(default)]
    pub entries: Vec<PlanEntry>,
    pub status: ItemStatus,
}

/// A turn-level failure, shown where it happened instead of only in a status line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorItem {
    pub item_id: String,
    pub message: String,
    pub status: ItemStatus,
}

/// One row in a conversation, as the interface renders it and as a future protocol frame would
/// carry it. An item is always identified by its `item_id` and always scoped by the event that
/// announces it ([`ThreadEvent::ItemStarted`](crate::event::ThreadEvent::ItemStarted),
/// `ItemDelta`, `ItemCompleted`), never by position in a list: a position only means something
/// while one task runs, and several tasks at once is exactly what the studio hardware makes
/// possible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Item {
    #[serde(rename = "userMessage")]
    UserMessage(UserMessageItem),
    #[serde(rename = "agentMessage")]
    AgentMessage(AgentMessageItem),
    #[serde(rename = "reasoning")]
    Reasoning(ReasoningItem),
    #[serde(rename = "commandExecution")]
    CommandExecution(CommandExecutionItem),
    #[serde(rename = "fileChange")]
    FileChange(FileChangeItem),
    #[serde(rename = "baudet/toolCall")]
    ToolCall(ToolCallItem),
    #[serde(rename = "baudet/image")]
    Image(ImageItem),
    #[serde(rename = "plan")]
    Plan(PlanItem),
    #[serde(rename = "error")]
    Error(ErrorItem),
}

impl Item {
    pub fn item_id(&self) -> &str {
        match self {
            Item::UserMessage(i) => &i.item_id,
            Item::AgentMessage(i) => &i.item_id,
            Item::Reasoning(i) => &i.item_id,
            Item::CommandExecution(i) => &i.item_id,
            Item::FileChange(i) => &i.item_id,
            Item::ToolCall(i) => &i.item_id,
            Item::Image(i) => &i.item_id,
            Item::Plan(i) => &i.item_id,
            Item::Error(i) => &i.item_id,
        }
    }

    pub fn status(&self) -> ItemStatus {
        match self {
            Item::UserMessage(i) => i.status,
            Item::AgentMessage(i) => i.status,
            Item::Reasoning(i) => i.status,
            Item::CommandExecution(i) => i.status,
            Item::FileChange(i) => i.status,
            Item::ToolCall(i) => i.status,
            Item::Image(i) => i.status,
            Item::Plan(i) => i.status,
            Item::Error(i) => i.status,
        }
    }

    pub fn kind(&self) -> ItemKind {
        match self {
            Item::UserMessage(_) => ItemKind::UserMessage,
            Item::AgentMessage(_) => ItemKind::AgentMessage,
            Item::Reasoning(_) => ItemKind::Reasoning,
            Item::CommandExecution(_) => ItemKind::CommandExecution,
            Item::FileChange(_) => ItemKind::FileChange,
            Item::ToolCall(_) => ItemKind::ToolCall,
            Item::Image(_) => ItemKind::Image,
            Item::Plan(_) => ItemKind::Plan,
            Item::Error(_) => ItemKind::Error,
        }
    }

    /// The app-server item tag this row serializes as.
    pub fn wire_name(&self) -> &'static str {
        self.kind().wire_name()
    }

    pub fn with_status(mut self, status: ItemStatus) -> Item {
        match &mut self {
            Item::UserMessage(i) => i.status = status,
            Item::AgentMessage(i) => i.status = status,
            Item::Reasoning(i) => i.status = status,
            Item::CommandExecution(i) => i.status = status,
            Item::FileChange(i) => i.status = status,
            Item::ToolCall(i) => i.status = status,
            Item::Image(i) => i.status = status,
            Item::Plan(i) => i.status = status,
            Item::Error(i) => i.status = status,
        }
        self
    }

    /// Append streamed text to a text-bearing row; a no-op for rows that carry no text.
    pub fn push_str(&mut self, chunk: &str) {
        match self {
            Item::AgentMessage(i) => i.text.push_str(chunk),
            Item::Reasoning(i) => i.text.push_str(chunk),
            Item::UserMessage(i) => i.text.push_str(chunk),
            Item::CommandExecution(i) => i.output.push_str(chunk),
            _ => {}
        }
    }
}

mod defaults {
    pub fn yes() -> bool {
        true
    }
}
