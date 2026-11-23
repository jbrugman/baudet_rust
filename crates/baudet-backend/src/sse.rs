
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

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
        } else if let Some(value) = line.strip_prefix("data:") {
            if have_data {
                data.push('\n');
            }
            data.push_str(strip_field(value));
            have_data = true;
        } else if let Some(value) = line.strip_prefix("event:") {
            event = Some(strip_field(value).to_string());
        }
    }
    if have_data && !data.is_empty() && data != "[DONE]" {
        events.push(SseEvent { event, data });
    }
    events
}

fn strip_field(value: &str) -> &str {
    value.strip_prefix(' ').unwrap_or(value)
}

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

    pub fn push_line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.take_event();
        }
        if line.starts_with(':') {
        } else if let Some(value) = line.strip_prefix("data:") {
            if self.have_data {
                self.data.push('\n');
            }
            self.data.push_str(strip_field(value));
            self.have_data = true;
        } else if let Some(value) = line.strip_prefix("event:") {
            self.event = Some(strip_field(value).to_string());
        }
        None
    }

    pub fn finish(&mut self) -> Option<SseEvent> {
        self.take_event()
    }

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
        assert!(framer.has_pending());
        assert_eq!(framer.push_line(""), Some(SseEvent { event: None, data: "half".into() }));
        assert!(!framer.has_pending());
    }
}
