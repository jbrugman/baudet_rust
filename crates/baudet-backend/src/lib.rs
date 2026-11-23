//! Model transport and the streamed turn interface.

use baudet_core::{CoreError, ThreadEvent};
use tokio::sync::mpsc;

pub mod openai;
pub mod protocol;
pub mod scripted;
pub mod sse;
pub mod transport;

#[derive(Debug, Clone, Default)]
pub struct SendRequest {
    pub turn_id: Option<String>,
    pub system: String,
    pub user_text: String,
    pub history: Vec<ThreadEvent>,
    pub previous_response_id: Option<String>,
    pub tools: Option<String>,
    pub image: Option<String>,
    pub max_output_tokens: Option<u32>,
    pub model: Option<String>,
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("backend task stopped before publishing a terminal event")]
    Abandoned,
    #[error("the listener stopped reading")]
    ListenerGone,
}

pub struct TurnHandle {
    pub turn_id: String,
    receiver: mpsc::UnboundedReceiver<ThreadEvent>,
    steer: Box<dyn Fn(String) + Send + Sync>,
    cancel: Box<dyn Fn() + Send + Sync>,
    result: Option<std::sync::Arc<std::sync::Mutex<Option<baudet_core::Turn>>>>,
}

impl TurnHandle {
    pub fn new(
        turn_id: impl Into<String>,
        receiver: mpsc::UnboundedReceiver<ThreadEvent>,
        steer: impl Fn(String) + Send + Sync + 'static,
        cancel: impl Fn() + Send + Sync + 'static,
    ) -> TurnHandle {
        TurnHandle { turn_id: turn_id.into(), receiver, steer: Box::new(steer), cancel: Box::new(cancel), result: None }
    }

    pub(crate) fn with_result(mut self, result: std::sync::Arc<std::sync::Mutex<Option<baudet_core::Turn>>>) -> Self {
        self.result = Some(result);
        self
    }

    // After the terminal event, this exposes metadata not carried by display events.
    pub fn take_result(&mut self) -> Option<baudet_core::Turn> {
        self.result.as_ref()?.lock().unwrap().take()
    }

    pub fn events(&mut self) -> &mut mpsc::UnboundedReceiver<ThreadEvent> {
        &mut self.receiver
    }

    pub fn steer(&self, text: impl Into<String>) {
        (self.steer)(text.into());
    }

    pub fn cancel(&self) {
        (self.cancel)();
    }
}

// The backend owns transport. Each handle exposes a single event stream.
pub trait Backend: Send + Sync {
    fn send(&self, request: SendRequest) -> Result<TurnHandle, BackendError>;

    fn context_endpoint(&self) -> Option<(&str, &str)> {
        None
    }

    // A separate, non-streaming call for an explicitly completed summary.
    fn complete_once(&self, system: &str, user_text: &str, max_tokens: u32) -> Result<baudet_core::Turn, BackendError> {
        let _ = (system, user_text, max_tokens);
        Err(BackendError::Core(CoreError::Unanswered { request: "completeOnce" }))
    }

    fn resolve_approval(&self, _item_id: &str, _decision: baudet_core::ApprovalDecision) {}
}
