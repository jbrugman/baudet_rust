
use serde::Deserialize;
use serde::Serialize;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// Image bytes stay in the display trail and out of model-facing observations.
pub struct ImageItem {
    pub item_id: String,
    pub tool: String,
    pub mime_type: String,
    pub base64: String,
    pub status: ItemStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessageItem {
    pub item_id: String,
    #[serde(default)]
    pub text: String,
    pub status: ItemStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningItem {
    pub item_id: String,
    #[serde(default)]
    pub text: String,
    pub status: ItemStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserMessageItem {
    pub item_id: String,
    pub text: String,
    pub status: ItemStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub item_id: String,
    #[serde(default)]
    pub entries: Vec<PlanEntry>,
    pub status: ItemStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorItem {
    pub item_id: String,
    pub message: String,
    pub status: ItemStatus,
}

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
