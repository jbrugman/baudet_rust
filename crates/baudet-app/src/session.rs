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

use crate::reducer::{reduce, Rows};
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
    rows: Mutex<Rows>,
    live_turns: AtomicUsize,
    turn_counter: AtomicUsize,
    live: Mutex<Option<Arc<Mutex<TurnHandle>>>>,
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

impl AppState {
    pub fn new(backend: Arc<dyn Backend>, system: impl Into<String>) -> AppState {
        AppState {
            backend,
            system: system.into(),
            rows: Mutex::new(Rows::new()),
            live_turns: AtomicUsize::new(0),
            turn_counter: AtomicUsize::new(0),
            live: Mutex::new(None),
        }
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

        let request = SendRequest {
            system: self.system.clone(),
            user_text: user_text.to_string(),
            history: Vec::new(),
            previous_response_id: None,
            tools: None,
            image: None,
            max_output_tokens: None,
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
