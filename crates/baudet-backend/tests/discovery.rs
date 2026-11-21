//! Finding a server on this machine, and what a listing is allowed to name in a probe.
//!
//! Both rules here were measured against real servers (2026-11-21) and both are about cost, not
//! style: a probe names a model a server may have to page in, and a sweep that asks HTTP questions of
//! closed ports pays a timeout per port. `scan` and `discover` take their network work as injected
//! closures, so these tests hold the decision without holding a socket.

use baudet_backend::protocol::{discover, judge_probe, likely_chat_model, scan, Discovery, Protocol, ScanOutcome};

/// An outcome a caller could have got back, so a sweep test only has to say which endpoint it was.
fn outcome(endpoint: &str, protocol: Option<Protocol>) -> ScanOutcome {
    ScanOutcome {
        endpoint: endpoint.to_string(),
        protocol,
        models: vec!["m".into()],
        notes: vec![format!("looked at {endpoint}")],
    }
}

#[test]
fn a_listing_of_embedders_alone_is_not_something_to_probe() {
    // Measured: LM Studio lists `text-embedding-nomic-embed-text-v1.5` beside its gemma, and the
    // gateway lists `bge-m3-mlx-fp16`, `chatterbox-*` and `whisper-*`. Naming one of those in a probe
    // spends a request that cannot answer, and on a server that pages models in, a load that
    // displaces the resident one.
    assert!(likely_chat_model("google/gemma-4-26b-a4b-qat"));
    assert!(!likely_chat_model("text-embedding-nomic-embed-text-v1.5"));
    assert!(!likely_chat_model("bge-m3-mlx-fp16"));
    assert!(!likely_chat_model("chatterbox-multilingual-v3"));
    assert!(!likely_chat_model("whisper-large-v3"));

    let named = std::cell::RefCell::new(Vec::new());
    let outcome = scan(
        "http://127.0.0.1:9/v1",
        || Ok(vec!["bge-m3-mlx-fp16".into(), "whisper-large-v3".into()]),
        |_, model, _| {
            named.borrow_mut().push(model.to_string());
            (Some(Protocol::ChatCompletions), "answered".into())
        },
    );
    let named = named.into_inner();
    assert!(named.is_empty(), "a listing with nothing to ask must not probe at all, named {named:?}");
    assert!(!outcome.usable());
    assert!(
        outcome.notes.iter().any(|n| n.contains("no model that can answer a turn")),
        "the notes must say it was the listing's shape: {:?}",
        outcome.notes
    );
}

#[test]
fn a_probe_names_a_chat_model_even_when_an_embedder_is_listed_first() {
    // Order in a listing is not a statement about usefulness. `models.first()` was the bug this
    // protects: it would have named the embedder merely because that server happened to list it first.
    let named = std::cell::RefCell::new(String::new());
    let outcome = scan(
        "http://127.0.0.1:9/v1",
        || Ok(vec!["text-embedding-nomic-embed-text-v1.5".into(), "google/gemma-4-26b-a4b-qat".into()]),
        |_, model, _| {
            *named.borrow_mut() = model.to_string();
            (Some(Protocol::ChatCompletions), "answered 200 with a chat body".into())
        },
    );
    assert_eq!(*named.borrow(), "google/gemma-4-26b-a4b-qat");
    assert_eq!(outcome.protocol, Some(Protocol::ChatCompletions));
}

#[test]
fn a_closed_candidate_is_answered_without_a_request() {
    // The gate exists because of a measurement: eighteen HTTP requests to closed ports cost 36 s on
    // this machine, while the same ports answered `refused` at the socket layer in 0.00 s.
    let requests = std::cell::Cell::new(0);
    let discovery = discover(
        &["http://127.0.0.1:9/v1".into()],
        |_| false,
        |_| {
            requests.set(requests.get() + 1);
            outcome("unused", None)
        },
    );
    assert_eq!(requests.get(), 0, "a port with nothing listening on it must not be asked anything");
    assert!(!discovery.usable());
    assert!(discovery.tried[0].notes[0].contains("nothing is listening"));
}

#[test]
fn a_sweep_stops_at_the_first_server_that_answers() {
    // Every probe names a model a server may page in, so candidates after the winner must cost
    // nothing. This is what makes a sweep safe to run against a machine holding several local servers.
    let asked = std::cell::RefCell::new(Vec::new());
    let candidates = vec![
        "http://127.0.0.1:1234/v1".to_string(),
        "http://127.0.0.1:8000/v1".to_string(),
        "http://127.0.0.1:11434/v1".to_string(),
    ];
    let discovery = discover(
        &candidates,
        |endpoint| {
            asked.borrow_mut().push(endpoint.to_string());
            true
        },
        |endpoint| outcome(endpoint, Some(Protocol::ChatCompletions)),
    );
    let asked = asked.into_inner();
    assert_eq!(asked, vec!["http://127.0.0.1:1234/v1"], "the sweep ran past its winner: {asked:?}");
    assert!(discovery.usable());
    assert_eq!(discovery.found.as_ref().expect("a winner").endpoint, "http://127.0.0.1:1234/v1");
}

#[test]
fn a_sweep_that_finds_nothing_still_says_what_it_saw() {
    // "No server" alone tells a person nothing to fix. Each candidate's own notes survive the sweep.
    let discovery = discover(
        &["http://127.0.0.1:1234/v1".to_string(), "http://127.0.0.1:8000/v1".to_string()],
        |_| true,
        |endpoint| outcome(endpoint, None),
    );
    assert!(!discovery.usable());
    assert_eq!(discovery.tried.len(), 2, "every candidate is reported, not just a winner");
    assert!(discovery.tried.iter().all(|o| o.notes[0].starts_with("looked at")));
    let _: &Discovery = &discovery;
}

#[test]
fn a_refusal_and_an_answer_are_told_apart_by_the_body_not_the_status() {
    // Both halves are measured: the gateway answers /v1/responses with 404 + {"error": …}, LM Studio
    // with 200 + {"object":"response", "status":"completed"}. A status-code test would call the
    // gateway a Responses server and LM Studio a refusal -- exactly backwards.
    let refusal = serde_json::json!({"error": "Model 'x' not found"});
    let (found, _) = judge_probe(Protocol::Responses, 404, &refusal);
    assert_eq!(found, None, "a 404 error body is not a responses answer");
    let answer = serde_json::json!({"id": "resp_1", "object": "response", "status": "completed"});
    let (found, _) = judge_probe(Protocol::Responses, 200, &answer);
    assert_eq!(found, Some(Protocol::Responses));
}

#[test]
fn a_refusal_on_the_chat_path_is_read_the_same_way() {
    // De andere helft van dezelfde meting (2026-11-21, gateway): `POST /v1/chat/completions` met een
    // model dat de server niet heeft, geeft 404 + {"error": ...}. De scan noemt áltijd een model uit
    // de listing van dezelfde server, dus dit is precies wat een 404-op-de-chat-pad daar betekent:
    // geen dialect, en de notitie moet de words van de server houden in plaats van een gok.
    let refusal = serde_json::json!({"error": "Model 'x' not found"});
    let (found, note) = judge_probe(Protocol::ChatCompletions, 404, &refusal);
    assert_eq!(found, None, "een weigering noemt geen dialect");
    assert!(note.contains("Model 'x' not found"), "de notitie verliest de melding niet: {note}");
}
