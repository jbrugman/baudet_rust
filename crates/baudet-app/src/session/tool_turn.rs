use super::{failed, history_events, receive_turn, AppState, RequestChoices, TurnBusy};
use baudet_backend::{Backend, SendRequest};
use baudet_store::ContextStrategy;
use crate::compaction::{summarize, CompactionOutcome, CompactionPlan, Compactor};
use baudet_core::{
    AgentMessageItem, ApprovalDecision, ApprovalRequest, CommandExecutionItem, FileChangeItem, ImageItem, Item,
    ItemKind, ItemStatus, Msg, ReasoningItem, ThreadEvent, ToolCallItem, UserMessageItem,
};
use baudet_projects::{tool_gate, FolderPermission, Project};
use baudet_settings::{config::ToolMode, Settings};
use baudet_tool::{
    call_extractor::{protocol_for, ExtraTool},
    edit_file::EditFileTool,
    execute_tool::ExecuteTool,
    git_tool::GitTool,
    mcp::McpServers,
    read_file::ReadFileTool,
    tool::{Tool, ToolContext, ToolRegistry},
    tool_loop::{ItemSink, LoopOutcome, LoopRun, ToolLoop},
    turn_source::{TurnFuture, TurnRequest, TurnSource},
};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

pub type RequestApproval = dyn Fn(ApprovalRequest) -> ApprovalDecision + Send + Sync;

pub struct ToolTurn<'a> {
    pub project: Option<&'a Project>,
    pub settings: &'a Settings,
    pub servers: &'a mut McpServers,
    pub approval: Arc<RequestApproval>,
    pub cancelled: Arc<AtomicBool>,
}

impl AppState {
    pub fn tools_active(settings: &Settings, project: Option<&Project>) -> bool {
        settings.tool_mode != ToolMode::Off
            && (tool_gate::decide(true, project).is_active()
                || settings.mcp_servers.iter().any(|server| server.enabled && server.valid()))
    }

    pub fn run_tools(&self, text: &str, image: Option<&str>, run: ToolTurn<'_>) -> Result<Option<String>, TurnBusy> {
        if !Self::tools_active(run.settings, run.project) {
            return Err(TurnBusy);
        }
        self.run_context_turn(text, image, run, true)
    }

    pub fn run_auto_chat(&self, text: &str, image: Option<&str>, run: ToolTurn<'_>) -> Result<Option<String>, TurnBusy> {
        if run.settings.context_strategy != baudet_settings::config::ContextStrategy::AutoCompact {
            return Err(TurnBusy);
        }
        self.run_context_turn(text, image, run, false)
    }

