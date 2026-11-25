
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

    let backend = Arc::new(ScriptedBackend::new(vec![]));
    state.replace_backend(backend.clone());
    let _sent = state.send("Continue the request").unwrap();
    let (request, _) = backend.last_request().unwrap();
    assert!(request.system.contains("retained facts"));
    assert!(!request.system.contains(baudet_store::CONTEXT_LOSS_NOTE));
    assert!(request.previous_response_id.is_none());
}

#[test]
fn tool_turns_replay_long_history_or_stop_without_losing_the_request() {
    use baudet_app::session::tool_turn::ToolTurn;
    use baudet_projects::{FolderPermission, Permissions, Project};
    use baudet_settings::config::ContextStrategy;
    use baudet_tool::mcp::McpServers;
    use std::sync::atomic::AtomicBool;

    let root = scratch("tool-context");
    let (backend, state) = persistent(&root, vec![answer("tu_1", "Started"), answer("tu_2", "Reviewed")]);
    let original = format!("Review the project and report findings.\n{}", "original context ".repeat(3_500));
    run_turn(&state, &original);
    let mut project = Project::new("p-1".into(), "Fixture", 0);
    project.folder_path = Some(root.to_string_lossy().into_owned());
    project.permissions = Permissions::encode([FolderPermission::Read]);
    let mut settings = baudet_settings::Settings {
        context_strategy: ContextStrategy::StopAtLimit,
        context_budget_tokens: 0,
        model: Some("unknown-fixture-model".into()),
        use_server_session: false,
        ..Default::default()
    };
    let submit = |text, settings: &baudet_settings::Settings| {
        state.run_tools(text, None, ToolTurn {
            project: Some(&project),
            settings,
            servers: &mut McpServers::default(),
            approval: Arc::new(|_| ApprovalDecision::Decline),
            cancelled: Arc::new(AtomicBool::new(false)),
        }).unwrap()
    };

    submit("Continue the review", &settings);
    let (request, _) = backend.last_request().unwrap();
    assert!(baudet_backend::protocol::wire_history(&request.history).iter().any(|msg| msg.content == original));
    assert!(!request.system.contains(baudet_store::CONTEXT_LOSS_NOTE));
    assert_eq!(backend.turns_started(), 2);

    settings.context_budget_tokens = 12_000;
    let stopped = submit("Check the last issue", &settings);
    assert!(baudet_core::error::looks_like_context_overflow(stopped.as_deref()));
    assert_eq!(backend.turns_started(), 2, "the oversized request never reaches the backend");
    assert_eq!(state.persist(), None);
    let messages = state.build_session().unwrap().messages;
    assert!(messages.iter().any(|msg| msg.content == original));
    assert_eq!(messages.last().unwrap().content, "Check the last issue");
}

