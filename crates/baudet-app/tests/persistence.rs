
use baudet_app::session::pump;
use baudet_app::AppState;
use baudet_backend::scripted::{Script, ScriptedBackend};
use baudet_backend::Backend;
use baudet_core::*;
use baudet_store::repository::page;
use std::sync::Arc;
use tokio::sync::mpsc;

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("baudet-persist-{}-{}-{}", name, std::process::id(), line!()))
}

fn answer(turn: &str, text: &str) -> Vec<ThreadEvent> {
    vec![
        ThreadEvent::ItemStarted {
            thread_id: "th_1".into(),
            turn_id: turn.into(),
            item: Item::AgentMessage(AgentMessageItem {
                item_id: format!("{turn}/msg"),
                text: String::new(),
                status: ItemStatus::InProgress,
            }),
        },
        ThreadEvent::ItemDelta {
            thread_id: "th_1".into(),
            turn_id: turn.into(),
            item_id: format!("{turn}/msg"),
            kind: ItemKind::AgentMessage,
            delta: text.into(),
        },
        ThreadEvent::ItemCompleted {
            thread_id: "th_1".into(),
            turn_id: turn.into(),
            item: Item::AgentMessage(AgentMessageItem {
                item_id: format!("{turn}/msg"),
                text: String::new(),
                status: ItemStatus::Completed,
            }),
        },
    ]
}

fn persistent(root: &std::path::Path, scripts: Vec<Vec<ThreadEvent>>) -> (Arc<ScriptedBackend>, Arc<AppState>) {
    let scripts: Vec<Script> = scripts
        .into_iter()
        .map(|events| events.into_iter().fold(Script::new(), |script, event| script.event(event)))
        .collect();
    let backend = Arc::new(ScriptedBackend::new(scripts));
    let state = AppState::new(Arc::clone(&backend) as Arc<dyn Backend>, "systeem")
        .with_persistence(root, "model-van-hier")
        .expect("de map is te openen");
    (backend, Arc::new(state))
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
    if let Some(reason) = state.persist() {
        panic!("opslaan mislukte: {reason}");
    }
}


fn mentions_exact(state: &AppState, tekst: &str) -> usize {
    state.rows().iter().filter(|row| baudet_app::reducer::streamed_text(row) == tekst).count()
}

#[test]
fn een_beurt_overleeft_het_handvat() {
    let root = scratch("herstart");
    let (_, state) = persistent(&root, vec![answer("tu_1", "het eerste antwoord")]);
    run_turn(&state, "vraag een");
    assert!(state.persists(), "met een map is dit gesprek niet puur in het geheugen");

    let id = state.session_id();
    let reopened = AppState::new(Arc::new(ScriptedBackend::new(vec![])) as Arc<dyn Backend>, "systeem")
        .on_session(&root, &id, "model-van-hier", None)
        .expect("hetzelfde gesprek, dezelfde map");

    let rows = reopened.rows();
    let bodies: Vec<String> = rows.iter().map(baudet_app::reducer::streamed_text).collect();
    assert_eq!(bodies, ["vraag een", "het eerste antwoord"]);
}

#[test]
fn een_herstart_leest_niet_het_hele_verleden() {
    let root = scratch("paging");
    let turns = 25;
    let scripts: Vec<Vec<ThreadEvent>> =
        (1..=turns).map(|index| answer(&format!("tu_{index}"), &format!("antwoord {index}"))).collect();
    let (_, state) = persistent(&root, scripts);
    for index in 1..=turns {
        run_turn(&state, &format!("vraag {index}"));
    }
    let total = (turns * 2) as u64;


    let id = state.session_id();
    let reopened = AppState::new(Arc::new(ScriptedBackend::new(vec![])) as Arc<dyn Backend>, "systeem")
        .on_session(&root, &id, "model-van-hier", None)
        .expect("hetzelfde gesprek, dezelfde map");

    assert_eq!(reopened.rows().len() as u64, page::INITIAL as u64, "openen leest de staart, niet het bestand");
    assert_eq!(baudet_app::reducer::streamed_text(reopened.rows().last().unwrap()), format!("antwoord {turns}"));
    let (has_more_older, cursor, _) = reopened.paging();
    assert!(has_more_older, "er staan {total} rijen op schijf, dit beeld houdt er {initial}", initial = page::INITIAL);
    assert!(cursor.is_some(), "een cursor hoort er te zijn zolang er ouder is");

    let mut walked = 0usize;
    while let (true, Some(before), _) = reopened.paging() {
        let added = reopened.load_older(before).expect("een ouder venster");
        if added == 0 {
            break;
        }
        walked += added;
        if walked > total as usize {
            panic!("de cursor boekt niet af: {walked} rijen geladen van {total}");
        }
    }
    let (has_more_older, _, _) = reopened.paging();
    assert!(!has_more_older, "scrollen eindigt bij de allereerste rij, niet halverwege");
    assert_eq!(reopened.rows().len() as u64, total, "elke rij één keer en de volledige lading: {} van {total}", reopened.rows().len());
    let actual: Vec<_> = reopened.rows().iter().map(baudet_app::reducer::streamed_text).collect();
    let expected: Vec<_> =
        (1..=turns).flat_map(|index| [format!("vraag {index}"), format!("antwoord {index}")]).collect();
    assert_eq!(actual, expected, "terugscrollen behoudt de gespreksvolgorde");
}