    fn run_context_turn(&self, text: &str, image: Option<&str>, run: ToolTurn<'_>, use_tools: bool) -> Result<Option<String>, TurnBusy> {
        self.live_turns.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).map_err(|_| TurnBusy)?;
        let result = self.tool_turn(text, image, run, use_tools);
        self.finish();
        result
    }

    fn tool_turn(&self, text: &str, image: Option<&str>, run: ToolTurn<'_>, use_tools: bool) -> Result<Option<String>, TurnBusy> {
        let turn_id = format!("tu_{}", self.turn_counter.fetch_add(1, Ordering::SeqCst) + 1);
        let sink = Sink {
            app: self,
            turn: turn_id.clone(),
            cancelled: Arc::clone(&run.cancelled),
            counter: AtomicUsize::new(0),
            reply: Mutex::new(None),
        };
        sink.completed(Item::UserMessage(UserMessageItem {
            item_id: format!("{turn_id}/user"),
            text: text.to_owned(),
            status: ItemStatus::Completed,
        }));
        // The tool loop owns model history. The sink writes display items only.
        let (history, summary, previous) = {
            let mut active = self.active.lock().unwrap();
            active.messages.push(Msg::user(text));
            (active.messages.clone(), active.summary.clone(), active.previous_response_id.clone())
        };

        let project = run.project.filter(|_| use_tools && tool_gate::decide(true, run.project).is_active());
        let root = project.and_then(|project| project.folder_path.as_deref());
        let read = project.is_some_and(|project| project.is_granted(FolderPermission::Read));
        let write = project.is_some_and(|project| project.is_granted(FolderPermission::Write));
        let exec = project.is_some_and(|project| project.is_granted(FolderPermission::Execute));
        let s = run.settings;
        let git_writes = read && write;
        let git = if git_writes { GitTool::with_writes() } else { GitTool::read_only() };
        let mut tools: Vec<Box<dyn Tool + Send + Sync>> = Vec::new();
        if read {
            tools.push(Box::new(ReadFileTool));
            tools.push(Box::new(git));
        }
        if write {
            tools.push(Box::new(EditFileTool));
        }
        if exec {
            tools.push(Box::new(ExecuteTool::for_project(git)));
        }
        if use_tools {
            tools.extend(run.servers.tools(&s.mcp_servers, &run.cancelled));
        }
        let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
        let extra: Vec<ExtraTool> = tools
            .iter()
            .filter(|tool| tool.name().starts_with("mcp__"))
            .map(|tool| ExtraTool {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                parameters: tool.parameters().to_string(),
            })
            .collect();
        let mut system = s.resolved_system_prompt().to_owned();
        if use_tools {
            system.push_str("\n\n");
            system.push_str(&protocol_for(&names, &extra, git_writes));
        }
        let registry = ToolRegistry::new(tools);
        let ctx = tool_context(run, root, read, write, exec);
        let choices = self.choices.lock().unwrap().clone();
        let backend = self.backend();
        let limits = Self::context_limits(&*backend, s, &choices);
        let previous = previous.as_deref().filter(|_| Self::server_session_eligible(s, &limits));
        let context_ceiling = if limits.strategy == ContextStrategy::StopAtLimit {
            limits.context_ceiling()
        } else {
            0
        };
        let source = Source {
            backend: &*backend,
            choices,
            system,
            replay: Mutex::new(Replay { summary, folded: 0, prefix: Vec::new(), initial_len: history.len() }),
            limits,
            cancelled: Arc::clone(&ctx.1),
            compacting: &self.auto_compacting,
        };
        let outcome = tokio::runtime::Builder::new_current_thread().build().expect("local tool runtime").block_on(
            ToolLoop::new(&source, &registry, s.max_tool_iterations)
                .chain_response_id(previous.is_some())
                .context_ceiling(context_ceiling)
                .run(&mut LoopRun {
                    history: &history,
                    ctx: &ctx.0,
                    previous_response_id: previous,
                    image,
                    on_delta: Box::new(|_| {}),
                    on_reasoning: Box::new(|_| {}),
                    on_message: Box::new(|_| {}),
                    sink: &sink,
                }),
        );
        let (messages, response_id, usage, final_event, mut failure) = match outcome {
            LoopOutcome::Completed { messages, final_turn, .. } => {
                (messages, final_turn.response_id, final_turn.usage, final_turn.final_event, final_turn.incomplete_reason)
            }
            LoopOutcome::CapReached { messages, incomplete_reason, .. } => {
                (messages, None, None, None, Some(incomplete_reason))
            }
        };
        {
            let mut active = self.active.lock().unwrap();
            active.messages.extend(messages.into_iter().filter(|msg| !msg.content.trim().is_empty()));
            if baudet_core::error::looks_like_context_overflow(failure.as_deref()) {
                // The server's last response may predate the locally preserved tool results.
                active.previous_response_id = None;
            }
        }
        let replay = source.replay.into_inner().unwrap();
        if replay.folded > 0 {
            let retained = {
                let active = self.active.lock().unwrap();
                active.messages.strip_prefix(replay.prefix.as_slice()).map(|tail| replay.retained(tail, text).into_owned())
            };
            let saved = retained.ok_or_else(|| "the compacted history changed before it could be saved".to_owned())
                .and_then(|retained| self.commit_compacted_context(&retained, replay.summary.as_deref().unwrap()));
            if let Err(error) = saved {
                failure = Some(format!("Automatic compaction could not be saved: {error}"));
            }
        }
        if !ctx.1.load(Ordering::Relaxed) {
            self.apply(&ThreadEvent::TurnCompleted {
                thread_id: "th_1".into(),
                turn_id,
                response_id,
                usage,
                final_event,
                incomplete_reason: failure.clone(),
            });
        }
        Ok(failure)
    }
}

