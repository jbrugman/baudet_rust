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

/// Frame a whole line sequence at once, the way the spec requires.
///
/// Kept as the shape the scripted player and the framing tests use. A live body cannot offer a
/// sequence, so it drives [`Framer`] line by line instead -- through the very same state machine,
/// which is the reason both exist: a wire and a script must not be framed by two sets of rules.
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

/// The same framing as [`frame`], one line at a time, for a body that arrives as it is read.
///
/// The state is the method's locals made explicit: an open `event:` name, the data joined so far,
/// and whether a `data:` line was seen at all. `push_line` returns an event exactly when a
/// complete one is bounded, so a caller never has to know where a payload started; `finish` closes
/// a last event the stream never terminated with a blank line, which the spec allows and a server
/// that hangs up does routinely.
#[derive(Debug, Default)]
pub struct Framer {
    event: Option<String>,
    data: String,
    have_data: bool,
}

impl Framer {
    pub fn new() -> Framer {
        Framer::default()
    }

    /// Feed one wire line. A complete event is returned as soon as its blank line is seen.
    pub fn push_line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.take_event();
        }
        if line.starts_with(':') {
            // comment or keep-alive
        } else if let Some(value) = line.strip_prefix("data:") {
            if self.have_data {
                self.data.push('\n');
            }
            self.data.push_str(strip_field(value));
            self.have_data = true;
        } else if let Some(value) = line.strip_prefix("event:") {
            self.event = Some(strip_field(value).to_string());
        }
        // id, retry, unknown field: ignored.
        None
    }

    /// The stream ended. A trailing event without its blank line is still real.
    pub fn finish(&mut self) -> Option<SseEvent> {
        self.take_event()
    }

    /// Whether a payload is in progress, so an abandoned stream can say so instead of guessing.
    pub fn has_pending(&self) -> bool {
        self.have_data
    }

    fn take_event(&mut self) -> Option<SseEvent> {
        let event = if self.have_data && !self.data.is_empty() && self.data != "[DONE]" {
            Some(SseEvent { event: self.event.take(), data: std::mem::take(&mut self.data) })
        } else {
            None
        };
        self.event = None;
        self.data.clear();
        self.have_data = false;
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reason [`Framer`] exists must be provable: it may not be a second implementation of the
    /// rules that happens to agree today. Same lines, same events.
    #[test]
    fn an_incremental_framer_agrees_with_the_batch_one() {
        let lines = [
            ": keep-alive",
            "event: response.output_text.delta",
            "data: {\"a\":1}",
            "",
            "id: 7",
            "data: {",
            "data:   \"b\":2}",
            "",
            "data: [DONE]",
            "",
            "event: trailing",
            "data: last",
        ];
        let batch = frame(lines);
        let mut incremental = Vec::new();
        let mut framer = Framer::new();
        for line in lines {
            if let Some(event) = framer.push_line(line) {
                incremental.push(event);
            }
        }
        incremental.extend(framer.finish());
        assert_eq!(batch, incremental);
    }

    #[test]
    fn a_payload_is_reported_only_when_its_line_arrives() {
        let mut framer = Framer::new();
        assert_eq!(framer.push_line("data: half"), None);
        // The payload is in progress; a stream abandoned here is a truncated answer, not silence.
        assert!(framer.has_pending());
        assert_eq!(framer.push_line(""), Some(SseEvent { event: None, data: "half".into() }));
        assert!(!framer.has_pending());
    }
}
