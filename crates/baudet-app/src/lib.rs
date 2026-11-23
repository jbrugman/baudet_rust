//! Conversation state, turn orchestration, and display-row reduction.

pub mod compaction;
pub mod reducer;
pub mod session;

pub use reducer::{reduce, Emitter, Rows};
pub use session::{AppState, Sent};
