//! One conversation, as the app holds it between the wire and the store.
//!
//! This is `session/SessionState.kt`, and it is the reason the sidebar can show more than one
//! chat. Before this module the app had a single transcript in `baudet_app::AppState`, which is
//! fine until a second conversation exists and the first one silently becomes the second one's
//! history.
//!
//! Four ideas carry the design, and none of them is mine:
//!
//!  * **the model's vocabulary and the interface's rows are separate lists.** `messages` is what
//!    goes over the wire; `items` is what was shown. Compaction trims the first and must never
//!    touch the second, because the trail is an audit of what the person actually saw.
//!  * **the store is the source of truth and grows by append.** This view mirrors it and carries
//!    the boundary (`persisted_messages`, `persisted_items_tail_id`) so persisting writes only
//!    the newer tail. A view that never held the whole conversation must not be able to rewrite
//!    the half it did not see.
//!  * **paging is a cursor over sequence numbers, not offsets into a list.** `oldest_loaded_seq`
//!    says whether older rows exist that this view has not read; prepending such a page does not
//!    disturb the write boundary, which is what makes scrolling and persisting safe together.
//!  * **there is one token estimate in this app.** [`estimate_tokens_of`] is the figure the status
//!    line shows, the figure the pre-send guard compares against and the figure the tool loop's
//!    ceiling trips on. Two estimates would disagree, and then a request gets stopped by a number
//!    the person was never shown.

use baudet_core::{Item, Msg};

/// Re-exported rather than redefined: the whole point of this module's estimate is that it is
/// the same number `baudet_store::history` and the tool loop already agree on.
pub use crate::history::PER_TURN_OVERHEAD;

/// One conversation: its turns, its rendered rows, and where the two end relative to the store.
#[derive(Debug, Clone)]
pub struct Session {
    pub session_id: String,
    pub model: String,
    /// The wire to the server-side session. `None` means this app sends the history itself.
    pub previous_response_id: Option<String>,
    /// The model's vocabulary: what a request is built from.
    pub messages: Vec<Msg>,
    /// The rich transcript rows shown for this conversation, kept so a reload rebuilds the cards
    /// instead of flattening them to prose.
    pub items: Vec<Item>,
    /// The store sequence of the oldest row in [`Session::items`], or `None` when `items` already
    /// reaches the start. Non-null means older rows exist that this view has not read.
    pub oldest_loaded_seq: Option<u64>,
    /// How many of this conversation's turns are already rows in the store. Set from a full read
    /// on open, and from a rewrite only when compaction renumbers the head.
    pub persisted_messages: usize,
    /// The `item_id` of the newest row already stored, or `None` when none is.
    pub persisted_items_tail_id: Option<String>,
    /// The compaction digest; replaces the older turns in the prompt.
    pub summary: Option<String>,
    /// The project this conversation belongs to; `None` is the Unassigned bucket.
    pub project_id: Option<String>,
}

impl Session {
    /// An empty conversation on a model, with nothing in it and nothing in the store.
    pub fn new(session_id: impl Into<String>, model: impl Into<String>) -> Session {
        Session {
            session_id: session_id.into(),
            model: model.into(),
            previous_response_id: None,
            messages: Vec::new(),
            items: Vec::new(),
            oldest_loaded_seq: None,
            persisted_messages: 0,
            persisted_items_tail_id: None,
            summary: None,
            project_id: None,
        }
    }

    /// Whether the store still holds rows older than [`Session::items`].
    pub fn has_more_older(&self) -> bool {
        self.oldest_loaded_seq.unwrap_or(0) > 0
    }

    /// The kept turns for client-side replay.
    ///
    /// The summary is deliberately **not** folded in here as a system row: the request builder and
    /// the tool transcript both drop system rows, so folding it in would silently lose it. It rides
    /// in the system instructions instead, which every path already sends.
    pub fn prompt_history(&self) -> Vec<Msg> {
        self.messages.clone()
    }

    /// The conversation as readable text, the way the origineler prints it for debugging.
    pub fn transcript(&self) -> String {
        let mut out = String::new();
        for msg in &self.messages {
            out.push_str(&msg.role);
            out.push_str(": ");
            out.push_str(&msg.content);
            out.push_str("\n\n");
        }
        out
    }

    /// Rough heuristic (~3 chars/token) for this conversation, summary included. No tokenizer:
    /// those are model-specific.
    pub fn estimate_tokens(&self) -> u64 {
        Session::estimate_tokens_of(&self.messages, self.summary.as_ref().map(|s| s.len()).unwrap_or(0))
    }

