//! Waar de volgende assistentbeurt vandaan komt.
//!
//! Poort van `tool/TurnSource.kt`. De loop is bewust onwetend van transport en persistentie: hij vraagt
//! alleen om het volgende antwoord op een transcriptie. Die ene naad is wat de loop testbaar maakt tegen
//! een scripted bron, en wat een latere native function-calling-backend in staat stelt prompted-JSON te
//! vervangen zonder de loop-body aan te raken. In de origineler heet die geïmplementeerde bron
//! `ResponsesTurnSource`; hier is het een trait, want een Rust-poort heeft geen overerving nodig voor een
//! single-method seam en een test-implementatie mag geen transport-replica zijn.

use crate::tool::ToolCall;
use baudet_core::msg::Msg;
use baudet_core::protocol::Turn;
use serde_json::Value;

/// De bron van de volgende beurt.
///
/// De argumenten zijn één struct en niet vijf losse parameters, om twee redenen. De origineler draagt er
/// zes door zeven aanroepplekken, waarvan er vier `..default..` laten, en een Rust-signatuur heeft die
/// defaults niet; en `history` moet van `&[Msg]` naar `Vec<Msg>` gekopieerd worden om naar een boxed future
/// te gaan, dus de bron beslist zelf over die kopie in plaats van de loop haar twee keer op te leggen.
///
/// De return is een `BoxFuture` en niet een kale `Turn`, omdat de echte bron een socket leest. De loop
/// zelf blijft synchronisatie-vrij: hij `.await` op wat hij krijgt en kent geen runtime.
pub struct TurnRequest<'a> {
    /// De hele lopende transcriptie, nieuwste boodschap laatst.
    pub history: &'a [Msg],
    /// De response-id van de vorige beurt, wanneer de aanroeper voor een server-side keten koos.
    pub previous_response_id: Option<&'a str>,
    /// De aangeboden function-definitions, of `None` wanneer er geen tools zijn. In de origineler een
    /// JSON-string; hier een `Value`, want de Requests-body die erop volgt bouwt er een object van en een
    /// omweg via tekst zou één extra kans op een vormfout zijn.
    pub tools: Option<&'a Value>,
    /// De streaming-haken. Ze gaan mee als waarden en niet als `&mut`, omdat de streams van een echte
    /// backend vanaf hun eigen taak schrijven; `Send` is wat de loop nodig heeft om erop te kunnen wachten.
    /// De implementatie moet het hele verzoek overnemen (`mut request`) om ze aan te roepen, want een
    /// `FnMut` leent niet: `&mut TurnRequest` zou elke bron dwingen zijn eigen `on_delta`-kopie te maken.
    ///
    /// Het leven is `'a` en niet het `'static` dat `Box<dyn FnMut(&str) + Send>` zou krijgen, want de
    /// loop geeft de callbacks van de aanroeper zélf door nadat hij ze met de sink heeft samengevoegd.
    /// Gemeten in een wegwerpcrate: met `'static` faalt die samenvoeging met "coercion requires that
    /// `'1` must outlive `'static`", en met een lening aan `'a` plus een `Sync`-sink compileert hij. De
    /// sink is hier `Sync` (`ItemSink: Send + Sync`), dus de sluiting die beide leent, mag `Send` heten.
    /// Bij een `Fn-`-type staan de bounds zonder haakjes om de dyn-clausel (`Box<dyn FnMut(&str) + Send +
    /// 'a>`); een `Box<dyn (…)>` wijst de compiler aan als incorrecte parentheses. Het leven moet hier een
    /// naam krijgen: `'_` is gereserveerd en mag niet in een typedef van een veld.
    pub on_delta: Box<dyn FnMut(&str) + Send + 'a>,
    pub on_reasoning: Box<dyn FnMut(&str) + Send + 'a>,
    /// Een beeld gaat mee op de eerste beurt van een beurt-reeks; latere iteraties spelen tekst.
    pub image: Option<&'a str>,
}

pub type TurnFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Turn> + Send + 'a>>;

pub trait TurnSource {
    /// Het volgende antwoord op deze transcriptie.
    fn next_turn<'a>(&'a self, request: TurnRequest<'a>) -> TurnFuture<'a>;
}

/// De calls die een beurt vroeg; leeg betekent dat de beurt een finale is.
///
/// Poort van `tool/CallExtractor.kt`. Dit is de enige naad tussen "het model antwoordde" en "het model
/// vroeg te handelen", dus prompted-JSON ruilen voor een native function-calling-backend is hier een
/// nieuwe implementatie en nooit een loop-bewerking. Hij is een `fn`-trait en geen `Fn`-closure, omdat de
/// ingebouwde lezer ([`PromptedCallExtractor`]) in de origineler een gewone klasse met een compiled
/// regex is en de loop hem per beurt vaker dan één keer raadpleegt.
pub trait CallExtractor {
    fn extract(&self, turn: &Turn) -> Vec<ToolCall>;
}

/// De fenced-code markering die een tool call in een prompted antwoord aanduidt.
pub const FENCE_TAG: &str = "baudet-tool";

#[cfg(test)]
mod tests {
    use super::*;
    use baudet_core::protocol::Turn;

    /// Een bron die de transcriptie terugzendt als antwoord: genoeg om de naad te raken zonder transport.
    struct Scripted;

    impl TurnSource for Scripted {
        fn next_turn<'a>(&'a self, mut request: TurnRequest<'a>) -> TurnFuture<'a> {
            Box::pin(async move {
                // De haken zijn `Box<dyn FnMut>`, dus de bron beslist wanneer hij ze aanroept; de loop
                // hoeft er geen `&mut` door te slepen.
                (request.on_delta)("halve ");
                let laatst = request.history.last().map(|msg| msg.content.clone()).unwrap_or_default();
                (request.on_reasoning)("denkwerk");
                Turn { text: format!("antwoord op {laatst}"), ..Turn::text(String::new()) }
            })
        }
    }

    /// Een `fn`-trait, dus de loop draagt hem als `&dyn` — precies zoals de origineler zijn klasse draagt.
    struct AlwaysEmpty;

    impl CallExtractor for AlwaysEmpty {
        fn extract(&self, _turn: &Turn) -> Vec<ToolCall> {
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
        // Dit is wat de loop daadwerkelijk doet: hij kent alleen deze trait, niet de implementatie.
        let source: &dyn TurnSource = &Scripted;
        let history = vec![Msg::user("vraag")];
        let turn = source.next_turn(TurnRequest { history: &history, ..leased() }).await;
        assert_eq!(turn.text, "antwoord op vraag");
        let extractor: &dyn CallExtractor = &AlwaysEmpty;
        assert!(extractor.extract(&turn).is_empty());
    }

    #[test]
    fn the_seam_crosses_a_thread() {
        // `TurnFuture` is `Send` omdat een echte bron op een socket leest. Zonder die belofte kan de
        // UI-taak niet op een beurt wachten, dus dat is hier de eigenschap die telt, niet de inhoud.
        let source = Scripted;
        let handle = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
            runtime.block_on(source.next_turn(leased())).text
        });
        assert_eq!(handle.join().unwrap(), "antwoord op ");
    }
}
