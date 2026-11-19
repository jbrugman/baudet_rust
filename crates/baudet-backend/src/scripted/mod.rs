//! A backend that replays a prepared turn, so the interface can be built and finished before a
//! real one exists.
//!
//! This is the Rust shape of what the Kotlin `AppState*Spec` suites already do -- drive the
//! run/stop/persist decision against a scripted backend -- promoted from a test helper to a
//! first-class product. Two rules keep it honest, because a mock that lies is worse than no mock:
//!
//!  * it publishes through the *same* [`Script`](Script) event-builder the real adapter will feed
//!    from its SSE frames, so a script that bundles deltas more nicely than a wire does cannot
//!    silently make a stuttering UI look finished;
//!  * it never invents a terminal event. A script that ends without `turn/completed` really does
//!    end without it, which is how "no answer" stays distinguishable from "budget exhausted".

pub mod script;

pub use script::{RawMapper, Script, ScriptedBackend, Step};
