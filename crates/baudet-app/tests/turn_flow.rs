
use baudet_app::session::pump;
use baudet_app::AppState;
use baudet_backend::scripted::{Script, ScriptedBackend};
use baudet_core::*;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

fn scripted(events: Vec<ThreadEvent>) -> Arc<ScriptedBackend> {
    let mut script = Script::new();
    for event in events {
        script = script.event(event);
    }
    Arc::new(ScriptedBackend::new(vec![script]))
}

fn run_turn(state: &Arc<AppState>, text: &str) -> Vec<ThreadEvent> {
    let sent = state.send(text).expect("the turn must be claimable");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let handle = sent.take_reader().expect("one reader per turn");
    let folder = Arc::clone(state);
    let pump_thread = std::thread::spawn(move || {
        pump(&handle, |event| folder.apply(event), &tx);
    });
    let mut seen = Vec::new();
    while let Some(event) = rx.blocking_recv() {
        seen.push(event);
    }
    pump_thread.join().unwrap();
    state.finish();
    seen
}

fn answer(thread: &str, turn: &str) -> Vec<ThreadEvent> {
    vec![
        ThreadEvent::TurnStarted { thread_id: thread.into(), turn_id: turn.into() },
        ThreadEvent::ItemStarted {
            thread_id: thread.into(),
            turn_id: turn.into(),
            item: Item::AgentMessage(AgentMessageItem {
                item_id: format!("{turn}/msg"),
                text: String::new(),
                status: ItemStatus::InProgress,
            }),
        },
        ThreadEvent::ItemDelta {
            thread_id: thread.into(),
            turn_id: turn.into(),
            item_id: format!("{turn}/msg"),
            kind: ItemKind::AgentMessage,
            delta: "Hallo".into(),
        },
        ThreadEvent::ItemCompleted {
            thread_id: thread.into(),
            turn_id: turn.into(),
            item: Item::AgentMessage(AgentMessageItem {
                item_id: format!("{turn}/msg"),
                text: "Hallo".into(),
                status: ItemStatus::Completed,
            }),
        },
        ThreadEvent::TurnCompleted {
            thread_id: thread.into(),
            turn_id: turn.into(),
            response_id: Some("resp_1".into()),
            usage: Some(Usage::new(Some(11), Some(4), Some(15))),
            final_event: Some("response.completed".into()),
            incomplete_reason: None,
        },
    ]
}

fn state_for(events: Vec<ThreadEvent>) -> (Arc<ScriptedBackend>, Arc<AppState>) {
    let backend = scripted(events);
    let state = Arc::new(AppState::new(Arc::clone(&backend) as Arc<_>, "system prompt"));
    (backend, state)
}

#[test]
fn the_instruction_is_a_row_before_the_answer_arrives() {
    let (_backend, state) = state_for(vec![]);
    let sent = state.send("en?").unwrap();
    let rows = state.rows();
    assert_eq!(rows.len(), 1, "the user row must exist before a single event is folded");
    assert_eq!(rows[0].kind(), ItemKind::UserMessage);
    assert_eq!(rows[0].status(), ItemStatus::Completed);
    drop(sent);
}

#[test]
fn a_turn_folds_into_the_rows_it_shows() {
    let (_backend, state) = state_for(answer("th_1", "tu_1"));
    let seen = run_turn(&state, "wat is er klaar?");
    assert_eq!(seen.len(), 5, "every scripted event must reach the renderer");
    let rows = state.rows();
    let kinds: Vec<ItemKind> = rows.iter().map(|i| i.kind()).collect();
    assert_eq!(kinds, vec![ItemKind::UserMessage, ItemKind::AgentMessage]);
    match &rows[1] {
        Item::AgentMessage(i) => assert_eq!(i.text, "Hallo"),
        other => panic!("expected an agentMessage, got {other:?}"),
    }
}

#[test]
fn a_running_turn_holds_the_session_and_finish_frees_it() {
    let (_backend, state) = state_for(answer("th_1", "tu_1"));
    let sent = state.send("eerste").unwrap();
    assert_eq!(state.live_turns(), 1);
    assert_eq!(
        state.send("tweede").err(),
        Some(baudet_app::session::TurnBusy),
        "a second live turn is refused, not interleaved"
    );
    drop(sent);
    state.finish();
    assert_eq!(state.live_turns(), 0);
    assert!(state.send("derde").is_ok(), "the next turn must be claimable once the first is done");
}

