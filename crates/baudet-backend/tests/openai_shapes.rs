
use baudet_backend::openai::{events_for_payload, request_body, Delta, StreamState, TurnAccumulator, REASONING_NONE};
use baudet_backend::protocol::Protocol;
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
    assert!(asked.get("max_output_tokens").is_none());
    let ceiling = body(baudet_backend::openai::RequestShape { max_output_tokens: Some(4096), ..shape("m", "hi") });
    assert_eq!(ceiling["max_output_tokens"], Value::from(4096));
}

#[test]
fn reasoning_default_sends_nothing_and_a_named_effort_names_it() {
    let plain = body(shape("m", "hi"));
    assert!(plain.get("reasoning").is_none());
    let effort = body(baudet_backend::openai::RequestShape { reasoning: Some("low"), ..shape("m", "hi") });
    assert_eq!(effort["reasoning"]["effort"], "low");
    assert_eq!(effort["reasoning"]["summary"], "auto");
    let summarising =
        body(baudet_backend::openai::RequestShape { reasoning: Some(REASONING_NONE), ..shape("m", "hi") });
    assert_eq!(summarising["reasoning"]["effort"], "none");
    assert!(summarising["reasoning"].get("summary").is_none());
}

#[test]
fn chat_completions_sends_the_selected_reasoning_effort() {
    use baudet_backend::{openai::RequestShape, protocol::chat_request_body};

    assert!(!chat_request_body(&shape("m", "hi")).contains_key("reasoning_effort"));
    for effort in ["minimal", "low", "medium", "high"] {
        let body = chat_request_body(&RequestShape { reasoning: Some(effort), ..shape("m", "hi") });
        assert_eq!(body["reasoning_effort"], effort);
    }
}

#[test]
fn chat_completions_advertises_native_functions() {
    use baudet_backend::{openai::RequestShape, protocol::chat_request_body};

    let tools = serde_json::json!([{
        "type": "function", "name": "generate_qwen_image", "description": "Generate an image",
        "parameters": {"type": "object", "properties": {"prompt": {"type": "string"}}}
    }]);
    let encoded = tools.to_string();
    let request = chat_request_body(&RequestShape { tools: Some(&encoded), ..shape("m", "hi") });
    let mut function = tools[0].clone();
    function.as_object_mut().unwrap().remove("type");
    assert_eq!(request["tools"], serde_json::json!([{"type": "function", "function": function}]));
    assert!(!chat_request_body(&shape("m", "hi")).contains_key("tools"));
}

#[test]
fn chat_completions_assembles_indexed_tool_calls_only_after_completion() {
    let chunks = [
        r#"{"object":"chat.completion.chunk","choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_2","function":{"name":"other","arguments":"{}"}},{"index":0,"id":"call_1","function":{"name":"generate_qwen_image","arguments":"{\"prompt\":"}}]}}]}"#,
        r#"{"object":"chat.completion.chunk","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"elephant\"}"}},{"index":2,"function":{"name":"invalid","arguments":"{"}}]}}]}"#,
    ];
    let mut turn = TurnAccumulator::new();
    let mut interrupted = TurnAccumulator::new();
    for chunk in chunks {
        turn.feed(None, chunk);
        interrupted.feed(None, chunk);
    }
    assert!(interrupted.finish().function_calls.is_empty());
    turn.feed(None, r#"{"object":"chat.completion.chunk","choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#);
    let calls = turn.finish().function_calls;
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].call_id.as_deref(), Some("call_1"));
    assert_eq!(calls[0].name, "generate_qwen_image");
    assert_eq!(calls[0].arguments, r#"{"prompt":"elephant"}"#);
    assert_eq!(calls[1].call_id.as_deref(), Some("call_2"));
}

