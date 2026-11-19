//! One transcript row as it should be painted: what it is, whether it is still working, and the
//! text a reader actually wants.
//!
//! Deliberately not a re-exposition of [`Item`]. The row keeps the three things a paint pass
//! cannot derive cheaply or honestly from the enum alone -- the status it must colour by, the
//! collapsible-versus-open decision, and which field holds the body -- and drops the rest. If a
//! field matters to the reader, it gets added here with a line saying so; until then the enum
//! stays the single place where the wire vocabulary lives.

use baudet_core::{Item, ItemKind, ItemStatus};

/// A row, reduced to what the transcript shows.
pub struct Row {
    pub kind: ItemKind,
    pub status: ItemStatus,
    /// The body: message text, command output, a diff, a tool's reply. Empty for rows that carry
    /// no prose of their own, which is what lets the paint pass skip the expander instead of
    /// painting an empty one.
    pub body: String,
    /// The one-line identity of the row: the command, the changed path, the tool name. `None` for
    /// prose rows, whose body *is* the heading.
    pub subject: Option<String>,
    /// Whether the body is worth an expander. Streaming prose and user messages read inline;
    /// output, diffs and reasoning are noise until asked for, which is how Baudet's JVM panel
    /// behaved and why a long turn stays readable while it runs.
    pub collapsed: bool,
}

impl Row {
    pub fn from(item: &Item) -> Row {
        let status = item.status();
        let kind = item.kind();
        match item {
            Item::UserMessage(i) => Row { kind, status, body: i.text.clone(), subject: None, collapsed: false },
            Item::AgentMessage(i) => Row { kind, status, body: i.text.clone(), subject: None, collapsed: false },
            // Thinking is worth reading occasionally and ruinous by default, so it is collapsed.
            Item::Reasoning(i) => Row { kind, status, body: i.text.clone(), subject: None, collapsed: true },
            Item::CommandExecution(i) => {
                Row { kind, status, body: i.output.clone(), subject: Some(i.command.clone()), collapsed: true }
            }
            Item::FileChange(i) => Row {
                kind,
                status,
                body: i.diff.clone().unwrap_or_default(),
                subject: Some(format!("{}  +{} -{}", i.path, i.additions, i.deletions)),
                collapsed: true,
            },
            Item::ToolCall(i) => Row {
                kind,
                status,
                body: i.output.clone(),
                subject: Some(format!("{} {}", i.tool, i.detail)),
                collapsed: true,
            },
            Item::Image(i) => Row {
                kind,
                status,
                body: String::new(),
                subject: Some(format!("{} ({})", i.tool, i.mime_type)),
                collapsed: false,
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
            },
            // An error is never collapsed: it is the one row that must interrupt reading.
            Item::Error(i) => Row { kind, status, body: i.message.clone(), subject: None, collapsed: false },
        }
    }

    /// The status spelled out. Colour plus this word, never colour alone.
    pub fn status_word(&self) -> &'static str {
        match self.status {
            ItemStatus::InProgress => "working",
            ItemStatus::Completed => "done",
            ItemStatus::Failed => "failed",
            ItemStatus::Interrupted => "stopped",
        }
    }

    /// Whether the row is mid-flight, which is what a spinner and the Stop button key on.
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
