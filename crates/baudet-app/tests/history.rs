//! Wat een beurt werkelijk meekrijgt aan gesprek.
//!
//! De events die een beurt publiceerd *zijn* het gesprek niet: of het gesprek meegaat in de
//! aanvraag is in die stream niet te zien. Daarom leest deze suite de aanvraag die de speler
//! onthoudt (`ScriptedBackend::last_request`) en vouwt hem daarna met dezelfde functie die de
//! transport-laag gebruikt (`wire_history`). Dat is de hele keten die een model te zien krijgt,
//! zonder dat er een server voor nodig is.

use baudet_app::session::pump;
use baudet_app::AppState;
use baudet_backend::openai::RequestShape;
use baudet_backend::protocol::{chat_request_body, wire_history, Protocol};
use baudet_backend::scripted::{Script, ScriptedBackend};
use baudet_core::*;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Een voltooid antwoord van de modelkant, in de vorm die de transport-laag zelf sluit.
///
/// `turn` is de id die de gesprekseigenaar de beurt geeft -- `tu_1` voor de eerste, `tu_2` voor de
/// tweede -- want het id van een rij bepaalt welke rij een delta raakt. Een script dat hier een
/// andere id kiest dan de eigenaar, opent een rij die bij een andere beurt hoort.
fn answer(thread: &str, turn: &str, text: &str) -> Vec<ThreadEvent> {
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
            delta: text.into(),
        },
        // De sluiting is ledig, zoals de transport-laag hem stuurt: `close_open_rows` sluit een rij
        // met `text: ""`, en de reducer houdt dan de gestroomde tekst aan. Een fixture die hier wél
        // tekst neerzet, test de gaten dat deze laag deugt in plaats van te bewijzen wat er gebeurt
        // als een server zwijgt bij het sluiten.
        ThreadEvent::ItemCompleted {
            thread_id: thread.into(),
            turn_id: turn.into(),
            item: Item::AgentMessage(AgentMessageItem {
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

/// De beurt zoals een venster hem zou afhandelen: claimen, vouwen, loslaten.
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

/// Een script van een reeks events, want `Script` kent één-event-per-stap.
fn script(events: Vec<ThreadEvent>) -> Script {
    events.into_iter().fold(Script::new(), |script, event| script.event(event))
}

/// De rolledraade van een beurt: de events uit de aanvraag, omgezet naar de draadvolgorde.
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
    // De vraag reist als `user_text`; de vraagblaadjes (`tu_1/user`) vouwen de transport-laag pas
    // in ná de transcript, dus hier mag die tekst niet nog een keer staan.
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
    // Een beurt die nooit voltooid raakt (afgebroken, mislukt, of simpelweg een script dat earlier
    // stopt) laat een rij na die niet `Completed` is. Zoiets terugsenturen is een leugen tegen het
    // model: het is nog niet waar terwijl de persoon het op scherm ziet veranderen.
    let half = vec![ThreadEvent::ItemStarted {
        thread_id: "th_1".into(),
        turn_id: "tu_1".into(),
        item: Item::AgentMessage(AgentMessageItem {
            item_id: "tu_1/msg".into(),
            text: "een hal".into(),
            status: ItemStatus::InProgress,
        }),
    }];
    // Het halve antwoord speelt in de eerste beurt, want `half` opent `tu_1/msg`.
    let backend = Arc::new(ScriptedBackend::new(vec![script(half), Script::default()]));
    let state = Arc::new(AppState::new(backend.clone(), "systeem"));
    run_turn(&state, "vraag een");
    run_turn(&state, "vraag twee");
    assert_eq!(sent_transcript(&backend), vec![Msg::user("vraag een")], "een onvoltooid antwoord telt niet als feit");
}

#[test]
fn redenering_en_foutrijen_reizen_niet_mee() {
    // Wat een model zich herinnert is het gesprek, niet zijn eigen denkwerk en niet een foutregel
    // die de persoon nooit naar hem stuurde.
    let mut events = answer("th_1", "tu_1", "antwoord");
    // Openén, dan pas sluiten: `close_open_rows` sluit alleen een rij die het zelf opende, en een
    // sluiting op een onbekende rij meldt de reducer als verwerpelijk (`reduce` geeft `false`).
    // Alles hier moet dus een vorm zijn die een backend werkelijk stuurt.
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
    // Eén beurt met alles erin: het gesprek van deze fixture is het denkwerk-zelf, en dat mag niet
    // meereizen naar de volgende vraag.
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
    // De twee bouwen van dezelfde transcript verschillende draden, maar beiden moeten systeem,
    // gesprek en vraag in die volgorde neerzetten -- omgewisseld leest een model zijn eigen vraag
    // als het vorige antwoord.
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
    };
    let body = chat_request_body(&shape);
    let roles: Vec<&str> = body["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, vec!["system", "user", "assistant", "user"], "{:?}", roles);
    assert_eq!(body["messages"][3]["content"], "vraag twee", "de vraag sluit de draad");
    let _ = Protocol::Responses; // beide dialecten delen deze builder; het woord is hier niet de vraag
}
