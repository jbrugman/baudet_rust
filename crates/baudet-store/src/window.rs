//! How much of a conversation one request may carry.
//!
//! Translated from `dev.baudet.config.ContextStrategy` and `dev.baudet.session.ContextTrimmer`,
//! whose central idea is worth keeping in words because it is easy to get backwards: the stored
//! transcript is never edited. Only the copy handed to the model is cut, so what the user reads
//! stays whole while what gets sent fits the model.

use crate::history::cost_chars;
use baudet_core::Msg;

/// What happens when a conversation outgrows the model's window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContextStrategy {
    /// Send only the most recent turns. Keeps a local model on task: it sees the whole recent
    /// exchange and never a pinned oldest turn with the middle missing, which is the arrangement
    /// that made models drift back to a subject the conversation had already left.
    #[default]
    RecentMessages,
    /// Fill the window as full as the model can carry and roll the oldest turns off the front.
    RollingWindow,
    /// Send nothing that does not fit. The user then decides to compact or start fresh.
    StopAtLimit,
}

impl ContextStrategy {
    /// Whether a server-side chain may carry context.
    ///
    /// Only `StopAtLimit`, because trimming edits what we send and so cannot shrink a chain the
    /// backend already holds. This is the rule that decides whether `previous_response_id` is
    /// honoured at all (see `chainedPrev` in `AppState.kt`), and getting it wrong produces a
    /// session that silently holds context the client believes it trimmed.
    pub fn supports_server_session(self) -> bool {
        self == ContextStrategy::StopAtLimit
    }

    /// Whether the window is bounded by a turn count rather than by the model's budget, which is
    /// the only case in which a "keep N recent turns" setting means anything.
    pub fn trims_by_turn_count(self) -> bool {
        self == ContextStrategy::RecentMessages
    }
}

/// The window `strategy` asks for: the whole transcript under `StopAtLimit` — a guard refused
/// anything oversized long before this point — otherwise the bounded window.
///
/// One place holds the whole-versus-trimmed choice, as in Kotlin, so a one-shot turn and a tool
/// loop cannot disagree about how large a request may be. `keep_recent` is read only by
/// `RecentMessages`, matching [`ContextStrategy::trims_by_turn_count`].
pub fn bound(history: &[Msg], strategy: ContextStrategy, budget_tokens: usize, keep_recent: usize) -> Vec<Msg> {
    if strategy == ContextStrategy::StopAtLimit {
        return history.to_vec();
    }
    trim(history, strategy, budget_tokens, keep_recent)
}

/// The bounded window. A zero budget sends everything, which is Kotlin's "no ceiling configured"
/// rather than a bug: an unset limit means unbounded, not nothing.
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
        ContextStrategy::StopAtLimit => return history.to_vec(),
    };
    fit(window, budget_chars)
}

/// `window` reduced to `budget_chars`: the newest turn always goes, older turns roll off the front
/// with a note for them, and a newest turn too large on its own is clamped rather than dropped.
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

/// The newest contiguous tail of `prior` that fits in `budget_chars` *including* its note, oldest
/// first, with a note standing in for whatever fell off the front.
///
/// The note is costed inside the search rather than added afterwards, because adding it afterwards
/// is what lets the window overshoot the budget the caller named. One costed candidate per
/// iteration keeps that arithmetic honest.
fn rolling(prior: &[Msg], budget_chars: usize) -> Vec<Msg> {
    if prior.is_empty() {
        return Vec::new();
    }
    // Everything fits: no note needed, because nothing was dropped.
    if prior.iter().map(cost_chars).sum::<usize>() <= budget_chars {
        return prior.to_vec();
    }
    for dropped in 1..prior.len() {
        // `dropped` is what falls off the front, so the tail is `prior[dropped..]` and the turn
        // after it always includes `prior`'s last: this window is the one whose newest turn the
        // caller already chose to keep.
        let tail = &prior[dropped..];
        let cost = tail.iter().map(cost_chars).sum::<usize>() + cost_chars(&crate::history::dropped_note(dropped));
        if cost <= budget_chars {
            let mut out = vec![crate::history::dropped_note(dropped)];
            out.extend_from_slice(tail);
            return out;
        }
    }
    // Not even one older turn fits beside a note, so nothing older goes and no note is needed.
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
        // Nothing is trimmed here: the refusing guard lives in the app layer and runs *before*
        // the request, so nothing gets billed for a turn that was never going to fit.
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
        // chars/3 rounds each message up, so the token figure is a ceiling, never an undercount.
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
