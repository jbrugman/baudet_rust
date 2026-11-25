
use baudet_core::Msg;
use baudet_store::window::{self, ContextStrategy};

pub fn scrub(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    let mut copied = 0;
    while index < bytes.len() {
        if bytes[index] == b'<' && text[index..].starts_with("<|") {
            let after_open = index + 2;
            let mut scan = after_open;
            while scan < bytes.len() && bytes[scan] != b'|' && bytes[scan] != b'>' {
                scan += 1;
            }
            if scan > after_open && text[scan..].starts_with("|>") {
                out.push_str(&text[copied..index]);
                index = scan + 2;
                copied = index;
                continue;
            }
        }
        index += 1;
    }
    out.push_str(&text[copied..]);
    out
}

pub fn fit(prior: &[Msg], max_chars: usize) -> Vec<Msg> {
    if max_chars == 0 {
        return prior.to_vec();
    }
    let mut used = 0usize;
    let mut first = prior.len();
    while first > 0 {
        let msg = &prior[first - 1];
        let cost = baudet_store::cost_chars(msg);
        if used + cost > max_chars {
            break;
        }
        used += cost;
        first -= 1;
    }
    let mut kept = prior[first..].to_vec();
    if first == 0 {
        return kept;
    }
    let note = format!(
        "[{first} earlier messages were dropped from this request to keep it inside the model's \
         context window. The oldest context is gone; the turns below are intact.]"
    );
    kept.insert(0, Msg::user(note));
    kept
}

pub fn current_input(history: &[Msg], under_server_session: bool) -> String {
    if !under_server_session {
        return history.last().map(|msg| scrub(&msg.content)).unwrap_or_default();
    }
    let last_assistant = history.iter().rposition(|msg| msg.role == "assistant");
    let tail: &[Msg] = match last_assistant {
        Some(index) => &history[index + 1..],
        None if !history.is_empty() => &history[history.len() - 1..],
        None => &[],
    };
    let joined = tail
        .iter()
        .map(|msg| scrub(&msg.content))
        .collect::<Vec<_>>()
        .join("\n\n");
    if joined.trim().is_empty() {
        return history.last().map(|msg| scrub(&msg.content)).unwrap_or_default();
    }
    joined
}

pub fn split_for_window(history: &[Msg]) -> (Vec<Msg>, String) {
    let user_text = history.last().map(|msg| scrub(&msg.content)).unwrap_or_default();
    let prior = history[..history.len().saturating_sub(1)]
        .iter()
        .map(scrubbed)
        .collect();
    (prior, user_text)
}

pub fn for_call(history: &[Msg]) -> (Vec<Msg>, String) {
    split_for_window(history)
}

pub fn for_loop(
    history: &[Msg],
    strategy: ContextStrategy,
    budget_tokens: usize,
    under_server_session: bool,
    keep_recent: usize,
) -> (Vec<Msg>, String) {
    if strategy == ContextStrategy::StopAtLimit {
        let (prior, _) = for_call(history);
        return (prior, current_input(history, under_server_session));
    }
    split_for_window(&window::bound(history, strategy, budget_tokens, keep_recent))
}

