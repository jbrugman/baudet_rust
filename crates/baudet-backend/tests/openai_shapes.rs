//! The wire shape and the delta mapping, tested against payloads rather than against a server.
//!
//! These are the two places where a "works on my machine" backend hides: what a request names its
//! fields, and which event name carries a delta. Both are tested as pure data, because a transport
//! that decides them would hide its guesses behind whichever server happened to be up.

use baudet_backend::openai::{events_for_payload, request_body, Delta, StreamState, TurnAccumulator, REASONING_NONE};
use baudet_core::{ItemKind, ThreadEvent};
use serde_json::Value;

fn body(shape: baudet_backend::openai::RequestShape<'_>) -> Value {
    Value::Object(request_body(&shape))
}

fn shape<'a>(model: &'a str, user_text: &'a str) -> baudet_backend::openai::RequestShape<'a> {
    baudet_backend::openai::RequestShape { model, user_text, ..Default::default() }
}

#[test]
fn a_turn_always_streams_and_never_sends_a_zero_ceiling() {
    let asked = body(shape("m", "hi"));
    assert_eq!(asked["stream"], Value::Bool(true));
    // Sending 0 is not the same as sending nothing: it caps the answer at nothing. So the field is
    // absent unless a real ceiling was asked for.
    assert!(asked.get("max_output_tokens").is_none());
    let ceiling = body(baudet_backend::openai::RequestShape { max_output_tokens: Some(4096), ..shape("m", "hi") });
    assert_eq!(ceiling["max_output_tokens"], Value::from(4096));
}

#[test]
fn reasoning_default_sends_nothing_and_a_named_effort_names_it() {
    // The panel's "default" must not be a silent "none": a server that reasons by default keeps
    // doing that, which is the behaviour the label promises.
    let plain = body(shape("m", "hi"));
    assert!(plain.get("reasoning").is_none());
    let effort = body(baudet_backend::openai::RequestShape { reasoning: Some("low"), ..shape("m", "hi") });
    assert_eq!(effort["reasoning"]["effort"], "low");
    let summarising =
        body(baudet_backend::openai::RequestShape { reasoning: Some(REASONING_NONE), ..shape("m", "hi") });
    assert_eq!(summarising["reasoning"]["effort"], "none");
}

