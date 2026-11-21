//!Wat er met de regel van een beurt gebeurt als de draad ophoudt te praten.
//!
//!Een draad die stopt, is geen beurt die klaar is. De UI kent twee deuren naar
//!dezelfde beurt: de kop telt de live beurten, een regel leest zijn eigen status.
//!Een transport die alleen de beurt afsluit en zijn regels open laat, geeft dus
//!een venster met `idle` boven een antwoord dat voor altijd `working` blijft --
//!gemeten op LM Studio (2026-11-21) nadat de sluit-events in een `let _ =`
//!verdwijnen. Deze test speelt de draad na, zodat die gebeurtenis een keer is
//!vastgelegd in plaats van een keer opgemerkt.

use baudet_backend::protocol::Protocol;
use baudet_backend::transport::{Credential, Endpoint, ResponsesBackend};
use baudet_backend::{Backend, SendRequest};
use baudet_core::{ItemStatus, ThreadEvent};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

/// Een chat-completions stroom zoals LM Studio die stuurt: delta's, een usage-chunk
/// zonder `choices`, `data: [DONE]`, en dan de body dicht. De volgorde is hier de
/// zaak: de sluiting moet úít het einde van de body volgen, niet uit een delta.
fn answer_chat_stream(mut socket: TcpStream) {
    let _ =
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n");
    let _ = socket.flush();
    for piece in ["Een", " antwoord"] {
        let line = format!(
            "data: {{\"object\":\"chat.completion.chunk\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{piece}\"}}}}]}}\n\n"
        );
        if write_chunk(&mut socket, line.as_bytes()).is_err() {
            return;
        }
    }
    let usage = "data: {\"object\":\"chat.completion.chunk\",\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3,\"total_tokens\":10}}\n\n";
    if write_chunk(&mut socket, usage.as_bytes()).is_err() {
        return;
    }
    let _ = write_chunk(&mut socket, b"data: [DONE]\n\n");
    let _ = socket.write_all(b"0\r\n\r\n");
    let _ = socket.flush();
    // De body is dicht; de socket loslaten is hoe een server dat doet.
    let _ = socket.shutdown(std::net::Shutdown::Both);
}

/// Eén HTTP-chunk, zoals een gateway streamt. Zonder die framing heeft het antwoord
/// geen einde en beslist de fixture in plaats van de transport hoeveel er leesbaar is.
fn write_chunk(socket: &mut TcpStream, payload: &[u8]) -> std::io::Result<()> {
    socket.write_all(format!("{:x}\r\n", payload.len()).as_bytes())?;
    socket.write_all(payload)?;
    socket.write_all(b"\r\n")?;
    socket.flush()
}

/// De server van deze test: één poort, elke verbinding krijgt de nagespeelde draad.
/// De `TcpListener` blijft in leven zolang de test loopt -- een gedropte listener
/// Ontbindt de poort en de klant meldt dan een transportfout in plaats van het gedrag
/// waarnaar gemeten wordt.
struct Wire {
    addr: std::net::SocketAddr,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Wire {
    fn start(serve: fn(TcpStream)) -> Wire {
        use std::sync::atomic::{AtomicBool, Ordering};
        let listener = TcpListener::bind("127.0.0.1:0").expect("een vrije poort");
        let addr = listener.local_addr().expect(" een gebonden listener meldt zijn adres");
        listener.set_nonblocking(true).expect(" een listener die gepold kan worden");
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let running = stop.clone();
        let handle = std::thread::spawn(move || {
            while !running.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((socket, _)) => {
                        std::thread::spawn(move || serve(socket));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            }
        });
        Wire { addr, stop, handle: Some(handle) }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// De beurt lezen zoals een venster dat doet: één voor één, tot het kanaal dichtgaat.
fn drain(handle: &mut baudet_backend::TurnHandle) -> Vec<ThreadEvent> {
    let mut events = Vec::new();
    while let Some(event) = handle.events().blocking_recv() {
        events.push(event);
    }
    events
}

#[test]
fn a_stream_that_ends_closes_the_row_it_opened() {
    let wire = Wire::start(answer_chat_stream);
    let endpoint = Endpoint {
        base_url: wire.base_url(),
        credential: Credential::none(),
        protocol: Protocol::ChatCompletions,
        model: "fixture-model".into(),
        store: false,
    };
    let backend = ResponsesBackend::new(endpoint, None).expect("the fixture speaks this dialect");
    let request = SendRequest { system: "sys".into(), user_text: "vraag".into(), ..Default::default() };
    let mut handle = backend.send(request).expect("the turn should reach the fixture");
    let events = drain(&mut handle);

    // De rij werd geopend, dus de transport had een rij om te sluiten. Anders bewijst
    // de onderstaande assert niets: hij zou dan alleen het ontbreken van een sluiting
    // misses taps.
    let opened = events.iter().any(|event| {
        matches!(event, ThreadEvent::ItemStarted { item, .. }
            if item.status() == ItemStatus::InProgress)
    });
    assert!(opened, "de fixture opende een regel die de beurt dicht");

    // De sluiting, en wel vóór de beurt zelf eindigt: de reducer vouwt events in
    // volgorde, en een sluiting ná `TurnCompleted` arriveert te laat voor het beeld.
    let closed_at = events.iter().position(|event| {
        matches!(event, ThreadEvent::ItemCompleted { item, .. }
            if item.status() == ItemStatus::Completed)
    });
    let turn_at = events.iter().position(|event| matches!(event, ThreadEvent::TurnCompleted { .. }));
    assert!(closed_at.is_some(), "de beurt eindigde zonder zijn regel te sluiten: het venster houdt een werkende spinner over een voltooid antwoord");
    assert!(
        closed_at < turn_at,
        "de sluiting moet vóór de beurt-eindiging reizen, anders leest het beeld de beurt als klaar en de regel als werkend"
    );

    // De sluiting draagt de tekst die stroomde niet opnieuw aan: `close_open_rows`
    // sluit een rij met de tekst die het zelf kent, en `ItemCompleted` met lege tekst
    // behoudt in de reducer wat er stond. Hier hoeft alleen de status waar te zijn.
    let completed = events.iter().find_map(|event| match event {
        ThreadEvent::ItemCompleted { item, .. } => Some(item.status()),
        _ => None,
    });
    assert_eq!(completed, Some(ItemStatus::Completed));
}

#[test]
fn a_closed_row_carries_the_turn_it_belongs_to() {
    // Het event moet zeggen welke beurt het sloot: een venster met meer beurten
    // vouwt op turn_id, en een sluiting zonder juist id sluit de verkeerde rij.
    let wire = Wire::start(answer_chat_stream);
    let endpoint = Endpoint {
        base_url: wire.base_url(),
        credential: Credential::none(),
        protocol: Protocol::ChatCompletions,
        model: "fixture-model".into(),
        store: false,
    };
    let backend = ResponsesBackend::new(endpoint, None).expect("the fixture speaks this dialect");
    let mut handle = backend
        .send(SendRequest { system: "sys".into(), user_text: "vraag".into(), ..Default::default() })
        .expect("the turn should reach the fixture");
    let turn_id = handle.turn_id.clone();
    let events = drain(&mut handle);

    let closing_turn = events.iter().find_map(|event| match event {
        ThreadEvent::ItemCompleted { turn_id, .. } => Some(turn_id.clone()),
        _ => None,
    });
    assert_eq!(closing_turn.as_deref(), Some(turn_id.as_str()), "de sluiting hoort de beurt te noemen die hem opende");
}
