//! The orchestrator: one owner of the transcript, driven by a stream of facts.
//!
//! This is where a Rust rewrite earns its keep. Baudet's `AppState` guards shared mutable state
//! with 30 `invokeLater`, 22 `synchronized` and 8 `@Volatile` sites, because a JVM worker thread
//! and the event-dispatch thread both reach the same object. Here there is one owner of the rows
//! and one receiver of events, so there is no shared state to defend: the reducer is a pure
//! function over `(rows, event)`, which also makes it directly testable -- the same trick that
//! lets the Kotlin `AppState*Spec` suites drive a turn without a frame on screen.

pub mod reducer;
pub mod session;

pub use reducer::{reduce, Rows};
pub use session::{AppState, Sent};