#[test]
fn auto_compact_keeps_a_running_tool_turn_and_persists_the_checkpoint() {
    use baudet_app::session::tool_turn::ToolTurn;
    use baudet_projects::{FolderPermission, Permissions, Project};
    use baudet_settings::config::ContextStrategy;
    use baudet_tool::mcp::McpServers;
    use std::sync::atomic::AtomicBool;

    let completed = |id: &str, text: &str| {
        let mut events = answer(id, text);
        events.push(ThreadEvent::TurnCompleted {
            thread_id: "th_1".into(), turn_id: id.into(), response_id: None,
            usage: None, final_event: Some("response.completed".into()), incomplete_reason: None,
        });
        events
    };
    let root = scratch("automatic-compact");
    let (backend, state) = persistent(&root, vec![
        completed("tu_1", "Ready"),
        completed("tu_2", r#"{"tool":"read_file","arguments":{"file":"example.txt"}}"#),
        completed("tu_3", "Continue the review and report the findings from the file."),
        completed("tu_4", "Review complete"),
    ]);
    run_turn(&state, &"previous research ".repeat(600));
    std::fs::write(root.join("example.txt"), "file observation ".repeat(600)).unwrap();
    let mut project = Project::new("p-1".into(), "Fixture", 0);
    project.folder_path = Some(root.to_string_lossy().into_owned());
    project.permissions = Permissions::encode([FolderPermission::Read]);
    let settings = baudet_settings::Settings {
        context_strategy: ContextStrategy::AutoCompact,
        context_budget_tokens: 6_000,
        context_keep_recent_messages: 1,
        model: Some("unknown-fixture-model".into()),
        ..Default::default()
    };
    assert_eq!(state.run_tools("Review the file", None, ToolTurn {
        project: Some(&project), settings: &settings,
        servers: &mut McpServers::default(),
        approval: Arc::new(|_| ApprovalDecision::Decline),
        cancelled: Arc::new(AtomicBool::new(false)),
    }).unwrap(), None);

    assert_eq!(backend.turns_started(), 4);
    let (request, _) = backend.last_request().unwrap();
    assert!(request.system.contains("Continue the review and report the findings"));
    assert!(request.user_text.contains("file observation"));
    assert!(baudet_backend::protocol::wire_history(&request.history).iter().any(|msg| msg.content == "Review the file"));
    assert!(request.previous_response_id.is_none());
    let rows = state.rows();
    assert_eq!(rows.iter().filter(|item| matches!(item, Item::ToolCall(_))).count(), 1);
    assert_eq!(state.persist(), None);
    let reopened = state.build_session().unwrap();
    assert!(reopened.summary.unwrap().contains("Continue the review"));
    assert!(reopened.messages.iter().any(|msg| msg.content.contains("file observation")));
    assert!(reopened.messages.iter().any(|msg| msg.content == "Review the file"));
    assert_eq!(reopened.items, rows);
}

#[test]
fn auto_compact_retries_plain_chat_after_an_unknown_window_rejects_the_request() {
    use baudet_app::session::tool_turn::ToolTurn;
    use baudet_settings::config::ContextStrategy;
    use baudet_tool::mcp::McpServers;
    use std::sync::atomic::AtomicBool;

    let terminal = |turn_id: &str, failure: Option<&str>| ThreadEvent::TurnCompleted {
        thread_id: "th_1".into(), turn_id: turn_id.into(), response_id: None, usage: None,
        final_event: Some(if failure.is_some() { "response.failed" } else { "response.completed" }.into()),
        incomplete_reason: failure.map(str::to_owned),
    };
    let root = scratch("auto-compact-server-limit");
    let mut summary = answer("tu_3", "The user is reviewing the project.");
    summary.push(terminal("tu_3", None));
    let mut resumed = answer("tu_4", "Continuing the review.");
    resumed.push(terminal("tu_4", None));
    let (backend, state) = persistent(&root, vec![
        answer("tu_1", "Ready"),
        vec![terminal("tu_2", Some("context_length_exceeded"))],
        summary,
        resumed,
    ]);
    run_turn(&state, "Start a project review.");
    let settings = baudet_settings::Settings {
        context_strategy: ContextStrategy::AutoCompact,
        model: Some("unknown-fixture-model".into()),
        ..Default::default()
    };
    let result = state.run_auto_chat("Continue", None, ToolTurn {
        project: None, settings: &settings, servers: &mut McpServers::default(),
        approval: Arc::new(|_| ApprovalDecision::Decline),
        cancelled: Arc::new(AtomicBool::new(false)),
    }).unwrap();
    let (request, _) = backend.last_request().unwrap();
    assert_eq!(backend.turns_started(), 4);
    assert_eq!(request.user_text, "Continue");
    assert!(request.system.contains("The user is reviewing the project."));
    assert!(request.tools.is_none());
    assert_eq!(result, None);
    assert_eq!(state.persist(), None);
    let reopened = state.build_session().unwrap();
    assert_eq!(reopened.messages.iter().filter(|msg| msg.content == "Continue").count(), 1);
}

#[test]
fn stopping_during_auto_compaction_cancels_the_summary_and_keeps_history() {
    use baudet_app::session::tool_turn::ToolTurn;
    use baudet_backend::{BackendError, SendRequest, TurnHandle};
    use baudet_settings::config::ContextStrategy;
    use baudet_tool::mcp::McpServers;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct StalledSummary {
        started: std::sync::mpsc::Sender<()>,
        cancelled: Arc<AtomicBool>,
    }
    impl Backend for StalledSummary {
        fn send(&self, request: SendRequest) -> Result<TurnHandle, BackendError> {
            assert_eq!(request.system, baudet_app::compaction::SUMMARY_PROMPT);
            let (tx, rx) = mpsc::unbounded_channel();
            self.started.send(()).unwrap();
            let cancelled = self.cancelled.clone();
            Ok(TurnHandle::new("summary", rx, |_| {}, move || {
                let _keep_open = &tx;
                cancelled.store(true, Ordering::Relaxed);
            }))
        }
    }

    let root = scratch("auto-compact-stop");
    let (_, state) = persistent(&root, vec![answer("tu_1", "Ready")]);
    let original = "previous context ".repeat(850);
    run_turn(&state, &original);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let handle_cancelled = Arc::new(AtomicBool::new(false));
    state.replace_backend(Arc::new(StalledSummary { started: started_tx, cancelled: handle_cancelled.clone() }));
    let settings = baudet_settings::Settings {
        context_strategy: ContextStrategy::AutoCompact,
        context_budget_tokens: 3_000,
        model: Some("unknown-fixture-model".into()),
        ..Default::default()
    };
    let stop = Arc::new(AtomicBool::new(false));
    std::thread::scope(|scope| {
        let thread_stop = stop.clone();
        let settings = &settings;
        let state = &state;
        let runner = scope.spawn(move || state.run_auto_chat("Continue the review", None, ToolTurn {
            project: None, settings, servers: &mut McpServers::default(),
            approval: Arc::new(|_| ApprovalDecision::Decline), cancelled: thread_stop,
        }));
        let started = started_rx.recv_timeout(std::time::Duration::from_secs(3));
        let showed_progress = state.is_auto_compacting();
        stop.store(true, Ordering::Relaxed);
        assert_eq!(runner.join().unwrap().unwrap().as_deref(), Some("cancelled"));
        assert!(started.is_ok() && showed_progress);
    });
    assert!(handle_cancelled.load(Ordering::Relaxed));
    assert!(!state.is_auto_compacting());
    assert_eq!(state.persist(), None);
    let reopened = state.build_session().unwrap();
    assert!(reopened.summary.is_none());
    assert!(reopened.messages.iter().any(|msg| msg.content == original));
    assert_eq!(reopened.messages.last().unwrap().content, "Continue the review");
}

#[test]
fn plain_read_edit_and_command_calls_use_the_existing_tool_checks() {
    use baudet_app::session::tool_turn::ToolTurn;
    use baudet_projects::{FolderPermission, Permissions, Project};
    use baudet_tool::mcp::McpServers;
    use std::sync::atomic::{AtomicBool, Ordering};

    let root = scratch("tool-turn");
    let read = r#"{"tool":"read_file","arguments":{"file":"example.txt"}}"#;
    let edit = r#"{"tool":"edit_file","arguments":{"file":"example.txt","edits":[{"old_text":"known observation","new_text":"updated"}]}}"#;
    let invalid_command = r#"{"tool":"run_command","arguments":{"timeout_seconds":1}}"#;
    let completed = |turn: &str, text: &str| {
        let mut events = answer(turn, text);
        events.push(ThreadEvent::TurnCompleted {
            thread_id: "th_1".into(), turn_id: turn.into(), response_id: None,
            usage: None, final_event: Some("response.completed".into()), incomplete_reason: None,
        });
        events
    };
    let (backend, state) = persistent(&root, vec![
        completed("tu_1", read), completed("tu_2", edit), completed("tu_3", invalid_command), completed("tu_4", "Read it."),
    ]);
    std::fs::write(root.join("example.txt"), "known observation").unwrap();
    let mut project = Project::new("p-1".into(), "Fixture", 0);
    project.folder_path = Some(root.to_string_lossy().into_owned());
    project.permissions = Permissions::encode([FolderPermission::Read, FolderPermission::Write, FolderPermission::Execute]);
    let settings = baudet_settings::Settings::default();
    let approved = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&approved);
    state.run_tools("Read and update the file", None, ToolTurn {
        project: Some(&project),
        settings: &settings,
        servers: &mut McpServers::default(),
        approval: Arc::new(move |request| match request {
            ApprovalRequest::ApplyPatch { .. } => {
                seen.store(true, Ordering::Relaxed);
                ApprovalDecision::Accept
            }
            _ => ApprovalDecision::Decline,
        }),
        cancelled: Arc::new(AtomicBool::new(false)),
    }).unwrap();
    let (request, _) = backend.last_request().unwrap();
    let tools: serde_json::Value = serde_json::from_str(&request.tools.unwrap()).unwrap();
    let git = tools.as_array().unwrap().iter().find(|tool| tool["name"] == "git").unwrap();
    let verbs = git["parameters"]["properties"]["command"]["enum"].as_array().unwrap();
    assert!(verbs.contains(&serde_json::json!("commit")));
    assert!(approved.load(Ordering::Relaxed));
    assert_eq!(std::fs::read_to_string(root.join("example.txt")).unwrap(), "updated");
    assert!(state.rows().iter().any(|item| matches!(item, Item::CommandExecution(command)
        if command.status == ItemStatus::Failed && command.output.contains("needs a command argument"))));
    assert_eq!(state.live_turns(), 0);
    assert!(state.rows().iter().any(|item| matches!(item, Item::ToolCall(call) if call.output.contains("known observation"))));
    assert_eq!(mentions_exact(&state, "Read it."), 1);
    assert_eq!(state.persist(), None);
    let reopened = state.build_session().unwrap();
    assert_eq!(reopened.messages.last().unwrap().content, "Read it.");
    assert!(reopened.messages.iter().any(|msg| msg.content.contains("known observation")));
}