#[test]
fn a_chained_turn_drops_the_replayed_history_because_the_server_has_it() {
    let history = vec![baudet_core::Msg::new("user", "earlier"), baudet_core::Msg::new("assistant", "before")];
    let unchained = body(baudet_backend::openai::RequestShape { history: &history, ..shape("m", "now") });
    let replayed: Vec<&str> =
        unchained["input"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(replayed, ["user", "assistant", "user"], "history is replayed, newest last");

    let chained = body(baudet_backend::openai::RequestShape {
        history: &history,
        previous_response_id: Some("resp_1"),
        ..shape("m", "now")
    });
    assert_eq!(chained["input"].as_array().unwrap().len(), 1, "only this turn's text");
    assert_eq!(chained["previous_response_id"], "resp_1");
}

#[test]
fn the_system_rides_as_instructions_once_rather_than_in_every_replayed_message() {
    let history = vec![baudet_core::Msg::new("system", "be brief"), baudet_core::Msg::new("user", "earlier")];
    let asked =
        body(baudet_backend::openai::RequestShape { system: "be brief", history: &history, ..shape("m", "hi") });
    assert_eq!(asked["instructions"], "be brief");
    let roles: Vec<&str> = asked["input"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["user", "user"], "instructions already carry the system turn");
}

#[test]
fn an_image_rides_as_content_parts_next_to_the_text() {
    let asked =
        body(baudet_backend::openai::RequestShape { image: Some("data:image/png;base64,AA"), ..shape("m", "what") });
    let parts = asked["input"].as_array().unwrap().last().unwrap()["content"].as_array().unwrap().clone();
    let kinds: Vec<&str> = parts.iter().map(|p| p["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["input_text", "input_image"]);
    assert_eq!(parts[1]["image_url"], "data:image/png;base64,AA");
}

#[test]
fn a_tools_string_that_is_not_an_array_is_left_out_rather_than_sent_half_formed() {
    // A server rejecting the whole body over one malformed definition costs the turn as well.
    let broken = body(baudet_backend::openai::RequestShape { tools: Some("{\"nope\":1}"), ..shape("m", "hi") });
    assert!(broken.get("tools").is_none());
    let good =
        body(baudet_backend::openai::RequestShape { tools: Some(r#"[{"type":"function"}]"#), ..shape("m", "hi") });
    assert_eq!(good["tools"].as_array().unwrap().len(), 1);
}

#[test]
fn a_delta_is_read_from_the_field_the_api_specifies() {
    let mut turn = TurnAccumulator::new();
    let delta = turn.feed(None, r#"{"type":"response.output_text.delta","delta":"Hel"}"#).unwrap();
    assert_eq!(delta.text.as_deref(), Some("Hel"));
    let reasoning = turn.feed(None, r#"{"type":"response.reasoning_summary_text.delta","delta":"think"}"#).unwrap();
    assert_eq!(reasoning.reasoning.as_deref(), Some("think"));
    let finished = turn.finish();
    assert_eq!(finished.text, "Hel");
    assert_eq!(finished.reasoning, "think");
}

#[test]
fn the_event_field_carries_the_name_when_the_payload_has_none() {
    // Measured: some local servers put the name nowhere but the `event:` line, and dropping those
    // payloads is how an answer silently becomes an empty one.
    let mut turn = TurnAccumulator::new();
    let delta = turn.feed(Some("response.output_text.delta"), r#"{"delta":"lo"}"#).unwrap();
    assert_eq!(delta.text.as_deref(), Some("lo"));
}

#[test]
fn a_chat_completion_chunk_is_read_through_its_choices() {
    let mut turn = TurnAccumulator::new();
    let delta =
        turn.feed(None, r#"{"object":"chat.completion.chunk","choices":[{"delta":{"content":"Hi"}}]}"#).unwrap();
    assert_eq!(delta.text.as_deref(), Some("Hi"));
    // A role-only opening chunk carries nothing and must not invent a delta.
    assert!(turn
        .feed(None, r#"{"object":"chat.completion.chunk","choices":[{"delta":{"role":"assistant"}}]}"#)
        .is_none());
}

#[test]
fn reasoning_by_another_name_is_still_reasoning() {
    let mut turn = TurnAccumulator::new();
    let delta = turn.feed(None, r#"{"type":"response.reasoning_text.delta","delta":"musing"}"#).unwrap();
    assert_eq!(delta.reasoning.as_deref(), Some("musing"));
}

#[test]
fn an_unknown_event_is_counted_and_sampled_but_never_fatal() {
    let mut turn = TurnAccumulator::new();
    let delta = turn.feed(None, r#"{"type":"response.output_text.delta","delta":"a"}"#).unwrap();
    assert_eq!(delta.text.as_deref(), Some("a"));
    // A dialect nobody predicted must not end the turn; the census is what tells the story.
    assert!(turn.feed(None, r#"{"type":"response.mystery.thing","x":1}"#).is_none());
    turn.feed(None, "not json at all");
    let finished = turn.finish();
    let names: Vec<&str> = finished.events.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["response.output_text.delta", "response.mystery.thing", "(not-json)"]);
    assert_eq!(finished.samples.iter().find(|(name, _)| name == "(not-json)").unwrap().1, "not json at all");
    assert_eq!(finished.text, "a", "an unknown event costs at most a delta, not the answer");
}

#[test]
fn usage_is_read_in_both_vocabularies() {
    // Measured on LM Studio: input_tokens/output_tokens with *_tokens_details beside them.
    let mut responses = TurnAccumulator::new();
    responses.feed(None, r#"{"type":"response.completed","response":{"id":"resp_9","usage":{"input_tokens":15,"output_tokens":0,"total_tokens":15}}}"#);
    let finished = responses.finish();
    assert_eq!(finished.response_id.as_deref(), Some("resp_9"));
    let usage = finished.usage.unwrap();
    assert_eq!((usage.prompt, usage.completion, usage.total), (Some(15), Some(0), Some(15)));

    let mut chat = TurnAccumulator::new();
    chat.feed(
        None,
        r#"{"type":"response.completed","usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}"#,
    );
    let usage = chat.finish().usage.unwrap();
    assert_eq!((usage.prompt, usage.completion, usage.total), (Some(7), Some(3), Some(10)));
}

#[test]
fn a_truncation_is_named_rather_than_left_as_silence() {
    // "No answer" and "budget exhausted before the answer started" look identical unless the reason
    // is surfaced, and only one of them is fixed by raising the ceiling.
    let mut turn = TurnAccumulator::new();
    turn.feed(None, r#"{"type":"response.incomplete","incomplete_details":{"reason":"max_output_tokens"}}"#);
    let finished = turn.finish();
    assert_eq!(finished.incomplete_reason.as_deref(), Some("max_output_tokens"));
    assert_eq!(finished.final_event.as_deref(), Some("response.incomplete"));
    assert!(!finished.completed(), "an incomplete response is not a completion");
}

#[test]
fn a_terminal_event_without_a_reason_still_names_the_error_code() {
    let mut turn = TurnAccumulator::new();
    turn.feed(None, r#"{"type":"response.failed","error":{"code":"server_error","message":"boom"}}"#);
    let finished = turn.finish();
    assert_eq!(finished.incomplete_reason.as_deref(), Some("server_error"));
}

#[test]
fn a_stream_that_only_reports_an_error_says_so() {
    let mut turn = TurnAccumulator::new();
    turn.feed(None, r#"{"type":"error","error":{"message":"model is loading"}}"#);
    let finished = turn.finish();
    assert!(finished.text.is_empty());
    assert_eq!(
        finished.incomplete_reason.as_deref(),
        Some(r#"{"error":{"message":"model is loading"},"type":"error"}"#)
    );
}

#[test]
fn a_finished_text_adopted_without_deltas_counts_once() {
    // The tolerant fallback: some servers skip deltas. Adopting the done payload when text already
    // streamed would print the answer twice.
    let mut silent = TurnAccumulator::new();
    silent.feed(None, r#"{"type":"response.output_text.done","text":"whole answer"}"#);
    assert_eq!(silent.finish().text, "whole answer");

    let mut streamed = TurnAccumulator::new();
    streamed.feed(None, r#"{"type":"response.output_text.delta","delta":"whole "}"#);
    streamed.feed(None, r#"{"type":"response.output_text.delta","delta":"answer"}"#);
    streamed.feed(None, r#"{"type":"response.output_text.done","text":"whole answer"}"#);
    assert_eq!(streamed.finish().text, "whole answer");
}

#[test]
fn a_function_call_is_collected_once_whatever_shape_it_arrives_in() {
    // The finished response carries calls in its output array, and some servers also surface each
    // item on its own done event. Reading both must not double-count.
    let mut turn = TurnAccumulator::new();
    turn.feed(
        None,
        r#"{"type":"response.output_item.done","item":{"type":"function_call","call_id":"c1","name":"read","arguments":"{\"p\":\"x\"}"}}"#,
    );
    turn.feed(
        None,
        r#"{"type":"response.completed","response":{"output":[{"type":"function_call","call_id":"c1","name":"read","arguments":"{\"p\":\"x\"}"}]}}"#,
    );
    let finished = turn.finish();
    assert_eq!(finished.function_calls.len(), 1);
    assert_eq!(finished.function_calls[0].name, "read");
    assert_eq!(finished.function_calls[0].call_id.as_deref(), Some("c1"));
}

#[test]
fn a_call_without_an_id_is_deduped_by_name_and_arguments() {
    let mut turn = TurnAccumulator::new();
    let item = r#"{"type":"response.output_item.done","item":{"type":"function_call","name":"read","arguments":"{}"}}"#;
    turn.feed(None, item);
    turn.feed(None, item);
    assert_eq!(turn.finish().function_calls.len(), 1);
}

#[test]
fn a_stream_that_closed_is_reported_as_such_rather_than_as_a_completion() {
    let mut turn = TurnAccumulator::new();
    turn.feed(None, r#"{"type":"response.output_text.delta","delta":"half"}"#);
    let finished = turn.finish();
    assert_eq!(finished.final_event.as_deref(), Some("stream_end_after_1 events"));
    assert!(!finished.completed());
}

fn item_id_of(event: &ThreadEvent) -> Option<&str> {
    match event {
        ThreadEvent::ItemDelta { item_id, .. } => Some(item_id),
        _ => None,
    }
}

#[test]
fn one_answer_row_and_one_reasoning_row_open_whatever_the_server_splits() {
    let mut state = StreamState::new("th_1");
    let mut turn = TurnAccumulator::new();
    let mut events = Vec::new();
    for payload in [
        r#"{"type":"response.reasoning_summary_text.delta","delta":"th"}"#,
        r#"{"type":"response.reasoning_summary_text.delta","delta":"ink"}"#,
        r#"{"type":"response.output_text.delta","delta":"He"}"#,
        r#"{"type":"response.output_text.delta","delta":"llo"}"#,
    ] {
        if let Some(delta) = turn.feed(None, payload) {
            events.extend(events_for_payload(&mut state, "tu_1", &delta));
        }
    }
    let started = events.iter().filter(|e| matches!(e, ThreadEvent::ItemStarted { .. })).count();
    let deltas = events.iter().filter(|e| matches!(e, ThreadEvent::ItemDelta { .. })).count();
    assert_eq!((started, deltas), (2, 4), "two rows announced once, four deltas");
    let ids: Vec<&str> = events.iter().filter_map(item_id_of).collect();
    assert_eq!(ids, ["tu_1/reasoning", "tu_1/reasoning", "tu_1/msg", "tu_1/msg"]);
    // The kinds must agree with their rows: a reasoning delta rendered as an answer is a lie.
    for event in &events {
        if let ThreadEvent::ItemDelta { kind, .. } = event {
            let expected =
                if item_id_of(event) == Some("tu_1/reasoning") { ItemKind::Reasoning } else { ItemKind::AgentMessage };
            assert_eq!(*kind, expected);
        }
    }
}

#[test]
fn closing_rows_names_the_status_the_turn_ended_with() {
    let mut state = StreamState::new("th_1");
    events_for_payload(&mut state, "tu_1", &Delta { text: Some("x".into()), reasoning: Some("y".into()) });
    assert!(state.any_open());
    let closed = state.close_open_rows("tu_1", baudet_core::ItemStatus::Interrupted);
    assert_eq!(closed.len(), 2);
    assert!(!state.any_open());
    for event in &closed {
        match event {
            ThreadEvent::ItemCompleted { item, .. } => assert_eq!(item.status(), baudet_core::ItemStatus::Interrupted),
            other => panic!("closing rows must complete, not delta: {other:?}"),
        }
    }
    // Closing twice must not announce rows that are already closed.
    assert!(state.close_open_rows("tu_1", baudet_core::ItemStatus::Completed).is_empty());
}
