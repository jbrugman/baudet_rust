use baudet_core::{Msg, Turn};
use baudet_store::ContextLimits;

pub const SUMMARY_PROMPT: &str = "Summarise the conversation so work can continue after compaction. \
    Preserve the active user request, constraints, decisions, completed actions and their results, \
    and work still outstanding. Keep relevant file names, symbols and factual conclusions. \
    Be concise and omit commentary and preamble.";

pub(crate) fn summarize(
    backend: &dyn baudet_backend::Backend,
    model: Option<String>,
    source: &str,
    max_tokens: u32,
    cancelled: impl Fn() -> bool,
) -> Turn {
    let request = baudet_backend::SendRequest {
        system: SUMMARY_PROMPT.to_owned(),
        user_text: source.to_owned(),
        model,
        reasoning: Some(baudet_backend::openai::REASONING_NONE.to_owned()),
        max_output_tokens: Some(max_tokens),
        store: Some(false),
        ..Default::default()
    };
    crate::session::receive_turn(backend, request, cancelled, &mut |_| {}, &mut |_| {})
}

#[derive(Debug, PartialEq, Eq)]
pub enum CompactionOutcome {
    Compacted { summary: String, folded: usize },
    NothingToFold,
    OversizedTurn { max_chars: usize },
    SummaryRefused(String),
}

pub struct CompactionPlan {
    pub keep_recent: usize,
    pub max_chars: usize,
    pub max_tokens: u32,
}

impl From<&ContextLimits> for CompactionPlan {
    fn from(limits: &ContextLimits) -> Self {
        let window = limits.context_window();
        let budget = limits.typed_budget().map_or(48_000, |tokens| tokens.saturating_mul(3));
        let source = window.map_or(48_000, |tokens| (tokens / 2).saturating_mul(3));
        Self {
            keep_recent: limits.keep_recent_messages,
            max_chars: 48_000.min(budget).min(source).max(4_000) as usize,
            max_tokens: window.map_or(2_048, |tokens| (tokens / 4).clamp(1_536, 2_048)),
        }
    }
}

const SUMMARY_PREFIX: &str = "(previous summary) ";

fn chars(text: &str) -> usize {
    text.chars().count()
}

fn message_cost(message: &Msg) -> usize {
    chars(&message.role) + chars(&message.content) + 4
}

fn source(summary: Option<&str>, messages: &[Msg]) -> String {
    let mut text = String::new();
    if let Some(summary) = summary {
        text.push_str(SUMMARY_PREFIX);
        text.push_str(summary);
        text.push_str("\n\n");
    }
    for message in messages {
        text.push_str(&message.role);
        text.push_str(": ");
        text.push_str(&message.content);
        text.push_str("\n\n");
    }
    text
}

fn summary_cost(summary: Option<&str>) -> usize {
    summary.map_or(0, |summary| SUMMARY_PREFIX.len() + chars(summary) + 2)
}

fn accepted(turn: Turn) -> Result<String, CompactionOutcome> {
    if let Some(reason) = &turn.incomplete_reason {
        return Err(CompactionOutcome::SummaryRefused(reason.clone()));
    }
    if !turn.completed() {
        return Err(CompactionOutcome::SummaryRefused("missing successful completion event".into()));
    }
    if turn.text.trim().is_empty() {
        return Err(CompactionOutcome::SummaryRefused("the model returned no summary text".into()));
    }
    Ok(turn.text.trim().to_owned())
}

pub struct Compactor;

impl Compactor {
    pub fn fold(
        plan: &CompactionPlan,
        messages: &[Msg],
        previous: Option<&str>,
        mut summarize: impl FnMut(&str, u32) -> Result<Turn, String>,
    ) -> Result<CompactionOutcome, String> {
        let mut remaining = &messages[..messages.len().saturating_sub(plan.keep_recent)];
        if remaining.is_empty() {
            return Ok(CompactionOutcome::NothingToFold);
        }
        let mut summary = previous.map(str::to_owned);
        let mut folded = 0;
        while let Some(first) = remaining.first() {
            let mut available = plan.max_chars.saturating_sub(summary_cost(summary.as_deref()));
            let count = remaining
                .iter()
                .take_while(|msg| {
                    let cost = message_cost(msg);
                    if cost > available {
                        return false;
                    }
                    available -= cost;
                    true
                })
                .count();
            let result = if count > 0 {
                summarize(&source(summary.as_deref(), &remaining[..count]), plan.max_tokens).map(accepted)
            } else {
                Self::fold_large(plan, first, summary.as_deref(), &mut summarize)
            };
            match result {
                Ok(Ok(next)) => {
                    summary = Some(next);
                    let count = count.max(1);
                    folded += count;
                    remaining = &remaining[count..];
                }
                Ok(Err(outcome)) if folded == 0 => return Ok(outcome),
                Err(err) if folded == 0 => return Err(err),
                _ => break,
            }
        }
        Ok(CompactionOutcome::Compacted { summary: summary.expect("a successful fold has a summary"), folded })
    }

    fn fold_large(
        plan: &CompactionPlan,
        message: &Msg,
        previous: Option<&str>,
        summarize: &mut impl FnMut(&str, u32) -> Result<Turn, String>,
    ) -> Result<Result<String, CompactionOutcome>, String> {
        let mut text = message.content.as_str();
        let mut summary = previous.map(str::to_owned);
        let oversized = || CompactionOutcome::OversizedTurn { max_chars: plan.max_chars };
        if text.is_empty() {
            return Ok(Err(oversized()));
        }
        // Commit a replacement summary only after every slice succeeds.
        while !text.is_empty() {
            let room = plan.max_chars.saturating_sub(summary_cost(summary.as_deref()) + chars(&message.role) + 4);
            if room == 0 {
                return Ok(Err(oversized()));
            }
            let end = text.char_indices().nth(room).map_or(text.len(), |(end, _)| end);
            let piece = Msg::new(&message.role, &text[..end]);
            match accepted(summarize(&source(summary.as_deref(), &[piece]), plan.max_tokens)?) {
                Ok(next) => summary = Some(next),
                Err(reason) => return Ok(Err(reason)),
            }
            text = &text[end..];
        }
        Ok(Ok(summary.expect("at least one slice was summarized")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_during_an_oversized_message_keeps_that_whole_message() {
        let messages = [Msg::user("short"), Msg::user("é".repeat(140)), Msg::user("recent")];
        let plan = CompactionPlan { keep_recent: 1, max_chars: 60, max_tokens: 512 };
        let mut calls = 0;
        let result = Compactor::fold(&plan, &messages, None, |source, _| {
            assert!(chars(source) <= 60);
            calls += 1;
            if calls == 3 {
                let mut refusal = Turn::text("partial");
                refusal.incomplete_reason = Some("length".into());
                Ok(refusal)
            } else {
                Ok(Turn::text(format!("summary {calls}")))
            }
        })
        .unwrap();
        assert_eq!(result, CompactionOutcome::Compacted { summary: "summary 1".into(), folded: 1 });
    }

    #[test]
    fn only_completed_nonempty_summaries_are_accepted() {
        let plan = CompactionPlan { keep_recent: 1, max_chars: 100, max_tokens: 512 };
        let messages = [Msg::user("old"), Msg::user("recent")];
        for response in [Turn::text(" "), Turn { final_event: None, ..Turn::text("looks complete") }] {
            assert!(matches!(
                Compactor::fold(&plan, &messages, None, |_, _| Ok(response.clone())),
                Ok(CompactionOutcome::SummaryRefused(_))
            ));
        }
        assert!(Compactor::fold(&plan, &messages, None, |_, _| Err("offline".into())).is_err());
    }
}
