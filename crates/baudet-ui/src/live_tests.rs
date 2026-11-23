use super::*;
use baudet_settings::{Settings, SettingsFile};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(5);

fn read_request(socket: &TcpStream) -> (String, String, Value) {
    socket.set_read_timeout(Some(WAIT)).unwrap();
    let mut reader = BufReader::new(socket);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    let mut header = String::new();
    let mut length = None;
    let mut auth = None;
    loop {
        header.clear();
        assert_ne!(reader.read_line(&mut header).unwrap(), 0);
        if header == "\r\n" {
            break;
        }
        let (name, value) = header.split_once(':').unwrap();
        if name.eq_ignore_ascii_case("content-length") {
            length = Some(value.trim().parse().unwrap());
        }
        if name.eq_ignore_ascii_case("authorization") {
            auth = Some(value.trim().to_owned());
        }
    }
    let mut body = vec![0; length.unwrap_or_default()];
    reader.read_exact(&mut body).unwrap();
    let body = if body.is_empty() { Value::Null } else { serde_json::from_slice(&body).unwrap() };
    (request_line, auth.unwrap(), body)
}

fn listen() -> (TcpListener, String) {
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", socket.local_addr().unwrap());
    (socket, url)
}

fn start_stream(socket: &mut TcpStream, data: &str) {
    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {data}\n\n")
        .unwrap();
    socket.flush().unwrap();
}

fn click_settings_button(
    ctx: &egui::Context,
    view: &mut SettingsView,
    commands: &mpsc::Sender<SettingsCommand>,
    label: &str,
) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 1800.0))),
        ..Default::default()
    };
    let output = ctx.run_ui(input.clone(), |ui| settings::body(ui, view, commands));
    let pos = output.shapes.iter().find_map(|shape| match &shape.shape {
        egui::Shape::Text(text) if text.galley.text() == label => Some(text.pos + text.galley.size() / 2.0),
        _ => None,
    });
    output.drop_without_applying_deltas();
    let pos = pos.unwrap_or_else(|| panic!("missing settings button: {label}"));
    for pressed in [true, false] {
        let mut input = input.clone();
        input.events = vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                pressed,
                button: egui::PointerButton::Primary,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        ctx.run_ui(input, |ui| settings::body(ui, view, commands)).drop_without_applying_deltas();
    }
}

