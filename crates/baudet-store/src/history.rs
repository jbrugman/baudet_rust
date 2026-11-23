
use baudet_core::Msg;

pub fn cost_chars(msg: &Msg) -> usize {
    msg.content.chars().count() + msg.role.chars().count() + PER_TURN_OVERHEAD
}

pub const PER_TURN_OVERHEAD: usize = 8;

pub fn estimate_tokens(messages: &[Msg]) -> usize {
    messages.iter().map(cost_chars).sum::<usize>() / 3
}

pub fn history_of(messages: &[Msg]) -> Vec<Msg> {
    messages.to_vec()
}

pub fn clamp_chars(messages: &[Msg], max_chars: usize) -> Vec<Msg> {
    if messages.is_empty() {
        return Vec::new();
    }
    let last = messages.last().unwrap();
    let last_cost = cost_chars(last);
    if last_cost > max_chars {
        return vec![clamp_one(last, max_chars)];
    }
    if messages.iter().map(cost_chars).sum::<usize>() <= max_chars {
        return messages.to_vec();
    }
    for dropped in 1..messages.len() {
        let tail = &messages[dropped..];
        let note_cost = cost_chars(&dropped_note(dropped));
        if tail.iter().map(cost_chars).sum::<usize>() + note_cost <= max_chars {
            let mut window = tail.to_vec();
            window.insert(0, dropped_note(dropped));
            return window;
        }
    }
    vec![last.clone()]
}

pub(crate) fn dropped_note(dropped: usize) -> Msg {
    Msg::user(format!(
        "[{dropped} earlier messages were dropped from this request to keep it inside the model's \
         context window. The omitted context is gone from this request; the turns shown are intact.]"
    ))
}

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
        assert!(window.iter().map(cost_chars).sum::<usize>() <= cost / 2);
    }

    #[test]
    fn everything_fits_when_it_fits() {
        let budget = 10_000;
        let all = turns(2);
        let cost = all.iter().map(cost_chars).sum::<usize>();
        assert!(cost <= budget, "the character ceiling must hold");
        assert!(estimate_tokens(&all) <= budget / 3, "the token ceiling must hold");
        assert_eq!(clamp_chars(&all, budget), all, "a transcript that fits goes whole");
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
        let window = clamp_chars(&[Msg::user("y".repeat(500))], 10);
        assert!(window[0].content.contains("truncated"));
    }

    #[test]
    fn nothing_is_invented_for_an_empty_transcript() {
        assert!(clamp_chars(&[], 100).is_empty());
        assert_eq!(estimate_tokens(&[]), 0);
    }
}
