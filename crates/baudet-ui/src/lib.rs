//! Baudet's first phase: the transcript, with no backend process behind it.
//!
//! The reason this crate can exist before a transport does is the seam it drives. `Backend` is
//! object-safe and returns events over a channel, so a scripted player satisfies it exactly as a
//! socket would -- which is the same trick the JVM suite used to drive a whole turn without a
//! frame on screen. Swapping in a real adapter later is one `impl Backend` and no change here.
//!
//! Two boundaries are deliberate:
//!
//!  * rendering reads folded rows and nothing else. It never accumulates deltas into its own copy
//!    of the truth, because the reducer already does that and two copies disagree eventually;
//!  * the conversation owner lives on one worker thread, not in the paint pass. eframe's closure
//!    borrows the app for the duration of a frame, so an owner reachable only behind `&mut self`
//!    could not be shared with the thread that drains the stream; one owner plus two message
//!    channels is both simpler and free of lock ordering.

mod app;
mod palette;
mod panels;
mod rows;

pub use app::{run, run_from, Startup, TranscriptApp};
pub use palette::DiffLine;
pub use rows::Row;
