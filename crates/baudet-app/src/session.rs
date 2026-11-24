
pub mod tool_turn;

use crate::reducer::{reduce, Emitter, Rows};
use baudet_backend::{Backend, SendRequest, TurnHandle};
use baudet_core::{Item, ItemStatus, Msg, ThreadEvent};
use baudet_store::session::Session;
use baudet_store::Transcript;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub struct Sent {
    pub turn_id: String,
    pub messages_before: usize,
    pub handle: Arc<Mutex<TurnHandle>>,
    reader: Mutex<Option<Arc<Mutex<TurnHandle>>>>,
}

impl Sent {
    pub fn take_reader(&self) -> Option<Arc<Mutex<TurnHandle>>> {
        self.reader.lock().unwrap().take()
    }
}

pub struct AppState {
    backend: Mutex<Arc<dyn Backend>>,
    system: String,
    choices: Mutex<RequestChoices>,
    rows: Mutex<Rows>,
    transcript: Mutex<Option<Transcript>>,
    active: Mutex<Session>,
    live_turns: AtomicUsize,
    turn_counter: AtomicUsize,
    project: Mutex<Option<String>>,
    live: Mutex<Option<Arc<Mutex<TurnHandle>>>>,
    last_turn: Mutex<Option<TurnReport>>,
}

#[derive(Clone)]
pub struct TurnReport {
    pub response_id: Option<String>,
    pub usage: Option<baudet_core::Usage>,
    pub final_event: Option<String>,
}

pub struct ContextStatus {
    pub messages: usize,
    pub reading: baudet_store::ContextReading,
    pub server_session: Option<String>,
    pub last_turn: Option<TurnReport>,
}

#[derive(Debug, Clone, Default)]
pub struct RequestChoices {
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a turn is already running; stop it first")]
pub struct TurnBusy;

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error(transparent)]
    Busy(#[from] TurnBusy),
    #[error("the request exceeds the local context limit")]
    ContextLimit,
}

// A turn has exactly one stream reader.
pub fn pump(
    handle: &Arc<Mutex<TurnHandle>>,
    mut apply: impl FnMut(&ThreadEvent) -> bool,
    forward: &mpsc::UnboundedSender<ThreadEvent>,
) {
    loop {
        let next = {
            let mut guard = handle.lock().unwrap();
            guard.events().try_recv()
        };
        match next {
            Ok(event) => {
                apply(&event);
                let _ = forward.send(event);
            }
            Err(mpsc::error::TryRecvError::Empty) => std::thread::sleep(std::time::Duration::from_millis(1)),
            Err(mpsc::error::TryRecvError::Disconnected) => break,
        }
    }
}

// The backend reverses this list before putting it on the wire.
fn history_events(messages: &[Msg]) -> Vec<ThreadEvent> {
    messages
        .iter()
        .filter(|msg| !msg.content.trim().is_empty())
        .rev()
        .map(|msg| {
            let item: Item = if msg.role == "user" {
                Item::UserMessage(baudet_core::UserMessageItem {
                    item_id: "history/user".to_string(),
                    text: msg.content.clone(),
                    status: ItemStatus::Completed,
                })
            } else {
                Item::AgentMessage(baudet_core::AgentMessageItem {
                    item_id: "history/agent".to_string(),
                    text: msg.content.clone(),
                    status: ItemStatus::Completed,
                })
            };
            ThreadEvent::ItemCompleted { thread_id: String::new(), turn_id: String::new(), item }
        })
        .collect()
}

fn msg_of(item: &Item) -> Option<Msg> {
    match item {
        Item::UserMessage(message) if !message.text.trim().is_empty() => Some(Msg::user(message.text.clone())),
        Item::AgentMessage(message) if !message.text.trim().is_empty() => Some(Msg::assistant(message.text.clone())),
        _ => None,
    }
}

