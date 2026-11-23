
use baudet_core::{Item, ItemKind, ItemStatus};

pub struct Row {
    pub kind: ItemKind,
    pub status: ItemStatus,
    pub body: String,
    pub subject: Option<String>,
    pub collapsed: bool,
    pub image: Option<(eframe::egui::Id, baudet_core::ImageItem)>,
}

impl Row {
    pub fn from(item: &Item) -> Row {
        let status = item.status();
        let kind = item.kind();
        match item {
            Item::UserMessage(i) => Row { kind, status, body: i.text.clone(), subject: None, collapsed: false, image: None },
            Item::AgentMessage(i) => Row { kind, status, body: i.text.clone(), subject: None, collapsed: false, image: None },
            Item::Reasoning(i) => Row { kind, status, body: i.text.clone(), subject: None, collapsed: true, image: None },
            Item::CommandExecution(i) => {
                Row { kind, status, body: i.output.clone(), subject: Some(i.command.clone()), collapsed: true, image: None }
            }
            Item::FileChange(i) => Row {
                kind,
                status,
                body: i.diff.clone().unwrap_or_default(),
                subject: Some(format!("{}  +{} -{}", i.path, i.additions, i.deletions)),
                collapsed: true,
                image: None,
            },
            Item::ToolCall(i) => Row {
                kind,
                status,
                body: i.output.clone(),
                subject: Some(format!("{} {}", i.tool, i.detail)),
                collapsed: true,
                image: None,
            },
            Item::Image(i) => Row {
                kind,
                status,
                body: String::new(),
                subject: Some(i.tool.clone()),
                collapsed: false,
                // Item IDs are local to a conversation. Include image content in the texture key.
                image: Some((eframe::egui::Id::new((&i.item_id, &i.mime_type, &i.base64)), i.clone())),
            },
            Item::Plan(i) => Row {
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
                image: None,
            },
            Item::Error(i) => Row { kind, status, body: i.message.clone(), subject: None, collapsed: false, image: None },
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