    /// The same rough estimate for any message list, so the three callers above cannot disagree.
    ///
    /// `summary_chars` counts the digest because it *is* sent, even though it is not one of the
    /// turns; leaving it out would under-report the request that carries it.
    pub fn estimate_tokens_of(messages: &[Msg], summary_chars: usize) -> u64 {
        let mut chars = summary_chars;
        for msg in messages {
            chars += msg.content.chars().count() + msg.role.chars().count() + PER_TURN_OVERHEAD;
        }
        (chars / 3) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baudet_core::{AgentMessageItem, ItemStatus, UserMessageItem};

    fn item(id: &str, text: &str) -> Item {
        Item::AgentMessage(AgentMessageItem {
            item_id: id.into(),
            text: text.into(),
            status: ItemStatus::Completed,
        })
    }

    #[test]
    fn a_new_session_is_empty_in_every_list_and_unattached() {
        let session = Session::new("s-x", "gemma");
        assert!(session.messages.is_empty() && session.items.is_empty());
        assert_eq!(session.previous_response_id, None, "without a wire we send the history");
        assert_eq!(session.project_id, None, "no project means Unassigned, not a guess");
        assert!(!session.has_more_older(), "nothing is loaded, so nothing older can be missing");
        assert_eq!(session.persisted_messages, 0);
        assert_eq!(session.estimate_tokens(), 0);
    }

    #[test]
    fn the_estimate_is_the_one_figure_the_whole_app_shares() {
        // Eerlijk gezegd: mijn eerste versie van deze test beweerde 5, 13 en 44 en was fout — ik
        // had de som van de beurten door elkaar gehaald. De ware reeks is 5, 12, dan 47 voor een
        // observatie van 93 tekens. Wat hier telt, is niet de handrekening maar dat het één reeks
        // is: baudet-tool::tool_loop rekent zijn plafond op exact deze functie, dus een verandering
        // hier verandert de lus mee, en de plafontoets daar drukt dat af.
        let messages = [Msg::user("vraag")];
        assert_eq!(Session::estimate_tokens_of(&messages, 0), 5);
        let longer = [Msg::user("vraag"), Msg::assistant("werk")];
        assert_eq!(Session::estimate_tokens_of(&longer, 0), 12);
        let longest =
            [Msg::user("vraag"), Msg::assistant("werk"), Msg::new("user", "o".repeat(93))];
        assert_eq!(Session::estimate_tokens_of(&longest, 0), 47);
    }

    #[test]
    fn a_summary_costs_tokens_even_though_it_is_not_a_turn() {
        let messages = [Msg::user("vraag")];
        // 5 from the turn, plus 3000/3 for a digest of 3 000 characters.
        assert_eq!(Session::estimate_tokens_of(&messages, 3_000), 5 + 1_000);
        let mut session = Session::new("s-x", "gemma");
        session.messages.push(Msg::user("vraag"));
        assert_eq!(session.estimate_tokens(), 5, "zonder samenvatting telt die niet mee");
        session.summary = Some("x".repeat(3_000));
        assert_eq!(session.estimate_tokens(), 1_005, "met samenvatting telt hij wél mee");
    }

    #[test]
    fn the_estimate_counts_characters_not_bytes() {
        // De origineler meet `String.length`. Bytes tellen — `Msg.content.len()` — blaast elk
        // accentwoord met een factor drie op en stopt een verzoek dat de persoon krijgt
        // voorgesteld als passend. Gemeten: 300 accenttekens zijn 600 bytes, en het verschil is
        // hier precies de bewering.
        let accents = [Msg::user("é".repeat(300))];
        assert_eq!(accents[0].content.len(), 600, "bytes zijn niet wat deze schatting telt");
        assert_eq!(Session::estimate_tokens_of(&accents, 0), (300 + 4 + 8) / 3);
    }

    #[test]
    fn paging_is_a_cursor_and_zero_is_not_a_cursor() {
        let mut session = Session::new("s-x", "gemma");
        // Reaching the start of the conversation is spelled None, not 0: a 0 would read as
        // "older rows exist below sequence 0", which is nothing and everything at once.
        session.oldest_loaded_seq = None;
        assert!(!session.has_more_older());
        session.oldest_loaded_seq = Some(7);
        assert!(session.has_more_older());
        session.oldest_loaded_seq = Some(0);
        assert!(!session.has_more_older(), "sequence 0 is the start, so there is nothing older");
    }

    #[test]
    fn prompt_history_is_a_copy_and_leaves_the_summary_out_of_the_wire() {
        let mut session = Session::new("s-x", "gemma");
        session.messages.push(Msg::user("vraag"));
        session.summary = Some("het gesprek tot nu toe".into());
        let history = session.prompt_history();
        assert_eq!(history.len(), 1, "de samenvatting is geen turn en mag hier niet opduiken");
        // "role: content" followed by a blank line, per turn, exactly as `transcript()` builds it.
        assert_eq!(session.transcript(), "user: vraag\n\n");
        assert_eq!(session.messages.len(), 1, "de oproeper mag de bron niet kunnen beschadigen");
    }

    #[test]
    fn the_write_boundary_is_a_count_and_an_id_not_a_position_in_items() {
        // The item trail is append-only and can grow at either end: a page of older rows is
        // prepended while a turn appends. The boundary is therefore the newest stored id, searched
        // from the back, so prepending leaves it on the genuine tail. That lookup lives here as the
        // one place allowed to answer "which rows are new".
        let mut session = Session::new("s-x", "gemma");
        session.items.push(item("i-1", "een"));
        session.items.push(item("i-2", "twee"));
        session.persisted_items_tail_id = Some("i-2".into());
        // Oudere rijen ervoor gezet, alsof de persoon omhoog scrolde.
        session.items.insert(0, item("i-0", "nul"));
        let start = session
            .items
            .iter()
            .rposition(|row| Some(row.item_id()) == session.persisted_items_tail_id.as_deref());
        let tail = &session.items[start.unwrap() + 1..];
        assert_eq!(tail.len(), 0, "voorzetten is geen write, dus er gaat niets naar de store");
        session.items.push(item("i-3", "drie"));
        session.items.push(Item::UserMessage(UserMessageItem {
            item_id: "i-4".into(),
            text: "vier".into(),
            status: ItemStatus::Completed,
        }));
        let start = session
            .items
            .iter()
            .rposition(|row| Some(row.item_id()) == session.persisted_items_tail_id.as_deref())
            .unwrap();
        let tail: Vec<&str> = session.items[start + 1..].iter().map(Item::item_id).collect();
        assert_eq!(tail, ["i-3", "i-4"], "de staart is wat er na de marker kwam, in volgorde");
    }
}
