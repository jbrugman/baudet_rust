//! The conversation owner: claim a turn, fold its events, publish them to whoever renders.
//!
//! Translated from the parts of `dev.baudet.app.AppState` that decide run/stop/persist. Two
//! rules carried over on purpose, and both are why this file can be free of locking games:
//!
//!  * the turn claim is a counter, as Baudet's already is. A second live turn is refused rather
//!    than interleaved, because raising that to "several sessions at once" is a deliberate
//!    follow-up on hardware that can afford it, not an accident of the data model -- every event
//!    already carries `thread_id` and `turn_id`, so the follow-up needs no new protocol;
//!  * exactly one reader drains the backend's stream. Folding and re-publishing happen in that
//!    reader, so there is no shared cursor and no second place where a delta could be applied.
//!
//! The re-publish is a bounded watch channel with the folded rows as its value, not a per-event
//! pipe: a transcript must never *lose* an event, which is what `sync::broadcast`'s lag would do,
//! and a renderer that wakes late still sees the complete current state.

use crate::reducer::{reduce, streamed_text, Rows};
use baudet_backend::{Backend, SendRequest, TurnHandle};
use baudet_core::{Item, ItemStatus, ThreadEvent};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// What a send call handed back: the live handle, a subscription to this turn's events, and the
/// join handle of the reader that folds them.
pub struct Sent {
    pub turn_id: String,
    pub handle: Arc<Mutex<TurnHandle>>,
    /// Taken by the one caller allowed to drain this turn's events. `None` means someone already
    /// reads it: two pumps on one receiver would split the stream, not double-book it.
    reader: Mutex<Option<Arc<Mutex<TurnHandle>>>>,
}

impl Sent {
    /// Claim this turn's event stream. `None` on the second call, which is the bug this guards.
    pub fn take_reader(&self) -> Option<Arc<Mutex<TurnHandle>>> {
        self.reader.lock().unwrap().take()
    }
}

/// The conversation owner.
pub struct AppState {
    backend: Arc<dyn Backend>,
    system: String,
    /// The model and reasoning level from settings, stamped on every request. `AppState` does not
    /// know what they mean -- it hands them to the backend, which does -- but the request is built
    /// here, so this is the only place they can enter it.
    ///
    /// Behind a `Mutex` for the same reason `rows` is: the value is written from the window's paint
    /// pass, when the settings panel saves, and read on the worker thread that builds the request.
    /// A plain field would make `AppState` `!Sync`, and every caller here already holds it in an
    /// `Arc`. Held for a clone only, never across a request.
    choices: Mutex<RequestChoices>,
    rows: Mutex<Rows>,
    live_turns: AtomicUsize,
    turn_counter: AtomicUsize,
    live: Mutex<Option<Arc<Mutex<TurnHandle>>>>,
}

/// What the settings panel chose for the next request.
#[derive(Debug, Clone, Default)]
pub struct RequestChoices {
    pub model: Option<String>,
    pub reasoning: Option<String>,
    /// The ceiling every subsequent turn may spend, from the same panel. `None` leaves the decision
    /// to the endpoint, which is what an empty field means and why this is an option rather than a
    /// number with a default: inventing a budget here would cap an answer nobody capped.
    pub max_output_tokens: Option<u32>,
}

/// A turn is already running. Baudet's rule: the running turn keeps the session, so a second send
/// is refused instead of queued or interleaved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a turn is already running; stop it first")]
pub struct TurnBusy;

/// The one thread-less way to run a turn to completion without an async runtime: drain the
/// backend stream on the calling thread, folding each event.
pub fn pump(
    handle: &Arc<Mutex<TurnHandle>>,
    apply: impl Fn(&ThreadEvent) -> bool,
    forward: &mpsc::UnboundedSender<ThreadEvent>,
) {
    loop {
        let next = {
            // The lock is held only for one non-blocking poll, never across a wait, so Steer and
            // Stop cannot be starved by a slow renderer.
            let mut guard = handle.lock().unwrap();
            guard.events().try_recv()
        };
        match next {
            Ok(event) => {
                apply(&event);
                let _ = forward.send(event);
            }
            // No event ready. A real UI pumps from its own frame clock; a test spins with a short
            // sleep, and both stop when the stream closes.
            Err(mpsc::error::TryRecvError::Empty) => std::thread::sleep(std::time::Duration::from_millis(1)),
            Err(mpsc::error::TryRecvError::Disconnected) => break,
        }
    }
}

/// Everything a turn may remember: the rows that are finished, as events, newest first -- the
/// order [`baudet_backend::protocol::wire_history`] reads them in, so the fold into `Msg` stays
/// entirely in that function and this helper decides nothing about wire order. It is the only place
/// that decides *what* travels.
///
/// A caller reading these events straight -- a test, a future recorder -- sees the conversation
/// backwards, which is the price of owning the input order of a function rather than its output;
/// it is cheaper than a second reversal here and a second opinion on the same conversation there.
///
/// `wire_history` reverses this into oldest-first and appends the caller's `user_text` afterwards,
/// which is why the question being asked is not in here: [`AppState::send`] reads the rows before
/// it adds its own row, so appending the question again is not a case this has to guard against.
///
/// Two kinds of row are kept, and the two that are dropped are dropped for a reason each:
///
///  * only [`ItemStatus::Completed`]: [`wire_history`] would swallow an `InProgress` row silently,
///    and replaying a half answer the person can still see change on screen would tell the model
///    something that is not yet true. An interrupted or failed answer is not a fact either;
///  * only user and agent messages: what a model is asked to remember is the conversation, not its
///    own reasoning, its shell output, or an error row the person never sent it.
///
/// Derives `turn_id` from the row's id, which `openai::StreamState::item_id` and the user-row id in
/// `send` both build as `"{turn}/msg"` and `"{turn}/user"`. No invented id: a turn's events are
/// keyed by it, and a wrong one would attribute an old answer to the wrong turn.
fn completed_messages(rows: &Rows, thread_id: &str) -> Vec<ThreadEvent> {
    rows.ordered()
        .iter()
        .rev()
        .filter(|item| matches!(**item, Item::UserMessage(_) | Item::AgentMessage(_)))
        .filter(|item| item.status() == ItemStatus::Completed)
        .filter(|item| !streamed_text(item).trim().is_empty())
        .map(|item| ThreadEvent::ItemCompleted {
            thread_id: thread_id.to_string(),
            turn_id: item.item_id().split('/').next().unwrap_or(item.item_id()).to_string(),
            item: (*item).clone(),
        })
        .collect()
}

