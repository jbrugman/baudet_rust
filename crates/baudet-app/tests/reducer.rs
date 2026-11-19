//! Folding a stream into rows. Translated from the `TranscriptRowsSpec` and
//! `AppStateItemSpec` intents: a transcript is rebuilt from events, never from a cached string.

use baudet_app::{reduce, Rows};
use baudet_core::*;

fn msg(id: &str, text: &str, status: ItemStatus) -> Item {
    Item::AgentMessage(AgentMessageItem { item_id: id.into(), text: text.into(), status })
}

#[test]
fn rows_appear_in_arrival_order() {
    let mut rows = Rows::new();
    for (id, text) in [("a", "first"), ("b", "second")] {
        reduce(
            &mut rows,
            &ThreadEvent::ItemStarted {
                thread_id: "t".into(),
                turn_id: "u".into(),
                item: msg(id, text, ItemStatus::Completed),
            },
        );
    }
    let ids: Vec<&str> = rows.ordered().iter().map(|i| i.item_id()).collect();
    assert_eq!(ids, vec!["a", "b"]);
}

#[test]
fn deltas_accumulate_into_the_row_they_name() {
    let mut rows = Rows::new();
    reduce(
        &mut rows,
        &ThreadEvent::ItemStarted {
            thread_id: "t".into(),
            turn_id: "u".into(),
            item: msg("m", "", ItemStatus::InProgress),
        },
    );
    for part in ["Hel", "lo", " there"] {
        reduce(
            &mut rows,
            &ThreadEvent::ItemDelta {
                thread_id: "t".into(),
                turn_id: "u".into(),
                item_id: "m".into(),
                kind: ItemKind::AgentMessage,
                delta: part.into(),
            },
        );
    }
    match rows.get("m").unwrap() {
        Item::AgentMessage(i) => assert_eq!(i.text, "Hello there"),
        other => panic!("expected an agentMessage, got {other:?}"),
    }
}

#[test]
fn a_delta_for_an_unknown_row_is_reported_not_swallowed() {
    let mut rows = Rows::new();
    let applied = reduce(
        &mut rows,
        &ThreadEvent::ItemDelta {
            thread_id: "t".into(),
            turn_id: "u".into(),
            item_id: "ghost".into(),
            kind: ItemKind::AgentMessage,
            delta: "x".into(),
        },
    );
    assert!(!applied, "a delta for a row that was never opened must be visible to the caller");
    assert!(rows.is_empty());
}

#[test]
fn a_close_may_not_erase_what_was_streamed() {
    // A completed item carries the final state, but if a delta arrived after the snapshot the
    // close was built from, dropping the streamed text would silently lose the answer.
    let mut rows = Rows::new();
    reduce(
        &mut rows,
        &ThreadEvent::ItemStarted {
            thread_id: "t".into(),
            turn_id: "u".into(),
            item: msg("m", "", ItemStatus::InProgress),
        },
    );
    reduce(
        &mut rows,
        &ThreadEvent::ItemDelta {
            thread_id: "t".into(),
            turn_id: "u".into(),
            item_id: "m".into(),
            kind: ItemKind::AgentMessage,
            delta: "the real answer".into(),
        },
    );
    reduce(
        &mut rows,
        &ThreadEvent::ItemCompleted {
            thread_id: "t".into(),
            turn_id: "u".into(),
            item: msg("m", "", ItemStatus::Completed),
        },
    );
    match rows.get("m").unwrap() {
        Item::AgentMessage(i) => {
            assert_eq!(i.text, "the real answer");
            assert_eq!(i.status, ItemStatus::Completed, "the close still owns the status");
        }
        other => panic!("expected an agentMessage, got {other:?}"),
    }
}

#[test]
fn a_close_with_its_own_text_is_the_truth() {
    let mut rows = Rows::new();
    reduce(
        &mut rows,
        &ThreadEvent::ItemStarted {
            thread_id: "t".into(),
            turn_id: "u".into(),
            item: msg("m", "partial", ItemStatus::InProgress),
        },
    );
    reduce(
        &mut rows,
        &ThreadEvent::ItemCompleted {
            thread_id: "t".into(),
            turn_id: "u".into(),
            item: msg("m", "complete", ItemStatus::Completed),
        },
    );
    match rows.get("m").unwrap() {
        Item::AgentMessage(i) => assert_eq!(i.text, "complete"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_truncated_turn_leaves_a_row_that_says_so() {
    // "No answer" and "budget exhausted before the answer started" must not look alike.
    let mut rows = Rows::new();
    reduce(
        &mut rows,
        &ThreadEvent::TurnCompleted {
            thread_id: "t".into(),
            turn_id: "u1".into(),
            response_id: None,
            usage: None,
            final_event: Some("response.incomplete".into()),
            incomplete_reason: Some("max_output_tokens".into()),
        },
    );
    assert_eq!(rows.len(), 1);
    match rows.ordered()[0] {
        Item::Error(i) => {
            assert_eq!(i.message, "max_output_tokens");
            assert_eq!(i.status, ItemStatus::Failed);
        }
        other => panic!("expected an error row, got {other:?}"),
    }
}

#[test]
fn a_clean_completion_invents_no_error_row() {
    let mut rows = Rows::new();
    reduce(
        &mut rows,
        &ThreadEvent::TurnCompleted {
            thread_id: "t".into(),
            turn_id: "u1".into(),
            response_id: Some("r".into()),
            usage: Some(Usage::new(Some(1), Some(2), Some(3))),
            final_event: Some("response.completed".into()),
            incomplete_reason: None,
        },
    );
    assert!(rows.is_empty(), "a finished turn must not look like a failure");
}
