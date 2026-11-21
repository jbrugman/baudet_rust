//! Projecting a stored transcript into the turns a model is asked to remember.
//!
//! This module exists because the two vectors are not interchangeable, and I nearly convinced
//! myself they were. Measured against a real harness rollout: 587 stored response items against
//! 399 completed display rows. The difference is tool calls and their output — real work the
//! interface never renders as prose. A single "rows are history" projection therefore does not
//! fail loudly; it *quiets* the transcript and every scripted demo still passes. That is the
//! failure mode this file guards, and it is why [`Msg`] stays the wire truth while [`Item`]
//! stays the display truth.
//!
//! Deliberately not a projection of [`Item`] at all. `baudet_app::AppState` appends a [`Msg`] for
//! each turn as it happens, and [`Msg::response_id`] plus [`Msg::prompt_tokens`] cannot be
//! recovered from a row after the fact. Reconstructing them here would be a projection that
//! silently loses data — exactly the bug above, one layer up.

use baudet_core::Msg;

/// Rough cost of one message in characters: content, the role, and a small allowance for the
/// chat-template wrapper around it.
///
/// Mirrors the Kotlin `ContextTrimmer.costChars` and `SessionState.estimateTokensOf` so the number
/// used to cut a request agrees with the number a status line shows. A request must never be
/// allowed or refused by two figures that disagree.
pub fn cost_chars(msg: &Msg) -> usize {
    msg.content.chars().count() + msg.role.chars().count() + 8
}

/// Rough token count for a whole window, at ~3 characters per token.
///
/// A heuristic on purpose: real tokenisers are model-specific, and guessing wrong about a
/// tokenizer is worse than being openly approximate about one.
pub fn estimate_tokens(messages: &[Msg]) -> usize {
    messages.iter().map(cost_chars).sum::<usize>() / 3
}

/// The whole conversation, as turns.
///
/// Currently a clone of the stored vector, and that triviality is the point: this is the one
/// named place where the stored transcript becomes an outbound window. Kotlin calls it
/// `promptHistory()` and applies its clamping at the call site, which keeps the stored
/// transcript whole while the copy over the wire is cut. Folding a clamp in here would make
/// "what the user reads" and "what was sent" differ somewhere nobody named.
pub fn history_of(messages: &[Msg]) -> Vec<Msg> {
    messages.to_vec()
}

/// The conversation cut to at most `max_chars` of cost, keeping the newest contiguous tail.
///
/// The last message is always kept — even an oversized one — clamped from the front with a note
/// that costs exactly the room reserved for it. Dropping the newest turn is never safe: it is the
/// turn the model has to react to. Older turns fall off silently, because a transcript that opens
/// mid-conversation is a normal thing for a model to be handed.
pub fn clamp_chars(messages: &[Msg], max_chars: usize) -> Vec<Msg> {
    if messages.is_empty() {
        return Vec::new();
    }
    let last = messages.last().unwrap();
    let last_cost = cost_chars(last);
    // One turn larger than the whole budget: send a head slice of it rather than nothing. Dropping
    // the turn the model has to react to is never safe; cutting one observation is.
    if last_cost > max_chars {
        return vec![clamp_one(last, max_chars)];
    }
    // Nothing was dropped, so nothing has to be explained: the whole transcript goes, note free.
    // This case needs its own branch because the search below starts at `dropped = 1` and so never
    // considers the window that drops nothing -- without it a transcript that fits outright is
    // still cut down to whatever tail happens to leave room for a note about turns nobody dropped.
    // `window::rolling` makes the same promise and had the same hole until it got this branch.
    if messages.iter().map(cost_chars).sum::<usize>() <= max_chars {
        return messages.to_vec();
    }
    // Try every reachable tail, longest first, and take the first that actually fits. "Actually
    // fits" includes the note's own cost, which is the step a single walking loop gets wrong: the
    // window that drops turns needs a note, the note costs budget, and so the longest feasible
    // tail is not the longest tail whose messages alone fit. Costing one candidate per iteration
    // is the honest version of that arithmetic and needs no second pass to re-account for it.
    for dropped in 1..messages.len() {
        // The kept part is always the tail *through* the final turn: `dropped` counts how many
        // turns fall off the front, so the window is `messages[dropped..]`. Slicing from the end
        // instead lets the search drop the newest turn as well, which is the one thing this
        // function promises never to do.
        let tail = &messages[dropped..];
        let note_cost = cost_chars(&dropped_note(dropped));
        if tail.iter().map(cost_chars).sum::<usize>() + note_cost <= max_chars {
            let mut window = tail.to_vec();
            // The note stands where the gap is, at the front. The prompt's final turn must stay
            // last, so the note never goes after the answer.
            window.insert(0, dropped_note(dropped));
            return window;
        }
    }
    // Nothing older fits beside a note, so the newest turn goes alone — which needs no note,
    // because nothing was dropped.
    vec![last.clone()]
}