impl AppState {
    // Commit storage before changing the conversation in memory.
    pub fn compact(&self, plan: &crate::compaction::CompactionPlan) -> Result<crate::compaction::CompactionOutcome, String> {
        use crate::compaction::{CompactionOutcome, Compactor, SUMMARY_PROMPT};

        self.live_turns.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).map_err(|_| TurnBusy.to_string())?;
        let result = (|| {
            let (messages, summary) = {
                let active = self.active.lock().unwrap();
                (active.messages.clone(), active.summary.clone())
            };
            let backend = self.backend();
            let outcome = Compactor::fold(plan, &messages, summary.as_deref(), |source, cap| {
                backend.complete_once(SUMMARY_PROMPT, source, cap).map_err(|error| error.to_string())
            })?;
            if let CompactionOutcome::Compacted { summary, folded } = &outcome {
                let retained = &messages[*folded..];
                let mut transcript = self.transcript.lock().unwrap();
                let mut active = self.active.lock().unwrap();
                if let Some(transcript) = transcript.as_mut() {
                    transcript.compact_context(retained, summary).map_err(|error| error.to_string())?;
                    active.persisted_messages = retained.len();
                }
                active.messages = retained.to_vec();
                active.summary = Some(summary.clone());
                active.previous_response_id = None;
                *self.last_turn.lock().unwrap() = None;
            }
            Ok(outcome)
        })();
        self.finish();
        result
    }

    pub fn new(backend: Arc<dyn Backend>, system: impl Into<String>) -> AppState {
        AppState {
            backend: Mutex::new(backend),
            system: system.into(),
            choices: Mutex::new(RequestChoices::default()),
            rows: Mutex::new(Rows::new()),
            transcript: Mutex::new(None),
            active: Mutex::new(Session::new("", "")),
            live_turns: AtomicUsize::new(0),
            turn_counter: AtomicUsize::new(0),
            live: Mutex::new(None),
            last_turn: Mutex::new(None),
            project: Mutex::new(None),
        }
    }

    fn backend(&self) -> Arc<dyn Backend> {
        Arc::clone(&self.backend.lock().unwrap())
    }

    pub fn replace_backend(&self, backend: Arc<dyn Backend>) {
        *self.last_turn.lock().unwrap() = None;
        *self.backend.lock().unwrap() = backend;
        self.active.lock().unwrap().previous_response_id = None;
    }

    pub fn set_request_choices(
        &self,
        model: Option<String>,
        reasoning: Option<String>,
        max_output_tokens: Option<u32>,
    ) {
        *self.choices.lock().unwrap() = RequestChoices { model, reasoning, max_output_tokens };
    }

    pub fn rows(&self) -> Vec<Item> {
        self.rows.lock().unwrap().ordered().into_iter().cloned().collect()
    }

    pub fn live_turns(&self) -> usize {
        self.live_turns.load(Ordering::SeqCst)
    }

    pub fn send(&self, user_text: &str) -> Result<Sent, TurnBusy> {
        self.send_with_image(user_text, None)
    }

    pub fn send_with_image(&self, user_text: &str, image: Option<String>) -> Result<Sent, TurnBusy> {
        self.send_inner(user_text, image, None).map_err(|_| TurnBusy)
    }

    pub fn send_with_context(
        &self,
        user_text: &str,
        image: Option<String>,
        settings: &baudet_settings::Settings,
    ) -> Result<Sent, SendError> {
        self.send_inner(user_text, image, Some(settings))
    }

    pub fn context_key(&self) -> Option<String> {
        let choices = self.choices.lock().unwrap();
        let backend = self.backend();
        let (url, model) = Self::context_endpoint(&*backend, &choices)?;
        (!model.is_empty()).then(|| baudet_store::window_key(url, model))
    }

    fn context_endpoint<'a>(backend: &'a dyn Backend, choices: &'a RequestChoices) -> Option<(&'a str, &'a str)> {
        let (url, default_model) = backend.context_endpoint()?;
        let model = choices.model.as_deref().filter(|value| !value.trim().is_empty()).unwrap_or(default_model);
        Some((url, model))
    }

    fn context_limits(
        backend: &dyn Backend,
        settings: &baudet_settings::Settings,
        choices: &RequestChoices,
    ) -> baudet_store::ContextLimits {
        let mut limits = match Self::context_endpoint(backend, choices) {
            Some((url, model)) => settings.context_limits_for(url, model),
            None => settings.context_limits(),
        };
        limits.max_output_tokens = choices.max_output_tokens.unwrap_or(0);
        limits
    }

    pub fn context_status(&self, settings: &baudet_settings::Settings, tools_active: bool) -> ContextStatus {
        let choices = self.choices.lock().unwrap().clone();
        let limits = Self::context_limits(&*self.backend(), settings, &choices);
        let active = self.active.lock().unwrap();
        let mut reading = limits.context_reading(&active.messages);
        if reading.trimming {
            reading.used += active.summary.as_deref().map_or(0, |summary| summary.chars().count().div_ceil(3) as u64);
        } else {
            reading.used = active.estimate_tokens();
        }
        if reading.budget != 0 {
            reading.percent = reading.used.saturating_mul(100) / u64::from(reading.budget);
        }
        let server_session = if tools_active && Self::server_session_eligible(settings, &limits) {
            active.previous_response_id.clone()
        } else {
            None
        };
        ContextStatus {
            messages: active.messages.len(),
            reading,
            server_session,
            last_turn: self.last_turn.lock().unwrap().clone(),
        }
    }

    fn server_session_eligible(settings: &baudet_settings::Settings, limits: &baudet_store::ContextLimits) -> bool {
        settings.use_server_session
            && settings.use_responses_api
            && settings.store_responses
            && settings.protocol_word() != Some("chat_completions")
            && limits.strategy.supports_server_session()
    }

    fn send_inner(
        &self,
        user_text: &str,
        image: Option<String>,
        settings: Option<&baudet_settings::Settings>,
    ) -> Result<Sent, SendError> {
        self.live_turns.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).map_err(|_| TurnBusy)?;
        let choices = self.choices.lock().unwrap().clone();
        let backend = self.backend();
        let thread_id = "th_1".to_string();
        let (turn_id, history, system) = {
            let mut active = self.active.lock().unwrap();
            let summary = active.summary.as_deref();
            let (history, system) = match settings {
                Some(settings) => {
                    let limits = Self::context_limits(&*backend, settings, &choices);
                    match Self::chat_context(
                        settings.resolved_system_prompt(), &active.messages, summary, user_text, &limits,
                    ) {
                        Ok(context) => context,
                        Err(error) => {
                            self.finish();
                            return Err(error);
                        }
                    }
                }
                None => (history_events(&active.messages), baudet_store::instructions(&self.system, summary, false)),
            };
            let turn_id = format!("tu_{}", self.turn_counter.fetch_add(1, Ordering::SeqCst) + 1);
            let question = Item::UserMessage(baudet_core::UserMessageItem {
                item_id: format!("{turn_id}/user"),
                text: user_text.to_string(),
                status: ItemStatus::Completed,
            });
            active.messages.push(Msg::user(user_text));
            let mut rows = self.rows.lock().unwrap();
            let mut emitter =
                Emitter::storing(&mut rows, &mut active.items, &thread_id, &turn_id);
            emitter.started(question.clone());
            emitter.completed(question);
            (turn_id, history, system)
        };

        let request = SendRequest {
            turn_id: Some(turn_id.clone()),
            system,
            user_text: user_text.to_string(),
            history,
            previous_response_id: None,
            tools: None,
            image,
            max_output_tokens: choices.max_output_tokens,
            model: choices.model.clone(),
            reasoning: choices.reasoning.clone(),
        };

        let handle = match backend.send(request) {
            Ok(handle) => Arc::new(Mutex::new(handle)),
            Err(_) => {
                self.live_turns.fetch_sub(1, Ordering::SeqCst);
                return Err(TurnBusy.into());
            }
        };
        *self.live.lock().unwrap() = Some(Arc::clone(&handle));

        Ok(Sent {
            turn_id,
            messages_before: self.active.lock().unwrap().messages.len() - 1,
            handle: Arc::clone(&handle),
            reader: Mutex::new(Some(handle)),
        })
    }

    fn chat_context(
        base_prompt: &str,
        prior: &[Msg],
        summary: Option<&str>,
        user_text: &str,
        limits: &baudet_store::ContextLimits,
    ) -> Result<(Vec<ThreadEvent>, String), SendError> {
        use baudet_store::{cost_chars, instructions, ContextStrategy};

        let first = if limits.strategy.trims_by_turn_count() {
            prior.len().saturating_sub(limits.keep_recent_messages.saturating_sub(1))
        } else {
            0
        };
        let prior = &prior[first..];
        let prompt_chars = cost_chars(&Msg::user(user_text));
        let prior_chars = prior.iter().map(cost_chars).sum::<usize>();
        let mut system = instructions(base_prompt, summary, first > 0);
        let budget = limits.trim_budget(100);
        let trim = !prior.is_empty()
            && limits.strategy != ContextStrategy::StopAtLimit
            && limits.over((prompt_chars + prior_chars + system.chars().count()).div_ceil(3) as u64, budget as u32);
        if trim {
            system = instructions(base_prompt, summary, true);
        }
        let input_tokens = (prompt_chars + system.chars().count()).div_ceil(3);
        let history_tokens = if limits.strategy == ContextStrategy::StopAtLimit {
            prior_chars.div_ceil(3)
        } else {
            0
        };
        if limits.over((input_tokens + history_tokens) as u64, limits.hard_ceiling()) {
            return Err(SendError::ContextLimit);
        }
        if !trim {
            return Ok((history_events(prior), system));
        }
        let room = budget.saturating_sub(input_tokens + limits.max_output_tokens as usize);
        let history = if room == 0 {
            Vec::new()
        } else {
            let bounded = baudet_store::bound(prior, limits.strategy, room, limits.keep_recent_messages);
            if bounded.iter().map(cost_chars).sum::<usize>() <= room.saturating_mul(3) {
                history_events(&bounded)
            } else {
                Vec::new()
            }
        };
        Ok((history, system))
    }

    pub fn emit(&self, item: &Item) {
        let mut active = self.active.lock().unwrap();
        let mut rows = self.rows.lock().unwrap();
        if rows.get(item.item_id()).is_some() {
            rows.replace_for_screen(item.clone());
            return;
        }
        let mut emitter = Emitter::storing(&mut rows, &mut active.items, "", "");
        emitter.started(item.clone());
        if item.status() == ItemStatus::Completed {
            emitter.completed(item.clone());
        }
        if let Some(msg) = msg_of(item) {
            active.messages.push(msg);
        }
    }

    pub fn apply(&self, event: &ThreadEvent) -> bool {
        let mut active = self.active.lock().unwrap();
        if let ThreadEvent::TurnCompleted { response_id, usage, final_event, .. } = event {
            *self.last_turn.lock().unwrap() = Some(TurnReport {
                response_id: response_id.clone(), usage: *usage, final_event: final_event.clone(),
            });
        }
        let mut rows = self.rows.lock().unwrap();
        let mut emitter = Emitter::storing(&mut rows, &mut active.items, "", "");
        match event {
            ThreadEvent::ItemStarted { item, .. } => emitter.started(item.clone()),
            ThreadEvent::ItemDelta { item_id, kind, delta, .. } => emitter.delta(item_id, *kind, delta),
            ThreadEvent::ItemCompleted { item, .. } => {
                let kept = emitter.completed(item.clone());
                let Some(row) = rows.get(item.item_id()) else { return kept };
                if let Some(msg) = msg_of(row) {
                    // Completion may supply the first text for this item. Append it without touching prior replies.
                    active.messages.push(msg);
                }
                kept
            }
            ThreadEvent::TurnCompleted { response_id: Some(id), incomplete_reason: None, .. } => {
                active.previous_response_id = Some(id.clone());
                reduce(&mut rows, event)
            }
            other => reduce(&mut rows, other),
        }
    }

    pub fn paging(&self) -> (bool, Option<u64>, u64) {
        let active = self.active.lock().unwrap();
        (active.has_more_older(), active.oldest_loaded_seq, active.items.len() as u64)
    }

    pub fn transcript_len(&self) -> (usize, usize) {
        let active = self.active.lock().unwrap();
        (active.items.len(), active.messages.len())
    }

    pub fn load_older(&self, before_seq: u64) -> Result<usize, String> {
        let window = {
            let transcript = self.transcript.lock().unwrap();
            let transcript = transcript.as_ref().ok_or("dit gesprek heeft geen opslag-map")?;
            transcript
                .items_window(baudet_store::repository::page::SCROLL, Some(before_seq))
                .map_err(|error| error.to_string())?
        };
        Ok(self.prepend_older(window.items, window.older_seq))
    }

    pub fn prepend_older(&self, mut older: Vec<Item>, older_seq: Option<u64>) -> usize {
        let mut active = self.active.lock().unwrap();
        let held: Vec<String> = active.items.iter().map(|row| row.item_id().to_string()).collect();
        older.retain(|row| !held.iter().any(|id| id == row.item_id()));
        let new_ids: Vec<String> = older.iter().map(|row| row.item_id().to_string()).collect();
        let added = older.len();
        if added == 0 {
            active.oldest_loaded_seq = older_seq;
            return 0;
        }
        for row in older.into_iter().rev() {
            active.items.insert(0, row);
        }
        active.oldest_loaded_seq = older_seq;
        let mut rows = self.rows.lock().unwrap();
        // Prepending in reverse preserves chronological order.
        for id in new_ids.iter().rev() {
            let Some(row) = active.items.iter().find(|held| held.item_id() == id) else { continue };
            rows.prepend_for_screen(row.clone());
        }
        added
    }

    pub fn adopt(&self, session: Session) {
        *self.last_turn.lock().unwrap() = None;
        let items = session.items.clone();
        *self.active.lock().unwrap() = session;
        self.draw(&items);
    }

    fn draw(&self, items: &[Item]) {
        let mut rows = self.rows.lock().unwrap();
        rows.clear();
        for item in items.iter().rev() {
            rows.prepend_for_screen(item.clone());
        }
    }

    pub fn build_session(&self) -> Result<Session, String> {
        let transcript = self.transcript.lock().unwrap();
        let transcript = transcript.as_ref().ok_or("dit gesprek heeft geen opslag-map")?;
        let mut session = transcript.build(&self.model()).map_err(|error| error.to_string())?;
        if session.model.trim().is_empty() {
            session.model = self.model();
        }
        if session.project_id.is_none() {
            session.project_id = self.project.lock().unwrap().clone();
        }
        Ok(session)
    }

    pub fn open_store(
        &self,
        root: &std::path::Path,
        session_id: &str,
        model: &str,
        project_id: Option<&str>,
    ) -> Result<(Transcript, Session), String> {
        let opened = Transcript::open_or_create(root, session_id, Some(model), None)
            .map_err(|error| error.to_string())?;
        let mut session = match opened.build(model) {
            Ok(session) => session,
            Err(_) => Session::new(session_id, model),
        };
        if session.model.trim().is_empty() {
            session.model = model.to_owned();
        }
        if session.project_id.is_none() {
            session.project_id = project_id.map(str::to_owned);
        }
        Ok((opened, session))
    }

    fn highest_turn_index(items: &[Item]) -> usize {
        items
            .iter()
            .filter_map(|item| {
                let rest = item.item_id().strip_prefix("tu_")?;
                let digits = rest.split('/').next()?;
                digits.parse::<usize>().ok()
            })
            .max()
            .unwrap_or(0)
    }

    pub fn switch_to(&self, transcript: Transcript, mut session: Session) -> Option<String> {
        *self.last_turn.lock().unwrap() = None;
        if session.project_id.is_none() {
            session.project_id = self.project.lock().unwrap().clone();
        }
        let project = session.project_id.clone();
        *self.project.lock().unwrap() = project.clone();
        let items = session.items.clone();
        self.turn_counter.fetch_max(Self::highest_turn_index(&items), Ordering::SeqCst);
        *self.transcript.lock().unwrap() = Some(transcript);
        *self.active.lock().unwrap() = session;
        self.draw(&items);
        project
    }

    pub fn project(&self) -> Option<String> {
        self.project.lock().unwrap().clone()
    }

    pub fn switch_project(&self, project_id: Option<String>) {
        *self.project.lock().unwrap() = project_id;
    }

    pub fn model(&self) -> String {
        self.choices.lock().unwrap().model.clone().unwrap_or_default()
    }

    pub fn session_id(&self) -> String {
        self.active.lock().unwrap().session_id.clone()
    }

    pub fn clear_session(&self) {
        *self.transcript.lock().unwrap() = None;
        self.adopt(Session::new("", self.model()));
    }

    pub fn disk_items(&self) -> Option<usize> {
        let transcript = self.transcript.lock().unwrap();
        let transcript = transcript.as_ref()?;
        transcript.item_count().ok()
    }

    pub fn persists(&self) -> bool {
        self.transcript.lock().unwrap().is_some()
    }

    pub fn persist(&self) -> Option<String> {
        let mut transcript = self.transcript.lock().unwrap();
        let transcript = transcript.as_mut()?;
        let mut active = self.active.lock().unwrap();
        transcript.persist(&mut active).err().map(|error| error.to_string())
    }

    pub fn cancel_turn(&self) {
        if let Some(handle) = self.live.lock().unwrap().as_ref() {
            handle.lock().unwrap().cancel();
        }
        let mut rows = self.rows.lock().unwrap();
        let open: Vec<String> = rows
            .ordered()
            .into_iter()
            .filter(|item| item.status() == ItemStatus::InProgress)
            .map(|item| item.item_id().to_string())
            .collect();
        for id in open {
            if let Some(item) = rows.get(&id) {
                let interrupted = item.clone().with_status(ItemStatus::Interrupted);
                reduce(
                    &mut rows,
                    &ThreadEvent::ItemCompleted { thread_id: "th_1".into(), turn_id: "live".into(), item: interrupted },
                );
            }
        }
    }

    pub fn with_persistence(self, root: &std::path::Path, model: &str) -> Result<AppState, String> {
        self.on_session(root, &baudet_projects::new_session_id(), model, None)
    }

    pub fn on_session(
        self,
        root: &std::path::Path,
        session_id: &str,
        model: &str,
        project_id: Option<&str>,
    ) -> Result<AppState, String> {
        let opened = Transcript::open_or_create(root, session_id, Some(model), None)
            .map_err(|error| error.to_string())?;
        let state = self;
        let mut active = match opened.build(model) {
            Ok(session) => session,
            Err(_) => Session::new(session_id, model),
        };
        active.project_id = project_id.map(str::to_owned).or(active.project_id);
        *state.project.lock().unwrap() = active.project_id.clone();
        let items = active.items.clone();
        state.turn_counter.fetch_max(Self::highest_turn_index(&items), Ordering::SeqCst);
        *state.transcript.lock().unwrap() = Some(opened);
        *state.active.lock().unwrap() = active;
        state.draw(&items);
        Ok(state)
    }

    pub fn finish(&self) {
        *self.live.lock().unwrap() = None;
        self.live_turns.fetch_sub(1, Ordering::SeqCst);
    }
}
