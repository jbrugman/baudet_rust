
use baudet_core::{ApprovalDecision, CommandExecutionItem, Item, ItemKind, ItemStatus, ThreadEvent};
use std::collections::{HashMap, hash_map::Entry};

#[derive(Debug, Default)]
pub struct Rows {
    order: Vec<String>,
    items: HashMap<String, Item>,
}

impl Rows {
    pub fn new() -> Rows {
        Rows::default()
    }

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

    fn open(&mut self, mut item: Item) {
        let id = item.item_id().to_string();
        if let Some(previous) = self.items.get(&id) {
            item.set_message_timestamp(previous.message_timestamp().or(item.message_timestamp()));
        } else {
            if matches!(item.kind(), ItemKind::UserMessage | ItemKind::AgentMessage)
                && item.message_timestamp().is_none()
            {
                let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                    .ok().and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok());
                item.set_message_timestamp(timestamp);
            }
            self.order.push(id.clone());
        }
        self.items.insert(id, item);
    }

    pub fn replace_for_screen(&mut self, mut item: Item) {
        let id = item.item_id().to_string();
        if let Some(previous) = self.items.get(&id) {
            item.set_message_timestamp(previous.message_timestamp().or(item.message_timestamp()));
        }
        self.items.insert(id, item);
    }

    pub fn clear(&mut self) {
        self.order.clear();
        self.items.clear();
    }

    pub fn prepend_for_screen(&mut self, item: Item) {
        let id = item.item_id().to_string();
        match self.items.entry(id) {
            Entry::Occupied(mut entry) => {
                entry.insert(item);
            }
            Entry::Vacant(entry) => {
                self.order.insert(0, entry.key().clone());
                entry.insert(item);
            }
        }
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

#[derive(Debug)]
pub struct Emitter<'a> {
    rows: &'a mut Rows,
    items: Option<&'a mut Vec<Item>>,
    messages: Vec<baudet_core::Msg>,
    seq: usize,
    thread_id: String,
    turn_id: String,
}

impl<'a> Emitter<'a> {
    pub fn display(rows: &'a mut Rows, thread_id: impl Into<String>, turn_id: impl Into<String>) -> Emitter<'a> {
        Emitter {
            rows,
            items: None,
            messages: Vec::new(),
            seq: 0,
            thread_id: thread_id.into(),
            turn_id: turn_id.into(),
        }
    }

    pub fn storing(
        rows: &'a mut Rows,
        items: &'a mut Vec<Item>,
        thread_id: impl Into<String>,
        turn_id: impl Into<String>,
    ) -> Emitter<'a> {
        Emitter {
            rows,
            items: Some(items),
            messages: Vec::new(),
            seq: 0,
            thread_id: thread_id.into(),
            turn_id: turn_id.into(),
        }
    }

    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }

    pub fn next_item_id(&mut self) -> String {
        self.seq += 1;
        format!("{}/{}/i{}", self.thread_id, self.turn_id, self.seq)
    }

    pub fn open(&mut self, item: Item) {
        self.started(item);
    }

    pub fn store_message(&mut self, msg: &baudet_core::Msg) {
        self.messages.push(msg.clone());
    }

    pub fn taken_messages(&mut self) -> Vec<baudet_core::Msg> {
        std::mem::take(&mut self.messages)
    }

    pub fn started(&mut self, item: Item) -> bool {
        reduce(
            self.rows,
            &ThreadEvent::ItemStarted {
                thread_id: self.thread_id.clone(),
                turn_id: self.turn_id.clone(),
                item,
            },
        )
    }

    pub fn delta(&mut self, item_id: &str, kind: ItemKind, delta: &str) -> bool {
        reduce(
            self.rows,
            &ThreadEvent::ItemDelta {
                thread_id: self.thread_id.clone(),
                turn_id: self.turn_id.clone(),
                item_id: item_id.to_string(),
                kind,
                delta: delta.to_string(),
            },
        )
    }

    pub fn completed(&mut self, item: Item) -> bool {
        let item_id = item.item_id().to_string();
        let store = item.kind() != ItemKind::Reasoning;
        let kept = reduce(
            self.rows,
            &ThreadEvent::ItemCompleted {
                thread_id: self.thread_id.clone(),
                turn_id: self.turn_id.clone(),
                item,
            },
        );
        if !store {
            return kept;
        }
        let Some(items) = self.items.as_deref_mut() else { return kept };
        let Some(closed) = self.rows.get(&item_id).cloned() else { return kept };
        debug_assert!(
            items.iter().all(|stored| stored.item_id() != item_id),
            "{item_id} state twee keer opgeslaan"
        );
        items.push(closed);
        kept
    }
}


pub fn reduce(rows: &mut Rows, event: &ThreadEvent) -> bool {
    match event {
        ThreadEvent::ItemStarted { item, .. } => {
            rows.open(item.clone());
            true
        }
        ThreadEvent::ItemCompleted { item, .. } => {
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
        ThreadEvent::TurnCompleted { turn_id, incomplete_reason, .. } => {
            if let Some(reason) = incomplete_reason.as_deref()
                .filter(|reason| *reason != baudet_tool::tool_loop::MISSING_COMPLETION)
            {
                rows.open(Item::Error(ErrorRow::build(turn_id, reason)));
            }
            true
        }
        ThreadEvent::ApprovalRequested { item_id, .. } => {
            if rows.get(item_id).is_some() {
                true
            } else {
                rows.open(pending_card(item_id));
                true
            }
        }
        ThreadEvent::ApprovalResolved { item_id, decision, .. } => {
            match rows.items.get_mut(item_id) {
                Some(Item::CommandExecution(card)) => {
                    card.approval = Some(*decision);
                    true
                }
                Some(Item::FileChange(change)) => {
                    change.status = match decision {
                        ApprovalDecision::Accept | ApprovalDecision::AcceptForSession => ItemStatus::Completed,
                        ApprovalDecision::Decline | ApprovalDecision::Cancel => ItemStatus::Failed,
                    };
                    true
                }
                Some(_) => true,
                None => false,
            }
        }
        _ => true,
    }
}

fn pending_card(item_id: &str) -> Item {
    Item::CommandExecution(CommandExecutionItem {
        item_id: item_id.to_string(),
        command: String::new(),
        cwd: None,
        timeout_seconds: None,
        status: ItemStatus::InProgress,
        exit_code: None,
        timed_out: false,
        duration_ms: 0,
        output: String::new(),
        approval: None,
    })
}

pub fn streamed_text(item: &Item) -> String {
    match item {
        Item::AgentMessage(i) => i.text.clone(),
        Item::Reasoning(i) => i.text.clone(),
        Item::UserMessage(i) => i.text.clone(),
        Item::CommandExecution(i) => i.output.clone(),
        _ => String::new(),
    }
}

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
