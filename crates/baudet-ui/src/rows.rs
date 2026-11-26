
use baudet_core::{Item, ItemKind, ItemStatus};

pub struct Row {
    pub item_id: String,
    pub message_timestamp: Option<i64>,
    pub kind: ItemKind,
    pub status: ItemStatus,
    pub body: String,
    pub subject: Option<String>,
    pub collapsed: bool,
    pub thumbnail: Option<String>,
    pub image: Option<(eframe::egui::Id, baudet_core::ImageItem)>,
}

impl Row {
    pub fn from(item: &Item) -> Row {
        let item_id = item.item_id().to_owned();
        let status = item.status();
        let kind = item.kind();
        let message_timestamp = item.message_timestamp();
        match item {
            Item::UserMessage(i) => Row { item_id, message_timestamp, kind, status, body: i.text.clone(), subject: None, collapsed: false, thumbnail: i.thumbnail.clone(), image: None },
            Item::AgentMessage(i) => Row { item_id, message_timestamp, kind, status, body: i.text.clone(), subject: None, collapsed: false, thumbnail: None, image: None },
            Item::Reasoning(i) => Row { item_id, message_timestamp, kind, status, body: i.text.clone(), subject: None, collapsed: true, thumbnail: None, image: None },
            Item::CommandExecution(i) => {
                Row { item_id, message_timestamp, kind, status, body: i.output.clone(), subject: Some(i.command.clone()), collapsed: true, thumbnail: None, image: None }
            }
            Item::FileChange(i) => Row {
                item_id,
                message_timestamp,
                kind,
                status,
                body: i.diff.clone().unwrap_or_default(),
                subject: Some(format!("{}  +{} -{}", i.path, i.additions, i.deletions)),
                collapsed: true,
                thumbnail: None,
                image: None,
            },
            Item::ToolCall(i) => Row {
                item_id,
                message_timestamp,
                kind,
                status,
                body: i.output.clone(),
                subject: Some(format!("{} {}", i.tool, i.detail)),
                collapsed: true,
                thumbnail: None,
                image: None,
            },
            Item::Image(i) => Row {
                item_id,
                message_timestamp,
                kind,
                status,
                body: String::new(),
                subject: Some(i.tool.clone()),
                collapsed: false,
                thumbnail: None,
                // Item IDs are local to a conversation. Include image content in the texture key.
                image: Some((eframe::egui::Id::new((&i.item_id, &i.mime_type, &i.base64)), i.clone())),
            },
            Item::Plan(i) => Row {
                item_id,
                message_timestamp,
                kind,
                status,
                body: i
                    .entries
                    .iter()
                    .map(|entry| format!("{} {}", marker(entry.status), entry.content))
                    .collect::<Vec<_>>()
                    .join("\n"),
                subject: None,
                collapsed: false,
                thumbnail: None,
                image: None,
            },
            Item::Error(i) => Row { item_id, message_timestamp, kind, status, body: i.message.clone(), subject: None, collapsed: false, thumbnail: None, image: None },
        }
    }

    pub fn status_word(&self) -> &'static str {
        match self.status {
            ItemStatus::InProgress => "working",
            ItemStatus::Completed => "done",
            ItemStatus::Failed => "failed",
            ItemStatus::Interrupted => "stopped",
        }
    }

    pub fn in_progress(&self) -> bool {
        self.status == ItemStatus::InProgress
    }
}

fn marker(status: baudet_core::PlanEntryStatus) -> &'static str {
    use baudet_core::PlanEntryStatus as S;
    match status {
        S::Pending => "[ ]",
        S::InProgress => "[>]",
        S::Completed => "[x]",
    }
}