#[test]
fn plain_git_calls_reach_approval_before_staging_and_committing() {
    use baudet_app::session::tool_turn::ToolTurn;
    use baudet_projects::{FolderPermission, Permissions, Project};
    use baudet_tool::mcp::McpServers;
    use std::process::Command;
    use std::sync::{atomic::AtomicBool, Mutex};

    let root = scratch("plain-git");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let result = Command::new("git").args(args).current_dir(&repo).output().unwrap();
        assert!(result.status.success(), "{args:?}: {}", String::from_utf8_lossy(&result.stderr));
        String::from_utf8(result.stdout).unwrap()
    };
    git(&["init", "-q"]);
    git(&["config", "user.name", "Baudet Test"]);
    git(&["config", "user.email", "baudet@example.invalid"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("README.md"), "Baseline\n").unwrap();
    git(&["add", "README.md"]);
    git(&["commit", "-qm", "Baseline"]);
    std::fs::write(repo.join("README.md"), "Updated README\n").unwrap();

    let add = concat!("Ik ga de README.md toevoegen aan de staging area met de interne git tool.\n\n",
        r#"{"tool": "git", "arguments": {"command": "add", "args": ["README.md"]}}"#);
    let commit = concat!("Ik ga de wijzigingen committen met de interne git tool.\n\n",
        r#"{"tool": "git", "arguments": {"command": "commit", "message": "docs: update README.md"}}"#);
    let scripts = [add, "Git result", add, "Git result", commit, "Git result", commit, "Git result"]
        .into_iter().enumerate().map(|(index, text)| {
            let id = format!("tu_{index}");
            let mut events = answer(&id, text);
            events.push(ThreadEvent::TurnCompleted {
                thread_id: "th_1".into(), turn_id: id, response_id: None, usage: None,
                final_event: Some("response.completed".into()), incomplete_reason: None,
            });
            events
        }).collect();
    let (_, state) = persistent(&root, scripts);
    let mut project = Project::new("p-1".into(), "Fixture", 0);
    project.folder_path = Some(repo.to_string_lossy().into_owned());
    project.permissions = Permissions::encode([FolderPermission::Read, FolderPermission::Write]);
    let settings = baudet_settings::Settings::default();

    for (prompt, verb, decision, staged, head) in [
        ("Stage README", "add", ApprovalDecision::Decline, "", "Baseline"),
        ("Stage README", "add", ApprovalDecision::Accept, "README.md\n", "Baseline"),
        ("Commit README", "commit", ApprovalDecision::Decline, "README.md\n", "Baseline"),
        ("Commit README", "commit", ApprovalDecision::Accept, "", "docs: update README.md"),
    ] {
        let approvals = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&approvals);
        state.run_tools(prompt, None, ToolTurn {
            project: Some(&project),
            settings: &settings,
            servers: &mut McpServers::default(),
            approval: Arc::new(move |request| {
                seen.lock().unwrap().push(request);
                decision.clone()
            }),
            cancelled: Arc::new(AtomicBool::new(false)),
        }).unwrap();
        assert!(matches!(&approvals.lock().unwrap()[..], [ApprovalRequest::RunCommand {
            command, cwd: Some(cwd), always_key: Some(_), ..
        }] if command.starts_with(&format!("git {verb} ")) && cwd == &repo.to_string_lossy()));
        assert_eq!(git(&["diff", "--cached", "--name-only"]), staged);
        assert_eq!(git(&["log", "-1", "--format=%s"]).trim(), head);
    }
    assert_eq!(git(&["show", "HEAD:README.md"]), "Updated README\n");
}

#[test]
fn mcp_echo_fixture() {
    use serde_json::{json, Value};
    use std::io::{BufRead, Write};

    if std::env::var_os("BAUDET_MCP_ECHO_FIXTURE").is_none() {
        return;
    }
    let mut stdout = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let request: Value = serde_json::from_str(&line).unwrap();
        let Some(id) = request.get("id") else { continue };
        let result = match request["method"].as_str().unwrap() {
            "initialize" => json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}}}),
            "tools/list" => json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}),
            "tools/call" => json!({"content":[{"type":"text","text":"fixture observation"}]}),
            method => panic!("unexpected MCP method: {method}"),
        };
        writeln!(stdout, "{}", json!({"jsonrpc":"2.0","id":id,"result":result})).unwrap();
        stdout.flush().unwrap();
    }
}

