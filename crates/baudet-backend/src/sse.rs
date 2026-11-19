//! SSE framing, translated 1:1 from `dev.baudet.api.SseFramer`.
//!
//! Framing happens on the blank line rather than per data line, and that is the whole point: a
//! payload that spans several lines would otherwise read as several events and parse as broken
//! JSON. Pure over a line sequence, so it is unit-testable without a socket -- which is also why
//! the [`crate::scripted`] player runs its scripts through this same code instead of handing the
//! interface prettily bundled deltas.

/// One framed event: the optional `event:` name and the joined `data:` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Frame `lines` the way the spec requires.
pub fn frame<I, S>(lines: I) -> Vec<SseEvent>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut events = Vec::new();
    let mut event: Option<String> = None;
    let mut data = String::new();
    let mut have_data = false;

    for line in lines {
        let line = line.as_ref();
        if line.is_empty() {
            if have_data && !data.is_empty() && data != "[DONE]" {
                events.push(SseEvent { event: event.take(), data: std::mem::take(&mut data) });
            }
            event = None;
            data.clear();
            have_data = false;
        } else if line.starts_with(':') {
            // comment or keep-alive
        } else if let Some(value) = line.strip_prefix("data:") {
            if have_data {
                data.push('\n');
            }
            data.push_str(strip_field(value));
            have_data = true;
        } else if let Some(value) = line.strip_prefix("event:") {
            event = Some(strip_field(value).to_string());
        }
        // id, retry, unknown field: ignored.
    }
    // A stream may end without a trailing blank line; that last event is real.
    if have_data && !data.is_empty() && data != "[DONE]" {
        events.push(SseEvent { event, data });
    }
    events
}

fn strip_field(value: &str) -> &str {
    value.strip_prefix(' ').unwrap_or(value)
}
