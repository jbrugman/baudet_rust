
use baudet_backend::protocol::{discover, judge_probe, likely_chat_model, scan, Discovery, Protocol, ScanOutcome};

fn outcome(endpoint: &str, protocol: Option<Protocol>) -> ScanOutcome {
    ScanOutcome {
        endpoint: endpoint.to_string(),
        protocols: protocol.into_iter().collect(),
        models: vec!["m".into()],
        notes: vec![format!("looked at {endpoint}")],
    }
}

#[test]
fn a_listing_of_embedders_alone_is_not_something_to_probe() {
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
    let named = std::cell::RefCell::new(String::new());
    let outcome = scan(
        "http://127.0.0.1:9/v1",
        || Ok(vec!["text-embedding-nomic-embed-text-v1.5".into(), "google/gemma-4-26b-a4b-qat".into()]),
        |dialect, model, _| {
            *named.borrow_mut() = model.to_string();
            (Some(dialect), "answered".into())
        },
    );
    assert_eq!(*named.borrow(), "google/gemma-4-26b-a4b-qat");
    assert_eq!(outcome.protocols, [Protocol::Responses, Protocol::ChatCompletions]);
}

#[test]
fn a_closed_candidate_is_answered_without_a_request() {
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
fn a_responses_probe_requires_a_successful_response_body() {
    let refusal = serde_json::json!({"error": "Model 'x' not found"});
    let (found, _) = judge_probe(Protocol::Responses, 404, &refusal);
    assert_eq!(found, None, "a 404 error body is not a responses answer");
    let answer = serde_json::json!({"id": "resp_1", "object": "response", "status": "completed"});
    let (found, _) = judge_probe(Protocol::Responses, 200, &answer);
    assert_eq!(found, Some(Protocol::Responses));
    assert_eq!(judge_probe(Protocol::Responses, 500, &answer).0, None);
    assert_eq!(judge_probe(Protocol::Responses, 200, &serde_json::json!({"id":"chatcmpl_1"})).0, None);
}

#[test]
fn a_refusal_on_the_chat_path_is_read_the_same_way() {
    let refusal = serde_json::json!({"error": "Model 'x' not found"});
    let (found, note) = judge_probe(Protocol::ChatCompletions, 404, &refusal);
    assert_eq!(found, None, "een weigering noemt geen dialect");
    assert!(note.contains("Model 'x' not found"), "de notitie verliest de melding niet: {note}");
}
