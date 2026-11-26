
use baudet_core::{Item, Msg};

pub use crate::history::PER_TURN_OVERHEAD;

#[derive(Debug, Clone)]
pub struct Session {
    pub session_id: String,
    pub model: String,
    pub previous_response_id: Option<String>,
    pub messages: Vec<Msg>,
    pub items: Vec<Item>,
    pub oldest_loaded_seq: Option<u64>,
    pub persisted_messages: usize,
    pub persisted_items_tail_id: Option<String>,
    pub summary: Option<String>,
    pub project_id: Option<String>,
}

impl Session {
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

    pub fn has_more_older(&self) -> bool {
        self.oldest_loaded_seq.unwrap_or(0) > 0
    }

    pub fn prompt_history(&self) -> Vec<Msg> {
        self.messages.clone()
    }

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

    pub fn estimate_tokens(&self) -> u64 {
        Session::estimate_tokens_of(&self.messages, self.summary.as_ref().map(|s| s.len()).unwrap_or(0))
    }

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
            created_at_ms: None,
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
        assert_eq!(Session::estimate_tokens_of(&messages, 3_000), 5 + 1_000);
        let mut session = Session::new("s-x", "gemma");
        session.messages.push(Msg::user("vraag"));
        assert_eq!(session.estimate_tokens(), 5, "zonder samenvatting telt die niet mee");
        session.summary = Some("x".repeat(3_000));
        assert_eq!(session.estimate_tokens(), 1_005, "met samenvatting telt hij wél mee");
    }

    #[test]
    fn the_estimate_counts_characters_not_bytes() {
        let accents = [Msg::user("é".repeat(300))];
        assert_eq!(accents[0].content.len(), 600, "bytes zijn niet wat deze schatting telt");
        assert_eq!(Session::estimate_tokens_of(&accents, 0), (300 + 4 + 8) / 3);
    }

    #[test]
    fn paging_is_a_cursor_and_zero_is_not_a_cursor() {
        let mut session = Session::new("s-x", "gemma");
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
        assert_eq!(session.transcript(), "user: vraag\n\n");
        assert_eq!(session.messages.len(), 1, "de oproeper mag de bron niet kunnen beschadigen");
    }

    #[test]
    fn the_write_boundary_is_a_count_and_an_id_not_a_position_in_items() {
        let mut session = Session::new("s-x", "gemma");
        session.items.push(item("i-1", "een"));
        session.items.push(item("i-2", "twee"));
        session.persisted_items_tail_id = Some("i-2".into());
        session.items.insert(0, item("i-0", "nul"));
        let start = session
            .items
            .iter()
            .rposition(|row| Some(row.item_id()) == session.persisted_items_tail_id.as_deref());
        let tail = &session.items[start.unwrap() + 1..];
        assert_eq!(tail.len(), 0, "voorzetten is geen write, dus er gaat niets naar de store");
        session.items.push(item("i-3", "drie"));
        session.items.push(Item::UserMessage(UserMessageItem {
            created_at_ms: None,
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
