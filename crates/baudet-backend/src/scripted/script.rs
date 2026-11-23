
use crate::sse::frame;
use crate::{Backend, BackendError, SendRequest, TurnHandle};
use baudet_core::ThreadEvent;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Clone)]
pub enum Step {
    Event(ThreadEvent),
    RawSse { lines: Vec<String>, to_event: RawMapper },
}

pub type RawMapper = Arc<dyn Fn(&str) -> Option<ThreadEvent> + Send + Sync>;

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

#[derive(Debug, Clone)]
struct RecordedRequest {
    request: SendRequest,
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

    pub fn turns_started(&self) -> usize {
        self.turn_counter.load(Ordering::SeqCst)
    }

    pub fn steers(&self) -> Vec<String> {
        self.last_turn.lock().unwrap().as_ref().map(|t| t.steered.lock().unwrap().clone()).unwrap_or_default()
    }

    pub fn last_request(&self) -> Option<(SendRequest, String)> {
        self.last_request.lock().unwrap().clone().map(|r| (r.request, r.turn_id))
    }

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
                    break;
                }
                publish(&step, &tx);
            }
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
