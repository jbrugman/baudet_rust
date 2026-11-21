//! Durability: what survives closing the window.
//!
//! This is the layer `baudet_rust` did not have. The GUI phase deliberately deferred it, which was
//! right for a transcript that had to prove it could render, and is wrong for one that has to be
//! reopened: without a store, every restart starts from an empty conversation, and
//! `baudet_app::AppState` had no way to be told about beurt 1 when it was asked for beurt 2.
//!
//! Kotlin keeps this in H2 (`dev.baudet.store`), a SQL database behind a
//! [`TranscriptStore`](trait-shaped interface) with paging, upserts and rewrites. Shipped as Rust,
//! the same durability is a directory of JSON Lines, which is how the harness I measured does it:
//! flat text, one record per line, append-only.
//!
//! That is a real narrowing, and deliberately so. What is kept: a session's identity with its
//! `cwd` and checkout, the rendered trail, and the turns that went over the wire. What is not,
//! because nothing consumes it yet: projects, a compaction summary, and offset paging through
//! [`crate::rollout::Rollout::items`] — a sidebar is the caller that would need them, and writing
//! an index for a reader that does not exist yet is exactly the speculative abstraction this
//! rewrite is meant to avoid.
//!
//! Layers, and why they are separate:
//!
//!  * [`records`] — what one line can be, and the conversation's identity;
//!  * [`rollout`] — the file: create, append, tolerate a torn tail, list;
//!  * [`history`] — the turns as the model gets them, and their cost;
//!  * [`window`] — how far back one request may reach, by strategy;
//!  * [`data_dir`] — where any of it lives.

pub mod history;
pub mod records;
pub mod rollout;
pub mod window;

pub use history::{clamp_chars, cost_chars, estimate_tokens, history_of};
pub use records::{Record, Session, SessionMeta};
pub use rollout::Rollout;
pub use window::{bound, trim, ContextStrategy};

/// Where Baudet keeps its data.
///
/// Ported from `AppState.dataDir()`, including its warning, because the failure it describes is
/// not hypothetical: a packaged macOS app launched by Finder has `cwd=/`, so a relative
/// "baudet-data" would resolve to the read-only filesystem root. Never depend on the working
/// directory for durability. `BAUDET_HOME` overrides, which is what a checkout-local store and a
/// test both want.
pub fn data_dir() -> std::path::PathBuf {
    if let Ok(home) = std::env::var("BAUDET_HOME") {
        return std::path::PathBuf::from(home);
    }
    let home = std::env::var("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    if cfg!(target_os = "macos") {
        return home.join("Library").join("Application Support").join("Baudet");
    }
    if cfg!(target_os = "windows") {
        return std::env::var("APPDATA")
            .map(|appdata| std::path::Path::new(&appdata).join("Baudet"))
            .unwrap_or_else(|_| home.join(".baudet"));
    }
    std::env::var("XDG_DATA_HOME")
        .map(|xdg| std::path::Path::new(&xdg).join("baudet"))
        .unwrap_or_else(|_| home.join(".local").join("share").join("baudet"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_override_wins_and_never_depends_on_the_working_directory() {
        // The rule under test is the one Kotlin learned the hard way. Set and restored by hand
        // rather than a crate: one variable, and leaking it would poison every other test here.
        std::env::set_var("BAUDET_HOME", "/tmp/baudet-override-test");
        assert_eq!(data_dir(), std::path::PathBuf::from("/tmp/baudet-override-test"));
        std::env::remove_var("BAUDET_HOME");
        let fallback = data_dir();
        assert!(fallback.is_absolute() || fallback.as_os_str().is_empty());
        assert!(!fallback.to_string_lossy().contains("baudet-data"));
    }
}