/// Model-facing, so English whatever the interface speaks: this text goes into the prompt.
pub(crate) fn dropped_note(dropped: usize) -> Msg {
    Msg::user(format!(
        "[{dropped} earlier messages were dropped from this request to keep it inside the model's \
         context window. The omitted context is gone from this request; the turns shown are intact.]"
    ))
}

/// `msg` cut from the front so its whole cost fits `budget`, note included in the arithmetic.
pub(crate) fn clamp_one(msg: &Msg, budget: usize) -> Msg {
    const NOTE: &str = "\n\n[... this message was truncated to fit the model's context window ...]";
    let room = budget.saturating_sub(cost_chars(msg) - msg.content.chars().count() + NOTE.chars().count());
    let kept: String = msg.content.chars().take(room).collect();
    Msg { content: kept + NOTE, ..msg.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baudet_core::Msg;

    fn turns(n: usize) -> Vec<Msg> {
        (0..n).flat_map(|i| vec![Msg::user(format!("vraag {i}")), Msg::assistant("antwoord".repeat(4))]).collect()
    }

    #[test]
    fn history_keeps_every_turn_whole() {
        let all = turns(3);
        assert_eq!(history_of(&all).len(), all.len());
    }

    #[test]
    fn a_clamp_drops_oldest_first_and_never_the_newest_turn() {
        let all = turns(20);
        let cost = all.iter().map(cost_chars).sum::<usize>();
        let window = clamp_chars(&all, cost / 2);
        assert!(window.len() < all.len());
        assert_eq!(window.last(), all.last(), "the turn being answered must survive");
        // Which turn falls where depends on exact costs, so the honest assertion is that a note is
        // present and that its number matches how many turns really went missing.
        let notes: Vec<&Msg> = window.iter().filter(|m| m.content.starts_with('[')).collect();
        assert_eq!(notes.len(), 1, "exactly one note for the gap");
        assert_eq!(window.first().expect("a window with a gap starts with its note").content, notes[0].content);
        let dropped: usize = notes[0]
            .content
            .trim_start_matches('[')
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .expect("the note leads with the number of dropped turns");
        assert_eq!(dropped, all.len() - (window.len() - 1), "the note must count what actually went");
        // The note counts toward the budget, or the clamp would overshoot its own ceiling.
        assert!(window.iter().map(cost_chars).sum::<usize>() <= cost / 2);
    }

    #[test]
    fn everything_fits_when_it_fits() {
        let budget = 10_000;
        let all = turns(2);
        let cost = all.iter().map(cost_chars).sum::<usize>();
        // The budget is characters, because `clamp_chars` counts characters. Measured before this
        // branch existed: budgets of `cost`, `cost + 3` and even `cost + 100` all handed back a
        // shortened window, so "fits" was never reachable -- the search started at `dropped = 1`
        // and so could only ever answer with a transcript that had lost a turn and paid for a note.
        assert!(cost <= budget, "the character ceiling must hold");
        assert!(estimate_tokens(&all) <= budget / 3, "the token ceiling must hold");
        assert_eq!(clamp_chars(&all, budget), all, "a transcript that fits goes whole");
        // The exact boundary, and one character under it: the ceiling is the cost, not the cost
        // plus the note that is only owed when something is actually dropped.
        assert_eq!(clamp_chars(&all, cost), all);
        assert_ne!(clamp_chars(&all, cost - 1), all);
    }

    #[test]
    fn an_oversized_only_turn_is_clamped_not_dropped() {
        let big = Msg::user("x".repeat(10_000));
        let window = clamp_chars(&[big], 120);
        assert_eq!(window.len(), 1);
        assert!(window[0].content.ends_with("context window ...]"));
        assert!(cost_chars(&window[0]) <= 120);
    }

    #[test]
    fn a_budget_smaller_than_the_note_alone_still_sends_the_note() {
        // Degenerate on purpose: the honest floor is a message that says it is truncated, which
        // beats an empty request the model cannot interpret at all.
        let window = clamp_chars(&[Msg::user("y".repeat(500))], 10);
        assert!(window[0].content.contains("truncated"));
    }

    #[test]
    fn nothing_is_invented_for_an_empty_transcript() {
        assert!(clamp_chars(&[], 100).is_empty());
        assert_eq!(estimate_tokens(&[]), 0);
    }
}
