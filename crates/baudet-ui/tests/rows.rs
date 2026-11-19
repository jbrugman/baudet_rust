//! What a row looks like to a reader, protected without a window.
//!
//! The paint pass itself is egui's job; the decisions worth testing are the ones this crate owns:
//! which body a row shows, whether it is worth an expander, and that a status never reads as
//! colour alone. If one of these drifts, the transcript lies to the reader rather than crashing.

use baudet_core::{Item, ItemKind, ItemStatus, PlanEntry, PlanEntryStatus, PlanItem};
use baudet_ui::Row;

fn plan(status: ItemStatus) -> Item {
    Item::Plan(PlanItem {
        item_id: "tu_1/plan".into(),
        entries: vec![
            PlanEntry::new("kern", PlanEntryStatus::Completed),
            PlanEntry::new("gui", PlanEntryStatus::InProgress),
            PlanEntry::new("transport", PlanEntryStatus::Pending),
        ],
        status,
    })
}

#[test]
fn a_plan_reads_as_a_checklist_in_order() {
    let row = Row::from(&plan(ItemStatus::InProgress));
    assert_eq!(row.kind, ItemKind::Plan);
    // Markers, not colours: a plan's progress has to survive a grey screenshot.
    assert_eq!(row.body, "[x] kern\n[>] gui\n[ ] transport");
    // Reading a plan inline is the point of a plan, so it never hides behind a disclosure.
    assert!(!row.collapsed);
    assert!(row.in_progress());
}

#[test]
fn prose_reads_inline_and_output_hides() {
    let answer = Item::AgentMessage(baudet_core::AgentMessageItem {
        item_id: "tu_1/msg".into(),
        text: "two facts, one table".into(),
        status: ItemStatus::Completed,
    });
    let row = Row::from(&answer);
    assert_eq!(row.body, "two facts, one table");
    assert!(!row.collapsed, "the answer is what the reader came for");
    assert!(row.subject.is_none(), "prose has no separate heading");

    let command = Item::CommandExecution(baudet_core::CommandExecutionItem {
        item_id: "tu_1/cmd".into(),
        command: "cargo test".into(),
        cwd: None,
        timeout_seconds: None,
        status: ItemStatus::Completed,
        exit_code: Some(0),
        timed_out: false,
        duration_ms: 12,
        output: "38 passed".into(),
        approval: None,
    });
    let row = Row::from(&command);
    // The command is the heading and its output the detail: the reverse would hide which command
    // ran behind however many lines it printed.
    assert_eq!(row.subject.as_deref(), Some("cargo test"));
    assert_eq!(row.body, "38 passed");
    assert!(row.collapsed);
}

#[test]
fn a_file_change_names_the_path_and_its_counts() {
    let item = Item::FileChange(baudet_core::FileChangeItem {
        item_id: "tu_1/diff".into(),
        path: "src/app.rs".into(),
        change: baudet_core::FileChangeKind::Update,
        status: ItemStatus::Completed,
        diff: Some("+1 -0".into()),
        additions: 4,
        deletions: 2,
    });
    let row = Row::from(&item);
    assert_eq!(row.subject.as_deref(), Some("src/app.rs  +4 -2"));
    assert_eq!(row.body, "+1 -0");
}

#[test]
fn every_status_has_its_own_word() {
    // Four states, four distinct words. Two sharing one would make `failed` and `interrupted`
    // indistinguishable, which is the distinction Baudet guards on the wire.
    let mut words = Vec::new();
    for status in [ItemStatus::InProgress, ItemStatus::Completed, ItemStatus::Failed, ItemStatus::Interrupted] {
        let item = Item::Error(baudet_core::ErrorItem { item_id: "x".into(), message: "m".into(), status });
        let row = Row::from(&item);
        words.push(row.status_word().to_string());
        assert_eq!(row.in_progress(), status == ItemStatus::InProgress);
    }
    words.sort();
    words.dedup();
    assert_eq!(words.len(), 4, "statuses must read as four different words: {words:?}");
}
