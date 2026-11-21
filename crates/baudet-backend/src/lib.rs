//! The transport seam the orchestrator talks to, and the implementations of it.
//!
//! Translated from `dev.baudet.api.ModelGateway` and `dev.baudet.tool.TurnSource`, whose doc
//! comments already state the reason the seam exists: so the run, stop and persist decision at
//! the heart of a turn can be driven against a scripted backend -- and, for a green project, so
//! a user interface can be built and finished before any backend exists at all.
//!
//! [`Backend`] is object-safe and returns events over an unbounded channel, so an implementation
//! is a plain `impl` rather than an `async fn` in a trait: a real SSE adapter, a scripted player
//! and a per-turn recorder all sit behind one `Box<dyn Backend>`.

use baudet_core::{CoreError, ThreadEvent};
use tokio::sync::mpsc;

pub mod openai;
pub mod scripted;
pub mod sse;

/// What one turn asks the backend to answer.
///
/// `history` is the whole running transcript, newest item last; `previous_response_id` chains a
/// server-side session when the caller opted in; `tools` is the enabled-function definitions to
/// offer the backend, or `None` when none apply. `image` is a base64 data URL riding the first
/// request of a turn.
#[derive(Debug, Clone, Default)]
pub struct SendRequest {
    pub system: String,
    pub user_text: String,
    pub history: Vec<ThreadEvent>,
    pub previous_response_id: Option<String>,
    pub tools: Option<String>,
    pub image: Option<String>,
    /// The ceiling this turn may spend, so a backend that invents a budget can be held to it.
    pub max_output_tokens: Option<u32>,
}

/// Where a turn went wrong, in terms a listener can act on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("backend task stopped before publishing a terminal event")]
    Abandoned,
    #[error("the listener stopped reading")]
    ListenerGone,
}

/// A turn's live handle: the event stream plus the ways to change what the turn does next.
///
/// The receiver is taken once, by whoever renders the turn, so there is exactly one reader per
/// turn and no shared cursor to defend.
pub struct TurnHandle {
    pub turn_id: String,
    receiver: mpsc::UnboundedReceiver<ThreadEvent>,
    steer: Box<dyn Fn(String) + Send + Sync>,
    cancel: Box<dyn Fn() + Send + Sync>,
}

impl TurnHandle {
    pub fn new(
        turn_id: impl Into<String>,
        receiver: mpsc::UnboundedReceiver<ThreadEvent>,
        steer: impl Fn(String) + Send + Sync + 'static,
        cancel: impl Fn() + Send + Sync + 'static,
    ) -> TurnHandle {
        TurnHandle { turn_id: turn_id.into(), receiver, steer: Box::new(steer), cancel: Box::new(cancel) }
    }

    /// The event stream, claimed by its one renderer.
    pub fn events(&mut self) -> &mut mpsc::UnboundedReceiver<ThreadEvent> {
        &mut self.receiver
    }

    /// Redirect the turn while it runs, the way Baudet's Steer button does.
    pub fn steer(&self, text: impl Into<String>) {
        (self.steer)(text.into());
    }

    /// Stop the turn deliberately: what must leave every open row interrupted rather than failed.
    pub fn cancel(&self) {
        (self.cancel)();
    }
}

/// The seam. Implementations publish the whole life of a turn as [`ThreadEvent`]s and close the
/// stream when the turn is over.
pub trait Backend: Send + Sync {
    /// Start a turn. The returned handle's stream must end with `turn/completed`, whether the
    /// turn finished cleanly, was cancelled, or failed -- a listener that has to clean up cards
    /// cannot wait for a backend that has already given up.
    fn send(&self, request: SendRequest) -> Result<TurnHandle, BackendError>;

    /// A one-off call that is not stored, used by the compactor. Not streaming: the compactor
    /// wants the whole summary or nothing.
    fn complete_once(&self, system: &str, user_text: &str, max_tokens: u32) -> Result<baudet_core::Turn, BackendError> {
        let _ = (system, user_text, max_tokens);
        Err(BackendError::Core(CoreError::Unanswered { request: "completeOnce" }))
    }

    /// Answer a pending approval. Implementations resolve the matching `approval/requested`.
    fn resolve_approval(&self, _item_id: &str, _decision: baudet_core::ApprovalDecision) {}
}
