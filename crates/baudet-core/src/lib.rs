//! Shared model requests, transcript items, and application events.

pub mod context;
pub mod error;
pub mod event;
pub mod item;
pub mod mcp;
pub mod model_windows;
pub mod msg;
pub mod protocol;
pub mod usage;

pub use context::{probe_changes, probe_git, ChangeKind, ChangedFile, GitMeta, SessionContext, GIT_PROBE_TIMEOUT};
pub use error::CoreError;
pub use event::{delta_method, ApprovalRequest, ThreadEvent};
pub use item::{
    AgentMessageItem, ApprovalDecision, CommandExecutionItem, DiffStats, ErrorItem, FileChangeItem, FileChangeKind,
    ImageItem, Item, ItemKind, ItemStatus, PlanEntry, PlanEntryStatus, PlanItem, ReasoningItem, ToolCallItem,
    UserMessageItem,
};
pub use msg::Msg;
pub use model_windows::known as known_context_window;
pub use protocol::{FunctionCall, Turn};
pub use usage::Usage;
