
use crate::call_extractor::PromptedCallExtractor;
use crate::tool::{ToolCall, ToolContext, ToolRegistry, ToolResult, ToolRunner, Undo};
use crate::turn_source::{CallExtractor, TurnRequest, TurnSource};
use baudet_core::error::CONTEXT_LIMIT_REASON;
use baudet_core::item::{
    CommandExecutionItem, FileChangeItem, FileChangeKind, ItemKind, ItemStatus, ToolCallItem,
};
use baudet_core::msg::Msg;
use baudet_core::protocol::Turn;
use serde_json::Value;
use std::sync::mpsc;
use std::thread;

const MAX_PARALLEL_TOOL_CALLS: usize = 2;

const MAX_BLANK_TURNS: u32 = 2;

const MAX_OBSERVATION_CHARS: usize = 32_000;

const TRUNCATION_SKELETON: &str = "\n\n[... output truncated: keeping the first 0000 and last 0000 of 000000 characters; the full output stays in the transcript ...]\n\n";

const MAX_ITERATIONS_REASON: &str = "max_tool_iterations";

pub const BLANK_NUDGE: &str = "Your previous turn returned neither an answer nor a tool call. If you intend to use a tool, emit the call now; otherwise reply with your final answer.";
const ACTION_NUDGE: &str = "Use a tool now if an action is still needed. Otherwise provide the answer or explain what blocked you, and end with the prescribed final marker.";
const MISSING_COMPLETION: &str = "The model did not provide a tool call or mark its final answer.";
pub const FINAL_DONE: &str = "[[BAUDET_DONE]]";
pub const FINAL_BLOCKED: &str = "[[BAUDET_BLOCKED]]";
const FINAL_MARKERS: [&str; 2] = [FINAL_DONE, FINAL_BLOCKED];

pub fn is_retry_nudge(message: &Msg) -> bool {
    message.role == "user" && matches!(message.content.as_str(), BLANK_NUDGE | ACTION_NUDGE)
}

fn strip_final_marker(text: &mut String) -> bool {
    let Some((answer, marker)) = text.trim_end().rsplit_once('\n') else {
        if FINAL_MARKERS.contains(&text.trim()) {
            text.clear();
        }
        return false;
    };
    if !FINAL_MARKERS.contains(&marker.trim()) {
        return false;
    }
    let answer = answer.trim_end();
    let valid = !is_blankish(answer);
    text.truncate(answer.len());
    valid
}

// A marker can cross stream chunks. Hold only the suffix that could start one.
fn visible_delta(chunk: &str, pending: &mut String) -> String {
    pending.push_str(chunk);
    for marker in FINAL_MARKERS {
        if pending.contains(marker) {
            *pending = pending.replace(marker, "");
        }
    }
    let keep = FINAL_MARKERS.iter().map(|marker| {
        (1..marker.len()).rev().find(|&length| pending.ends_with(&marker[..length])).unwrap_or(0)
    }).max().unwrap_or(0);
    let rest = pending.split_off(pending.len() - keep);
    std::mem::replace(pending, rest)
}

fn is_blankish(text: &str) -> bool {
    text.chars().all(|c| c.is_whitespace() || c == '.' || c == '\u{2026}')
}

pub fn cap_model_text(text: &str, max_chars: usize) -> String {
    let chars = text.chars().count();
    if max_chars == 0 || chars <= max_chars {
        return text.to_string();
    }
    let budget = max_chars.saturating_sub(TRUNCATION_SKELETON.chars().count());
    let head = budget * 3 / 5;
    let tail = budget - head;
    let head_bytes = text.char_indices().nth(head).map(|(i, _)| i).unwrap_or(text.len());
    let tail_bytes = text.char_indices().nth(chars - tail).map(|(i, _)| i).unwrap_or(text.len());
    let marker = format!(
        "\n\n[... output truncated: keeping the first {head} and last {tail} of {chars} characters; the full output stays in the transcript ...]\n\n"
    );
    format!("{}{}{}", &text[..head_bytes], marker, &text[tail_bytes..])
}

fn estimate_tokens_of(messages: &[Msg]) -> u64 {
    messages.iter().map(|msg| baudet_store::cost_chars(msg) as u64).sum::<u64>() / 3
}

fn detail_of(arguments: &Value) -> String {
    ["file", "command"].iter().filter_map(|key| arguments.get(*key).and_then(Value::as_str)).next().unwrap_or_default().to_string()
}

fn timeout_of(arguments: &Value) -> Option<i32> {
    match arguments.get("timeout_seconds")? {
        Value::Number(number) => number.as_i64().and_then(|v| i32::try_from(v).ok()),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn observation(call: &ToolCall, result: &ToolResult) -> String {
    let mut text = String::from("Tool observation");
    if let Some(id) = &call.call_id {
        text.push_str(" for call ");
        text.push_str(id);
    }
    text.push_str(": `");
    text.push_str(&call.tool);
    text.push('`');
    text.push_str(if result.ok { " succeeded (ok=true): " } else { " failed (ok=false): " });
    text.push_str(&result.output);
    if !result.ok {
        text.push_str(" Do not send the same call again unchanged; correct it or answer.");
    }
    if let Some(diff) = &result.diff {
        let fence = "\u{60}\u{60}\u{60}";
        text.push('\n');
        text.push_str(fence);
        text.push_str("diff\n");
        text.push_str(diff);
        text.push('\n');
        text.push_str(fence);
    }
    cap_model_text(&text, MAX_OBSERVATION_CHARS)
}

pub trait ItemSink: Send + Sync {
    // Images are display data. Only their text description enters model history.
    fn image_completed(&self, _item: baudet_core::ImageItem) {}
    fn next_item_id(&self) -> String;
    fn reply_started(&self) {}
    fn reply_completed(&self, _text: &str) {}
    fn text_delta(&self, _chunk: &str) {}
    fn reasoning_delta(&self, _chunk: &str) {}
    fn command_completed(&self, _item: CommandExecutionItem) {}
    fn file_change_completed(&self, _item: FileChangeItem) {}
    fn tool_call_completed(&self, _item: ToolCallItem) {}


    fn command_started(&self, _item: CommandExecutionItem) {}
    fn file_change_started(&self, _item: FileChangeItem) {}
    fn tool_call_started(&self, _item: ToolCallItem) {}
}

fn started_command_card(item_id: String, call: &ToolCall) -> CommandExecutionItem {
    CommandExecutionItem {
        item_id,
        command: call.arg_str("command").unwrap_or_default().to_string(),
        cwd: None,
        timeout_seconds: timeout_of(&call.arguments),
        status: ItemStatus::InProgress,
        exit_code: None,
        timed_out: false,
        duration_ms: 0,
        output: String::new(),
        approval: None,
    }
}

fn started_file_change_card(item_id: String, call: &ToolCall) -> FileChangeItem {
    FileChangeItem {
        item_id,
        path: call.arg_str("file").unwrap_or_default().to_string(),
        change: FileChangeKind::Update,
        status: ItemStatus::InProgress,
        diff: None,
        additions: 0,
        deletions: 0,
    }
}

fn started_tool_call_card(item_id: String, call: &ToolCall) -> ToolCallItem {
    ToolCallItem {
        item_id,
        tool: call.tool.clone(),
        detail: detail_of(&call.arguments),
        status: ItemStatus::InProgress,
        ok: false,
        output: String::new(),
    }
}

fn command_card(item_id: String, call: &ToolCall, result: &ToolResult) -> CommandExecutionItem {
    CommandExecutionItem {
        item_id,
        command: call.arg_str("command").unwrap_or_default().to_string(),
        cwd: result.cwd.clone(),
        timeout_seconds: timeout_of(&call.arguments),
        status: if result.ok { ItemStatus::Completed } else { ItemStatus::Failed },
        exit_code: result.exit_code,
        timed_out: result.timed_out,
        duration_ms: result.duration_ms as i64,
        output: result.output.clone(),
        approval: None,
    }
}

fn file_change_card(item_id: String, call: &ToolCall, result: &ToolResult) -> FileChangeItem {
    let stats = baudet_core::item::DiffStats::of(result.diff.as_deref());
    FileChangeItem {
        item_id,
        path: call.arg_str("file").unwrap_or_default().to_string(),
        change: FileChangeKind::Update,
        status: if result.ok { ItemStatus::Completed } else { ItemStatus::Failed },
        diff: result.diff.clone(),
        additions: stats.additions,
        deletions: stats.deletions,
    }
}

fn tool_call_card(item_id: String, call: &ToolCall, result: &ToolResult) -> ToolCallItem {
    ToolCallItem {
        item_id,
        tool: call.tool.clone(),
        detail: detail_of(&call.arguments),
        status: if result.ok { ItemStatus::Completed } else { ItemStatus::Failed },
        ok: result.ok,
        output: result.output.clone(),
    }
}

pub struct NullSink;

impl ItemSink for NullSink {
    fn next_item_id(&self) -> String {
        String::new()
    }
}

#[derive(Debug)]
pub struct ToolStep {
    pub assistant_text: String,
    pub calls: Vec<ToolCall>,
    pub results: Vec<ToolResult>,
}

pub enum LoopOutcome {
    Completed {
        final_text: String,
        final_turn: Turn,
        response_id: Option<String>,
        steps: Vec<ToolStep>,
        messages: Vec<Msg>,
        undos: Vec<Undo>,
        cap_reached: bool,
    },
    CapReached {
        response_id: Option<String>,
        steps: Vec<ToolStep>,
        messages: Vec<Msg>,
        undos: Vec<Undo>,
        incomplete_reason: String,
    },
}

impl std::fmt::Debug for LoopOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoopOutcome::Completed { final_text, response_id, steps, messages, undos, cap_reached, .. } => f
                .debug_struct("Completed")
                .field("final_text", final_text)
                .field("response_id", response_id)
                .field("steps", steps)
                .field("messages", messages)
                .field("undos", &undos.len())
                .field("cap_reached", cap_reached)
                .finish(),
            LoopOutcome::CapReached { response_id, steps, messages, undos, incomplete_reason } => f
                .debug_struct("CapReached")
                .field("response_id", response_id)
                .field("steps", steps)
                .field("messages", messages)
                .field("undos", &undos.len())
                .field("incomplete_reason", incomplete_reason)
                .finish(),
        }
    }
}

