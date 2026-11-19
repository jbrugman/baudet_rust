//! Baudet's internal seam: the vocabulary of a conversation, as facts rather than as text.
//!
//! This crate holds no UI, no transport and no storage. It is the contract every other layer
//! speaks, translated 1:1 from the Kotlin originals so behaviour is protected by the same
//! intents the JVM suite already encodes:
//!
//!  * [`item`] -- what a row is and what state it is in (`Item.kt`);
//!  * [`event`] -- everything that happens, each event scoped by thread and turn (`ThreadEvent.kt`);
//!  * [`protocol`] -- what a turn returned, including the backend's own terminal signals (`Turn.kt`);
//!  * [`usage`] -- token accounting, where a missing number stays missing (`Usage.kt`);
//!  * [`error`] -- a backend complaint, and the one error a user can act on (`BackendException.kt`).

pub mod error;
pub mod event;
pub mod item;
pub mod protocol;
pub mod usage;

pub use error::CoreError;
pub use event::{delta_method, ApprovalRequest, ThreadEvent};
pub use item::{
    AgentMessageItem, ApprovalDecision, CommandExecutionItem, DiffStats, ErrorItem, FileChangeItem, FileChangeKind,
    ImageItem, Item, ItemKind, ItemStatus, PlanEntry, PlanEntryStatus, PlanItem, ReasoningItem, ToolCallItem,
    UserMessageItem,
};
pub use protocol::{FunctionCall, Turn};
pub use usage::Usage;
