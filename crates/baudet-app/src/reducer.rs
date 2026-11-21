//! Fold a stream of facts into rows.
//!
//! Pure on purpose: no backend, no clock, no widget. Given the same events, the same rows come
//! out, which is what lets a UI be verified against a script instead of against a GPU.

use baudet_core::{Item, ItemStatus, ThreadEvent};
use std::collections::HashMap;

/// The rows of one conversation, in the order they appeared, plus the live index by id.
#[derive(Debug, Default)]
pub struct Rows {
    order: Vec<String>,
    items: HashMap<String, Item>,
}

impl Rows {
    pub fn new() -> Rows {
        Rows::default()
    }

    /// Rows in arrival order, oldest first.
    pub fn ordered(&self) -> Vec<&Item> {
        self.order.iter().filter_map(|id| self.items.get(id)).collect()
    }

    pub fn get(&self, item_id: &str) -> Option<&Item> {
        self.items.get(item_id)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn open(&mut self, item: Item) {
        let id = item.item_id().to_string();
        if !self.items.contains_key(&id) {
            self.order.push(id.clone());
        }
        self.items.insert(id, item);
    }

    fn text_of(&self, item_id: &str) -> String {
        self.items
            .get(item_id)
            .and_then(|item| match item {
                Item::AgentMessage(i) => Some(i.text.clone()),
                Item::Reasoning(i) => Some(i.text.clone()),
                Item::UserMessage(i) => Some(i.text.clone()),
                Item::CommandExecution(i) => Some(i.output.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }
}

/// Apply one event. Returns `false` when the event named an item that was never opened, which a
/// caller may want to count: a delta for an unknown row means the stream was replayed from the
/// middle, and silently dropping it is how a transcript loses an answer without saying so.
pub fn reduce(rows: &mut Rows, event: &ThreadEvent) -> bool {
    match event {
        ThreadEvent::ItemStarted { item, .. } => {
            rows.open(item.clone());
            true
        }
        ThreadEvent::ItemCompleted { item, .. } => {
            // The completed item carries the final state, so a renderer never keeps its own copy
            // of the truth -- but a delta that arrived after an earlier snapshot must not be lost,
            // so an empty text on the close keeps what was streamed instead of erasing it.
            let id = item.item_id().to_string();
            let streamed = rows.text_of(&id);
            if streamed_text(item).is_empty() && !streamed.is_empty() {
                let mut kept = item.clone();
                kept.push_str(&streamed);
                rows.open(kept);
                return true;
            }
            rows.open(item.clone());
            true
        }
        ThreadEvent::ItemDelta { item_id, delta, .. } => match rows.items.get_mut(item_id) {
            Some(item) => {
                item.push_str(delta);
                true
            }
            None => false,
        },
        // Not row-bearing, but a turn's terminal failure deserves a row of its own so the reader
        // sees it where it happened rather than only in a status line.
        ThreadEvent::TurnCompleted { turn_id, incomplete_reason, .. } => {
            if let Some(reason) = incomplete_reason {
                rows.open(Item::Error(ErrorRow::build(turn_id, reason)));
            }
            true
        }
        _ => true,
    }
}

pub(crate) fn streamed_text(item: &Item) -> String {
    match item {
        Item::AgentMessage(i) => i.text.clone(),
        Item::Reasoning(i) => i.text.clone(),
        Item::UserMessage(i) => i.text.clone(),
        Item::CommandExecution(i) => i.output.clone(),
        _ => String::new(),
    }
}

/// Builds the error row a truncated turn leaves behind. Spelled out so the `interrupted`
/// distinction stays visible in one place.
struct ErrorRow;

impl ErrorRow {
    fn build(turn_id: &str, reason: &str) -> baudet_core::ErrorItem {
        baudet_core::ErrorItem {
            item_id: format!("{turn_id}/incomplete"),
            message: reason.to_string(),
            status: ItemStatus::Failed,
        }
    }
}
