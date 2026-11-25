
use crate::history::cost_chars;
use baudet_core::Msg;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
// Context strategies decide whether to trim older messages or refuse an oversized request.
pub enum ContextStrategy {
    #[default]
    RecentMessages,
    RollingWindow,
    StopAtLimit,
    AutoCompact,
}

impl ContextStrategy {
    pub fn supports_server_session(self) -> bool {
        self == ContextStrategy::StopAtLimit
    }

    pub fn trims_history(self) -> bool {
        matches!(self, Self::RecentMessages | Self::RollingWindow)
    }

    pub fn trims_by_turn_count(self) -> bool {
        self == ContextStrategy::RecentMessages
    }
}

pub fn bound(history: &[Msg], strategy: ContextStrategy, budget_tokens: usize, keep_recent: usize) -> Vec<Msg> {
    if !strategy.trims_history() {
        return history.to_vec();
    }
    trim(history, strategy, budget_tokens, keep_recent)
}

pub fn trim(history: &[Msg], strategy: ContextStrategy, budget_tokens: usize, keep_recent: usize) -> Vec<Msg> {
    if budget_tokens == 0 || history.is_empty() {
        return history.to_vec();
    }
    let budget_chars = budget_tokens * 3;
    let window: &[Msg] = match strategy {
        ContextStrategy::RecentMessages if keep_recent > 0 && keep_recent < history.len() => {
            &history[history.len() - keep_recent..]
        }
        ContextStrategy::RecentMessages | ContextStrategy::RollingWindow => history,
        ContextStrategy::StopAtLimit | ContextStrategy::AutoCompact => return history.to_vec(),
    };
    fit(window, budget_chars)
}

fn fit(window: &[Msg], budget_chars: usize) -> Vec<Msg> {
    let last = window.last().expect("callers guard the empty window");
    let last_cost = cost_chars(last);
    if last_cost > budget_chars {
        return vec![crate::history::clamp_one(last, budget_chars)];
    }
    let prior = &window[..window.len() - 1];
    if prior.is_empty() {
        return vec![last.clone()];
    }
    rolling(prior, budget_chars - last_cost).into_iter().chain(std::iter::once(last.clone())).collect()
}

fn rolling(prior: &[Msg], budget_chars: usize) -> Vec<Msg> {
    if prior.is_empty() {
        return Vec::new();
    }
    if prior.iter().map(cost_chars).sum::<usize>() <= budget_chars {
        return prior.to_vec();
    }
    for dropped in 1..prior.len() {
        let tail = &prior[dropped..];
        let cost = tail.iter().map(cost_chars).sum::<usize>() + cost_chars(&crate::history::dropped_note(dropped));
        if cost <= budget_chars {
            let mut out = vec![crate::history::dropped_note(dropped)];
            out.extend_from_slice(tail);
            return out;
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::estimate_tokens;
    use baudet_core::Msg;

    fn turns(n: usize) -> Vec<Msg> {
        (0..n).flat_map(|i| vec![Msg::user(format!("vraag {i}")), Msg::assistant(format!("antwoord {i}"))]).collect()
    }

    #[test]
    fn stop_at_limit_sends_the_whole_transcript() {
        let all = turns(4);
        assert_eq!(bound(&all, ContextStrategy::StopAtLimit, 1, 1), all);
        assert!(ContextStrategy::StopAtLimit.supports_server_session());
        assert!(!ContextStrategy::RecentMessages.supports_server_session());
    }

    #[test]
    fn recent_messages_reaches_back_by_count_only() {
        let all = turns(10);
        let window = bound(&all, ContextStrategy::RecentMessages, usize::MAX / 4, 3);
        assert_eq!(window.len(), 3);
        assert_eq!(window.last(), all.last());
    }

    #[test]
    fn rolling_window_reaches_back_as_far_as_the_budget_allows() {
        let all = turns(30);
        let tokens = estimate_tokens(&all) / 2;
        let window = bound(&all, ContextStrategy::RollingWindow, tokens, usize::MAX);
        assert!(window.len() > 1, "budget, not count, sets the reach");
        assert!(window.len() < all.len());
        assert_eq!(window.last(), all.last());
        assert!(estimate_tokens(&window) <= tokens, "{} > {tokens}", estimate_tokens(&window));
    }

    #[test]
    fn trimming_never_edits_the_transcript_it_was_handed() {
        let all = turns(10);
        let before = all.clone();
        let _ = bound(&all, ContextStrategy::RollingWindow, 1, 1);
        assert_eq!(all, before, "the stored transcript is not the outbound copy");
    }

    #[test]
    fn an_unconfigured_budget_means_unbounded_not_empty() {
        let all = turns(3);
        assert_eq!(trim(&all, ContextStrategy::RollingWindow, 0, usize::MAX), all);
    }

    #[test]
    fn only_recent_messages_reads_a_turn_count() {
        assert!(ContextStrategy::RecentMessages.trims_by_turn_count());
        assert!(!ContextStrategy::RollingWindow.trims_by_turn_count());
        assert!(!ContextStrategy::StopAtLimit.trims_by_turn_count());
    }
}