impl LoopOutcome {
    pub fn messages(&self) -> &[Msg] {
        match self {
            LoopOutcome::Completed { messages, .. } | LoopOutcome::CapReached { messages, .. } => messages,
        }
    }

    pub fn undos(&self) -> &[Undo] {
        match self {
            LoopOutcome::Completed { undos, .. } | LoopOutcome::CapReached { undos, .. } => undos,
        }
    }

    pub fn response_id(&self) -> Option<&str> {
        match self {
            LoopOutcome::Completed { response_id, .. } | LoopOutcome::CapReached { response_id, .. } => {
                response_id.as_deref()
            }
        }
    }
}

fn prompt_tokens_of(turn: &Turn) -> Option<u64> {
    turn.usage.as_ref().and_then(|usage| usage.prompt).map(|prompt| prompt as u64)
}

pub struct LoopRun<'a> {
    pub history: &'a [Msg],
    pub ctx: &'a ToolContext,
    pub previous_response_id: Option<&'a str>,
    pub image: Option<&'a str>,
    pub on_delta: Box<dyn FnMut(&str) + Send + 'a>,
    pub on_reasoning: Box<dyn FnMut(&str) + Send + 'a>,
    pub on_message: Box<dyn FnMut(&Msg) + Send + 'a>,
    pub sink: &'a dyn ItemSink,
}

fn boxed<'a, F>(f: F) -> Box<dyn FnMut(&str) + Send + 'a>
where
    F: FnMut(&str) + Send + 'a,
{
    Box::new(f)
}

// Its borrowed context stays inside scoped workers.
struct SharedContext<'a>(&'a ToolContext);

// Parallel calls must leave callbacks untouched. Only tools that declare themselves safe to run in parallel enter this path.
unsafe impl Send for SharedContext<'_> {}
unsafe impl Sync for SharedContext<'_> {}

pub struct ToolLoop<'a> {
    turn_source: &'a dyn TurnSource,
    registry: &'a ToolRegistry,
    extractor: &'a dyn CallExtractor,
    runner: ToolRunner<'a>,
    pub max_iterations: u32,
    pub chain_response_id: bool,
    pub context_ceiling: u64,
    require_completion: bool,
}