#[test]
fn opslaan_schrijft_per_beurt_allen_de_staart() {
    let root = scratch("tail");
    let (_, state) = persistent(&root, vec![answer("tu_1", "antwoord een"), answer("tu_2", "antwoord twee")]);
    run_turn(&state, "vraag een");
    let lines_een = count_lines(&root);
    run_turn(&state, "vraag twee");
    let lines_twee = count_lines(&root);

    assert_eq!(lines_een, 2, "één beurt laat precies vraag en antwoord achter: {lines_een}");
    let growth = lines_twee - lines_een;
    assert_eq!(growth, 2, "de tweede beurt hangt zijn eigen twee regels aan, niet het hele gesprek: {growth}");
    assert_eq!(mentions_exact(&state, "vraag een"), 1, "het verleden wordt niet opnieuw geschreven");
}

#[test]
fn een_herstart_houdt_het_project_van_het_gesprek() {
    let root = scratch("project");
    let state = AppState::new(Arc::new(ScriptedBackend::new(vec![])) as Arc<dyn Backend>, "systeem")
        .on_session(&root, "s-project", "model-van-hier", Some("p-tuis"))
        .expect("een nieuw gesprek op schijf");
    if let Some(reason) = state.persist() {
        panic!("opslaan mislukte: {reason}");
    }

    let reopened = AppState::new(Arc::new(ScriptedBackend::new(vec![])) as Arc<dyn Backend>, "systeem")
        .on_session(&root, "s-project", "model-van-hier", None)
        .expect("hetzelfde gesprek, dezelfde map");
    assert_eq!(reopened.project().as_deref(), Some("p-tuis"), "de kop draagt het project");
}

#[test]
fn openen_ergens_raakt_de_huidige_staat_niet_aan() {
    let root = scratch("wissel");
    let (_, state) = persistent(&root, vec![answer("tu_1", "het eerste antwoord")]);
    run_turn(&state, "vraag een");
    let hier = state.session_id();

    let (opened, session) = state
        .open_store(&root, "s-ander", "model-van-hier", Some("p-ander"))
        .expect("een tweede gesprek op dezelfde map");
    assert_eq!(state.session_id(), hier, "er is nog niets geruild");
    assert_eq!(state.rows().len(), 2, "het scherm draagt nog steeds dit gesprek");
    assert_eq!(session.project_id.as_deref(), Some("p-ander"), "het project komt mee uit de keuze");

    let project = state.switch_to(opened, session);
    assert_eq!(project.as_deref(), Some("p-ander"), "de ruil geeft het project terug");
    assert_eq!(state.session_id(), "s-ander", "dit handvat bedient nu dat gesprek");
    assert!(state.rows().is_empty(), "een leeg gesprek tekent een leeg beeld");
    assert!(state.persists(), "het handvat ging mee, anders schreef het volgende feit in het oude bestand");
}

