
use baudet_app::{reduce, Emitter, Rows};
use baudet_core::*;


fn command_card(item_id: &str) -> Item {
    Item::CommandExecution(CommandExecutionItem {
        item_id: item_id.to_string(),
        command: "cargo test".to_string(),
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

fn patch_card(item_id: &str) -> Item {
    Item::FileChange(FileChangeItem {
        item_id: item_id.to_string(),
        path: "src/main.rs".to_string(),
        change: FileChangeKind::Update,
        status: ItemStatus::InProgress,
        diff: None,
        additions: 0,
        deletions: 0,
    })
}

fn opened(item: Item) -> ThreadEvent {
    ThreadEvent::ItemStarted { thread_id: "th".to_string(), turn_id: "tu".to_string(), item }
}

fn asked(item_id: &str) -> ThreadEvent {
    ThreadEvent::ApprovalRequested {
        thread_id: "th".to_string(),
        turn_id: "tu".to_string(),
        item_id: item_id.to_string(),
        request: ApprovalRequest::RunCommand {
            command: "cargo test".to_string(),
            cwd: None,
            timeout_seconds: None,
            always_key: None,
        },
    }
}

fn answered(item_id: &str, decision: ApprovalDecision) -> ThreadEvent {
    ThreadEvent::ApprovalResolved {
        thread_id: "th".to_string(),
        turn_id: "tu".to_string(),
        item_id: item_id.to_string(),
        decision,
    }
}

#[test]
fn a_question_lands_on_the_card_it_is_holding() {
    let mut rows = Rows::new();
    reduce(&mut rows, &opened(command_card("th/tu/i1")));
    assert!(reduce(&mut rows, &asked("th/tu/i1")), "de vraag raakte de kaart niet");
    assert_eq!(rows.len(), 1, "de vraag maakte een tweede regel");
    match rows.get("th/tu/i1") {
        Some(Item::CommandExecution(card)) => {
            assert_eq!(card.status, ItemStatus::InProgress, "een vraag deed alsof de beurt klaar was");
            assert!(card.approval.is_none(), "er is nog niets beslist");
        }
        other => panic!("verwacht de geopende kaart, got {other:?}"),
    }
}

#[test]
fn a_question_on_a_row_that_never_was_gets_a_card_of_its_own() {
    let mut rows = Rows::new();
    assert!(reduce(&mut rows, &asked("th/tu/i7")));
    assert_eq!(rows.len(), 1);
    assert!(rows.get("th/tu/i7").is_some(), "de vraag verdween spoorloos");
}

#[test]
fn an_answer_settles_the_card_and_says_which_way() {
    let mut rows = Rows::new();
    reduce(&mut rows, &opened(command_card("th/tu/i1")));
    reduce(&mut rows, &asked("th/tu/i1"));
    assert!(reduce(&mut rows, &answered("th/tu/i1", ApprovalDecision::Accept)));
    match rows.get("th/tu/i1") {
        Some(Item::CommandExecution(card)) => {
            assert_eq!(card.approval, Some(ApprovalDecision::Accept), "de beslissing verdween")
        }
        other => panic!("verwacht de kaart, got {other:?}"),
    }
}

#[test]
fn a_declined_patch_shows_that_it_was_declined() {
    let mut rows = Rows::new();
    reduce(&mut rows, &opened(patch_card("th/tu/i2")));
    assert!(reduce(&mut rows, &answered("th/tu/i2", ApprovalDecision::Decline)));
    match rows.get("th/tu/i2") {
        Some(Item::FileChange(change)) => {
            assert_eq!(change.status, ItemStatus::Failed, "een weigering deed alsof het lukte")
        }
        other => panic!("verwacht de patch-kaart, got {other:?}"),
    }
}

#[test]
fn an_answer_for_a_row_that_never_was_is_reported_not_fabricated() {
    let mut rows = Rows::new();
    assert!(!reduce(&mut rows, &answered("th/tu/i9", ApprovalDecision::Accept)));
    assert!(rows.is_empty(), "er verscheen een regel uit het niets");
}

#[test]
fn a_turn_divides_ids_that_do_not_collide() {
    let mut rows = Rows::new();
    let mut emitter = Emitter::display(&mut rows, "th_1", "tu_1");
    let first = emitter.next_item_id();
    let second = emitter.next_item_id();
    assert_eq!(first, "th_1/tu_1/i1");
    assert_ne!(first, second, "twee regels van één beurt heetten hetzelfde");
    reduce(&mut rows, &opened(command_card(&first)));
    reduce(&mut rows, &opened(command_card(&second)));
    assert_eq!(rows.len(), 2, "de beurt verdeelde dubbele id's");
}

#[test]
fn an_observation_reaches_the_model_without_becoming_a_row() {
    let mut rows = Rows::new();
    let mut emitter = Emitter::display(&mut rows, "th_1", "tu_1");
    emitter.store_message(&Msg::user("the file holds 12 lines"));
    let observed = emitter.taken_messages().len();
    assert_eq!(observed, 1, "de observatie haalde het model niet");
    assert!(rows.is_empty(), "een observatie tekende een rij");
}

fn msg(id: &str, text: &str, status: ItemStatus) -> Item {
    Item::AgentMessage(AgentMessageItem { created_at_ms: None, item_id: id.into(), text: text.into(), status })
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