fn tool_context(run: ToolTurn<'_>, root: Option<&str>, read: bool, write: bool, exec: bool) -> (ToolContext, Arc<AtomicBool>) {
    let mut ctx = root.map_or_else(ToolContext::default, |root| ToolContext::for_folder(root, read, write, exec));
    let s = run.settings;
    let approval = Arc::clone(&run.approval);
    let stopped = Arc::clone(&run.cancelled);
    let automatic = s.tool_mode == ToolMode::Auto;
    ctx.approve = Box::new(move |path, diff| {
        if stopped.load(Ordering::Relaxed) {
            return false;
        }
        let allowed = automatic
            || matches!(
                approval(ApprovalRequest::ApplyPatch { path: path.into(), diff: Some(diff.into()) }),
                ApprovalDecision::Accept
            );
        allowed && !stopped.load(Ordering::Relaxed)
    });
    let approval = Arc::clone(&run.approval);
    let stopped = Arc::clone(&run.cancelled);
    let pinned = Arc::new(Mutex::new(s.auto_allow_commands.clone()));
    let approval_pins = Arc::clone(&pinned);
    ctx.approve_command = Box::new(move |command, cwd, timeout, pin| {
        if stopped.load(Ordering::Relaxed) {
            return false;
        }
        match approval(ApprovalRequest::RunCommand {
            command: command.into(),
            cwd: Some(cwd.into()),
            timeout_seconds: i32::try_from(timeout).ok(),
            always_key: Some(pin.into()),
        }) {
            ApprovalDecision::AcceptForSession if !stopped.load(Ordering::Relaxed) => {
                approval_pins.lock().unwrap().push(pin.to_owned());
                true
            }
            ApprovalDecision::Accept => !stopped.load(Ordering::Relaxed),
            _ => false,
        }
    });
    let approval = run.approval;
    let stopped = Arc::clone(&run.cancelled);
    let allowed = Mutex::new(s.mcp_allow_servers.clone());
    ctx.approve_mcp = Box::new(move |server, tool, args| {
        if stopped.load(Ordering::Relaxed) {
            return false;
        }
        if allowed.lock().unwrap().iter().any(|id| id == server) {
            return true;
        }
        match approval(ApprovalRequest::McpTool {
            server_id: server.into(),
            tool: tool.into(),
            arguments: args.to_string(),
        }) {
            ApprovalDecision::Accept => true,
            ApprovalDecision::AcceptForSession => {
                allowed.lock().unwrap().push(server.into());
                true
            }
            _ => false,
        }
    });
    ctx.auto_approve_safe = s.auto_approve_safe_commands;
    ctx.auto_allow_provider = Box::new(move || pinned.lock().unwrap().clone());
    let trusted = s.trusted_commands.clone();
    ctx.trusted_commands_provider = Box::new(move || trusted.clone());
    (ctx, run.cancelled)
}

struct Replay {
    summary: Option<String>,
    folded: usize,
    prefix: Vec<Msg>,
    initial_len: usize,
}

impl Replay {
    fn retained<'a>(&self, tail: &'a [Msg], user_text: &str) -> std::borrow::Cow<'a, [Msg]> {
        if self.folded < self.initial_len {
            return std::borrow::Cow::Borrowed(tail);
        }
        let mut retained = Vec::with_capacity(tail.len() + 1);
        // Keep the active request verbatim in its original user role, ahead of tool results.
        retained.push(Msg::user(user_text));
        retained.extend_from_slice(tail);
        std::borrow::Cow::Owned(retained)
    }
}

struct Source<'a> {
    backend: &'a dyn Backend,
    choices: RequestChoices,
    system: String,
    replay: Mutex<Replay>,
    limits: baudet_store::ContextLimits,
    cancelled: Arc<AtomicBool>,
    compacting: &'a AtomicBool,
}