#[test]
fn na_de_wissel_schrijft_de_beurt_in_het_nieuwe_spoor() {
    let root = scratch("spoor");
    let (_backend, state) = persistent(&root, vec![answer("tu_1", "hier"), answer("tu_2", "ginder")]);
    run_turn(&state, "vraag hier");
    let hier = state.session_id();

    let (opened, session) = state.open_store(&root, "s-ginder", "model-van-hier", None).expect("open");
    state.switch_to(opened, session);
    run_turn(&state, "vraag ginder");

    assert_eq!(count_lines_in(&root, "s-ginder"), 2, "de beurt hangt aan het gesprek dat openstaat");
    assert_eq!(count_lines_in(&root, &hier), 2, "het vorige gesprek houdt zijn eigen twee regels");
}

#[test]
fn een_herstart_nummert_door_waar_het_gesprek_ophield() {
    let root = scratch("dornummer");
    let (_, state) = persistent(&root, vec![answer("tu_1", "het eerste antwoord")]);
    run_turn(&state, "vraag een");
    let id = state.session_id();

    let script = answer("tu_2", "het tweede antwoord")
        .into_iter()
        .fold(Script::new(), |s, event| s.event(event));
    let reopened_backend = ScriptedBackend::new(vec![script]);
    let reopened = Arc::new(
        AppState::new(Arc::new(reopened_backend) as Arc<dyn Backend>, "systeem")
            .on_session(&root, &id, "model-van-hier", None)
            .expect("hetzelfde gesprek, dezelfde map"),
    );
    assert_eq!(reopened.rows().len(), 2, "de herstart draagt vraag en antwoord");

    run_turn(&reopened, "vraag twee");

    let rows = reopened.rows();
    let bodies: Vec<String> = rows.iter().map(baudet_app::reducer::streamed_text).collect();
    assert_eq!(bodies.len(), 4, "vraag een + antwoord een + vraag twee + antwoord twee: {bodies:?}");
    assert!(bodies.iter().any(|body| body == "vraag twee"), "de nieuwe vraag staat er: {bodies:?}");
    assert!(bodies.iter().any(|body| body == "het tweede antwoord"), "het antwoord van de tweede beurt staat er: {bodies:?}");
    assert_eq!(mentions_exact(&reopened, "vraag een"), 1, "de oude vraag blijft één keer: {bodies:?}");
}

fn count_lines_in(root: &std::path::Path, session_id: &str) -> usize {
    let mut total = 0usize;
    for day in std::fs::read_dir(root).expect("de data-map") {
        let dir = day.expect("een bucket").path().join(session_id);
        let Ok(items) = std::fs::read_to_string(dir.join("items.jsonl")) else { continue };
        total += items.lines().filter(|line| !line.trim().is_empty()).count();
    }
    total
}

#[test]
fn de_handvat_telling_is_de_schrijftelling() {
    let root = scratch("telling");
    let (_, state) = persistent(&root, vec![answer("tu_1", "het eerste antwoord"), answer("tu_2", "het tweede antwoord")]);
    assert_eq!(state.disk_items(), Some(0), "een gesprek dat nog niets zei, staat met nul regels op schijf");
    run_turn(&state, "vraag een");
    let na_een = count_lines(&root);
    assert_eq!(state.disk_items(), Some(na_een), "na één beurt: handvat {:#?} tegen schijf {na_een}", state.disk_items());
    assert!(na_een > 0, "er moet iets op schijf staan, anders meet deze test niets");
    run_turn(&state, "vraag twee");
    let na_twee = count_lines(&root);
    assert_eq!(state.disk_items(), Some(na_twee), "na twee beurten: handvat {:?} tegen schijf {na_twee}", state.disk_items());
    assert!(na_twee > na_een, "de tweede beurt moet meer regels hebben achtergelaten");

    let (_, leads) = ledig_handvat();
    assert_eq!(leads.disk_items(), None, "zonder handvat is er niets te tellen");
}

fn ledig_handvat() -> (Arc<ScriptedBackend>, Arc<AppState>) {
    let backend = Arc::new(ScriptedBackend::new(vec![]));
    let state = AppState::new(Arc::clone(&backend) as Arc<dyn Backend>, "systeem");
    (backend, Arc::new(state))
}

