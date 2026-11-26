
use crate::tool::{ToolCall, ToolRegistry};
use crate::turn_source::{CallExtractor, FENCE_TAG};
use baudet_core::protocol::Turn;
use serde_json::Value;

fn tagged_blocks(text: &str, tag: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut blocks = Vec::new();
    let mut at = 0usize;
    while at + 3 <= bytes.len() {
        match entry_at(text, bytes, at, tag) {
            Some(body) => {
                let open = body.as_ptr() as usize - text.as_ptr() as usize;
                blocks.push(body.to_string());
                at = open + body.len() + 3;
            }
            None => at += 1,
        }
    }
    blocks
}

fn entry_at<'a>(text: &'a str, bytes: &[u8], at: usize, tag: &str) -> Option<&'a str> {
    if bytes[at] != b'`' || bytes[at + 1] != b'`' || bytes[at + 2] != b'`' {
        return None;
    }
    let after_open = &text[at + 3..];
    let line_end = after_open.find('\n')?;
    if after_open[..line_end].trim_end_matches([' ', '\t']) != tag {
        return None;
    }
    let body_start = at + 3 + line_end + 1;
    let close = text[body_start..].find("```")?;
    Some(&text[body_start..body_start + close])
}

fn calls_from_body(body: &str) -> Vec<ToolCall> {
    let parsed: Value = match serde_json::from_str(body.trim()) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    match parsed {
        Value::Array(entries) => entries.iter().filter_map(call_from).collect(),
        object @ Value::Object(_) => call_from(&object).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn call_from(element: &Value) -> Option<ToolCall> {
    let object = element.as_object()?;
    let name = object.get("tool").and_then(Value::as_str).or_else(|| object.get("name").and_then(Value::as_str))?;
    let arguments = object
        .get("arguments")
        .or_else(|| object.get("args"))
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    Some(ToolCall { tool: name.to_string(), arguments, call_id: None })
}

fn native_arguments(raw: &str) -> Value {
    serde_json::from_str(raw.trim()).ok().filter(Value::is_object).unwrap_or_else(|| Value::Object(Default::default()))
}

fn trailing_tool_call(text: &str) -> Option<ToolCall> {
    let mut lines = text.trim_end().lines().rev();
    let mut paragraph: Vec<_> = lines.by_ref().take_while(|line| !line.trim().is_empty()).collect();
    if !paragraph.last()?.starts_with('{') {
        return None;
    }
    // Keep untagged calls outside Markdown examples, even if a fence is left open.
    if lines.any(|line| {
        let line = line.trim_start();
        line.starts_with("```") || line.starts_with("~~~") || line.starts_with('{')
    }) {
        return None;
    }
    paragraph.reverse();
    let value: Value = serde_json::from_str(&paragraph.join("\n")).ok()?;
    let object = value.as_object()?;
    if object.len() != 2 || !object.get("arguments").or_else(|| object.get("args"))?.is_object() {
        return None;
    }
    call_from(&value)
}

#[derive(Debug, Clone)]
pub struct ExtraTool {
    pub name: String,
    pub description: String,
    pub parameters: String,
}

// Advertise only tools registered for this turn.
pub fn protocol_for(enabled: &[&str], extra: &[ExtraTool], git_writes: bool) -> String {
    let can_read = enabled.contains(&"read_file");
    let can_edit = enabled.contains(&"edit_file");
    let can_run = enabled.contains(&"run_command");
    let can_git = enabled.contains(&"git");
    let mut advertised: Vec<&str> = Vec::new();
    for name in ["read_file", "edit_file", "run_command", "git"] {
        if enabled.contains(&name) {
            advertised.push(name);
        }
    }
    let mut out = String::new();
    out.push_str("You can act on the project only through the tools below, and only with the exact format shown. The only tools available to you are: ");
    let named: Vec<String> = advertised.iter().map(|name| format!("\"{name}\"")).chain(extra.iter().map(|tool| format!("\"{}\"", tool.name))).collect();
    out.push_str(&named.join(", "));
    out.push_str(". Do not describe a tool call or a patch in prose, and never invent another tool name or markup. To act, emit one fenced block tagged ");
    out.push_str(FENCE_TAG);
    out.push_str(" that contains a single JSON object, one object per call.\n\n");

    if can_read {
        out.push_str("Read a file before you change it or reason about it, so you work from the current text:\n");
        out.push_str("```baudet-tool\n{\"tool\": \"read_file\", \"arguments\": {\"file\": \"relative/path\"}}\n```\n\n");
    }

    if can_edit {
        out.push_str("Change a file by replacing text you saw in that read:\n");
        out.push_str("```baudet-tool\n{\"tool\": \"edit_file\", \"arguments\": {\"file\": \"relative/path\", \"edits\": [{\"old_text\": \"...\", \"new_text\": \"...\"}]}}\n```\n\n");
    }
    if can_run {
        out.push_str("Run a shell command in the project folder. The project settings determine whether approval is required.\n");
        out.push_str("```baudet-tool\n{\"tool\": \"run_command\", \"arguments\": {\"command\": \"mvn -B test\"}}\n```\n\n");
    }
    if can_git {
        out.push_str("Use the git tool for repository operations. Inspect with status, diff or log:\n");
        out.push_str("```baudet-tool\n{\"tool\": \"git\", \"arguments\": {\"command\": \"status\"}}\n```\n");
        if git_writes {
            out.push_str("Git writes require approval. Stage and commit with separate calls, waiting for each result:\n");
            out.push_str("```baudet-tool\n{\"tool\": \"git\", \"arguments\": {\"command\": \"add\", \"args\": [\"relative/path\"]}}\n```\n");
            out.push_str("```baudet-tool\n{\"tool\": \"git\", \"arguments\": {\"command\": \"commit\", \"message\": \"Commit message\"}}\n```\n");
        } else {
            out.push_str("Git writes are disabled without Read and Write permissions for this project folder.\n");
        }
        out.push_str("Use the git tool for its supported operations, and do not perform Git writes through the shell.\n\n");
    }

    for tool in extra {
        out.push_str("You may also call an external MCP tool, named and shaped exactly as below; use its advertised name and an arguments object that matches its schema:\n");
        out.push_str(&format!("   Tool name: \"{}\". Argument schema: {}\n", tool.name, tool.parameters));
        out.push_str(&format!("   Purpose: {}\n\n", tool.description));
    }
    out.push_str("Prose may come before or after a block. Each call returns an observation that says ok true or ok false; read it before acting again. On ok false, do not send the same call again unchanged; fix the arguments or answer without a block.");
    if can_read {
        out.push_str(" On ok true from read_file, choose old_text from the text it returned.");
    }
    if can_edit {
        out.push_str(" On ok true from edit_file, either answer or make the next call you still need.");
    }
    if !(can_read || can_edit) {
        out.push_str(" Otherwise either answer or make the next call you still need.");
    }
    if !named.is_empty() {
        out.push_str("\nIf you need a tool, emit the call now. An announcement is not a tool call. ");
        out.push_str("When your answer is complete, write it normally and put ");
        out.push_str(crate::tool_loop::FINAL_DONE);
        out.push_str(" on its own final line. If you cannot complete the request, explain why and put ");
        out.push_str(crate::tool_loop::FINAL_BLOCKED);
        out.push_str(" on its own final line. Do not use a final marker when making a tool call.");
    }
    out
}

pub struct PromptedCallExtractor;

impl CallExtractor for PromptedCallExtractor {
    fn extract(&self, turn: &Turn, registry: &ToolRegistry) -> Vec<ToolCall> {
        let mut calls: Vec<ToolCall> = turn
            .function_calls
            .iter()
            .enumerate()
            .map(|(index, call)| ToolCall {
                tool: call.name.clone(),
                arguments: native_arguments(&call.arguments),
                call_id: Some(call.call_id.clone().unwrap_or_else(|| format!("native-{index}"))),
            })
            .collect();

        calls.extend(tagged_blocks(&turn.text, FENCE_TAG).iter().flat_map(|body| calls_from_body(body)));
        if calls.is_empty() {
            calls.extend(trailing_tool_call(&turn.text).filter(|call| registry.get(&call.tool).is_some()));
        }
        calls
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baudet_core::protocol::FunctionCall;
    use serde_json::json;

    fn extract(turn: &Turn) -> Vec<ToolCall> {
        let registry = ToolRegistry::new(vec![Box::new(crate::git_tool::GitTool::with_writes())]);
        PromptedCallExtractor.extract(turn, &registry)
    }

    fn prose(text: impl Into<String>) -> Turn {
        Turn::text(text)
    }

    fn call_of(calls: &[ToolCall], index: usize) -> (&str, &Value, Option<&str>) {
        (calls[index].tool.as_str(), &calls[index].arguments, calls[index].call_id.as_deref())
    }

    #[test]
    fn a_tagged_block_becomes_a_call() {
        let turn = prose(concat!(
            "Ik lees het eerst.\n",
            "```baudet-tool\n",
            "{\"tool\": \"read_file\", \"arguments\": {\"file\": \"a.txt\"}}\n",
            "```\n",
            "Daarna zie ik verder.",
        ));
        let calls = extract(&turn);
        assert_eq!(calls.len(), 1);
        assert_eq!(call_of(&calls, 0), ("read_file", &json!({"file": "a.txt"}), None));
    }

    #[test]
    fn prose_around_a_block_is_ignored_and_several_blocks_keep_their_order() {
        let turn = prose(concat!(
            "```baudet-tool\n{\"tool\": \"git\", \"arguments\": {\"command\": \"status\"}}\n```\n",
            "en dan\n",
            "```baudet-tool\n{\"name\": \"read_file\", \"args\": {\"file\": \"b.txt\"}}\n```\n",
        ));
        let calls = extract(&turn);
        assert_eq!(calls.len(), 2, "naam-als-`name` en argumenten-als-`args`Tellers doen mee, zoals in de origineler");
        assert_eq!(calls[0].tool, "git");
        assert_eq!(call_of(&calls, 1), ("read_file", &json!({"file": "b.txt"}), None));
    }

    #[test]
    fn an_array_block_is_a_fan_out() {
        let turn = prose(concat!(
            "```baudet-tool\n",
            "[{\"tool\": \"read_file\", \"arguments\": {\"file\": \"a\"}}, {\"tool\": \"read_file\", \"arguments\": {\"file\": \"b\"}}]\n",
            "```\n",
        ));
        let calls = extract(&turn);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].arg_str("file"), Some("b"));
    }

    #[test]
    fn a_block_that_will_not_parse_is_skipped_not_thrown() {
        let broken = prose("```baudet-tool\n{\"tool\": \"read_file\", \n```\n");
        assert!(extract(&broken).is_empty());

        let mixed = prose(concat!(
            "```baudet-tool\n{kapot\n```\n",
            "```baudet-tool\n{\"tool\": \"git\", \"arguments\": {\"command\": \"log\"}}\n```\n",
        ));
        let calls = extract(&mixed);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arg_str("command"), Some("log"));
    }

    #[test]
    fn ordinary_code_is_not_a_call() {
        let turn = prose("```rust\nlet x = \"{\\\"tool\\\": \\\"read_file\\\"}\";\n```\n");
        assert!(extract(&turn).is_empty());
        assert!(extract(&prose("```baudet-tool\n{\"tool\": \"git\"}\n")).is_empty());
    }

    #[test]
    fn an_untagged_call_must_name_a_registered_tool_in_the_final_standalone_object() {
        let git = r#"{"tool":"git","arguments":{"command":"add","args":["README.md"]}}"#;
        let committed = r#"{"tool":"git","arguments":{"command":"commit","message":"docs: update README.md"}}"#;
        let calls = extract(&prose(format!("I will stage it.\n\n{git}")));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments["args"][0], "README.md");
        assert_eq!(extract(&prose(committed))[0].arg_str("message"), Some("docs: update README.md"));
        for text in [
            format!("> {git}"),
            format!("> example\n{git}"),
            format!("    {git}"),
            format!("```json\n\n{git}"),
            format!("~~~json\n\n{git}\n~~~"),
            format!("{git}\n\n{committed}"),
            format!("{git}\n\nHere is an example."),
            r#"{"tool":"run_command","arguments":{"command":"git add README.md"}}"#.to_owned(),
        ] {
            assert!(extract(&prose(&text)).is_empty(), "{text}");
        }
        let indented_json = "{\n  \"name\": \"git\",\n  \"args\": {\"command\": \"status\"}\n}";
        assert_eq!(extract(&prose(indented_json))[0].arg_str("command"), Some("status"));
        let tagged = format!("```baudet-tool\n{git}\n```\n\n{committed}");
        assert_eq!(extract(&prose(tagged)).len(), 1);
        let mut native = prose(committed);
        native.function_calls.push(FunctionCall { call_id: None, name: "git".into(), arguments: "{}".into() });
        assert_eq!(extract(&native).len(), 1);
    }

    #[test]
    fn a_tag_with_trailing_spaces_is_not_a_block() {
        assert_eq!(tagged_blocks("```baudet-tool  \n{}\n```\n", FENCE_TAG), vec!["{}\n".to_string()]);
        assert_eq!(tagged_blocks("```baudet-tool\t\n{}\n```\n", FENCE_TAG), vec!["{}\n".to_string()]);
        assert!(tagged_blocks("```baudet-tool\r\n{}\n```\n", FENCE_TAG).is_empty(), "\\r staat vóór de \\n");
        assert!(tagged_blocks("```\n{}\n```\n", FENCE_TAG).is_empty());
        assert!(tagged_blocks("``baudet-tool\n{}\n```\n", FENCE_TAG).is_empty());
    }

    #[test]
    fn a_near_miss_tag_cannot_hide_the_next_block() {
        let text = "```baudet ```baudet-tool\n{\"tool\": \"git\", \"arguments\": {\"command\": \"show\"}}\n```\n";
        let calls = extract(&prose(text));
        assert_eq!(calls.len(), 1, "het echte blok mag niet verloren gaan");
        assert_eq!(calls[0].arg_str("command"), Some("show"));
    }

    #[test]
    fn native_calls_come_first_and_win() {
        let mut turn =
            prose("```baudet-tool\n{\"tool\": \"read_file\", \"arguments\": {\"file\": \"uit de tekst\"}}\n```\n");
        turn.function_calls = vec![FunctionCall {
            call_id: Some("call_1".into()),
            name: "git".into(),
            arguments: "{\"command\": \"status\"}".into(),
        }];
        let calls = extract(&turn);
        assert_eq!(calls.len(), 2, "beide kanalen worden geboden, native eerst");
        assert_eq!(call_of(&calls, 0), ("git", &json!({"command": "status"}), Some("call_1")));
        assert_eq!(calls[1].arg_str("file"), Some("uit de tekst"));
    }

    #[test]
    fn a_native_call_without_an_id_gets_a_stable_one() {
        let mut turn = prose("");
        turn.function_calls = vec![FunctionCall { call_id: None, name: "read_file".into(), arguments: "{}".into() }];
        let calls = extract(&turn);
        assert_eq!(calls[0].call_id.as_deref(), Some("native-0"));
    }

    #[test]
    fn unparseable_native_arguments_become_an_empty_object() {
        let mut turn = prose("");
        turn.function_calls =
            vec![FunctionCall { call_id: None, name: "read_file".into(), arguments: "geen json".into() }];
        let calls = extract(&turn);
        assert_eq!(calls[0].arguments, json!({}));
        turn.function_calls[0].arguments = "[1,2]".into();
        assert_eq!(extract(&turn)[0].arguments, json!({}));
    }

    #[test]
    fn prose_before_a_block_survives_the_scan() {
        let accented = "\u{00e9}```baudet-tool\n{\"tool\": \"git\"}\n```\n";
        assert_eq!(extract(&prose(accented)).len(), 1);
        let many = "\u{00e9}".repeat(4) + "```baudet-tool\n{\"tool\": \"git\"}\n```\n";
        assert_eq!(extract(&prose(many)).len(), 1);
        assert!(tagged_blocks("`", FENCE_TAG).is_empty());
        assert!(tagged_blocks("\t\t", FENCE_TAG).is_empty());
    }

    #[test]
    fn four_backticks_are_not_a_longer_fence() {
        assert_eq!(tagged_blocks("```baudet-tool\n{}\n````", FENCE_TAG), vec!["{}\n".to_string()]);
        assert_eq!(tagged_blocks("```baudet-tool\n{}\n``````", FENCE_TAG), vec!["{}\n".to_string()]);
        assert_eq!(tagged_blocks("````baudet-tool\n{}\n```", FENCE_TAG), vec!["{}\n".to_string()]);
    }

    #[test]
    fn a_block_without_a_name_is_not_a_call() {
        for body in ["{}", "{\"arguments\": {\"file\": \"a\"}}", "\"{\"tool\": \"read_file\"}\""] {
            let turn = prose(format!("```baudet-tool\n{body}\n```\n"));
            assert!(extract(&turn).is_empty(), "{body}");
        }
    }

    #[test]
    fn the_protocol_names_exactly_the_enabled_tools() {
        let only_read = protocol_for(&["read_file"], &[], false);
        assert!(only_read.contains("\"read_file\""), "{only_read}");
        for absent in ["edit_file", "run_command", "git"] {
            assert!(!only_read.contains(absent), "een tool die er niet is, mag niet aangeboden worden: {absent}");
        }
        assert!(only_read.contains("On ok true from read_file, choose old_text from the text it returned."));

        let editing = protocol_for(&["read_file", "edit_file"], &[], false);
        assert!(editing.contains("\"edit_file\""));
        assert!(editing.contains("\"old_text\""));
        assert!(editing.contains("On ok true from edit_file, either answer or make the next call you still need."));
        assert!(!editing.contains("run_command"), "uitgeschreven rechten blijven weg uit de prompt");

        let edit_without_read = protocol_for(&["edit_file"], &[], false);
        assert!(edit_without_read.contains("\"edit_file\""), "aanwezig blijft aangeboden: {edit_without_read}");
        assert!(!edit_without_read.contains("\"read_file\""), "afwezig blijft verzwegen: {edit_without_read}");
        assert!(!edit_without_read.contains("old_text from the text it returned"), "geen advies over een afwezige tool");
        assert!(edit_without_read.contains("On ok true from edit_file, either answer or make the next call you still need."));

        let empty = protocol_for(&[], &[], false);
        for tool in ["read_file", "edit_file", "run_command", "git"] {
            assert!(!empty.contains(&format!("\"{tool}\"")), "ledige registry adverteert niets: {tool}");
        }
        assert!(!empty.contains(crate::tool_loop::FINAL_DONE));
        assert!(empty.contains(FENCE_TAG), "de vorm blijft, ook zonder tools");

        let full = protocol_for(&["read_file", "edit_file", "run_command", "git"], &[], false);
        for tool in ["read_file", "edit_file", "run_command", "git"] {
            assert!(full.contains(&format!("\"{tool}\"")), "{tool} mist in {full}");
        }
        let listing = full.split(". Do not describe").next().expect("één opsomming");
        assert!(listing.find("\"read_file\"") < listing.find("\"edit_file\""));
        assert!(listing.find("\"run_command\"") < listing.find("\"git\""));

        for text in [&only_read, &editing, &full] {
            assert!(text.contains(FENCE_TAG), "de prompt moet de tag noemen die deze module leest");
        }
        assert!(editing.contains("{\"tool\": \"edit_file\""));
        assert!(full.contains("{\"tool\": \"run_command\""));
        assert!(full.contains("{\"tool\": \"git\""));
    }

    #[test]
    fn git_prompt_matches_the_write_permission() {
        let read_only = protocol_for(&["git", "run_command"], &[], false);
        assert!(read_only.contains("Git writes are disabled"));
        assert!(!read_only.contains("\"command\": \"commit\""));

        let writable = protocol_for(&["git", "run_command"], &[], true);
        assert!(writable.contains("Git writes require approval"));
        assert!(writable.contains("\"command\": \"add\""));
        assert!(writable.contains("\"command\": \"commit\""));
        assert!(writable.contains("do not perform Git writes through the shell"));
    }

    #[test]
    fn an_extra_tool_is_advertised_with_its_schema() {
        let extra = ExtraTool {
            name: "mcp__time__now".to_string(),
            description: "Current time".to_string(),
            parameters: "{\"type\":\"object\"}".to_string(),
        };
        let text = protocol_for(&["read_file"], &[extra], false);
        assert!(text.contains("\"mcp__time__now\""), "ook een extra tool moet geadverteerd worden: {text}");
        assert!(text.contains("Purpose: Current time"));
        assert!(text.contains("{\"type\":\"object\"}"), "een prompted backend leest de schema uit de prompt");
        assert!(text.contains("{\"tool\": \"read_file\""));
    }
}
