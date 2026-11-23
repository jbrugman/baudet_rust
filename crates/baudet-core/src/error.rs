
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
    for capture in LIMIT_CAPTURES.iter().filter_map(|re| first_number(re, &text)) {
        let cleaned: String = capture.chars().filter(|c| *c != ',' && *c != '_' && !c.is_whitespace()).collect();
        if let Ok(value) = cleaned.parse::<u32>() {
            if (MIN_WINDOW..=MAX_WINDOW).contains(&value) {
                return Some(value);
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

const LIMIT_CAPTURES: [&str; 4] = [
    "maximum context length is %n",
    "%n maximum context length",
    "context window %n",
    "token limit %n",
];

fn first_number(pattern: &str, text: &str) -> Option<String> {
    let (prefix, _) = pattern.split_once("%n")?;
    let anchor = text.find(prefix)?;
    let rest = &text[anchor + prefix.len()..];
    let start = rest.find(|c: char| c.is_ascii_digit())?;
    if start > 20 {
        return None;
    }
    let digits: String =
        rest[start..].chars().take_while(|c| c.is_ascii_digit() || matches!(c, ',' | '_' | ' ')).collect();
    Some(digits)
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