fn count_lines(root: &std::path::Path) -> usize {
    const FILES: [&str; 1] = ["items.jsonl"];
    let mut total = 0usize;
    let dag = match std::fs::read_dir(root) {
        Ok(iter) => iter,
        Err(_) => return 0,
    };
    for bucket in dag.flatten() {
        let bucket = bucket.path();
        if !bucket.is_dir() {
            continue;
        }
        let sessies = match std::fs::read_dir(&bucket) {
            Ok(iter) => iter,
            Err(_) => continue,
        };
        for sessie in sessies.flatten() {
            let dir = sessie.path();
            if !dir.is_dir() {
                continue;
            }
            for name in FILES {
                if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
                    total += text.lines().filter(|line| !line.trim().is_empty()).count();
                }
            }
        }
    }
    total
}

#[test]
fn compaction_commits_summary_with_retained_context_and_keeps_the_display_trail() {
    use baudet_app::compaction::{CompactionOutcome, CompactionPlan};
    use baudet_backend::{BackendError, SendRequest, TurnHandle};
    use baudet_core::Turn;

    struct Summarizer;
    impl Backend for Summarizer {
        fn send(&self, _: SendRequest) -> Result<TurnHandle, BackendError> {
            unreachable!()
        }

        fn complete_once(&self, _: &str, text: &str, _: u32) -> Result<Turn, BackendError> {
            assert!(text.contains("old question"));
            assert!(!text.contains("recent question"));
            Ok(Turn::text("retained facts"))
        }
    }

    let root = scratch("compaction");
    let (_, state) = persistent(&root, vec![answer("tu_1", "old answer"), answer("tu_2", "recent answer")]);
    run_turn(&state, "old question");
    run_turn(&state, "recent question");
    let id = state.session_id();
    let state = AppState::new(Arc::new(Summarizer), "system").on_session(&root, &id, "model", None).unwrap();
    let before = state.rows();
    let plan = CompactionPlan { keep_recent: 2, max_chars: 4000, max_tokens: 1024 };
    assert_eq!(state.compact(&plan).unwrap(), CompactionOutcome::Compacted { summary: "retained facts".into(), folded: 2 });
    assert_eq!(state.rows(), before);
    let reopened = state.build_session().unwrap();
    assert_eq!(reopened.summary.as_deref(), Some("retained facts"));
    assert_eq!(reopened.messages.iter().map(|msg| msg.content.as_str()).collect::<Vec<_>>(), ["recent question", "recent answer"]);
    assert_eq!(reopened.items, before);
}

#[test]
fn a_tool_turn_feeds_the_observation_back_and_persists_the_final_answer() {
    use baudet_app::session::tool_turn::ToolTurn;
    use baudet_projects::{FolderPermission, Permissions, Project};
    use baudet_tool::mcp::McpServers;
    use std::sync::atomic::AtomicBool;

    let root = scratch("tool-turn");
    let first = ["```baudet-tool", r#"{"tool":"read_file","arguments":{"file":"example.txt"}}"#, "```"].join("\n");
    let completed = |turn: &str, text: &str| {
        let mut events = answer(turn, text);
        events.push(ThreadEvent::TurnCompleted {
            thread_id: "th_1".into(), turn_id: turn.into(), response_id: None,
            usage: None, final_event: Some("response.completed".into()), incomplete_reason: None,
        });
        events
    };
    let (_, state) = persistent(&root, vec![completed("tu_1", &first), completed("tu_2", "Read it.")]);
    std::fs::write(root.join("example.txt"), "known observation").unwrap();
    let mut project = Project::new("p-1".into(), "Fixture", 0);
    project.folder_path = Some(root.to_string_lossy().into_owned());
    project.permissions = Permissions::encode([FolderPermission::Read, FolderPermission::Write]);
    let settings = baudet_settings::Settings::default();
    state.run_tools("Read the file", None, ToolTurn {
        project: &project,
        settings: &settings,
        servers: &mut McpServers::default(),
        approval: Arc::new(|_| ApprovalDecision::Decline),
        cancelled: Arc::new(AtomicBool::new(false)),
    }).unwrap();
    assert_eq!(state.live_turns(), 0);
    assert!(state.rows().iter().any(|item| matches!(item, Item::ToolCall(call) if call.output.contains("known observation"))));
    assert_eq!(mentions_exact(&state, "Read it."), 1);
    assert_eq!(state.persist(), None);
    let reopened = state.build_session().unwrap();
    assert_eq!(reopened.messages.last().unwrap().content, "Read it.");
    assert!(reopened.messages.iter().any(|msg| msg.content.contains("known observation")));
}
