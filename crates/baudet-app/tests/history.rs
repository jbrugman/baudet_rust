
use baudet_app::session::pump;
use baudet_app::AppState;
use baudet_backend::openai::RequestShape;
use baudet_backend::protocol::{chat_request_body, wire_history, Protocol};
use baudet_backend::scripted::{Script, ScriptedBackend};
use baudet_core::*;
use std::sync::Arc;
use tokio::sync::mpsc;

fn answer(thread: &str, turn: &str, text: &str) -> Vec<ThreadEvent> {
    vec![
        ThreadEvent::TurnStarted { thread_id: thread.into(), turn_id: turn.into() },
        ThreadEvent::ItemStarted {
            thread_id: thread.into(),
            turn_id: turn.into(),
            item: Item::AgentMessage(AgentMessageItem {
                created_at_ms: None,
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
            delta: text.into(),
        },
        ThreadEvent::ItemCompleted {
            thread_id: thread.into(),
            turn_id: turn.into(),
            item: Item::AgentMessage(AgentMessageItem {
                created_at_ms: None,
                item_id: format!("{turn}/msg"),
                text: String::new(),
                status: ItemStatus::Completed,
            }),
        },
        ThreadEvent::TurnCompleted {
            thread_id: thread.into(),
            turn_id: turn.into(),
            response_id: None,
            usage: None,
            final_event: Some("response.completed".into()),
            incomplete_reason: None,
        },
    ]
}

fn run_turn(state: &Arc<AppState>, text: &str) {
    let sent = state.send(text).expect("de beurt moet opeisbaar zijn");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let handle = sent.take_reader().expect("één lezer per beurt");
    let folder = Arc::clone(state);
    let reader = std::thread::spawn(move || pump(&handle, |event| folder.apply(event), &tx));
    while rx.blocking_recv().is_some() {}
    reader.join().unwrap();
    state.finish();
}

fn script(events: Vec<ThreadEvent>) -> Script {
    events.into_iter().fold(Script::new(), |script, event| script.event(event))
}

fn sent_transcript(backend: &ScriptedBackend) -> Vec<Msg> {
    let (request, _) = backend.last_request().expect("de speler onthoudt zijn aanvraag");
    wire_history(&request.history)
}

#[test]
fn een_eerste_beurt_neemt_niets_mee() {
    let backend = Arc::new(ScriptedBackend::new(vec![Script::default()]));
    let state = Arc::new(AppState::new(backend.clone(), "systeem"));
    run_turn(&state, "halve wereld");
    assert!(sent_transcript(&backend).is_empty(), "een leeg gesprek mag niet als transcript meegaan");
}

#[test]
fn de_vraag_van_nu_staat_niet_in_de_transcript() {
    let backend = Arc::new(ScriptedBackend::new(vec![Script::default()]));
    let state = Arc::new(AppState::new(backend.clone(), "systeem"));
    run_turn(&state, "halve wereld");
    let (request, _) = backend.last_request().unwrap();
    assert!(request.user_text == "halve wereld", "de vraag gaat in `user_text`");
    assert!(sent_transcript(&backend).is_empty(), "de vraag van nu staat niet in de transcript");
}

#[test]
fn een_beurt_ertoe_doet_het_gesprek_erbij() {
    let backend =
        Arc::new(ScriptedBackend::new(vec![script(answer("th_1", "tu_1", "het eerste antwoord")), Script::default()]));
    let state = Arc::new(AppState::new(backend.clone(), "systeem"));
    run_turn(&state, "vraag een");
    run_turn(&state, "vraag twee");

    let transcript = sent_transcript(&backend);
    assert_eq!(
        transcript,
        vec![Msg::user("vraag een"), Msg::assistant("het eerste antwoord")],
        "het gesprek gaat oudst-eerst mee, en de vraag van nu blijft erbuiten"
    );
}

#[test]
fn een_half_antwoord_blijft_erbuiten() {
    let half = vec![
        ThreadEvent::ItemStarted {
            thread_id: "th_1".into(),
            turn_id: "tu_1".into(),
            item: Item::AgentMessage(AgentMessageItem {
                created_at_ms: None,
                item_id: "tu_1/msg".into(),
                text: String::new(),
                status: ItemStatus::InProgress,
            }),
        },
        ThreadEvent::ItemDelta {
            thread_id: "th_1".into(),
            turn_id: "tu_1".into(),
            item_id: "tu_1/msg".into(),
            kind: ItemKind::AgentMessage,
            delta: "een hal".into(),
        },
    ];
    let backend = Arc::new(ScriptedBackend::new(vec![script(half), Script::default()]));
    let state = Arc::new(AppState::new(backend.clone(), "systeem"));
    run_turn(&state, "vraag een");
    run_turn(&state, "vraag twee");
    assert_eq!(sent_transcript(&backend), vec![Msg::user("vraag een")], "een onvoltooid antwoord telt niet als feit");
}

#[test]
fn redenering_en_foutrijen_reizen_niet_mee() {
    let mut events = answer("th_1", "tu_1", "antwoord");
    events.push(ThreadEvent::ItemStarted {
        thread_id: "th_1".into(),
        turn_id: "tu_1".into(),
        item: Item::Reasoning(ReasoningItem {
            item_id: "tu_1/reasoning".into(),
            text: String::new(),
            status: ItemStatus::InProgress,
        }),
    });
    events.push(ThreadEvent::ItemDelta {
        thread_id: "th_1".into(),
        turn_id: "tu_1".into(),
        item_id: "tu_1/reasoning".into(),
        kind: ItemKind::Reasoning,
        delta: "denkwerk".into(),
    });
    events.push(ThreadEvent::ItemCompleted {
        thread_id: "th_1".into(),
        turn_id: "tu_1".into(),
        item: Item::Reasoning(ReasoningItem {
            item_id: "tu_1/reasoning".into(),
            text: String::new(),
            status: ItemStatus::Completed,
        }),
    });
    let backend = Arc::new(ScriptedBackend::new(vec![script(events), Script::default()]));
    let state = Arc::new(AppState::new(backend.clone(), "systeem"));
    run_turn(&state, "vraag een");
    run_turn(&state, "vraag twee");
    assert_eq!(
        sent_transcript(&backend),
        vec![Msg::user("vraag een"), Msg::assistant("antwoord")],
        "denkwerk en foutrijen zijn geen gesprek"
    );
}

#[test]
fn de_draad_zet_systeem_transcript_vraag_in_die_volgorde() {
    let backend =
        Arc::new(ScriptedBackend::new(vec![script(answer("th_1", "tu_1", "antwoord een")), Script::default()]));
    let state = Arc::new(AppState::new(backend.clone(), "systeem van hier"));
    run_turn(&state, "vraag een");
    run_turn(&state, "vraag twee");

    let (request, _) = backend.last_request().unwrap();
    let history = wire_history(&request.history);
    let shape = RequestShape {
        model: "model",
        system: &request.system,
        history: &history,
        user_text: &request.user_text,
        previous_response_id: None,
        tools: None,
        image: None,
        max_output_tokens: None,
        reasoning: None,
        store: false,
        ..Default::default()
    };
    let body = chat_request_body(&shape);
    let roles: Vec<&str> = body["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, vec!["system", "user", "assistant", "user"], "{:?}", roles);
    assert_eq!(body["messages"][3]["content"], "vraag twee", "de vraag sluit de draad");
    let _ = Protocol::Responses;
}