impl Source<'_> {
    fn compact(&self, replay: &mut Replay, history: &[Msg]) -> Result<(), String> {
        let remaining = &history[replay.folded..];
        if remaining.len() < 2 {
            return Err(baudet_core::error::CONTEXT_LIMIT_REASON.to_owned());
        }
        let mut plan = CompactionPlan::from(&self.limits);
        plan.keep_recent = plan.keep_recent.clamp(1, remaining.len() / 2);
        self.compacting.store(true, Ordering::Relaxed);
        let outcome = Compactor::fold(&plan, remaining, replay.summary.as_deref(), |text, max_tokens| {
            if self.cancelled.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            Ok(summarize(self.backend, self.choices.model.clone(), text, max_tokens, || {
                self.cancelled.load(Ordering::Relaxed)
            }))
        });
        self.compacting.store(false, Ordering::Relaxed);
        if self.cancelled.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        match outcome? {
            CompactionOutcome::Compacted { summary, folded } => {
                replay.folded += folded;
                replay.prefix = history[..replay.folded].iter().enumerate()
                    .filter(|(index, msg)| *index < replay.initial_len
                        || (!msg.content.trim().is_empty()
                            && !(msg.role == "user" && msg.content == baudet_tool::tool_loop::BLANK_NUDGE)))
                    .map(|(_, msg)| msg.clone())
                    .collect();
                replay.summary = Some(summary);
                Ok(())
            }
            CompactionOutcome::NothingToFold => Err(baudet_core::error::CONTEXT_LIMIT_REASON.to_owned()),
            CompactionOutcome::OversizedTurn { .. } => Err("Automatic compaction could not fit the oldest message".into()),
            CompactionOutcome::SummaryRefused(reason) => Err(format!("Automatic compaction failed: {reason}")),
        }
    }
}

impl TurnSource for Source<'_> {
    fn next_turn<'a>(&'a self, mut request: TurnRequest<'a>) -> TurnFuture<'a> {
        Box::pin(async move {
            let mut replay = self.replay.lock().unwrap();
            let auto = self.limits.strategy == ContextStrategy::AutoCompact;
            let tools = request.tools.map(serde_json::Value::to_string);
            let mut retried = false;
            loop {
                if self.cancelled.load(Ordering::Relaxed) {
                    return failed("cancelled");
                }
                let active_request = &request.history[replay.initial_len - 1].content;
                let replay_history = replay.retained(&request.history[replay.folded..], active_request);
                let previous = request.previous_response_id.filter(|_| !auto);
                let (history, user_text) = baudet_tool::transcript::for_loop(
                    &replay_history,
                    self.limits.strategy,
                    self.limits.trim_budget(100),
                    previous.is_some(),
                    self.limits.keep_recent_messages,
                );
                let trimmed = history.len() + 1 < replay_history.len() && previous.is_none();
                let system = baudet_store::instructions(&self.system, replay.summary.as_deref(), trimmed);
                if !self.limits.strategy.trims_history() {
                    let chars = replay_history.iter().map(baudet_store::cost_chars).sum::<usize>()
                        + system.chars().count()
                        + tools.as_ref().map_or(0, |tools| tools.chars().count());
                    let tokens = chars.div_ceil(3) as u64;
                    let over = self.limits.over(tokens, self.limits.hard_ceiling());
                    if auto && (over || (request.history.len() - replay.folded > self.limits.keep_recent_messages.max(1)
                        && self.limits.over(tokens, self.limits.fold_ceiling())))
                    {
                        match self.compact(&mut replay, request.history) {
                            Ok(()) => continue,
                            Err(error) => return failed(error),
                        }
                    }
                    if over {
                        return failed(baudet_core::error::CONTEXT_LIMIT_REASON);
                    }
                }

                let emitted = std::cell::Cell::new(false);
                let turn = receive_turn(self.backend, SendRequest {
                    turn_id: None,
                    system,
                    user_text,
                    history: history_events(&history),
                    previous_response_id: previous.map(str::to_owned),
                    tools: tools.clone(),
                    image: request.image.map(str::to_owned),
                    model: self.choices.model.clone(),
                    reasoning: self.choices.reasoning.clone(),
                    max_output_tokens: self.choices.max_output_tokens,
                    store: None,
                }, || self.cancelled.load(Ordering::Relaxed), &mut |delta| {
                    emitted.set(true);
                    (request.on_delta)(delta);
                }, &mut |delta| {
                    emitted.set(true);
                    (request.on_reasoning)(delta);
                });
                let overflow = baudet_core::error::looks_like_context_overflow(turn.incomplete_reason.as_deref());
                if auto && overflow && !retried && !emitted.get() && turn.text.is_empty() && request.history.len() - replay.folded > 1 {
                    if let Err(error) = self.compact(&mut replay, request.history) {
                        return failed(error);
                    }
                    retried = true;
                    continue;
                }
                return turn;
            }
        })
    }
}

