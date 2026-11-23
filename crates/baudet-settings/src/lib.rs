//! Persistent settings and provider configuration.

pub mod config;
pub mod provider;
pub mod store;

pub use config::{
    Settings, DEFAULT_KEY_ENV_VAR, DEFAULT_MAX_PARALLEL_TURNS, DEFAULT_MAX_TOOL_ITERATIONS, DEFAULT_SYSTEM_PROMPT,
};
pub use provider::{ListingError, Model, Provider, LISTING_TIMEOUT};
pub use store::{ReadOutcome, SettingsFile, WriteError};