fn scrubbed(msg: &Msg) -> Msg {
    let mut copy = msg.clone();
    copy.content = scrub(&msg.content);
    copy
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(role: &str, content: &str) -> Msg {
        Msg::new(role, content)
    }

    fn token(name: &str) -> String {
        format!("<|{}|>", name)
    }

    #[test]
    fn an_im_start_never_survives_and_prose_is_untouched() {
        let leaked = format!("voor {} na", token("im_start"));
        assert_eq!(scrub(&leaked), "voor  na");
        assert_eq!(scrub(&token("endoftext")), "");
        assert_eq!(scrub(&format!("{}{}x", token("a"), token("b"))), "x");
        for prose in ["a < b and c > d", "x | y", "a <| b", "<a|b>", "price <|a|", "a|>b", "", "   "] {
            assert_eq!(scrub(prose), prose, "{prose:?} is geen token");
        }
    }

    #[test]
    fn the_sweeper_meets_the_reference_pattern_on_every_border_case() {
        let cases = [
            ("<|a|>", ""),
            ("<|a|>|>", "|>"),
            ("<|a|>>", ">"),
            ("<<|a|>", "<"),
            ("<|a|b|>", "<|a|b|>"),
            ("<||>", "<||>"),
            ("<|>", "<|>"),
            ("<|a|", "<|a|"),
            ("a|b|>", "a|b|>"),
            ("<|a\nb|>", ""),
            ("<|a|>x<|b|>", "x"),
            ("<|went|> bewaard, <|leeft|> mee", " bewaard,  mee"),
        ];
        for (input, expected) in cases {
            assert_eq!(scrub(input), expected, "scrub({input:?})");
        }
    }

    #[test]
    fn a_broken_token_stays_text_instead_of_getting_eaten() {
        assert_eq!(scrub("<|a|"), "<|a|");
        assert_eq!(scrub("<||>"), "<||>");
        assert_eq!(scrub("<|>"), "<|>");
        assert_eq!(scrub("<|a|>rest"), "rest");
        assert_eq!(scrub("<|a|>|>"), "|>");
    }

    #[test]
    fn fit_keeps_a_contiguous_tail_and_names_what_went() {
        let prior: Vec<Msg> = (0..10).map(|_| chat("user", &"y".repeat(100))).collect();
        let kept = fit(&prior, 350);

        let dropped = note_count(&kept).expect("er viel iets weg, dus er staat een noot");
        let sources = kept.len() - 1;
        assert_eq!(dropped + sources, 10, "niets verdwijnt dubbel, de staart is aaneengesloten");
        assert_eq!(sources, 3);
        assert_eq!(baudet_store::cost_chars(&kept[0]), 165);
        assert_eq!(sources * 112, 336);
        assert!(kept[1..].iter().all(|m| m.content == "y".repeat(100)));
        assert_eq!(kept[0].role, "user");
        assert!(kept[0].content.contains("The oldest context is gone; the turns below are intact."),
            "de noot is de tekst van `Transcript.kt:102-103`, niet een eigen verzonnen");

        let tight = fit(&prior, 112);
        assert_eq!(tight.len(), 2, "noot plus één bron");
        assert_eq!(note_count(&tight), Some(9));
        assert_eq!(baudet_store::cost_chars(&tight[0]), 165);

        let whole = fit(&prior, 10_000);
        assert_eq!(whole.len(), 10);
        assert!(note_count(&whole).is_none());
        assert_eq!(fit(&prior, 0).len(), 10);
        assert!(fit(&[], 50).is_empty());
    }

    fn note_count(msgs: &[Msg]) -> Option<usize> {
        let head = msgs.first()?;
        let rest = head.content.strip_prefix("[")?;
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok()
    }

    #[test]
    fn the_current_input_replays_client_side_and_joins_server_side() {
        let history = vec![chat("user", "vraag"), chat("assistant", "antwoord"), chat("tool", "obs")];
        assert_eq!(current_input(&history, false), "obs");
        let with_two = vec![
            chat("user", "vraag"),
            chat("assistant", "antwoord"),
            chat("tool", "obs1"),
            chat("tool", "obs2"),
        ];
        assert_eq!(current_input(&with_two, true), "obs1\n\nobs2");
        assert_eq!(current_input(&[chat("user", "alle")], true), "alle");
        let blank = vec![chat("user", "vraag"), chat("assistant", "antwoord"), chat("tool", "   ")];
        assert_eq!(current_input(&blank, true), "   ");
        assert_eq!(current_input(&[], true), "");
        assert_eq!(current_input(&[], false), "");
    }

    #[test]
    fn scrubbing_reaches_assistant_turns_too_and_the_last_message_stands_apart() {
        let history = vec![
            chat("assistant", &format!("a {} b", token("im_end"))),
            chat("user", &format!("verder {}", token("x"))),
        ];
        let (prior, input) = for_call(&history);
        assert_eq!(prior.len(), 1);
        assert_eq!(prior[0].content, "a  b", "assistent-tekst wordt gezuiverd, niet gespaard");
        assert_eq!(input, "verder ", "de huidige invoer is schoon en staat buiten de replay");
        assert_eq!(prior[0].role, "assistant", "het recht blijft staan, alleen de inhoud schuift");
        let (prior, input) = for_call(&[]);
        assert!(prior.is_empty());
        assert_eq!(input, "");
    }

    #[test]
    fn for_loop_under_stop_at_limit_keeps_all_history_even_above_the_old_replay_limit() {
        let big = "z".repeat(30_000);
        let history = vec![chat("user", &big), chat("assistant", &big), chat("user", "nu")];
        for budget in [0, 1_000_000] {
            let (prior, input) = for_loop(&history, ContextStrategy::StopAtLimit, budget, false, usize::MAX);
            assert_eq!(prior, history[..2]);
            assert_eq!(input, "nu");
        }
        let (prior, _) = for_loop(&history, ContextStrategy::RecentMessages, 0, false, usize::MAX);
        assert_eq!(prior.len(), 2, "geen budget betekent hier niet de vaste klem");
        let (prior, _) = for_loop(&history, ContextStrategy::RecentMessages, 1, false, usize::MAX);
        assert!(prior.len() <= 2);
    }

    #[test]
    fn for_loop_under_stop_at_limit_asks_the_server_for_the_input() {
        let history = vec![chat("user", "v"), chat("assistant", "a"), chat("tool", "obs1"), chat("tool", "obs2")];
        let (_, input) = for_loop(&history, ContextStrategy::StopAtLimit, 0, true, usize::MAX);
        assert_eq!(input, "obs1\n\nobs2");
        let (_, input) = for_loop(&history, ContextStrategy::RecentMessages, 0, true, usize::MAX);
        assert_eq!(input, "obs2", "client-naspelen kent de server niet");
    }
}