struct Sink<'a> {
    app: &'a AppState,
    turn: String,
    cancelled: Arc<AtomicBool>,
    counter: AtomicUsize,
    reply: Mutex<Option<(String, String)>>,
}

impl Sink<'_> {
    fn started(&self, item: Item) {
        self.app.apply(&ThreadEvent::ItemStarted { thread_id: "th_1".into(), turn_id: self.turn.clone(), item });
    }

    // Do not fold this display item into model history a second time.
    fn completed(&self, item: Item) {
        let mut active = self.app.active.lock().unwrap();
        let mut rows = self.app.rows.lock().unwrap();
        crate::Emitter::storing(&mut rows, &mut active.items, "th_1", &self.turn).completed(item);
    }

    fn delta(&self, id: &str, kind: ItemKind, delta: &str) {
        self.app.apply(&ThreadEvent::ItemDelta {
            thread_id: "th_1".into(),
            turn_id: self.turn.clone(),
            item_id: id.into(),
            kind,
            delta: delta.into(),
        });
    }
}

impl ItemSink for Sink<'_> {
    fn next_item_id(&self) -> String {
        format!("{}/tool/{}", self.turn, self.counter.fetch_add(1, Ordering::Relaxed))
    }
    fn reply_started(&self) {
        let reply = self.next_item_id();
        let reasoning = self.next_item_id();
        *self.reply.lock().unwrap() = Some((reply, reasoning));
    }
    fn text_delta(&self, chunk: &str) {
        if let Some((id, _)) = self.reply.lock().unwrap().as_ref() {
            if self.app.rows.lock().unwrap().get(id).is_none() {
                self.started(Item::AgentMessage(AgentMessageItem {
                    item_id: id.clone(),
                    text: String::new(),
                    status: ItemStatus::InProgress,
                }));
            }
            self.delta(id, ItemKind::AgentMessage, chunk);
        }
    }
    fn reasoning_delta(&self, chunk: &str) {
        if let Some((_, id)) = self.reply.lock().unwrap().as_ref() {
            if self.app.rows.lock().unwrap().get(id).is_none() {
                self.started(Item::Reasoning(ReasoningItem {
                    item_id: id.clone(),
                    text: String::new(),
                    status: ItemStatus::InProgress,
                }));
            }
            self.delta(id, ItemKind::Reasoning, chunk);
        }
    }
    fn reply_completed(&self, text: &str) {
        if let Some((id, reasoning)) = self.reply.lock().unwrap().take() {
            let status =
                if self.cancelled.load(Ordering::Relaxed) { ItemStatus::Interrupted } else { ItemStatus::Completed };
            let thought = self.app.rows.lock().unwrap().get(&reasoning).cloned();
            if let Some(item) = thought {
                self.completed(item.with_status(status));
            }
            if !text.is_empty() {
                self.completed(Item::AgentMessage(AgentMessageItem { item_id: id, text: text.to_owned(), status }));
            }
        }
    }
    fn command_started(&self, item: CommandExecutionItem) {
        self.started(Item::CommandExecution(item));
    }
    fn command_completed(&self, item: CommandExecutionItem) {
        self.completed(Item::CommandExecution(item));
    }
    fn file_change_started(&self, item: FileChangeItem) {
        self.started(Item::FileChange(item));
    }
    fn file_change_completed(&self, item: FileChangeItem) {
        self.completed(Item::FileChange(item));
    }
    fn tool_call_started(&self, item: ToolCallItem) {
        self.started(Item::ToolCall(item));
    }
    fn tool_call_completed(&self, item: ToolCallItem) {
        self.completed(Item::ToolCall(item));
    }
    fn image_completed(&self, item: ImageItem) {
        self.completed(Item::Image(item));
    }
}