impl AppState {
    pub fn new(backend: Arc<dyn Backend>, system: impl Into<String>) -> AppState {
        AppState {
            backend,
            system: system.into(),
            choices: Mutex::new(RequestChoices::default()),
            rows: Mutex::new(Rows::new()),
            live_turns: AtomicUsize::new(0),
            turn_counter: AtomicUsize::new(0),
            live: Mutex::new(None),
        }
    }

    /// Set the model and reasoning level every subsequent request names.
    ///
    /// A setter rather than a constructor argument because these are the settings panel's values, and
    /// the panel can change them while a conversation is open; a turn already running keeps whatever
    /// it was sent with, since its request has already left.
    pub fn set_request_choices(
        &self,
        model: Option<String>,
        reasoning: Option<String>,
        max_output_tokens: Option<u32>,
    ) {
        *self.choices.lock().unwrap() = RequestChoices { model, reasoning, max_output_tokens };
    }

    /// Rows as they stand right now, oldest first. The only accessor a renderer needs.
    pub fn rows(&self) -> Vec<Item> {
        self.rows.lock().unwrap().ordered().into_iter().cloned().collect()
    }

    /// How many turns are live, so a test can assert the claim is honest.
    pub fn live_turns(&self) -> usize {
        self.live_turns.load(Ordering::SeqCst)
    }

    /// Start a turn. The caller must drain it exactly once -- see [`pump`] -- because the event
    /// stream has one reader by design, and closing happens in [`finish`].
    pub fn send(&self, user_text: &str) -> Result<Sent, TurnBusy> {
        // Claim before the backend is asked, so a backend that fails mid-handshake still leaves a
        // counter the caller can release.
        if self.live_turns.fetch_add(1, Ordering::SeqCst) != 0 {
            self.live_turns.fetch_sub(1, Ordering::SeqCst);
            return Err(TurnBusy);
        }
        let index = self.turn_counter.fetch_add(1, Ordering::SeqCst);
        let turn_id = format!("tu_{}", index + 1);
        let thread_id = "th_1".to_string();

        // Everything the model is allowed to remember, read before this turn's question is added
        // below: the request's transcript must not carry the question twice, because `user_text` is
        // appended after it by both request builders.
        let history = {
            let rows = self.rows.lock().unwrap();
            completed_messages(&rows, &thread_id)
        };

        // The instruction becomes a row before the request leaves, so a turn that never answers
        // still shows what was asked.
        reduce(
            &mut self.rows.lock().unwrap(),
            &ThreadEvent::ItemStarted {
                thread_id: thread_id.clone(),
                turn_id: turn_id.clone(),
                item: Item::UserMessage(baudet_core::UserMessageItem {
                    item_id: format!("{turn_id}/user"),
                    text: user_text.to_string(),
                    status: ItemStatus::Completed,
                }),
            },
        );

        let choices = self.choices.lock().unwrap().clone();
        let request = SendRequest {
            system: self.system.clone(),
            user_text: user_text.to_string(),
            history,
            previous_response_id: None,
            tools: None,
            image: None,
            max_output_tokens: choices.max_output_tokens,
            model: choices.model.clone(),
            reasoning: choices.reasoning.clone(),
        };

        let handle = match self.backend.send(request) {
            Ok(handle) => Arc::new(Mutex::new(handle)),
            Err(_) => {
                self.live_turns.fetch_sub(1, Ordering::SeqCst);
                return Err(TurnBusy);
            }
        };
        *self.live.lock().unwrap() = Some(Arc::clone(&handle));

        // The receiver half of this mirror is deliberately unused here: readers subscribe through
        // `Sent::events`, so nothing may hold this one open -- closing it is what ends the turn.
        // Nothing is read here on purpose. Folding and mirroring belong to the one caller that
        // claims the receiver, so `send` hands the turn over without consuming a single event.
        Ok(Sent { turn_id, handle: Arc::clone(&handle), reader: Mutex::new(Some(handle)) })
    }

    /// Fold one event into the rows. Used by tests and by any caller that drives events itself.
    pub fn apply(&self, event: &ThreadEvent) -> bool {
        reduce(&mut self.rows.lock().unwrap(), event)
    }

    /// Stop the live turn and close its open rows as interrupted. A deliberate Stop must not leave
    /// a card that reads as a failure.
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

    /// Let the next turn be claimed.
    pub fn finish(&self) {
        self.live_turns.fetch_sub(1, Ordering::SeqCst);
        *self.live.lock().unwrap() = None;
    }
}