#[test]
fn each_protocol_can_send_a_thinking_override() {
    use baudet_backend::{openai::RequestShape, protocol::chat_request_body};

    for serialize in [request_body, chat_request_body] {
        assert!(!serialize(&shape("m", "hi")).contains_key("chat_template_kwargs"));
        for (effort, enabled) in [("low", true), (REASONING_NONE, false)] {
            let body = serialize(&RequestShape {
                reasoning: Some(effort),
                enable_thinking: Some(enabled),
                ..shape("m", "hi")
            });
            assert_eq!(body["chat_template_kwargs"]["enable_thinking"], enabled);
        }
    }
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
fn reasoning_delivered_at_the_end_is_rendered_once() {
    for event in [
        r#"{"type":"response.reasoning_summary_text.done","text":"the summary"}"#,
        r#"{"type":"response.completed","response":{"output":[{"type":"reasoning","summary":[{"type":"summary_text","text":"the summary"}]}]}}"#,
    ] {
        let mut turn = TurnAccumulator::new();
        let delta = turn.feed(None, event).unwrap();
        assert_eq!(delta.reasoning.as_deref(), Some("the summary"));
        assert!(turn.feed(None, event).is_none());
        assert_eq!(turn.finish().reasoning, "the summary");
    }
}

#[test]
fn an_unknown_event_is_counted_and_sampled_but_never_fatal() {
    let mut turn = TurnAccumulator::new();
    let delta = turn.feed(None, r#"{"type":"response.output_text.delta","delta":"a"}"#).unwrap();
    assert_eq!(delta.text.as_deref(), Some("a"));
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
fn a_context_failure_preserves_the_window_reported_in_the_message() {
    for event in [
        r#"{"type":"response.failed","error":{"code":"context_length_exceeded","message":"Maximum context length is 8,192 tokens."}}"#,
        r#"{"error":{"code":"context_length_exceeded","message":"Maximum context length is 8,192 tokens."}}"#,
    ] {
        let mut turn = TurnAccumulator::new();
        turn.feed(None, event);
        let failure = turn.finish().incomplete_reason;
        assert_eq!(baudet_core::error::claimed_context_tokens_of(failure.as_deref()), Some(8192));
    }
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

#[test]
fn chat_finish_reason_stop_marks_a_complete_summary() {
    let mut turn = TurnAccumulator::new();
    turn.feed(None, r#"{"object":"chat.completion.chunk","choices":[{"delta":{"content":"Summary"},"finish_reason":null}]}"#);
    turn.feed(None, r#"{"object":"chat.completion.chunk","choices":[{"delta":{},"finish_reason":"stop"}]}"#);
    let finished = turn.finish();
    assert_eq!(finished.text, "Summary");
    assert!(finished.completed());
}

#[test]
fn responses_terminal_output_supplies_text_when_no_text_was_streamed() {
    let terminal = r#"{"type":"response.completed","response":{"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Summary"}]}]}}"#;
    let mut terminal_only = TurnAccumulator::new();
    assert_eq!(terminal_only.feed(None, terminal).unwrap().text.as_deref(), Some("Summary"));
    let completed = terminal_only.finish();
    assert!(completed.completed());
    assert_eq!(completed.text, "Summary");

    let mut streamed = TurnAccumulator::new();
    streamed.feed(None, r#"{"type":"response.output_text.delta","delta":"Summary"}"#);
    assert!(streamed.feed(None, terminal).is_none());
    assert_eq!(streamed.finish().text, "Summary");
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
    assert!(state.close_open_rows("tu_1", baudet_core::ItemStatus::Completed).is_empty());
}

#[test]
fn a_usage_chunk_without_choices_keeps_its_accounting() {
    let mut turn = TurnAccumulator::new();
    turn.feed(None, r#"{"object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"ja"}}]}"#);
    assert!(
        turn.feed(None, r#"{"object":"chat.completion.chunk","choices":[],"usage":{"prompt_tokens":41,"completion_tokens":9,"total_tokens":50}}"#)
            .is_none(),
        "a chunk with no delta must not invent one"
    );
    let finished = turn.finish();
    let usage = finished.usage.expect("the usage chunk carries the turn's accounting");
    assert_eq!((usage.prompt, usage.completion, usage.total), (Some(41), Some(9), Some(50)));
    assert_eq!(finished.text, "ja", "and the answer that streamed stays intact");
}

#[test]
fn a_body_that_ends_at_its_terminator_is_an_ending_not_a_failure() {
    let turn = TurnAccumulator::new();
    assert_eq!(turn.event_count(), 0, "the terminator is not an event");
    let finished = turn.finish();
    assert!(finished.text.is_empty());
    assert_eq!(finished.final_event.as_deref(), Some("stream_end_after_0 events"));
    assert!(finished.incomplete_reason.is_none(), "a clean end is not a failure");
}

#[test]
fn a_settings_word_round_trips_and_an_unknown_word_is_refused() {
    for protocol in [Protocol::Responses, Protocol::ChatCompletions] {
        assert_eq!(Protocol::from_word(protocol.word()), Some(protocol), "{} must round-trip", protocol.word());
    }
    for word in ["", "  ", "Responses", "openai", "chat/completions", "gpt"] {
        assert_eq!(Protocol::from_word(word), None, "{word:?} names no dialect this build can send to");
    }
    assert_eq!(Protocol::ChatCompletions.label(), "chat/completions");
    assert_eq!(Protocol::ChatCompletions.word(), "chat_completions");
}
