use super::{history_events, AppState, RequestChoices, TurnBusy};
use baudet_backend::{Backend, SendRequest};
use baudet_core::{
    AgentMessageItem, ApprovalDecision, ApprovalRequest, CommandExecutionItem, FileChangeItem, ImageItem, Item,
    ItemKind, ItemStatus, Msg, ReasoningItem, ThreadEvent, ToolCallItem, Turn, UserMessageItem,
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
use std::time::Duration;

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
        self.live_turns.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).map_err(|_| TurnBusy)?;
        let result = self.tool_turn(text, image, run);
        self.finish();
        result
    }

    fn tool_turn(&self, text: &str, image: Option<&str>, run: ToolTurn<'_>) -> Result<Option<String>, TurnBusy> {
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

        let project = run.project.filter(|_| tool_gate::decide(true, run.project).is_active());
        let root = project.and_then(|project| project.folder_path.as_deref());
        let read = project.is_some_and(|project| project.is_granted(FolderPermission::Read));
        let write = project.is_some_and(|project| project.is_granted(FolderPermission::Write));
        let exec = project.is_some_and(|project| project.is_granted(FolderPermission::Execute));
        let s = run.settings;
        let git_writes = read && write && s.git_allow_writes;
        let mut tools: Vec<Box<dyn Tool + Send + Sync>> = Vec::new();
        if read {
            tools.push(Box::new(ReadFileTool));
            tools.push(Box::new(if git_writes {
                GitTool::with_writes()
            } else {
                GitTool::read_only()
            }));
        }
        if write {
            tools.push(Box::new(EditFileTool));
        }
        if exec {
            tools.push(Box::new(ExecuteTool::for_project()));
        }
        tools.extend(run.servers.tools(&s.mcp_servers, &run.cancelled));
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
        let system = format!("{}\n\n{}", s.resolved_system_prompt(), protocol_for(&names, &extra, git_writes));
        let registry = ToolRegistry::new(tools);
        let ctx = tool_context(run, root, read, write, exec);
        let choices = self.choices.lock().unwrap().clone();
        let backend = self.backend();
        let limits = Self::context_limits(&*backend, s, &choices);
        let previous = previous.as_deref().filter(|_| Self::server_session_eligible(s, &limits));
        let source =
            Source { backend: &*backend, choices, system, summary, limits, cancelled: Arc::clone(&ctx.1) };
        let outcome = tokio::runtime::Builder::new_current_thread().build().expect("local tool runtime").block_on(
            ToolLoop::new(&source, &registry, s.max_tool_iterations)
                .chain_response_id(previous.is_some())
                .context_ceiling(source.limits.context_ceiling())
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
        let (messages, response_id, usage, final_event, failure) = match outcome {
            LoopOutcome::Completed { messages, final_turn, .. } => {
                (messages, final_turn.response_id, final_turn.usage, final_turn.final_event, final_turn.incomplete_reason)
            }
            LoopOutcome::CapReached { messages, incomplete_reason, .. } => {
                (messages, None, None, None, Some(incomplete_reason))
            }
        };
        self.active.lock().unwrap().messages.extend(messages.into_iter().filter(|msg| !msg.content.trim().is_empty()));
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

struct Source<'a> {
    backend: &'a dyn Backend,
    choices: RequestChoices,
    system: String,
    summary: Option<String>,
    limits: baudet_store::ContextLimits,
    cancelled: Arc<AtomicBool>,
}

fn failed(detail: impl Into<String>) -> Turn {
    Turn { final_event: None, incomplete_reason: Some(detail.into()), ..Turn::text("") }
}

impl TurnSource for Source<'_> {
    fn next_turn<'a>(&'a self, mut request: TurnRequest<'a>) -> TurnFuture<'a> {
        Box::pin(async move {
            if self.cancelled.load(Ordering::Relaxed) {
                return failed("cancelled");
            }
            let (history, user_text) = baudet_tool::transcript::for_loop(
                request.history,
                self.limits.strategy,
                self.limits.trim_budget(100),
                request.previous_response_id.is_some(),
                self.limits.keep_recent_messages,
            );
            let trimmed = history.len() + 1 < request.history.len() && request.previous_response_id.is_none();
            let mut handle = match self.backend.send(SendRequest {
                turn_id: None,
                system: baudet_store::instructions(&self.system, self.summary.as_deref(), trimmed),
                user_text,
                history: history_events(&history),
                previous_response_id: request.previous_response_id.map(str::to_owned),
                tools: request.tools.map(serde_json::Value::to_string),
                image: request.image.map(str::to_owned),
                model: self.choices.model.clone(),
                reasoning: self.choices.reasoning.clone(),
                max_output_tokens: self.choices.max_output_tokens,
            }) {
                Ok(handle) => handle,
                Err(error) => return failed(error.to_string()),
            };
            let mut turn = failed("stream ended before completion");
            loop {
                if self.cancelled.load(Ordering::Relaxed) {
                    handle.cancel();
                    return Turn { text: turn.text, ..failed("cancelled") };
                }
                match handle.events().try_recv() {
                    Ok(ThreadEvent::ItemDelta { kind: ItemKind::AgentMessage, delta, .. }) => {
                        turn.text.push_str(&delta);
                        (request.on_delta)(&delta);
                    }
                    Ok(ThreadEvent::ItemDelta { kind: ItemKind::Reasoning, delta, .. }) => {
                        (request.on_reasoning)(&delta)
                    }
                    Ok(ThreadEvent::ItemCompleted { item: Item::AgentMessage(message), .. })
                        if !message.text.is_empty() =>
                    {
                        turn.text = message.text;
                    }
                    Ok(ThreadEvent::TurnCompleted { response_id, usage, final_event, incomplete_reason, .. }) => {
                        if let Some(result) = handle.take_result() {
                            return result;
                        }
                        turn.response_id = response_id;
                        turn.usage = usage;
                        turn.final_event = final_event;
                        turn.incomplete_reason = incomplete_reason;
                        return turn;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(5)),
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return turn,
                }
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
