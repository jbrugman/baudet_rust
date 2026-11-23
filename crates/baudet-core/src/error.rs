
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CoreError {
    #[error("HTTP {status} from backend: {detail}")]
    Http { status: u16, detail: String },
    #[error("stream closed before a terminal event: {detail}")]
    Incomplete { detail: String },
    #[error("no listener answered {request}")]
    Unanswered { request: &'static str },
}

pub fn claimed_context_tokens_of(message: Option<&str>) -> Option<u32> {
    let raw = message?;
    if !looks_like_context_overflow(Some(raw)) {
        return None;
    }
    let text = raw.to_lowercase();
    for anchor in LIMIT_ANCHORS {
        for (start, _) in text.match_indices(anchor) {
            let after = text[start + anchor.len()..].trim_start();
            let after = after.strip_prefix("is ").or_else(|| after.strip_prefix("of ")).unwrap_or(after);
            let end = after.find(|c| !number_char(c)).unwrap_or(after.len());
            if let Some(value) = parse_window(&after[..end]) {
                return Some(value);
            }
            if anchor == "maximum context length" {
                let before = text[..start].trim_end();
                let number = before.rsplit(|c| !number_char(c)).next().unwrap_or_default();
                if let Some(value) = parse_window(number) {
                    return Some(value);
                }
            }
        }
    }
    None
}

const MIN_WINDOW: u32 = 512;
const MAX_WINDOW: u32 = 10_000_000;

pub fn looks_like_context_overflow(message: Option<&str>) -> bool {
    let Some(raw) = message else { return false };
    let text = raw.to_lowercase();
    OVERFLOW_WORDS.iter().any(|needle| text.contains(needle))
}

const OVERFLOW_WORDS: [&str; 6] = [
    "prompt is too long",
    "context_length_exceeded",
    "maximum context length",
    "context length exceeded",
    "too many tokens",
    "input is too long",
];

const LIMIT_ANCHORS: [&str; 4] = ["maximum context length", "context window", "context length limit", "token limit"];

fn number_char(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, ',' | '_' | ' ')
}

fn parse_window(text: &str) -> Option<u32> {
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|value| (MIN_WINDOW..=MAX_WINDOW).contains(value))
}

impl CoreError {
    pub fn context_overflow(&self) -> bool {
        match self {
            CoreError::Http { detail, .. } => looks_like_context_overflow(Some(detail)),
            _ => false,
        }
    }

    pub fn claimed_context_tokens(&self) -> Option<u32> {
        match self {
            CoreError::Http { detail, .. } => claimed_context_tokens_of(Some(detail)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::claimed_context_tokens_of;

    #[test]
    fn only_explicit_model_limits_are_learned() {
        for message in [
            "Maximum context length of 8,192 tokens, but the request used 10,024",
            "10,024 tokens > 8,192 maximum context length",
            "context_length_exceeded: context window 8_192",
        ] {
            assert_eq!(claimed_context_tokens_of(Some(message)), Some(8192), "{message}");
        }
        assert_eq!(claimed_context_tokens_of(Some("maximum context length exceeded by 800 tokens")), None);
        assert_eq!(claimed_context_tokens_of(Some("too many tokens: request used 10,024")), None);
    }
}
