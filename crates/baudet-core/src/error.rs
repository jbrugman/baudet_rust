//! A backend that answered with an error.
//!
//! Translated from `dev.baudet.api.BackendException`. `context_overflow` singles out the one
//! error the user can act on without knowing anything about token maths: the prompt no longer
//! fits the model. Matching is on the words the two common backend families print, because the
//! status code alone does not separate "too long" from "bad field" and the raw body is not
//! something to show.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CoreError {
    /// The backend answered with an HTTP error carrying `detail`, the body as it arrived.
    #[error("HTTP {status} from backend: {detail}")]
    Http { status: u16, detail: String },
    /// The stream closed, or was cancelled, before any terminal event arrived.
    #[error("stream closed before a terminal event: {detail}")]
    Incomplete { detail: String },
    /// A decision was asked of a listener that is not there; the turn cannot continue.
    #[error("no listener answered {request}")]
    Unanswered { request: &'static str },
}

/// The window this backend says it has, read out of the same complaint that makes
/// [`looks_like_context_overflow`] true, or `None` when the wording names no number. This is how
/// the app learns the size of a model it has never heard of: the refusal states the limit, so
/// the next turn is guarded against the truth instead of against a guess.
pub fn claimed_context_tokens_of(message: Option<&str>) -> Option<u32> {
    let raw = message?;
    if !looks_like_context_overflow(Some(raw)) {
        return None;
    }
    let text = raw.to_lowercase();
    for capture in LIMIT_CAPTURES.iter().filter_map(|re| first_number(re, &text)) {
        let cleaned: String = capture.chars().filter(|c| *c != ',' && *c != '_' && !c.is_whitespace()).collect();
        if let Ok(value) = cleaned.parse::<u32>() {
            // Below this a number is a token count in a breakdown, not a window; above it a typo.
            if (MIN_WINDOW..=MAX_WINDOW).contains(&value) {
                return Some(value);
            }
        }
    }
    None
}

const MIN_WINDOW: u32 = 512;
const MAX_WINDOW: u32 = 10_000_000;

/// Whether `message` carries a context-window complaint, whatever backend wrote it.
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

/// The limit, sought only next to the words that name a limit. Reading every number in the body
/// instead would pick up the count that overflowed and the token breakdown some backends print
/// with it, and a limit learned from those is wrong in the one direction that matters: too high,
/// so the next request fails the same way.
///
/// Each entry is a literal prefix followed by `%n`, the placeholder this helper reads as "the
/// first run of digits (with separators) here". Spelled as literals rather than as a regex
/// dependency, because these are the four wordings two backend families were observed to print.
const LIMIT_CAPTURES: [&str; 4] = [
    // "maximum context length is 128072 tokens" / "...length of 128072"
    "maximum context length is %n",
    // "129232 tokens > 128000 maximum context length"
    "%n maximum context length",
    // "context window of 131072" / "context length limit 32768"
    "context window %n",
    // "token limit of 128000" / "exceeds the 128000 token limit"
    "token limit %n",
];

/// Find the `%n` placeholder in `pattern` and return the digit run that follows it in `text`.
fn first_number(pattern: &str, text: &str) -> Option<String> {
    let (prefix, _) = pattern.split_once("%n")?;
    let anchor = text.find(prefix)?;
    let rest = &text[anchor + prefix.len()..];
    let start = rest.find(|c: char| c.is_ascii_digit())?;
    // Only a plausible gap between the wording and the number; "of" and "is" are 2-3 chars, a
    // 20-char gap means the digits belong to something else entirely.
    if start > 20 {
        return None;
    }
    let digits: String =
        rest[start..].chars().take_while(|c| c.is_ascii_digit() || matches!(c, ',' | '_' | ' ')).collect();
    Some(digits)
}

impl CoreError {
    /// Whether this error is the context-window complaint.
    pub fn context_overflow(&self) -> bool {
        match self {
            CoreError::Http { detail, .. } => looks_like_context_overflow(Some(detail)),
            _ => false,
        }
    }

    /// The window named in the error body, if any.
    pub fn claimed_context_tokens(&self) -> Option<u32> {
        match self {
            CoreError::Http { detail, .. } => claimed_context_tokens_of(Some(detail)),
            _ => None,
        }
    }
}
