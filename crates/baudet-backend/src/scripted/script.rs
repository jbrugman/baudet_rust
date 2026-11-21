//! A prepared turn, and the backend that plays it.

use crate::sse::frame;
use crate::{Backend, BackendError, SendRequest, TurnHandle};
use baudet_core::ThreadEvent;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// One step a script can take. A script is data, not a closure, so it can be written down as a
/// fixture and read back, which is what lets a UI test and a future wire recording share it.
#[derive(Clone)]
pub enum Step {
    /// Publish one event verbatim.
    Event(ThreadEvent),
    /// Raw SSE lines: framed through [`crate::sse`], so the interface sees exactly the delta
    /// bundling a wire would give it. `Fn` maps a framed payload to an event; return `None` for
    /// anything the real framer would drop.
    RawSse { lines: Vec<String>, to_event: RawMapper },
}

/// Turns one framed SSE payload into an event, the way a real adapter would.
pub type RawMapper = Arc<dyn Fn(&str) -> Option<ThreadEvent> + Send + Sync>;

/// A scripted turn: the events it publishes, in order.
#[derive(Clone, Default)]
pub struct Script {
    pub steps: Vec<Step>,
}

impl std::fmt::Debug for Script {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Script").field("steps", &self.steps.len()).finish()
    }
}

impl Script {
    pub fn new() -> Script {
        Script { steps: Vec::new() }
    }

    pub fn step(mut self, step: Step) -> Script {
        self.steps.push(step);
        self
    }

    pub fn event(mut self, event: ThreadEvent) -> Script {
        self.steps.push(Step::Event(event));
        self
    }
}

/// The scripted [`Backend`]: each `send` plays the next script in order, so a test drives a
/// whole conversation rather than one turn.
///
/// Cancellation is real: after a `cancel`, the remaining steps are dropped and only an interrupt
/// close is published, which is how a UI learns to leave rows `interrupted` rather than `failed`.
#[derive(Default)]
pub struct ScriptedBackend {
    scripts: Mutex<Vec<Script>>,
    turn_counter: AtomicUsize,
    last_turn: Mutex<Option<Arc<TurnState>>>,
    last_request: Mutex<Option<RecordedRequest>>,
}

struct TurnState {
    cancelled: AtomicBool,
    steered: Mutex<Vec<String>>,
}

/// What the most recent turn was asked for.
///
/// A value behind a lock, not a borrow: `send` is given the `SendRequest` by move, so nothing here
/// could outlive the call as a reference, and the conversation owner hands its transcript over in
/// exactly the shape a caller would want to assert on.
#[derive(Debug, Clone)]
struct RecordedRequest {
    request: SendRequest,
    /// `tu_1` for the first turn -- the same id the player publishes, so an answer can be matched
    /// back to the request that produced it.
    turn_id: String,
}

impl ScriptedBackend {
    pub fn new(scripts: Vec<Script>) -> ScriptedBackend {
        ScriptedBackend {
            scripts: Mutex::new(scripts),
            turn_counter: AtomicUsize::new(0),
            last_turn: Mutex::new(None),
            last_request: Mutex::new(None),
        }
    }

    /// How many turns have been started, so a test can assert a turn was claimed exactly once.
    pub fn turns_started(&self) -> usize {
        self.turn_counter.load(Ordering::SeqCst)
    }

    /// What the human steered into the most recent turn, in order.
    pub fn steers(&self) -> Vec<String> {
        self.last_turn.lock().unwrap().as_ref().map(|t| t.steered.lock().unwrap().clone()).unwrap_or_default()
    }

    /// The whole request of the most recent turn, with the id that turn was published under.
    ///
    /// Exists so a caller can assert what it *asked*, not only what it replayed. Whether a turn
    /// carried its conversation is invisible in the published events, so without this the player
    /// could drop the transcript and every test here would still pass.
    pub fn last_request(&self) -> Option<(SendRequest, String)> {
        self.last_request.lock().unwrap().clone().map(|r| (r.request, r.turn_id))
    }

    /// Whether the most recent turn was cancelled.
    pub fn cancelled(&self) -> bool {
        self.last_turn.lock().unwrap().as_ref().map(|t| t.cancelled.load(Ordering::SeqCst)).unwrap_or(false)
    }
}

impl Backend for ScriptedBackend {
    fn send(&self, request: SendRequest) -> Result<TurnHandle, BackendError> {
        let index = self.turn_counter.fetch_add(1, Ordering::SeqCst);
        let script = {
            let scripts = self.scripts.lock().unwrap();
            scripts.get(index).cloned().unwrap_or_default()
        };
        let turn_id = format!("tu_{}", index + 1);
        *self.last_request.lock().unwrap() = Some(RecordedRequest { request, turn_id: turn_id.clone() });
        let state = Arc::new(TurnState { cancelled: AtomicBool::new(false), steered: Mutex::new(Vec::new()) });
        *self.last_turn.lock().unwrap() = Some(Arc::clone(&state));

        let (tx, rx) = mpsc::unbounded_channel();
        let steps = script.steps;
        let player_state = Arc::clone(&state);
        std::thread::spawn(move || {
            for step in steps {
                if player_state.cancelled.load(Ordering::SeqCst) {
                    // Deliberate Stop: publish nothing more, and let the close below interrupt.
                    break;
                }
                publish(&step, &tx);
            }
            // The sender drops here, which is what closes the stream and tells the owner the
            // script is done. A Stop must not look like a completion, so no terminal event is
            // invented here either; the orchestrator owns the interrupted close.
        });

        let steer_state = Arc::clone(&state);
        let cancel_state = Arc::clone(&state);
        Ok(TurnHandle::new(
            turn_id,
            rx,
            move |text| steer_state.steered.lock().unwrap().push(text),
            move || cancel_state.cancelled.store(true, Ordering::SeqCst),
        ))
    }
}

fn publish(step: &Step, tx: &mpsc::UnboundedSender<ThreadEvent>) {
    match step {
        Step::Event(event) => {
            let _ = tx.send(event.clone());
        }
        Step::RawSse { lines, to_event } => {
            for framed in frame(lines.iter()) {
                if let Some(event) = to_event(&framed.data) {
                    let _ = tx.send(event);
                }
            }
        }
    }
}