#[test]
fn a_stopped_turn_leaves_its_open_rows_interrupted_not_failed() {
    let (_backend, state) = state_for(answer("th_1", "tu_1"));
    let _sent = state.send("ga").unwrap();
    state.apply(&ThreadEvent::ItemStarted {
        thread_id: "th_1".into(),
        turn_id: "tu_1".into(),
        item: Item::AgentMessage(AgentMessageItem {
            item_id: "tu_1/msg".into(),
            text: String::new(),
            status: ItemStatus::InProgress,
        }),
    });
    state.cancel_turn();
    let rows = state.rows();
    let open: Vec<&Item> = rows.iter().filter(|i| i.kind() == ItemKind::AgentMessage).collect();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].status(), ItemStatus::Interrupted, "a deliberate Stop is not a failure");
}

#[test]
fn a_cleanly_closed_turn_leaves_no_row_working() {
    let (_backend, state) = state_for(answer("th_1", "tu_1"));
    run_turn(&state, "ga");
    state.apply(&ThreadEvent::ItemStarted {
        thread_id: "th_1".into(),
        turn_id: "tu_1".into(),
        item: Item::Plan(PlanItem {
            item_id: "tu_1/plan".into(),
            entries: vec![PlanEntry::new("gui", PlanEntryStatus::InProgress)],
            status: ItemStatus::InProgress,
        }),
    });
    assert!(
        state.rows().iter().any(|item| item.status() == ItemStatus::InProgress),
        "the fixture should have left a working row to settle"
    );
    state.apply(&ThreadEvent::ItemCompleted {
        thread_id: "th_1".into(),
        turn_id: "tu_1".into(),
        item: Item::Plan(PlanItem {
            item_id: "tu_1/plan".into(),
            entries: vec![PlanEntry::new("gui", PlanEntryStatus::InProgress)],
            status: ItemStatus::Completed,
        }),
    });
    let working: Vec<ItemKind> =
        state.rows().iter().filter(|item| item.status() == ItemStatus::InProgress).map(|item| item.kind()).collect();
    assert_eq!(working, Vec::<ItemKind>::new(), "a closed turn must not paint a working row");
}

#[test]
fn a_second_turn_appends_rather_than_replaces() {
    let backend: Arc<ScriptedBackend> = Arc::new(ScriptedBackend::new(vec![
        {
            let mut s = Script::new();
            for e in answer("th_1", "tu_1") {
                s = s.event(e);
            }
            s
        },
        {
            let mut s = Script::new();
            for e in answer("th_1", "tu_2") {
                s = s.event(e);
            }
            s
        },
    ]));
    let state = Arc::new(AppState::new(backend as Arc<_>, "system"));
    run_turn(&state, "eerste");
    run_turn(&state, "tweede");
    let rows = state.rows();
    assert_eq!(rows.len(), 4, "got {:?}", rows.iter().map(|i| i.kind()).collect::<Vec<_>>());
    assert_eq!(rows[0].kind(), ItemKind::UserMessage);
    assert_eq!(rows[1].kind(), ItemKind::AgentMessage);
    assert_eq!(rows[2].kind(), ItemKind::UserMessage);
    assert_eq!(rows[3].kind(), ItemKind::AgentMessage);
}

#[test]
fn one_reader_can_take_the_turn_only_once() {
    let (_backend, state) = state_for(answer("th_1", "tu_1"));
    let sent = state.send("kijk").unwrap();
    assert!(sent.take_reader().is_some(), "the first claim wins");
    assert!(sent.take_reader().is_none(), "a second reader must be refused, not served torn events");
}

#[test]
fn the_reader_folds_every_event_it_drains() {
    let (_backend, state) = state_for(answer("th_1", "tu_1"));
    let sent = state.send("kijk").unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let handle = sent.take_reader().unwrap();
    let folder = Arc::clone(&state);
    let pump_thread = std::thread::spawn(move || pump(&handle, |e| folder.apply(e), &tx));
    let mut seen = Vec::new();
    while let Some(e) = rx.blocking_recv() {
        seen.push(e);
    }
    pump_thread.join().unwrap();
    assert_eq!(seen.len(), 5, "the one reader must forward the whole turn");
    assert_eq!(state.rows().len(), 2, "and the owner must have folded the same turn");
}

#[test]
fn the_state_is_shareable_behind_one_handle() {
    let (_backend, state) = state_for(vec![]);
    let shared = Arc::new(Mutex::new(state));
    let guard = shared.lock().unwrap();
    let _ = guard.send("kan dit").unwrap();
    assert_eq!(guard.rows().len(), 1);
}
