//! Conversation history, display trails, and context limits.

pub mod budget;
pub mod history;
pub mod paging;
pub mod records;
pub mod repository;
pub mod rollout;
pub mod session;
pub mod transcript;
pub mod window;

pub use budget::{
    ContextLimits, ContextReading, CONTEXT_LOSS_NOTE, FOLD_WINDOW_PERCENT, INPUT_OVERHEAD_TOKENS,
    MAX_OVERFLOW_RETRIES, MIN_CONTEXT_CEILING_TOKENS, OVERFLOW_SHRINK_STEP, ROLLING_WINDOW_PERCENT,
    WINDOW_MARGIN_TOKENS, instructions, window_key,
};
pub use history::{clamp_chars, cost_chars, estimate_tokens, history_of};
pub use paging::ItemWindow;
pub use records::{Record, Session, SessionDetail, SessionMeta};
pub use repository::{page, Repository};
pub use rollout::Rollout;
pub use transcript::Transcript;
pub use window::{bound, trim, ContextStrategy};

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
        std::env::set_var("BAUDET_HOME", "/tmp/baudet-override-test");
        assert_eq!(data_dir(), std::path::PathBuf::from("/tmp/baudet-override-test"));
        std::env::remove_var("BAUDET_HOME");
        let fallback = data_dir();
        assert!(fallback.is_absolute() || fallback.as_os_str().is_empty());
        assert!(!fallback.to_string_lossy().contains("baudet-data"));
    }
}
