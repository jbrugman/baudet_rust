//! The context window a model is known to have.
//!
//! Translated from `dev.baudet.api.ModelWindows`. The point of a table instead of a number typed in
//! Settings is that a turn should be guarded against the size the backend actually refuses.
//!
//! Every value is deliberately pessimistic, and the direction of the pessimism is not arbitrary: a
//! window that is too low only folds a conversation a little early, while one that is too high lets
//! a request through that the backend is certain to reject -- the very failure this table exists to
//! prevent. Where a family ships both a plain and a long-context variant, only the id saying so is
//! admitted at the larger size.
//!
//! The table is a floor under the budget the user types, never a replacement for it: a budget set
//! below the window still wins, because that is someone keeping a conversation cheap on purpose.

/// Id fragments and their window, most specific first: the first fragment contained in the id
/// decides, so `gpt-4.1` has to be listed before `gpt-4` and `llama-3.1` before `llama-3`.
const KNOWN: &[(&str, u32)] = &[
    // OpenAI: the 4.1 line really is that large, the plain gpt-4 id is not.
    ("gpt-4.1", 1_047_576),
    ("gpt-4o", 128_000),
    ("gpt-4-turbo", 128_072),
    ("gpt-oss", 131_072),
    ("gpt-4", 8_192),
    ("gpt-3.5", 16_385),
    // Llama: only 3.1 and up widened the window, the earlier ids stayed small.
    ("llama-3.1", 131_072),
    ("llama-3.2", 131_072),
    ("llama-3.3", 131_072),
    ("llama-3", 8_192),
    ("llama-2", 4_096),
    ("codellama", 16_384),
    ("llama", 4_096),
    // Qwen: many builds run with YaRN applied, which is not visible in the id.
    ("qwen", 32_768),
    // Mistral family: nemo is the documented exception at 128k.
    ("mistral-nemo", 128_000),
    ("mistral", 32_768),
    ("mixtral", 32_768),
    ("command-r", 128_000),
    ("gemma-3", 32_768),
    ("gemma", 8_192),
    // Small local models: several run at 4k in practice regardless of their papers.
    ("phi", 4_096),
    ("deepseek", 32_768),
    ("glm-4", 32_768),
    ("yi-", 32_768),
];

/// The window for `model`, or `None` when this id is not in the table.
///
/// Matching is on the lowercased id, so a served id carrying a quantisation suffix or a publisher
/// prefix still resolves through its family fragment.
pub fn known(model: &str) -> Option<u32> {
    let id = model.to_lowercase();
    KNOWN.iter().find(|(fragment, _)| id.contains(fragment)).map(|(_, window)| *window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn most_specific_fragment_wins() {
        // Each of these ids contains two fragments, and only the longer one is right.
        assert_eq!(known("gpt-4.1-mini"), Some(1_047_576));
        assert_eq!(known("llama-3.1-70b-instruct"), Some(131_072));
        assert_eq!(known("mistral-nemo-12b"), Some(128_000));
        assert_eq!(known("gemma-3-4b-it"), Some(32_768));
    }

    #[test]
    fn plain_family_ids_get_their_own_window() {
        assert_eq!(known("gpt-4"), Some(8_192));
        assert_eq!(known("gpt-3.5-turbo"), Some(16_385));
        assert_eq!(known("llama-3-8b"), Some(8_192));
        assert_eq!(known("llama-2-7b"), Some(4_096));
        assert_eq!(known("codellama-13b"), Some(16_384));
        assert_eq!(known("mistral-7b"), Some(32_768));
        assert_eq!(known("mixtral-8x7b"), Some(32_768));
        assert_eq!(known("gemma-2b"), Some(8_192));
    }

    #[test]
    fn matching_ignores_case() {
        assert_eq!(known("GPT-4o"), Some(128_000));
        assert_eq!(known("Qwen3.8-Flash-Next-oQ4e-mtp"), Some(32_768));
    }

    #[test]
    fn an_unknown_id_stays_unknown() {
        // Nothing here guesses: inventing a window is exactly the too-high value the table avoids.
        assert_eq!(known("gpt-5"), None);
        assert_eq!(known(""), None);
    }
}
