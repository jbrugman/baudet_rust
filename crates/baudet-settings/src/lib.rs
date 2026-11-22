//! What the app remembers, and what it can ask for.
//!
//! Two things a person chooses before a turn is worth sending: where requests go, and which model
//! answers them. Kotlin splits this across `SettingsStore`, `AppConfig`, `ModelsClient` and
//! `ModelInfo`; the split is kept here because the two halves change for different reasons —
//! a file format versus an HTTP response — but it is a smaller split than four files, because none
//! of them holds much.
//!
//! Boundaries, and why:
//!
//!  * [`config`] — the choices and their defaults, tolerant of a file written by another build;
//!  * [`store`] — reading and writing that file, and the rule that an unreadable file is never
//!    written over;
//!  * [`provider`] — the endpoint derived from those choices, and the one network call: which
//!    models it has.
//!
//! Deliberately absent, because nothing consumes it yet: per-model pricing (the reference app does
//! not carry it either), a provider-registry, and a keychain. Adding one of those is a decision
//! with a reader attached, not a field to reserve.

pub mod config;
pub mod provider;
pub mod store;

pub use config::{
    Settings, DEFAULT_KEY_ENV_VAR, DEFAULT_MAX_PARALLEL_TURNS, DEFAULT_MAX_TOOL_ITERATIONS, DEFAULT_SYSTEM_PROMPT,
};
pub use provider::{ListingError, Model, Provider, LISTING_TIMEOUT};
pub use store::{ReadOutcome, SettingsFile, WriteError};