impl<'a> ToolLoop<'a> {
    pub fn new(turn_source: &'a dyn TurnSource, registry: &'a ToolRegistry, max_iterations: u32) -> ToolLoop<'a> {
        ToolLoop::with_extractor(turn_source, registry, &PromptedCallExtractor, max_iterations)
    }

    pub fn with_extractor(
        turn_source: &'a dyn TurnSource,
        registry: &'a ToolRegistry,
        extractor: &'a dyn CallExtractor,
        max_iterations: u32,
    ) -> ToolLoop<'a> {
        ToolLoop {
            turn_source,
            registry,
            extractor,
            runner: ToolRunner::new(registry),
            max_iterations: max_iterations.max(1),
            chain_response_id: false,
            context_ceiling: 0,
            require_completion: false,
        }
    }

    pub fn chain_response_id(mut self, on: bool) -> ToolLoop<'a> {
        self.chain_response_id = on;
        self
    }

    pub fn context_ceiling(mut self, ceiling: u64) -> ToolLoop<'a> {
        self.context_ceiling = ceiling;
        self
    }

    pub fn require_completion(mut self, enabled: bool) -> Self {
        self.require_completion = enabled;
        self
    }

    pub async fn run(&self, run: &mut LoopRun<'_>) -> LoopOutcome {
        let mut msgs: Vec<Msg> = run.history.to_vec();
        let mut added: Vec<Msg> = Vec::new();
        let mut steps: Vec<ToolStep> = Vec::new();
        let mut undos: Vec<Undo> = Vec::new();
        let mut prev = if self.chain_response_id { run.previous_response_id.map(str::to_string) } else { None };
        let tools: Option<Value> = if self.registry.is_empty() { None } else { Some(self.registry.definitions()) };
        let mut pending_image = run.image;
        let mut call_steps = 0u32;
        let mut blanks = 0u32;
        let explicit_completion = self.require_completion && tools.is_some();
        let mut retried_completion = false;

        for iteration in 0..=self.max_iterations.saturating_add(u32::from(explicit_completion)) {
            if self.context_ceiling > 0 && estimate_tokens_of(&msgs) > self.context_ceiling {
                return LoopOutcome::CapReached {
                    response_id: prev,
                    steps,
                    messages: added,
                    undos,
                    incomplete_reason: CONTEXT_LIMIT_REASON.to_string(),
                };
            }
            if iteration - u32::from(retried_completion) == self.max_iterations {
                break;
            }
            run.sink.reply_started();
            let mut turn = self.ask(run, &mut prev, &msgs, tools.as_ref(), &mut pending_image).await;
            let final_answer = explicit_completion && strip_final_marker(&mut turn.text);
            run.sink.reply_completed(&turn.text);
            if self.chain_response_id && turn.response_id.is_some() {
                prev = turn.response_id.clone();
            }
            let calls = if turn.incomplete_reason.is_none() { self.extractor.extract(&turn, self.registry) } else { Vec::new() };

            if calls.is_empty() {
                if explicit_completion && !final_answer && turn.incomplete_reason.is_none() {
                    if !retried_completion {
                        retried_completion = true;
                        if !is_blankish(&turn.text) {
                            self.add_assistant(run, &mut msgs, &mut added, &turn);
                        }
                        msgs.push(Msg::user(ACTION_NUDGE));
                        continue;
                    }
                    turn.incomplete_reason = Some(MISSING_COMPLETION.to_owned());
                }
                if !explicit_completion && turn.incomplete_reason.is_none() && is_blankish(&turn.text)
                    && blanks < MAX_BLANK_TURNS
                {
                    blanks += 1;
                    msgs.push(Msg::user(BLANK_NUDGE));
                    continue;
                }
                self.add_assistant(run, &mut msgs, &mut added, &turn);
                steps.push(ToolStep { assistant_text: turn.text.clone(), calls: Vec::new(), results: Vec::new() });
                return LoopOutcome::Completed {
                    final_text: turn.text.clone(),
                    final_turn: turn.clone(),
                    response_id: turn.response_id.clone(),
                    steps,
                    messages: added,
                    undos,
                    cap_reached: call_steps >= self.max_iterations,
                };
            }

            self.add_assistant(run, &mut msgs, &mut added, &turn);
            let results = self.run_calls(run, &calls, &mut msgs, &mut added, &mut undos).await;
            steps.push(ToolStep { assistant_text: turn.text, calls, results });
            call_steps += 1;
        }

        run.sink.reply_started();
        let mut settle = self.ask(run, &mut prev, &msgs, tools.as_ref(), &mut pending_image).await;
        let final_answer = explicit_completion && strip_final_marker(&mut settle.text);
        run.sink.reply_completed(&settle.text);
        if self.chain_response_id && settle.response_id.is_some() {
            prev = settle.response_id.clone();
        }
        self.add_assistant(run, &mut msgs, &mut added, &settle);
        if settle.incomplete_reason.is_some() || self.extractor.extract(&settle, self.registry).is_empty() {
            if explicit_completion && !final_answer && settle.incomplete_reason.is_none() {
                settle.incomplete_reason = Some(MISSING_COMPLETION.to_owned());
            }
            steps.push(ToolStep { assistant_text: settle.text.clone(), calls: Vec::new(), results: Vec::new() });
            return LoopOutcome::Completed {
                final_text: settle.text.clone(),
                final_turn: settle.clone(),
                response_id: settle.response_id.clone(),
                steps,
                messages: added,
                undos,
                cap_reached: true,
            };
        }
        LoopOutcome::CapReached {
            response_id: prev,
            steps,
            messages: added,
            undos,
            incomplete_reason: MAX_ITERATIONS_REASON.to_string(),
        }
    }

    async fn ask<'r>(
        &self,
        run: &'r mut LoopRun<'_>,
        prev: &'r mut Option<String>,
        msgs: &'r [Msg],
        tools: Option<&'r Value>,
        pending_image: &'r mut Option<&'a str>,
    ) -> Turn {
        let sink = run.sink;
        let on_delta = &mut *run.on_delta;
        let on_reasoning = &mut *run.on_reasoning;
        let explicit_completion = self.require_completion && tools.is_some();
        let mut pending_marker = String::new();
        let request = TurnRequest {
            history: msgs,
            previous_response_id: prev.as_deref(),
            tools,
            on_delta: boxed(move |chunk: &str| {
                if explicit_completion {
                    let visible = visible_delta(chunk, &mut pending_marker);
                    if !visible.is_empty() {
                        sink.text_delta(&visible);
                        on_delta(&visible);
                    }
                } else {
                    sink.text_delta(chunk);
                    on_delta(chunk);
                }
            }),
            on_reasoning: boxed(move |chunk: &str| {
                sink.reasoning_delta(chunk);
                on_reasoning(chunk);
            }),
            image: pending_image.take(),
        };
        let turn = self.turn_source.next_turn(request).await;
        if self.chain_response_id && turn.response_id.is_some() {
            *prev = turn.response_id.clone();
        }
        turn
    }

    async fn run_calls(
        &self,
        run: &mut LoopRun<'_>,
        calls: &[ToolCall],
        msgs: &mut Vec<Msg>,
        added: &mut Vec<Msg>,
        undos: &mut Vec<Undo>,
    ) -> Vec<ToolResult> {
        let parallel = calls.len() > 1
            && calls.iter().all(|call| {
                self.registry.get(&call.tool).is_some_and(|tool| tool.can_run_parallel(&call.arguments, run.ctx))
            });
        if parallel {
            return self.run_calls_in_parallel(run, calls, msgs, added, undos).await;
        }
        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            let item_id = self.announce(run, call);
            let mut result = self.runner.run(call, run.ctx);
            self.report(run, call, &result, item_id);
            if let Some(undo) = result.undo.take() {
                undos.push(undo);
            }
            self.add_observation(run, msgs, added, call, &result);
            results.push(result);
        }
        results
    }

    async fn run_calls_in_parallel(
        &self,
        run: &mut LoopRun<'_>,
        calls: &[ToolCall],
        msgs: &mut Vec<Msg>,
        added: &mut Vec<Msg>,
        undos: &mut Vec<Undo>,
    ) -> Vec<ToolResult> {
        let runner = &self.runner;
        let ctx = SharedContext(run.ctx);
        let (sender, receiver) = mpsc::channel::<(usize, ToolResult)>();
        let mut slots: Vec<Option<ToolResult>> = (0..calls.len()).map(|_| None).collect();

        thread::scope(|scope| {
            let mut next = 0usize;
            let mut outstanding = 0usize;
            while next < calls.len() && outstanding < MAX_PARALLEL_TOOL_CALLS {
                let call = &calls[next];
                let slot = next;
                next += 1;
                outstanding += 1;
                let sender = sender.clone();
                let ctx = &ctx;
                scope.spawn(move || {
                    let mut result = runner.run(call, ctx.0);
                    let undo = result.undo.take();
                    debug_assert!(undo.is_none(), "een paralelle call schrijft niets en heeft dus geen handgreep");
                    let _ = sender.send((slot, result));
                });
            }
            let refill = sender;
            while outstanding > 0 {
                match receiver.recv() {
                    Ok((slot, result)) => {
                        slots[slot] = Some(result);
                        outstanding -= 1;
                    }
                    Err(_) => break,
                }
                while next < calls.len() && outstanding < MAX_PARALLEL_TOOL_CALLS {
                    let call = &calls[next];
                    let slot = next;
                    next += 1;
                    outstanding += 1;
                    let sender = refill.clone();
                    let ctx = &ctx;
                    scope.spawn(move || {
                        let mut result = runner.run(call, ctx.0);
                        let undo = result.undo.take();
                        debug_assert!(undo.is_none(), "een paralelle call schrijft niets en heeft dus geen handgreep");
                        let _ = sender.send((slot, result));
                    });
                }
            }
        });

        let mut ordered = Vec::with_capacity(calls.len());
        for (index, call) in calls.iter().enumerate() {
            let mut result = slots[index].take().unwrap_or_else(|| {
                ToolResult::fail(format!("tool `{}` produced no result", call.tool))
            });
            let item_id = self.announce(run, call);
            self.report(run, call, &result, item_id);
            if let Some(undo) = result.undo.take() {
                undos.push(undo);
            }
            self.add_observation(run, msgs, added, call, &result);
            ordered.push(result);
        }
        ordered
    }

    fn announce(&self, run: &LoopRun<'_>, call: &ToolCall) -> String {
        let sink = run.sink;
        let id = sink.next_item_id();
        match self.registry.get(&call.tool).map(|tool| tool.item_kind()).unwrap_or(ItemKind::ToolCall) {
            ItemKind::CommandExecution => sink.command_started(started_command_card(id.clone(), call)),
            ItemKind::FileChange => sink.file_change_started(started_file_change_card(id.clone(), call)),
            _ => sink.tool_call_started(started_tool_call_card(id.clone(), call)),
        }
        id
    }

    fn report(&self, run: &LoopRun<'_>, call: &ToolCall, result: &ToolResult, item_id: String) {
        let sink = run.sink;
        let id = item_id;
        match self.registry.get(&call.tool).map(|tool| tool.item_kind()).unwrap_or(ItemKind::ToolCall) {
            ItemKind::CommandExecution => sink.command_completed(command_card(id, call, result)),
            ItemKind::FileChange => sink.file_change_completed(file_change_card(id, call, result)),
            _ => sink.tool_call_completed(tool_call_card(id, call, result)),
        }
        for image in &result.images {
            sink.image_completed(baudet_core::ImageItem { item_id: sink.next_item_id(), ..image.clone() });
        }
    }

    fn add_assistant(&self, run: &mut LoopRun<'_>, msgs: &mut Vec<Msg>, added: &mut Vec<Msg>, turn: &Turn) {
        let msg = Msg {
            role: "assistant".to_string(),
            content: turn.text.clone(),
            response_id: turn.response_id.clone(),
            prompt_tokens: prompt_tokens_of(turn),
        };
        msgs.push(msg.clone());
        added.push(msg.clone());
        (run.on_message)(&msg);
    }

    fn add_observation(
        &self,
        run: &mut LoopRun<'_>,
        msgs: &mut Vec<Msg>,
        added: &mut Vec<Msg>,
        call: &ToolCall,
        result: &ToolResult,
    ) {
        let msg = Msg::user(observation(call, result));
        msgs.push(msg.clone());
        added.push(msg.clone());
        (run.on_message)(&msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::Tool;
    use crate::turn_source::TurnFuture;
    use baudet_core::protocol::FunctionCall;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    const CAP: usize = MAX_OBSERVATION_CHARS;

    fn call(tool: &str, arguments: &Value, id: Option<&str>) -> ToolCall {
        ToolCall { tool: tool.to_string(), arguments: arguments.clone(), call_id: id.map(str::to_string) }
    }


    #[test]
    fn an_observation_names_the_call_and_its_verdict() {
        let text = observation(
            &call("read_file", &json!({"file": "a.txt"}), Some("call_7")),
            &ToolResult::ok("inhoud"),
        );
        assert!(text.contains("for call call_7"), "{text}");
        assert!(text.contains(": `read_file`"), "{text}");
        assert!(text.contains(" succeeded (ok=true): inhoud"), "{text}");
        assert!(!text.contains("ok=false"), "{text}");
        assert!(!text.contains("Do not send the same call again"), "een geslaagde call hoeft niet te corrigeren");
    }

    #[test]
    fn a_failed_observation_tells_the_model_not_to_repeat() {
        let text = observation(
            &call("execute", &json!({"command": "false"}), None),
            &ToolResult::fail("exit code: 1"),
        );
        assert!(text.contains(" failed (ok=false): exit code: 1"), "{text}");
        assert!(text.contains("Do not send the same call again unchanged; correct it or answer."), "{text}");
        assert!(!text.contains("for call "), "{text}");
    }

    #[test]
    fn an_observation_carries_its_diff_in_a_fence() {
        let mut result = ToolResult::ok("g written");
        result.diff = Some("--- a\n+++ b\n+line".to_string());
        let text = observation(&call("edit_file", &json!({"file": "g"}), Some("c1")), &result);
        let fence = "\u{60}\u{60}\u{60}";
        assert!(text.contains(&format!("\n{fence}diff\n--- a\n+++ b\n+line\n{fence}")), "{text}");
    }


    #[test]
    fn text_under_the_cap_comes_back_unchanged() {
        let text = "een regel\nmet\nmeerdere regels";
        assert_eq!(cap_model_text(text, CAP), text);
        assert_eq!(cap_model_text("", CAP), "");
    }

    #[test]
    fn a_capped_observation_stays_within_the_cap_plus_the_marker_digits() {
        let huge = "x".repeat(CAP * 3);
        let capped = cap_model_text(&huge, CAP);
        let width = capped.chars().count() - CAP;
        assert!(width <= 14, "ouder dan het plafond met {width} tekens");
        assert!(capped.contains("output truncated"), "{capped}");
        assert!(capped.starts_with('x') && capped.ends_with('x'));
        assert!(capped.chars().count() < huge.chars().count());
    }

    #[test]
    fn capping_never_splits_a_character() {
        let text = "é".repeat(CAP * 3);
        let capped = cap_model_text(&text, CAP);
        assert!(capped.chars().all(|c| c == 'é' || c.is_ascii()), "geen vervangingstekens");
        assert!(!capped.contains('\u{fffd}'), "{capped}");
        assert!(capped.chars().count() < text.chars().count());
    }


    #[derive(Default)]
    struct RecordingSink {
        lines: std::sync::Arc<Mutex<Vec<String>>>,
        ids: AtomicUsize,
    }

    impl RecordingSink {
        fn shared(&self) -> std::sync::Arc<Mutex<Vec<String>>> {
            self.lines.clone()
        }

        fn record(&self, line: impl Into<String>) {
            self.lines.lock().unwrap().push(line.into());
        }

        fn lines(&self) -> Vec<String> {
            self.lines.lock().unwrap().clone()
        }

        fn count(&self, needle: &str) -> usize {
            self.lines().iter().filter(|line| line.contains(needle)).count()
        }

        fn count_opened(&self, marker: &str) -> usize {
            self.lines().iter().filter(|line| line.starts_with(&format!("open {marker} "))).count()
        }

        fn count_closed(&self, marker: &str) -> usize {
            self.lines().iter().filter(|line| line.starts_with(&format!("{marker} item-"))).count()
        }
    }

    impl ItemSink for RecordingSink {
        fn next_item_id(&self) -> String {
            format!("item-{}", self.ids.fetch_add(1, Ordering::SeqCst) + 1)
        }
        fn reply_started(&self) {
            self.record("reply-started");
        }
        fn reply_completed(&self, text: &str) {
            self.record(format!("reply-completed {text}"));
        }
        fn text_delta(&self, chunk: &str) {
            self.record(format!("delta {chunk}"));
        }
        fn reasoning_delta(&self, chunk: &str) {
            self.record(format!("reasoning {chunk}"));
        }
        fn command_completed(&self, item: CommandExecutionItem) {
            self.record(format!("command {} {}", item.item_id, item.command));
        }
        fn file_change_completed(&self, item: FileChangeItem) {
            self.record(format!("filechange {} {}", item.item_id, item.path));
        }
        fn tool_call_completed(&self, item: ToolCallItem) {
            self.record(format!("toolcall {} {}", item.item_id, item.tool));
        }
        fn command_started(&self, item: CommandExecutionItem) {
            self.record(format!("open command {} {}", item.item_id, item.command));
        }
        fn file_change_started(&self, item: FileChangeItem) {
            self.record(format!("open filechange {} {}", item.item_id, item.path));
        }
        fn tool_call_started(&self, item: ToolCallItem) {
            self.record(format!("open toolcall {} {}", item.item_id, item.tool));
        }
    }

    struct SinkWitnessTool {
        lines: std::sync::Arc<Mutex<Vec<String>>>,
        item: ItemKind,
    }

    impl Tool for SinkWitnessTool {
        fn name(&self) -> &str {
            "witness"
        }
        fn description(&self) -> &str {
            "stub that reports into the sink"
        }
        fn parameters(&self) -> Value {
            json!({"type": "object"})
        }
        fn item_kind(&self) -> ItemKind {
            self.item
        }
        fn can_run_parallel(&self, _args: &Value, _ctx: &ToolContext) -> bool {
            true
        }
        fn execute(&self, _args: &Value, _ctx: &ToolContext) -> ToolResult {
            self.lines.lock().unwrap().push("inside-execute".to_string());
            ToolResult::ok("ok")
        }
    }

    struct StubTool {
        name: &'static str,
        item: ItemKind,
        parallel: bool,
        delay: Option<std::time::Duration>,
        gate: Option<std::sync::Arc<Gate>>,
        barrier: bool,
    }

    impl StubTool {
        fn new(name: &'static str) -> StubTool {
            StubTool { name, item: ItemKind::ToolCall, parallel: false, delay: None, gate: None, barrier: false }
        }
    }

    impl Tool for StubTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "stub"
        }
        fn parameters(&self) -> Value {
            json!({"type": "object"})
        }
        fn item_kind(&self) -> ItemKind {
            self.item
        }
        fn can_run_parallel(&self, _args: &Value, _ctx: &ToolContext) -> bool {
            self.parallel
        }
        fn execute(&self, args: &Value, _ctx: &ToolContext) -> ToolResult {
            if self.barrier {
                let gate = self.gate.clone().expect("een barrier-call draagt een poort");
                gate.arrive();
                gate.wait();
            }
            if let Some(delay) = self.delay {
                std::thread::sleep(delay);
            }
            let file = args.get("file").and_then(Value::as_str).unwrap_or("-");
            ToolResult::ok(format!("({} {file})", self.name))
        }
    }

    struct WriterTool {
        writes: std::sync::Arc<AtomicUsize>,
    }

    impl Tool for WriterTool {
        fn name(&self) -> &str {
            "edit_file"
        }
        fn description(&self) -> &str {
            "stub writer"
        }
        fn parameters(&self) -> Value {
            json!({"type": "object"})
        }
        fn item_kind(&self) -> ItemKind {
            ItemKind::FileChange
        }
        fn execute(&self, args: &Value, _ctx: &ToolContext) -> ToolResult {
            let file = args.get("file").and_then(Value::as_str).unwrap_or("-").to_string();
            let mut result = ToolResult::ok(format!("written {file}"));
            let writes = self.writes.clone();
            result.undo = Some(Box::new(move || {
                writes.fetch_add(1, Ordering::SeqCst);
            }));
            result
        }
    }

    struct Gate {
        state: Mutex<GateState>,
        changed: std::sync::Condvar,
        needed: usize,
    }

    #[derive(Default)]
    struct GateState {
        inside: usize,
        max_concurrent: usize,
        released: bool,
    }

    impl Gate {
        fn new(needed: usize) -> Gate {
            Gate { state: Mutex::new(GateState::default()), changed: std::sync::Condvar::new(), needed }
        }

        fn arrive(&self) {
            let mut state = self.state.lock().unwrap();
            state.inside += 1;
            state.max_concurrent = state.max_concurrent.max(state.inside);
            if state.inside >= self.needed {
                state.released = true;
            }
            self.changed.notify_all();
        }

        fn wait(&self) {
            let mut state = self.state.lock().unwrap();
            while !state.released {
                state = self.changed.wait(state).unwrap();
            }
            state.inside -= 1;
        }

        fn peak(&self) -> usize {
            self.state.lock().unwrap().max_concurrent
        }
    }

    struct StubSource {
        script: Mutex<Vec<(String, Vec<(String, Value)>)>>,
        calls: AtomicUsize,
        seen: Mutex<Vec<(Option<String>, Option<String>, usize)>>,
        estimates: Mutex<Vec<u64>>,
        last_id: Mutex<Option<String>>,
    }

    impl StubSource {
        fn prose(text: impl Into<String>) -> (String, Vec<(String, Value)>) {
            (text.into(), Vec::new())
        }

        fn calls_for(text: impl Into<String>, calls: &[(&str, Value)]) -> (String, Vec<(String, Value)>) {
            (text.into(), calls.iter().map(|(name, args)| (name.to_string(), args.clone())).collect())
        }
    }

    impl TurnSource for StubSource {
        fn next_turn<'r>(&'r self, mut request: TurnRequest<'r>) -> TurnFuture<'r> {
            Box::pin(async move {
                let index = self.calls.fetch_add(1, Ordering::SeqCst);
                self.seen.lock().unwrap().push((
                    request.previous_response_id.map(str::to_string),
                    request.image.map(str::to_string),
                    request.history.len(),
                ));
                self.estimates.lock().unwrap().push(estimate_tokens_of(request.history));
                if let Some(delta) = request.history.last() {
                    (request.on_delta)(&delta.content);
                }
                (request.on_reasoning)("denk");
                let (text, wanted) = {
                    let script = self.script.lock().unwrap();
                    script.get(index).cloned().unwrap_or_else(|| Self::prose("einde"))
                };
                let id = format!("resp-{index}");
                *self.last_id.lock().unwrap() = Some(id.clone());
                let turns = wanted
                    .into_iter()
                    .enumerate()
                    .map(|(n, (name, args))| FunctionCall {
                        call_id: Some(format!("call-{index}-{n}")),
                        name,
                        arguments: args.to_string(),
                    })
                    .collect();
                Turn { text, response_id: Some(id), function_calls: turns, ..Turn::text(String::new()) }
            })
        }
    }

    fn one(tool: StubTool) -> ToolRegistry {
        ToolRegistry::new(vec![Box::new(tool)])
    }

    fn two_readers() -> ToolRegistry {
        let mut reader_a = StubTool::new("read_file");
        reader_a.parallel = true;
        let mut reader_b = StubTool::new("grep");
        reader_b.parallel = true;
        ToolRegistry::new(vec![Box::new(reader_a), Box::new(reader_b)])
    }

    fn writer_only() -> ToolRegistry {
        ToolRegistry::new(vec![Box::new(WriterTool { writes: std::sync::Arc::new(AtomicUsize::new(0)) })])
    }

    async fn drive(
        registry: &ToolRegistry,
        source: &StubSource,
        history: &[Msg],
        ctx: &ToolContext,
        max_iterations: u32,
        configure: impl FnOnce(ToolLoop<'_>) -> ToolLoop<'_>,
    ) -> (LoopOutcome, RecordingSink) {
        let sink = RecordingSink::default();
        let outcome = drive_into(registry, source, history, ctx, max_iterations, configure, &sink).await;
        (outcome, sink)
    }

    async fn drive_into(
        registry: &ToolRegistry,
        source: &StubSource,
        history: &[Msg],
        ctx: &ToolContext,
        max_iterations: u32,
        configure: impl FnOnce(ToolLoop<'_>) -> ToolLoop<'_>,
        sink: &RecordingSink,
    ) -> LoopOutcome {
        let mut run = LoopRun {
            history,
            ctx,
            previous_response_id: None,
            image: None,
            on_delta: Box::new(|_| {}),
            on_reasoning: Box::new(|_| {}),
            on_message: Box::new(|_| {}),
            sink,
        };
        let loop_ = configure(ToolLoop::new(source, registry, max_iterations));
        loop_.run(&mut run).await
    }

    async fn drive_chained(
        registry: &ToolRegistry,
        source: &StubSource,
        history: &[Msg],
        ctx: &ToolContext,
        max_iterations: u32,
        configure: impl FnOnce(ToolLoop<'_>) -> ToolLoop<'_>,
        sink: &RecordingSink,
        previous: Option<&str>,
    ) -> LoopOutcome {
        let mut run = LoopRun {
            history,
            ctx,
            previous_response_id: previous,
            image: None,
            on_delta: Box::new(|_| {}),
            on_reasoning: Box::new(|_| {}),
            on_message: Box::new(|_| {}),
            sink,
        };
        let loop_ = configure(ToolLoop::new(source, registry, max_iterations).chain_response_id(true));
        loop_.run(&mut run).await
    }

    fn source(script: Vec<(String, Vec<(String, Value)>)>) -> StubSource {
        StubSource {
            script: Mutex::new(script),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
            estimates: Mutex::new(Vec::new()),
            last_id: Mutex::new(None),
        }
    }

    fn turns(source: &StubSource) -> usize {
        source.calls.load(Ordering::SeqCst)
    }

    fn seen(source: &StubSource) -> Vec<(Option<String>, Option<String>, usize)> {
        source.seen.lock().unwrap().clone()
    }

    fn ctx() -> ToolContext {
        ToolContext::for_folder("/", true, true, true)
    }



    #[tokio::test]
    async fn a_card_is_open_while_the_tool_runs_and_closes_the_same_row() {
        let reader = StubTool::new("read_file");
        let reg = ToolRegistry::new(vec![Box::new(reader)]);
        let src = source(vec![StubSource::calls_for("kijk", &[("read_file", json!({"file": "a"}))]), StubSource::prose("klaar")]);
        let sink = RecordingSink::default();
        let outcome = drive_into(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l, &sink).await;
        assert!(matches!(outcome, LoopOutcome::Completed { .. }));
        let lines = sink.lines();
        let opened = lines.iter().position(|l| l.starts_with("open toolcall")).expect("rij geopend");
        let closed = lines.iter().position(|l| l.starts_with("toolcall item")).expect("rij gesloten");
        assert!(opened < closed, "kaart open vóór de afronding: {lines:?}");
        assert_eq!(id_of(&lines[opened]), id_of(&lines[closed]), "één rij per call: {lines:?}");
    }

    #[tokio::test]
    async fn a_decision_asked_inside_execute_finds_its_card_already_open() {
        let sink = RecordingSink::default();
        let witness = SinkWitnessTool { lines: sink.shared(), item: ItemKind::FileChange };
        let reg = ToolRegistry::new(vec![Box::new(witness)]);
        let src = source(vec![StubSource::calls_for("schrijf", &[("witness", json!({"file": "b"}))]), StubSource::prose("klaar")]);
        let outcome = drive_into(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l, &sink).await;
        assert!(matches!(outcome, LoopOutcome::Completed { .. }));
        let lines = sink.lines();
        let opened = lines.iter().position(|l| l.starts_with("open filechange")).expect("schrijfkaart geopend");
        let inside = lines.iter().position(|l| l == "inside-execute").expect("de tool heeft gewerkt");
        let closed = lines.iter().position(|l| l.starts_with("filechange item")).expect("gesloten");
        assert!(opened < inside, "gemeld aan een rij die er al stond: {lines:?}");
        assert!(inside < closed, "dezelfde rij sluit ná het werk: {lines:?}");
        assert!(lines[opened].contains('b'), "de kaart noemt het pad dat de call vroeg: {lines:?}");
        assert_eq!(id_of(&lines[opened]), id_of(&lines[closed]), "openen en sluiten gaan over één rij");
    }

    #[tokio::test]
    async fn a_fanned_out_batch_opens_one_card_per_call() {
        let mut a = StubTool::new("read_file");
        a.parallel = true;
        a.delay = Some(std::time::Duration::from_millis(40));
        let mut b = StubTool::new("grep");
        b.parallel = true;
        let reg = ToolRegistry::new(vec![Box::new(a), Box::new(b)]);
        let src = source(vec![StubSource::calls_for("twee lezingen", &[("read_file", json!({"file": "a"})), ("grep", json!({"file": "b"}))]), StubSource::prose("klaar")]);
        let sink = RecordingSink::default();
        let outcome = drive_into(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l, &sink).await;
        assert!(matches!(outcome, LoopOutcome::Completed { .. }));
        let lines = sink.lines();
        let opened: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| l.starts_with("open ")).map(|(i, _)| i).collect();
        assert_eq!(opened.len(), 2, "elke call opent een rij: {lines:?}");
        assert!(lines[opened[0]].contains("read_file") && lines[opened[1]].contains("grep"), "de openingen volgen het model: {lines:?}");
        for marker in ["command", "filechange", "toolcall"] {
            let opens: Vec<Option<String>> = lines
                .iter()
                .filter(|l| l.starts_with(&format!("open {marker} ")))
                .map(|l| id_of(l))
                .collect();
            let closed: Vec<Option<String>> = lines
                .iter()
                .filter(|l| l.starts_with(&format!("{marker} item")))
                .map(|l| id_of(l))
                .collect();
            assert_eq!(opens, closed, "elke opening sluit op dezelfde rij, in dezelfde volgorde: {marker} in {lines:?}");
        }
    }

    fn id_of(line: &str) -> Option<String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        words.get(words.len().saturating_sub(2)).copied().map(str::to_string)
    }

    #[tokio::test]
    async fn a_prose_turn_ends_the_run_without_calling_a_tool() {
        let reg = two_readers();
        let src = source(vec![StubSource::prose("het antwoord")]);
        let (outcome, sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        let LoopOutcome::Completed { final_text, cap_reached, steps, .. } = &outcome else {
            panic!("verwacht een finale, got {outcome:?}");
        };
        assert_eq!(final_text, "het antwoord");
        assert!(!*cap_reached, "een beurt zonder calls is geen kap");
        assert_eq!(steps.len(), 1);
        assert!(steps[0].calls.is_empty());
        assert_eq!(turns(&src), 1);
        assert_eq!(sink.count("reply-started"), 1);
        assert_eq!(sink.count("reply-completed het antwoord"), 1);
    }

    #[tokio::test]
    async fn one_tool_call_returns_and_the_next_turn_finishes() {
        let reg = two_readers();
        let src = source(vec![
            StubSource::calls_for(" ik lees even", &[("read_file", json!({"file": "a.txt"}))]),
            StubSource::prose("klaar"),
        ]);
        let (outcome, sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        let LoopOutcome::Completed { steps, messages, cap_reached, .. } = &outcome else {
            panic!("verwacht een finale, got {outcome:?}");
        };
        assert!(!*cap_reached);
        assert_eq!(steps.len(), 2, "één call-stap en één finale");
        assert_eq!(steps[0].calls.len(), 1);
        assert_eq!(steps[0].results[0].output, "(read_file a.txt)");
        let obs = messages.iter().find(|m| m.role == "user").expect("observatie ontbreekt");
        assert!(obs.content.contains(" succeeded (ok=true): (read_file a.txt)"), "{}", obs.content);
        assert!(obs.content.contains("call-0-0"), "de call-id van de bron staat erin");
        assert_eq!(
            sink.lines(),
            vec![
                "reply-started",
                "delta vraag",
                "reasoning denk",
                "reply-completed  ik lees even",
                "open toolcall item-1 read_file",
                "toolcall item-1 read_file",
                "reply-started",
                "delta Tool observation for call call-0-0: `read_file` succeeded (ok=true): (read_file a.txt)",
                "reasoning denk",
                "reply-completed klaar",
            ]
        );
    }

    #[tokio::test]
    async fn given_a_news_fetch_announcement_when_the_model_continues_then_the_edit_requires_approval() {
        // Given a model that announces the action before eventually sending a tool call.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("news.txt");
        std::fs::write(&file, "old").unwrap();
        let registry = ToolRegistry::new(vec![Box::new(crate::edit_file::EditFileTool)]);
        let source = source(vec![
            StubSource::prose("Ik haal het nieuws opnieuw op."),
            StubSource::calls_for(format!("Proceeding.\n{FINAL_DONE}"), &[("edit_file", json!({
                "file": "news.txt", "edits": [{"old_text": "old", "new_text": "updated"}]
            }))]),
            StubSource::prose(format!("De wijziging is geweigerd.\n{FINAL_BLOCKED}")),
        ]);
        let approvals = std::sync::Arc::new(AtomicUsize::new(0));
        let observed = approvals.clone();
        let mut context = ToolContext::for_folder(dir.path(), true, true, false);
        context.approve = Box::new(move |path, _| {
            assert_eq!(path, "news.txt");
            observed.fetch_add(1, Ordering::SeqCst);
            false
        });

        // When the user requests the news.
        let (outcome, _) = drive(&registry, &source, &[Msg::user("Haal het nieuws op")], &context, 4, |loop_| loop_.require_completion(true)).await;

        // Then the turn continues once and the declined edit never reaches disk.
        assert_eq!(turns(&source), 3);
        assert_eq!(approvals.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read_to_string(file).unwrap(), "old");
        assert!(!outcome.messages().iter().any(is_retry_nudge));
        assert!(matches!(outcome, LoopOutcome::Completed { final_text, .. } if final_text == "De wijziging is geweigerd."));
    }

    #[test]
    fn given_a_final_marker_split_across_chunks_when_streamed_then_only_answer_text_is_visible() {
        let mut pending = String::new();
        let visible: String = ["Reported.\n[", "[BAUDET_", "DONE", "]]"]
            .iter()
            .map(|chunk| visible_delta(chunk, &mut pending))
            .collect();

        assert_eq!(visible, "Reported.\n");
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn given_tools_when_the_model_returns_news_then_the_answer_completes_without_retry() {
        let registry = two_readers();
        let answer = "Ik heb het nieuws opgehaald. Het belangrijkste bericht staat bovenaan.";
        let source = source(vec![StubSource::prose(format!("{answer}\n{FINAL_DONE}"))]);

        let (outcome, _) = drive(&registry, &source, &[Msg::user("Haal het nieuws op")], &ctx(), 4, |loop_| loop_.require_completion(true)).await;

        assert_eq!(turns(&source), 1);
        assert!(matches!(outcome, LoopOutcome::Completed { final_text, .. } if final_text == answer));
    }

    #[tokio::test]
    async fn given_an_announcement_retry_when_the_model_only_promises_again_then_the_turn_reports_missing_completion() {
        let registry = two_readers();
        let source = source(vec![
            StubSource::prose("Ik haal het nieuws opnieuw op."),
            StubSource::prose("Ik haal 'm opnieuw op — nu de volledige feed, zodat ik de nieuwste items (die vooraan staan) goed kan zien."),
        ]);

        let (outcome, _) = drive(&registry, &source, &[Msg::user("Haal het nieuws op")], &ctx(), 4, |loop_| loop_.require_completion(true)).await;

        assert_eq!(turns(&source), 2);
        assert!(matches!(outcome, LoopOutcome::Completed { final_turn, .. }
            if final_turn.incomplete_reason.as_deref() == Some(MISSING_COMPLETION)));
    }

    #[tokio::test]
    async fn the_nudge_fights_a_blank_turn_but_never_reaches_the_transcript() {
        let reg = two_readers();
        let src = source(vec![
            StubSource::prose("..."),
            StubSource::prose("."),
            StubSource::prose("nu wel"),
        ]);
        let (outcome, _sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 6, |l| l).await;
        let LoopOutcome::Completed { final_text, messages, .. } = &outcome else {
            panic!("verwacht een finale, got {outcome:?}");
        };
        assert_eq!(final_text, "nu wel");
        assert_eq!(turns(&src), 3, "twee nudges en dan de echte beurt");
        let counts: Vec<usize> = seen(&src).iter().map(|(_, _, len)| *len).collect();
        assert_eq!(counts, vec![1, 2, 3], "de werkende kop groeit met alleen de nudge erbij");
        assert!(!messages.iter().any(|m| m.content == BLANK_NUDGE), "de nudge blijft buiten het transcript");
        assert_eq!(messages.len(), 1, "alleen de echte finale wordt gepersisteerd");
    }

    #[tokio::test]
    async fn a_source_that_stays_blank_ends_at_the_blank_ceiling() {
        let reg = two_readers();
        let blanks = StubSource::prose(" ");
        let src = source(vec![blanks.clone(), blanks.clone(), blanks.clone(), blanks]);
        let (outcome, _sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 9, |l| l).await;
        assert_eq!(turns(&src), 3);
        let LoopOutcome::Completed { final_text, .. } = &outcome else { panic!("got {outcome:?}") };
        assert_eq!(final_text, " ");
    }

    #[tokio::test]
    async fn an_empty_tool_turn_settles_in_prose_before_the_cap() {
        let reg = two_readers();
        let asking = StubSource::calls_for("werk", &[("read_file", json!({"file": "a"}))]);
        let src = source(vec![asking.clone(), asking, StubSource::prose("genoeg gedaan")]);
        let (outcome, _sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 2, |l| l).await;
        let LoopOutcome::Completed { cap_reached, steps, messages, final_text, .. } = &outcome else {
            panic!("verwacht een finale, got {outcome:?}");
        };
        assert!(*cap_reached, "het antwoord kwam pas na het budget");
        assert_eq!(steps.len(), 3, "twee call-stappen en de settle-beurt");
        assert_eq!(turns(&src), 3, "twee beurten plus de settle-poging");
        assert!(steps[2].calls.is_empty(), "de settle-stap vraagt niets");
        assert_eq!(final_text, "genoeg gedaan");
        assert_eq!(messages.last().map(|m| m.content.as_str()), Some("genoeg gedaan"));
    }

    #[tokio::test]
    async fn a_model_that_keeps_asking_ends_as_cap_reached() {
        let reg = two_readers();
        let asking = StubSource::calls_for("werk", &[("read_file", json!({"file": "a"}))]);
        let src = source(vec![asking.clone(), asking.clone(), asking.clone(), asking]);
        let (outcome, _sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 3, |l| l).await;
        let LoopOutcome::CapReached { incomplete_reason, steps, .. } = &outcome else { panic!("verwacht een kap, got {outcome:?}") };
        assert_eq!(incomplete_reason, MAX_ITERATIONS_REASON);
        assert_eq!(steps.len(), 3, "alleen de call-stappen; de settle-beurt vroeg weer een call");
        assert_eq!(turns(&src), 4, "drie beurten plus de settle-poging");
    }

    #[tokio::test]
    async fn an_empty_registry_offers_the_model_no_tools() {
        let reg = ToolRegistry::default();
        assert!(reg.is_empty());
        let src = source(vec![StubSource::prose("antwoord")]);
        let (outcome, _sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 3, |l| l).await;
        assert!(matches!(outcome, LoopOutcome::Completed { .. }));
        assert_eq!(turns(&src), 1);
    }

    #[tokio::test]
    async fn the_card_kind_comes_from_the_tool_and_undos_are_carried() {
        let reg = writer_only();
        let src = source(vec![
            StubSource::calls_for("schrijf", &[("edit_file", json!({"file": "g.txt"}))]),
            StubSource::prose("gedaan"),
        ]);
        let (outcome, sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        let lines = sink.lines();
        assert!(lines.iter().any(|l| l == "filechange item-1 g.txt"), "{lines:?}");
        assert!(!lines.iter().any(|l| l.starts_with("toolcall")), "niet de generieke kaart: {lines:?}");
        let LoopOutcome::Completed { undos, .. } = &outcome else { panic!("got {outcome:?}") };
        assert_eq!(undos.len(), 1);
        (undos[0])();
    }

    #[tokio::test]
    async fn an_unknown_tool_is_reported_as_a_failed_observation() {
        let reg = two_readers();
        let src = source(vec![
            StubSource::calls_for("vreemd", &[("bestaan_niet", json!({}))]),
            StubSource::prose("ik stel bij"),
        ]);
        let (outcome, sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        assert!(matches!(outcome, LoopOutcome::Completed { .. }));
        assert!(sink.lines().iter().any(|l| l.starts_with("toolcall item-1 bestaan_niet")), "{:?}", sink.lines());
        let obs = outcome.messages().iter().find(|m| m.role == "user").expect("observatie");
        assert!(obs.content.contains("failed (ok=false): no tool named"), "{}", obs.content);
    }

    #[tokio::test]
    async fn streaming_reaches_both_the_sink_and_the_caller() {
        let reg = two_readers();
        let src = source(vec![StubSource::prose("antwoord")]);
        let rights = ctx();
        let sink = RecordingSink::default();
        let deltas = std::sync::Arc::new(Mutex::new(Vec::new()));
        let reasonings = std::sync::Arc::new(Mutex::new(Vec::new()));
        let seen_deltas = deltas.clone();
        let seen_reasonings = reasonings.clone();
        let mut run = LoopRun {
            history: &[Msg::user("losse tekst")],
            ctx: &rights,
            previous_response_id: None,
            image: None,
            on_delta: Box::new(move |chunk| deltas.lock().unwrap().push(chunk.to_string())),
            on_reasoning: Box::new(move |chunk| reasonings.lock().unwrap().push(chunk.to_string())),
            on_message: Box::new(|_| {}),
            sink: &sink,
        };
        let outcome = ToolLoop::new(&src, &reg, 3).run(&mut run).await;
        assert!(matches!(outcome, LoopOutcome::Completed { .. }));
        assert_eq!(*seen_deltas.lock().unwrap(), vec!["losse tekst".to_string()]);
        assert_eq!(*seen_reasonings.lock().unwrap(), vec!["denk".to_string()]);
        assert_eq!(sink.count("delta losse tekst"), 1);
        assert_eq!(sink.count("reasoning denk"), 1);
    }

    #[tokio::test]
    async fn an_image_plays_once_and_a_chain_rides_only_when_asked() {
        let reg = two_readers();
        let asking = StubSource::calls_for("werk", &[("read_file", json!({"file": "a"}))]);
        let src = source(vec![asking, StubSource::prose("klaar")]);
        let rights = ctx();
        let sink = RecordingSink::default();
        let mut run = LoopRun {
            history: &[Msg::user("bekijk dit")],
            ctx: &rights,
            previous_response_id: Some("resp-uit-de-sessie"),
            image: Some("base64-afbeelding"),
            on_delta: Box::new(|_| {}),
            on_reasoning: Box::new(|_| {}),
            on_message: Box::new(|_| {}),
            sink: &sink,
        };
        let outcome = ToolLoop::new(&src, &reg, 4).chain_response_id(true).run(&mut run).await;
        assert!(matches!(outcome, LoopOutcome::Completed { .. }));
        let looked = seen(&src);
        assert_eq!(looked[0].1.as_deref(), Some("base64-afbeelding"));
        assert_eq!(looked[1].1, None, "een beeld speelt niet twee keer mee");
        assert_eq!(looked[0].0.as_deref(), Some("resp-uit-de-sessie"));
        assert_eq!(looked[1].0.as_deref(), Some("resp-0"));
        assert_eq!(outcome.response_id(), Some("resp-1"));
    }

    #[tokio::test]
    async fn without_a_chain_every_turn_starts_from_the_client() {
        let reg = two_readers();
        let asking = StubSource::calls_for("werk", &[("read_file", json!({"file": "a"}))]);
        let src = source(vec![asking, StubSource::prose("klaar")]);
        let (outcome, _sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        assert!(matches!(outcome, LoopOutcome::Completed { .. }));
        let looked = seen(&src);
        assert_eq!(looked[0].0, None);
        assert_eq!(looked[1].0, None);
        assert_eq!(outcome.response_id(), Some("resp-1"));
    }

    #[tokio::test]
    async fn a_chained_run_stops_before_a_request_it_cannot_afford() {
        let reg = two_readers();
        let src = source(vec![StubSource::calls_for("had nooit mogen starten", &[("read_file", json!({"file": "a"}))])]);
        let sink = RecordingSink::default();
        let history = [Msg::user("x".repeat(3_000))];
        assert!(estimate_tokens_of(&history) > 10, "de kop moet het plafond overschrijden");
        let outcome = drive_chained(
            &reg,
            &src,
            &history,
            &ctx(),
            4,
            |l| l.context_ceiling(10),
            &sink,
            Some("resp-vorige-beurt"),
        )
        .await;
        let LoopOutcome::CapReached { incomplete_reason, steps, .. } = &outcome else {
            panic!("verwacht een kap, got {outcome:?}");
        };
        assert_eq!(incomplete_reason, CONTEXT_LIMIT_REASON);
        assert!(steps.is_empty(), "geen enkele stap: het verzoek is nooit verstuurd");
        assert_eq!(turns(&src), 0, "de bron heeft geen enkele beurt gezien");
        assert!(sink.lines().is_empty(), "er is niets op het scherm gezet: {:?}", sink.lines());
    }

    #[tokio::test]
    async fn a_chained_run_keeps_going_while_the_ceiling_leaves_room() {
        let reg = two_readers();
        let asking = StubSource::calls_for("werk", &[("read_file", json!({"file": "a"}))]);
        let src = source(vec![asking.clone(), asking, StubSource::prose("had af kunnen sluiten")]);
        let history = [Msg::user("vraag")];
        assert!(estimate_tokens_of(&history) <= 60, "de eerste beurt moet door het plafond heen passen");
        let outcome = drive_chained(
            &reg,
            &src,
            &history,
            &ctx(),
            5,
            |l| l.context_ceiling(60),
            &RecordingSink::default(),
            Some("resp-vorige-beurt"),
        )
        .await;
        let LoopOutcome::CapReached { incomplete_reason, steps, .. } = &outcome else {
            panic!("verwacht een kap, got {outcome:?}");
        };
        assert_eq!(incomplete_reason, CONTEXT_LIMIT_REASON);
        assert_eq!(steps.len(), 2, "twee betaalbare beurten draaiden, het derde verzoek niet");
        assert_eq!(turns(&src), 2, "het derde verzoek is nooit verstuurd");
        let verzocht = src.estimates.lock().unwrap().clone();
        assert_eq!(verzocht.len(), 2, "de bron zag precies twee verzoeken");
        assert!(verzocht.iter().all(|e| *e <= 60), "élk verzoek dat de bron zag, pastte: {verzocht:?}");
        println!("schatting per verzoek dat de bron zag: {verzocht:?} van plafond 60");
    }

    #[tokio::test]
    async fn the_ceiling_is_enforced_without_a_server_session() {
        let reg = two_readers();
        let asking = StubSource::calls_for("werk", &[("read_file", json!({"file": "a"}))]);
        let src = source(vec![asking, StubSource::prose("klaar")]);
        let (outcome, _sink) =
            drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 5, |l| l.context_ceiling(1)).await;
        assert!(matches!(outcome, LoopOutcome::CapReached { incomplete_reason, .. }
            if incomplete_reason == CONTEXT_LIMIT_REASON));
        assert_eq!(turns(&src), 0);
    }

    #[tokio::test]
    async fn a_large_result_is_retained_when_the_final_request_would_exceed_the_ceiling() {
        let output = "saved observation ".repeat(200);
        let reg = ToolRegistry::new(vec![Box::new(StubToolWithOutput {
            inner: StubTool::new("read_file"),
            output: output.clone(),
        })]);
        let src = source(vec![
            StubSource::calls_for("Inspect the file", &[("read_file", json!({"file": "a"}))]),
            StubSource::prose("This request should not be sent"),
        ]);
        let history = [Msg::user("Review the project")];
        let (outcome, _) = drive(&reg, &src, &history, &ctx(), 1, |loop_| loop_.context_ceiling(100)).await;
        let LoopOutcome::CapReached { incomplete_reason, messages, steps, .. } = outcome else {
            panic!("expected the context ceiling to stop the final request");
        };
        assert_eq!(incomplete_reason, CONTEXT_LIMIT_REASON);
        assert_eq!(turns(&src), 1);
        assert_eq!(steps[0].results[0].output, output);
        assert!(messages.iter().any(|msg| msg.content.contains(&output)));
    }

    #[tokio::test]
    async fn a_budget_of_zero_still_leaves_the_loop_one_request() {
        let reg = one(StubTool::new("read_file"));
        let src = source(vec![StubSource::prose("een enkele beurt volstond")]);
        let (outcome, _sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 0, |l| l).await;
        assert!(matches!(outcome, LoopOutcome::Completed { .. }), "budget 0 mag de lus niet leeg maken: {outcome:?}");
        assert_eq!(turns(&src), 1, "één verzoek, niet nul");
    }

    #[tokio::test]
    async fn a_batch_that_must_run_serially_stays_serial() {
        let mut reader = StubTool::new("read_file");
        reader.parallel = true;
        reader.delay = Some(std::time::Duration::from_millis(60));
        let reg = ToolRegistry::new(vec![
            Box::new(reader),
            Box::new(WriterTool { writes: std::sync::Arc::new(AtomicUsize::new(0)) }),
        ]);
        let src = source(vec![
            StubSource::calls_for("lees en schrijf", &[("read_file", json!({"file": "a"})), ("edit_file", json!({"file": "b"}))]),
            StubSource::prose("klaar"),
        ]);
        let started = std::time::Instant::now();
        let (outcome, sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        let elapsed = started.elapsed();
        assert!(matches!(outcome, LoopOutcome::Completed { .. }));
        assert!(elapsed >= std::time::Duration::from_millis(55), "serialiteit moet meetbaar zijn: {elapsed:?}");
        let lines = sink.lines();
        let read = lines.iter().position(|l| l.starts_with("toolcall")).unwrap();
        let write = lines.iter().position(|l| l.starts_with("filechange")).unwrap();
        assert!(read < write, "de kaarten volgen het model, niet de afloop: {lines:?}");
    }

    #[tokio::test]
    async fn a_parallel_batch_overlaps_yet_reports_in_model_order() {
        let gate = std::sync::Arc::new(Gate::new(2));
        let mut reader_a = StubTool::new("read_file");
        reader_a.parallel = true;
        reader_a.barrier = true;
        reader_a.gate = Some(gate.clone());
        let mut reader_b = StubTool::new("grep");
        reader_b.parallel = true;
        reader_b.barrier = true;
        reader_b.gate = Some(gate.clone());
        let reg = ToolRegistry::new(vec![Box::new(reader_a), Box::new(reader_b)]);
        let src = source(vec![
            StubSource::calls_for("twee lezingen", &[("grep", json!({"file": "twee"})), ("read_file", json!({"file": "een"}))]),
            StubSource::prose("klaar"),
        ]);
        let (outcome, sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        let LoopOutcome::Completed { steps, .. } = &outcome else { panic!("got {outcome:?}") };
        assert_eq!(gate.peak(), 2, "de calls hebben elkander werkelijk gepasseerd");
        assert_eq!(steps[0].calls[0].tool, "grep");
        assert_eq!(steps[0].results[0].output, "(grep twee)");
        assert_eq!(steps[0].results[1].output, "(read_file een)");
        let lines = sink.lines();
        let positions: Vec<usize> = ["toolcall item-1 grep", "toolcall item-2 read_file"]
            .iter()
            .map(|needle| lines.iter().position(|l| l.starts_with(needle)).unwrap_or_else(|| panic!("missing {needle} in {lines:?}")))
            .collect();
        assert!(positions[0] < positions[1], "de kaarten volgen het model: {lines:?}");
        let between = &lines[positions[0] + 1..positions[1]];
        assert!(between.iter().all(|l| l.starts_with("open toolcall item-2 ")), "geen vreemde rij tussen twee kaarten: {lines:?}");
        let observations: Vec<&str> = outcome.messages().iter().filter(|m| m.role == "user").map(|m| m.content.as_str()).collect();
        assert_eq!(observations.len(), 2);
        assert!(observations[0].contains("`grep`"), "{}", observations[0]);
        assert!(observations[1].contains("`read_file`"), "{}", observations[1]);
    }

    #[tokio::test]
    async fn a_batch_larger_than_the_pool_finishes_every_call() {
        let mut reader = StubTool::new("read_file");
        reader.parallel = true;
        reader.delay = Some(std::time::Duration::from_millis(30));
        let reg = one(reader);
        let asks: Vec<(&str, Value)> =
            (1..=4).map(|n| ("read_file", json!({"file": format!("f{n}")}))).collect();
        let src = source(vec![StubSource::calls_for("vier keer", &asks), StubSource::prose("klaar")]);
        let (outcome, sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        let LoopOutcome::Completed { steps, .. } = &outcome else { panic!("got {outcome:?}") };
        assert_eq!(steps[0].results.len(), 4);
        assert_eq!(steps[0].results.iter().filter(|r| r.ok).count(), 4);
        assert_eq!(sink.count_closed("toolcall"), 4);
        assert_eq!(sink.count_opened("toolcall"), 4);
        let files: Vec<String> = steps[0].results.iter().map(|r| r.output.clone()).collect();
        assert_eq!(files, vec!["(read_file f1)", "(read_file f2)", "(read_file f3)", "(read_file f4)"]);
        let ids: Vec<String> = sink
            .lines()
            .into_iter()
            .filter(|l| l.starts_with("toolcall item-"))
            .map(|l| l.split_whitespace().nth(1).unwrap_or_default().to_string())
            .collect();
        assert_eq!(ids, vec!["item-1", "item-2", "item-3", "item-4"], "de kaarten volgen het model");
    }

    #[tokio::test]
    async fn a_sixteen_thousand_char_observation_keeps_the_head_and_tail() {
        let output = "y".repeat(CAP * 2);
        let mut big = StubToolWithOutput { inner: StubTool::new("read_file"), output };
        big.inner.parallel = true;
        let reg = ToolRegistry::new(vec![Box::new(big)]);
        let src = source(vec![
            StubSource::calls_for("grote uitvoer", &[("read_file", json!({"file": "groot"}))]),
            StubSource::prose("klaar"),
        ]);
        let (outcome, _sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        let obs = outcome.messages().iter().find(|m| m.role == "user").expect("observatie");
        assert!(obs.content.contains("output truncated"), "{}", &obs.content[..120]);
        assert!(obs.content.starts_with("Tool observation"), "kop blijft vooraan");
        assert!(obs.content.ends_with('y'), "staart blijft bewaard");
        assert!(obs.content.chars().count() <= CAP + 14, "ouder dan het plafond: {}", obs.content.chars().count());
    }

    struct StubToolWithOutput {
        inner: StubTool,
        output: String,
    }

    impl Tool for StubToolWithOutput {
        fn name(&self) -> &str {
            self.inner.name
        }
        fn description(&self) -> &str {
            "stub"
        }
        fn parameters(&self) -> Value {
            json!({"type": "object"})
        }
        fn item_kind(&self) -> ItemKind {
            self.inner.item
        }
        fn can_run_parallel(&self, _args: &Value, _ctx: &ToolContext) -> bool {
            self.inner.parallel
        }
        fn execute(&self, _args: &Value, _ctx: &ToolContext) -> ToolResult {
            ToolResult::ok(self.output.clone())
        }
    }

}
