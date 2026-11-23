
const KNOWN: &[(&str, u32)] = &[
    ("gpt-4.1", 1_047_576),
    ("gpt-4o", 128_000),
    ("gpt-4-turbo", 128_072),
    ("gpt-oss", 131_072),
    ("gpt-4", 8_192),
    ("gpt-3.5", 16_385),
    ("llama-3.1", 131_072),
    ("llama-3.2", 131_072),
    ("llama-3.3", 131_072),
    ("llama-3", 8_192),
    ("llama-2", 4_096),
    ("codellama", 16_384),
    ("llama", 4_096),
    ("qwen", 32_768),
    ("mistral-nemo", 128_000),
    ("mistral", 32_768),
    ("mixtral", 32_768),
    ("command-r", 128_000),
    ("gemma-3", 32_768),
    ("gemma", 8_192),
    ("phi", 4_096),
    ("deepseek", 32_768),
    ("glm-4", 32_768),
    ("yi-", 32_768),
];

pub fn known(model: &str) -> Option<u32> {
    let id = model.to_lowercase();
    KNOWN.iter().find(|(fragment, _)| id.contains(fragment)).map(|(_, window)| *window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn most_specific_fragment_wins() {
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
        assert_eq!(known("gpt-5"), None);
        assert_eq!(known(""), None);
    }
}
