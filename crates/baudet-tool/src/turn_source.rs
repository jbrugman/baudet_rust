
use crate::tool::{ToolCall, ToolRegistry};
use baudet_core::msg::Msg;
use baudet_core::protocol::Turn;
use serde_json::Value;

pub struct TurnRequest<'a> {
    pub history: &'a [Msg],
    pub previous_response_id: Option<&'a str>,
    pub tools: Option<&'a Value>,
    pub on_delta: Box<dyn FnMut(&str) + Send + 'a>,
    pub on_reasoning: Box<dyn FnMut(&str) + Send + 'a>,
    pub image: Option<&'a str>,
}

pub type TurnFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Turn> + Send + 'a>>;

pub trait TurnSource {
    fn next_turn<'a>(&'a self, request: TurnRequest<'a>) -> TurnFuture<'a>;
}

pub trait CallExtractor {
    fn extract(&self, turn: &Turn, registry: &ToolRegistry) -> Vec<ToolCall>;
}

pub const FENCE_TAG: &str = "baudet-tool";

#[cfg(test)]
mod tests {
    use super::*;
    use baudet_core::protocol::Turn;

    struct Scripted;

    impl TurnSource for Scripted {
        fn next_turn<'a>(&'a self, mut request: TurnRequest<'a>) -> TurnFuture<'a> {
            Box::pin(async move {
                (request.on_delta)("halve ");
                let laatst = request.history.last().map(|msg| msg.content.clone()).unwrap_or_default();
                (request.on_reasoning)("denkwerk");
                Turn { text: format!("antwoord op {laatst}"), ..Turn::text(String::new()) }
            })
        }
    }

    struct AlwaysEmpty;

    impl CallExtractor for AlwaysEmpty {
        fn extract(&self, _turn: &Turn, _registry: &ToolRegistry) -> Vec<ToolCall> {
            Vec::new()
        }
    }

    fn leased() -> TurnRequest<'static> {
        TurnRequest {
            history: &[],
            previous_response_id: None,
            tools: None,
            on_delta: Box::new(|_| {}),
            on_reasoning: Box::new(|_| {}),
            image: None,
        }
    }

    #[tokio::test]
    async fn a_source_is_driven_through_the_seam_alone() {
        let source: &dyn TurnSource = &Scripted;
        let history = vec![Msg::user("vraag")];
        let turn = source.next_turn(TurnRequest { history: &history, ..leased() }).await;
        assert_eq!(turn.text, "antwoord op vraag");
        let extractor: &dyn CallExtractor = &AlwaysEmpty;
        assert!(extractor.extract(&turn, &ToolRegistry::default()).is_empty());
    }

    #[test]
    fn the_seam_crosses_a_thread() {
        let source = Scripted;
        let handle = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
            runtime.block_on(source.next_turn(leased())).text
        });
        assert_eq!(handle.join().unwrap(), "antwoord op ");
    }
}
