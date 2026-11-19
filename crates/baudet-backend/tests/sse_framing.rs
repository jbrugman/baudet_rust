//! Framing intents, translated from `SseFramerSpec`.

use baudet_backend::sse::frame;

#[test]
fn a_blank_line_ends_an_event() {
    let events = frame(["data: one", "", "data: two", ""]);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].data, "one");
    assert_eq!(events[1].data, "two");
}

#[test]
fn several_data_lines_join_with_a_newline() {
    // A payload spanning lines must read as ONE event, or it parses as broken JSON.
    let events = frame(["data: {", "data: \"a\":1}", ""]);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "{\n\"a\":1}");
}

#[test]
fn one_leading_space_after_the_colon_is_stripped_and_no_more() {
    let events = frame(["data:  two spaces", ""]);
    assert_eq!(events[0].data, " two spaces");
}

#[test]
fn the_event_name_travels_with_the_payload() {
    let events = frame(["event: response.completed", "data: {}", ""]);
    assert_eq!(events[0].event.as_deref(), Some("response.completed"));
}

#[test]
fn comments_and_keepalives_are_dropped() {
    let events = frame([": keep-alive", "data: real", ""]);
    assert_eq!(events.len(), 1);
}

#[test]
fn id_and_retry_fields_are_ignored() {
    let events = frame(["id: 7", "retry: 1000", "data: body", ""]);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "body");
    assert_eq!(events[0].event, None);
}

#[test]
fn done_is_not_an_event() {
    let events = frame(["data: [DONE]", ""]);
    assert!(events.is_empty(), "[DONE] is a stream terminator, not a payload");
}

#[test]
fn a_stream_may_end_without_a_trailing_blank_line() {
    let events = frame(["data: last"]);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "last");
}
