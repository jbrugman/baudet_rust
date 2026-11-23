
use baudet_backend::scripted::{Script, ScriptedBackend, Step};
use baudet_backend::{Backend, SendRequest};
use baudet_core::*;
use std::sync::Arc;

fn ev(deltas: &[&str], thread: &str, turn: &str) -> Vec<ThreadEvent> {
    let mut out = vec![
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
    ];
    for delta in deltas {
        out.push(ThreadEvent::ItemDelta {
            thread_id: thread.into(),
            turn_id: turn.into(),
            item_id: format!("{turn}/msg"),
            kind: ItemKind::AgentMessage,
            delta: (*delta).into(),
        });
    }
    out
}

fn drain(backend: &ScriptedBackend, text: &str) -> Vec<ThreadEvent> {
    let mut handle = backend.send(SendRequest { user_text: text.into(), ..Default::default() }).unwrap();
    let mut seen = Vec::new();
    while let Some(event) = handle.events().blocking_recv() {
        seen.push(event);
    }
    seen
}

#[test]
fn a_script_publishes_its_events_in_order() {
    let events = ev(&["Hel", "lo"], "th_1", "tu_1");
    let mut script = Script::new().event(events[0].clone());
    for event in &events[1..] {
        script = script.event(event.clone());
    }
    let backend = ScriptedBackend::new(vec![script]);
    let seen = drain(&backend, "hi");
    assert_eq!(seen, events, "a script must publish exactly what it says");
}

#[test]
fn the_second_turn_gets_the_second_script() {
    let backend = ScriptedBackend::new(vec![
        Script::new().event(ThreadEvent::TurnStarted { thread_id: "t".into(), turn_id: "tu_1".into() }),
        Script::new().event(ThreadEvent::TurnStarted { thread_id: "t".into(), turn_id: "tu_2".into() }),
    ]);
    assert_eq!(drain(&backend, "one").len(), 1);
    assert_eq!(backend.turns_started(), 1);
    assert_eq!(drain(&backend, "two").len(), 1);
    assert_eq!(backend.turns_started(), 2);
}

#[test]
fn a_script_that_never_completes_really_never_completes() {
    let seen = drain(&ScriptedBackend::new(vec![Script::new()]), "hi");
    assert!(seen.is_empty());
    assert!(!seen.iter().any(|e| e.wire_method() == "turn/completed"));
}

#[test]
fn raw_sse_goes_through_the_real_framer() {
    let script = Script::new().step(Step::RawSse {
        lines: vec![
            "event: response.output_text.delta".into(),
            "data: {\"delta\":\"Hel\"}".into(),
            "".into(),
            "data: {".into(),
            "data: \"delta\":\"lo\"}".into(),
            "".into(),
            "data: [DONE]".into(),
            "".into(),
        ],
        to_event: Arc::new(|data| {
            let value: serde_json::Value = serde_json::from_str(data).ok()?;
            let delta = value.get("delta")?.as_str()?;
            Some(ThreadEvent::ItemDelta {
                thread_id: "th_1".into(),
                turn_id: "tu_1".into(),
                item_id: "tu_1/msg".into(),
                kind: ItemKind::AgentMessage,
                delta: delta.to_string(),
            })
        }),
    });
    let seen = drain(&ScriptedBackend::new(vec![script]), "hi");
    let joined: String = seen
        .iter()
        .filter_map(|e| match e {
            ThreadEvent::ItemDelta { delta, .. } => Some(delta.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(joined, "Hello", "framing must produce the same deltas a socket would");
    assert_eq!(seen.len(), 2, "[DONE] must not become an event");
}

#[test]
fn a_cancel_reaches_the_turn_and_a_drained_stream_closes() {
    let backend = ScriptedBackend::new(vec![
        Script::new().event(ThreadEvent::TurnStarted { thread_id: "th_1".into(), turn_id: "tu_1".into() })
    ]);
    let mut handle = backend.send(SendRequest::default()).unwrap();
    assert!(handle.events().blocking_recv().is_some());
    assert!(handle.events().blocking_recv().is_none(), "the stream must close when the script ends");
    handle.cancel();
    handle.steer("stop that");
    assert!(backend.cancelled(), "a cancel must be observable by the owner");
    assert_eq!(backend.steers(), vec!["stop that".to_string()], "a steer must be observable");
}