#[test]
fn settings_buttons_send_the_environment_key_to_the_endpoint() {
    const ENV_NAME: &str = "BAUDET_SETTINGS_LIVE_AUTH_TEST";
    const TEST_KEY: &str = "settings-fixture-key";
    let (listener, base_url) = listen();
    let server = std::thread::spawn(move || {
        for (expected, data) in [
            ("GET /v1/models ", r#"{"data":[{"id":"fixture-model"}]}"#),
            ("GET /v1/models ", r#"{"data":[{"id":"fixture-model"}]}"#),
            ("POST /v1/chat/completions ", r#"{"choices":[{"message":{"content":"ok"}}]}"#),
        ] {
            let (mut socket, _) = listener.accept().unwrap();
            let (path, auth, _) = read_request(&socket);
            assert!(path.starts_with(expected));
            assert_eq!(auth, format!("Bearer {TEST_KEY}"));
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{data}",
                data.len()
            ).unwrap();
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let file = SettingsFile::at(dir.path().join("settings.json"));
    file.write(&Settings { base_url, api_key_env_var: ENV_NAME.into(), ..Default::default() }).unwrap();
    let mut view = SettingsView::new();
    view.load(&file);
    let (commands, edits) = mpsc::channel();
    let (report, events) = mpsc::channel();
    let worker_file = file.clone();
    let worker = std::thread::spawn(move || settings_worker_loop(edits, report, None, worker_file));
    let ctx = egui::Context::default();

    unsafe { std::env::set_var(ENV_NAME, TEST_KEY) };
    click_settings_button(&ctx, &mut view, &commands, Texts::get(Key::FetchModels));
    let listing = events.recv_timeout(WAIT).unwrap();
    assert!(matches!(&listing, SettingsEvent::Listing(Ok(models), _) if models.len() == 1));
    view.apply_event(listing);
    click_settings_button(&ctx, &mut view, &commands, "Scan endpoint");
    let probe = events.recv_timeout(WAIT);
    unsafe { std::env::remove_var(ENV_NAME) };
    assert!(matches!(probe, Ok(SettingsEvent::Probed(Ok(Some(Protocol::ChatCompletions)), _))));
    assert_eq!(file.read().settings().api_key, None);

    drop(commands);
    worker.join().unwrap();
    server.join().unwrap();
}

fn wait_for_snapshot(snapshots: &mut ampcc::UnboundedReceiver<Snapshot>, matches: impl Fn(&Snapshot) -> bool) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if snapshots.try_recv().is_ok_and(|snapshot| matches(&snapshot)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("expected snapshot did not arrive");
}

#[test]
fn stop_releases_a_silent_stream_and_the_next_turn_uses_the_saved_connection() {
    let (first, base_url) = listen();
    let (second, next_base_url) = listen();
    let (closed, closure) = mpsc::channel();
    let first_server = std::thread::spawn(move || {
        let (mut socket, _) = first.accept().unwrap();
        let (path, auth, body) = read_request(&socket);
        assert!(path.starts_with("POST /v1/responses "));
        assert_eq!(auth, "Bearer first-test-key");
        assert_eq!(body["model"], "first-model");
        assert_eq!(body["instructions"], baudet_settings::DEFAULT_SYSTEM_PROMPT);
        assert_eq!(body["temperature"], 0.3);
        assert!(body.get("top_p").is_none());
        start_stream(&mut socket, r#"{"type":"response.output_text.delta","delta":"partial answer"}"#);
        let disconnected = match socket.read(&mut [0]) {
            Ok(0) => true,
            Err(error) => error.kind() == std::io::ErrorKind::ConnectionReset,
            Ok(_) => false,
        };
        closed.send(disconnected).unwrap();
    });
    let (captured, requests) = mpsc::channel();
    let second_server = std::thread::spawn(move || {
        let (mut socket, _) = second.accept().unwrap();
        captured.send(read_request(&socket)).unwrap();
        start_stream(&mut socket, r#"{"object":"chat.completion.chunk","choices":[{"delta":{"content":"second answer"},"finish_reason":"stop"}]}"#);
    });

    let directory = tempfile::tempdir().unwrap();
    let file = SettingsFile::at(directory.path().join("settings.json"));
    let initial = Settings {
        base_url,
        protocol: Some("responses".into()),
        model: Some("first-model".into()),
        api_key: Some("first-test-key".into()),
        temperature: 0.3,
        ..Default::default()
    };
    file.write(&initial).unwrap();
    let backend = configured_backend(&initial, endpoint_from_settings(&initial).unwrap()).unwrap();
    let state = Arc::new(AppState::new(backend, "test"));
    let catalog = baudet_projects::Catalog::open(directory.path()).unwrap();
    let (intents, commands) = mpsc::channel();
    let (snapshots, mut frames) = ampcc::unbounded_channel();
    let (settings, edits) = mpsc::channel();
    let (report, events) = mpsc::channel();
    let worker_intents = intents.clone();
    let settings_worker = std::thread::spawn(move || settings_worker_loop(edits, report, Some(worker_intents), file));
    let worker_settings = settings.clone();
    let worker_state = Arc::clone(&state);
    let worker_root = directory.path().to_owned();
    let worker = std::thread::spawn(move || {
        worker_loop(worker_state, Some(worker_root), commands, snapshots, Arc::default(), catalog, worker_settings);
    });

    intents.send(Intent::NewSession(None)).unwrap();
    intents.send(Intent::Submit("first question".into(), None)).unwrap();
    wait_for_snapshot(&mut frames, |frame| frame.busy && frame.rows.iter().any(|row| row.body == "partial answer"));
    let mut next = Settings {
        base_url: next_base_url,
        protocol: Some("chat_completions".into()),
        api_key: Some("second-test-key".into()),
        model: Some("second-model".into()),
        temperature: 0.6,
        top_p: Some(0.8),
        ..initial
    };
    settings.send(SettingsCommand::Save(next.clone())).unwrap();
    assert!(matches!(events.recv_timeout(WAIT).unwrap(), SettingsEvent::ConnectionChanged(None)));
    assert!(matches!(events.recv_timeout(WAIT).unwrap(), SettingsEvent::Saved(Ok(()))));
    next.max_output_tokens = Some(123);
    next.system_prompt = "Use the project glossary.\nKeep identifiers intact.".into();
    settings.send(SettingsCommand::Save(next)).unwrap();
    assert!(matches!(events.recv_timeout(WAIT).unwrap(), SettingsEvent::Saved(Ok(()))));
    assert!(matches!(closure.try_recv(), Err(mpsc::TryRecvError::Empty)));

    intents.send(Intent::Stop).unwrap();
    assert!(closure.recv_timeout(Duration::from_millis(500)).unwrap());
    wait_for_snapshot(&mut frames, |frame| !frame.busy);
    assert_eq!(state.disk_items(), Some(2));
    assert!(state.rows().iter().any(|row| {
        row.status() == ItemStatus::Interrupted && baudet_app::reducer::streamed_text(row) == "partial answer"
    }));

    intents.send(Intent::Submit("next question".into(), None)).unwrap();
    let (path, auth, body) = requests.recv_timeout(WAIT).unwrap();
    assert!(path.starts_with("POST /v1/chat/completions "));
    assert_eq!(auth, "Bearer second-test-key");
    assert_eq!(body["model"], "second-model");
    assert_eq!(body["temperature"], 0.6);
    assert_eq!(body["top_p"], 0.8);
    assert_eq!(body["max_tokens"], 123);
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"], "Use the project glossary.\nKeep identifiers intact.");
    assert_eq!(body["messages"][2]["content"], "partial answer");
    wait_for_snapshot(&mut frames, |frame| !frame.busy && frame.rows.iter().any(|row| row.body == "second answer"));
    assert_eq!(state.disk_items(), Some(4));

    intents.send(Intent::Quit).unwrap();
    worker.join().unwrap();
    drop(settings);
    settings_worker.join().unwrap();
    first_server.join().unwrap();
    second_server.join().unwrap();
}