#[test]
fn mcp_is_advertised_and_approved_without_project_write_access() {
    use baudet_app::session::tool_turn::ToolTurn;
    use baudet_projects::{FolderPermission, Permissions, Project};
    use baudet_settings::{config::ToolMode, Settings};
    use baudet_tool::mcp::McpServers;
    use std::sync::atomic::{AtomicBool, Ordering};

    let root = scratch("mcp-tool-turn");
    let mut project = Project::new("read-only".into(), "Fixture", 0);
    project.folder_path = Some(root.to_string_lossy().into_owned());
    project.permissions = Permissions::encode([FolderPermission::Read]);
    let mut settings = Settings::default();
    settings.mcp_servers.push(baudet_core::mcp::McpServerConfig {
        id: "fixture".into(),
        command: std::env::current_exe().unwrap().to_str().unwrap().into(),
        args: vec!["--exact".into(), "mcp_echo_fixture".into(), "--nocapture".into()],
        env: [("BAUDET_MCP_ECHO_FIXTURE".into(), "1".into())].into_iter().collect(),
        enabled: true,
        timeout_seconds: Some(5),
    });
    let call = r#"{"tool":"mcp__fixture__echo","arguments":{}}"#;
    let completed = |turn_id: &str, text: &str| {
        let mut events = answer(turn_id, text);
        events.push(ThreadEvent::TurnCompleted {
            thread_id: "th_1".into(),
            turn_id: turn_id.into(),
            response_id: None,
            usage: None,
            final_event: Some("response.completed".into()),
            incomplete_reason: None,
        });
        events
    };
    for (name, project) in [("read-only", Some(&project)), ("unassigned", None)] {
        let (backend, state) = persistent(&root.join(name), vec![completed("tu_1", call), completed("tu_2", "Done.")]);
        let approved = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&approved);
        assert!(AppState::tools_active(&settings, project));
        state.run_tools("Call echo", None, ToolTurn {
            project,
            settings: &settings,
            servers: &mut McpServers::default(),
            approval: Arc::new(move |request| match request {
                ApprovalRequest::McpTool { server_id, tool, .. } if server_id == "fixture" && tool == "echo" => {
                    seen.store(true, Ordering::Relaxed);
                    ApprovalDecision::Accept
                }
                _ => ApprovalDecision::Decline,
            }),
            cancelled: Arc::new(AtomicBool::new(false)),
        }).unwrap();
        assert!(approved.load(Ordering::Relaxed));
        assert!(state.rows().iter().any(|item| matches!(item, Item::ToolCall(call) if call.output.contains("fixture observation"))));
        let (request, _) = backend.last_request().unwrap();
        let tools: serde_json::Value = serde_json::from_str(&request.tools.unwrap()).unwrap();
        let names: Vec<_> = tools.as_array().unwrap().iter().map(|tool| tool["name"].as_str().unwrap()).collect();
        if project.is_some() {
            assert_eq!(names, ["read_file", "git", "mcp__fixture__echo"]);
            let verbs = tools[1]["parameters"]["properties"]["command"]["enum"].as_array().unwrap();
            assert!(verbs.contains(&serde_json::json!("status")));
            assert!(!verbs.contains(&serde_json::json!("commit")));
        } else {
            assert_eq!(names, ["mcp__fixture__echo"]);
        }
    }
    settings.tool_mode = ToolMode::Off;
    assert!(!AppState::tools_active(&settings, Some(&project)));
    assert!(!AppState::tools_active(&settings, None));
}
