//! De bestuurder: modelbeurt, dan uitvoeren, dan het resultaat teruggeven, dan opnieuw.
//!
//! Poort van `tool/ToolLoop.kt`. Hij orkestreert drie geïnjecteerde naden — [`TurnSource`] voor het
//! volgende antwoord, [`CallExtractor`] om de calls te zien, [`ToolRunner`] om ze uit te voeren — en draagt
//! zelf geen transport, persistentie of UI, zodat elke dependentie een naad is die een test kan verwisselen.
//!
//! [`ItemSink`] is de vierde naad en de enige die hier nieuw is: de origineler kent een `ItemSink` uit het
//! transcript-pakket, deze repo heeft nog geen transcript-laag, dus de loop definiet hem en laat de
//! persistentielaag hem straks implementeren. De loop persisteert dus zélf niets — hij vertelt alleen wat er
//! gebeurt, in dezelfde woorden die de rijen later krijgen.

use crate::call_extractor::PromptedCallExtractor;
use crate::tool::{ToolCall, ToolContext, ToolRegistry, ToolResult, ToolRunner, Undo};
use crate::turn_source::{CallExtractor, TurnRequest, TurnSource};
use baudet_core::item::{CommandExecutionItem, FileChangeItem, ItemKind, ToolCallItem};
use baudet_core::msg::Msg;
use baudet_core::protocol::Turn;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Tool-calls die binnen één fan-out-batch samen mogen draaien. Twee, want deze app mikt op een lokale
/// bak: een paar read-only calls overlappen nuttig, terwijl een vloot alleen een hosted backend met
/// elastische workers helpt. Dit begrenst calls binnen één beurt, niet hele beurten.
const MAX_PARALLEL_TOOL_CALLS: usize = 2;

/// Plafond op opeenvolgende lege, callloze beurten. Een redeneermodel dat zijn hele antwoord in de
/// reasoning-kanaal parkeert en op het antwoord-kanaal niets achterlaat, is niet klaar; één verzoek
/// meer is wat het in beweging brengt. Begrensd, zodat een model dat leeg blijft niet in plaats van
/// antwoorden de lus kan laten draaien.
const MAX_BLANK_TURNS: u32 = 2;

/// Plafond op de tekst die één observatie op het model-kanaal mag zetten, in tekens. De volledige
/// uitvoer blijft in de transcriptierij — het sink-pad is onaangeroerd — en alleen wat het model te
/// zien krijgt, is begrensd. Een grote diff of grep die elke latere beurt wordt meegegeven, blaast
/// anders de hele historie op, wat een lange sessie traag maakt. Kop-gericht gehouden, zodat het model
/// nog leest hoe de call begon; de mens leest alles in de kaart.
const MAX_OBSERVATION_CHARS: usize = 32_000;

/// Het skelet met vaste vorm dat ruimte gereserveerd voor de marker, zodat kop + marker + staart onder
/// het plafond blijft; de echte marker ruilt alleen de nul-vullingen om voor cijfers van niet grotere
/// breedte. Eén vorm voor beide, anders verschilt de gereserveerde breedte van de marker die erin komt.
const TRUNCATION_SKELETON: &str = "\n\n[... output truncated: keeping the first 0000 and last 0000 of 000000 characters; the full output stays in the transcript ...]\n\n";

/// Een korte werkinstructie die aan de werkende transcriptie wordt toegevoegd wanneer het model een
/// beurt leeg parkeert, zodat het volgende verzoek verschilt van het net lege. Een deterministisch lokaal
/// model speelt identieke boodschappen terug in een identiek leeg antwoord — de kale herbepaling veranderde
/// dus niets — dus alleen invoer die verschilde kon het in beweging brengen. Blijft buiten de gepersisteerde
/// historie (nooit in `added`), zodat het transcript schoon blijft.
const BLANK_NUDGE: &str = "Your previous turn returned neither an answer nor a tool call. If you intend to use a tool, emit the call now; otherwise reply with your final answer.";

/// De tool-observatie naar het model; blijft Engels.
const OBSERVATION_FENCE: &str = "```";

/// Of een callloze beurt niets draagt waarop een lezer kan handelen: leeg, of alleen placeholder-
/// leestekens zoals de "..." die een redeneermodel soms op het antwoord-kanaal achterlaat. Geen van
/// beide is een echt antwoord, dus beide verdienen één verzoek meer in plaats van een stille beurt.
fn is_blankish(text: &str) -> bool {
    text.chars().all(|c| c.is_whitespace() || c == '.' || c == '\u{2026}')
}

/// Begrens `text` tot ongeveer `max_chars` door het eerste drievijfde en het laatste tweevijfde om een
/// afkapmarker te houden. Modelgericht, dus Engels. Onder het plafond komt `text` ongeewijzigd terug; de
/// afgemaakte string overschrijdt `max_chars` nooit met meer dan de cijfers van de marker, dus de eigen
/// guard van een aanroeper op dat plafond houdt stand.
///
/// De snijpunten vallen op UTF-8-grenzen, wat de origineler niet garandeert: zijn `substring` gooit op
/// een meerbaitig teken op precies die plek. De grens wordt teruggeschoven naar het dichtstbijzijnde
/// granair, zodat één accent in een lange uitvoer geen crasht maar een iets korter kop of staart.
pub fn cap_model_text(text: &str, max_chars: usize) -> String {
    if max_chars == 0 || text.chars().count() <= max_chars {
        return text.to_string();
    }
    let budget = max_chars.saturating_sub(TRUNCATION_SKELETON.chars().count());
    let head = budget * 3 / 5;
    let tail = budget - head;
    // De teken-inderingen naar byte-inderingen, op granair-grenzen.
    let head_bytes = text.char_indices().nth(head).map(|(i, _)| i).unwrap_or(text.len());
    let keep = text.chars().count() - tail;
    let tail_bytes = text.char_indices().nth(keep).map(|(i, _)| i).unwrap_or(text.len());
    let marker = format!(
        "\n\n[... output truncated: keeping the first {head} and last {tail} of {} characters; the full output stays in the transcript ...]\n\n",
        text.chars().count()
    );
    format!("{}{}{}", &text[..head_bytes], marker, &text[tail_bytes..])
}

/// Wat er tijdens een beurt op het scherm verschijnt.
///
/// De methods zijn uitdrukkelijk `&self` en niet `&mut self`: de streaming-haken van
/// [`TurnRequest`] komen vanaf de bron — in een echte backend vanaf zijn eigen taak — dus een sink mag
/// geen uniek eigendom van de loop vergen. Daardoor is de naad `Send + Sync`, en kan de UI-taak hem
/// aanraken terwijl de lus op een andere taak draait. Methodes die in de origineler een kaart *openen*
/// en *sluiten* (`commandStarted`/`commandCompleted`), hebben hier één methode per afronding: de lus
/// opent en sluit in één adempauze rond één call, en een rij-id komt uit [`Self::next_item_id`].
pub trait ItemSink: Send + Sync {
    /// Een uniek id voor één rij. De loop verzinnet ze niet zelf, want de persistentielaag is degene
    /// die weet welke ids er al bestaan.
    fn next_item_id(&self) -> String;
    /// Een antwoord is begonnen; de teller die de origineler rond elke beurt optelt.
    fn reply_started(&self) {}
    /// Het antwoord van deze beurt is af, met de volledige tekst.
    fn reply_completed(&self, _text: &str) {}
    /// Een stukje antwoord- of reasoning-tekst. Beide bestaan omdat de oirspronkelijke `onDelta`- en
    /// `onReasoning`-callbacks naast de sink blijven: de sink noteert het als rijinhoud, de callback
    /// geeft het aan wie meekijkt, en de een vervangt de ander niet.
    fn text_delta(&self, _chunk: &str) {}
    fn reasoning_delta(&self, _chunk: &str) {}
    /// Een commando-kaart, met wat de run er werkelijk van maakte.
    fn command_completed(&self, _item: CommandExecutionItem) {}
    /// Een schrijfkaart. `change` is `Update` en niet de `item_kind` van de tool: `edit_file` patched
    /// tekst in een bestand dat het eerst leest, dus de soorten komen overeen, en een afgewezen write
    /// wordt gedragen door de status van het item.
    fn file_change_completed(&self, _item: FileChangeItem) {}
    /// Een generieke call-kaart.
    fn tool_call_completed(&self, _item: ToolCallItem) {}
}

/// De sink die niets bijhoudt, voor een aanroeper die niet meekijkt.
pub struct NullSink;

impl ItemSink for NullSink {}

/// Één stap van de lus: wat het model schreef, wat het vroeg, wat ervoor terugkwam.
#[derive(Debug, Clone)]
pub struct ToolStep {
    pub assistant_text: String,
    pub calls: Vec<ToolCall>,
    pub results: Vec<ToolResult>,
}

/// Het resultaat van één draai.
#[derive(Debug)]
pub enum LoopOutcome {
    /// Het model antwoordde zonder te vragen. `cap_reached` zegt dat dat pas gebeurde nadat het budget
    /// op was, zodat de aanroeper dat van een gewone finale kan onderscheiden.
    Completed {
        final_text: String,
        final_turn: Turn,
        response_id: Option<String>,
        steps: Vec<ToolStep>,
        messages: Vec<Msg>,
        undos: Vec<Undo>,
        cap_reached: bool,
    },
    /// Het budget op en het model wilde nog handelen. `incomplete_reason` is `Some("context_limit")`
    /// wanneer de plafond de lus stopte vóórdat er een verzoek uitging.
    CapReached {
        previous_response_id: Option<String>,
        steps: Vec<ToolStep>,
        messages: Vec<Msg>,
        undos: Vec<Undo>,
        incomplete_reason: Option<String>,
    },
}

impl LoopOutcome {
    /// De boodschappen die deze draai heeft toegevoegd, voor de aanroeper die ze persisteert.
    pub fn messages(&self) -> &[Msg] {
        match self {
            LoopOutcome::Completed { messages, .. } | LoopOutcome::CapReached { messages, .. } => messages,
        }
    }
}

/// De argumenten van één draai. De origineler draagt acht parameters met zes defaults door; deze repo
/// heeft die defaults niet, dus ze staan als één struct met `Default` en een paar verplichte velden.
pub struct LoopRun<'a> {
    pub history: &'a [Msg],
    pub ctx: &'a ToolContext,
    /// De response-id van de vorige beurt, alleen bruikbaar wanneer `chain_response_id` aan staat.
    pub previous_response_id: Option<&'a str>,
    /// Een beeld gaat mee op het eerste verzoek van deze draai; latere iteraties spelen tekst.
    pub image: Option<&'a str>,
    pub on_delta: Box<dyn FnMut(&str) + Send>,
    pub on_reasoning: Box<dyn FnMut(&str) + Send>,
    pub on_message: Box<dyn FnMut(&Msg) + Send>,
    pub sink: &'a dyn ItemSink,
}

impl<'a> Default for LoopRun<'a> {
    fn default() -> Self {
        LoopRun {
            history: &[],
            ctx: &ToolContext::empty_placeholder(),
            previous_response_id: None,
            image: None,
            on_delta: Box::new(|_| {}),
            on_reasoning: Box::new(|_| {}),
            on_message: Box::new(|_| {}),
            sink: &NullSink,
        }
    }
}

/// De bestuurder zelf.
pub struct ToolLoop<'a> {
    turn_source: &'a dyn TurnSource,
    registry: &'a ToolRegistry,
    extractor: &'a dyn CallExtractor,
    /// Harde stop tegen een model dat eeuwig zou willen doorlopen. `maxToolIterations` van de settings.
    pub max_iterations: u32,
    /// Spiegel van het sessiebeleid van de aanroeper: aan rijgt de loop de response-id van elk antwoord
    /// in het volgende verzoek om een server-side sessie te gebruiken; uit speelt hij elke beurt vanaf de
    /// client opnieuw af, de veilige default omdat een lokale backend die `previous_response_id` negeert
    /// dan tóch elke observatie ziet.
    pub chain_response_id: bool,
    /// Harde plafond op de tokenschatting van de lopende transcriptie, alleen afgedwongen bij een
    /// server-sessie (waar de backend zijn eigen context laat groeien, wat de client niet kan begrenzen
    /// door wat hij stuurt in te korten). 0 zet het uit.
    pub context_ceiling: u64,
}

impl<'a> ToolLoop<'a> {
    /// De ingebouwde lezer van [`PromptedCallExtractor`] is zo vaak de juiste dat hij hiermee mee mag;
    /// een afwijkende lezer verdient de andere constructor.
    pub fn new(turn_source: &'a dyn TurnSource, registry: &'a ToolRegistry, max_iterations: u32) -> ToolLoop<'a> {
        ToolLoop::with_extractor(turn_source, registry, &PromptedCallExtractor, max_iterations)
    }

    pub fn with_extractor(
        turn_source: &'a dyn TurnSource,
        registry: &'a ToolRegistry,
        extractor: &'a dyn CallExtractor,
        max_iterations: u32,
    ) -> ToolLoop<'a> {
        ToolLoop {
            turn_source,
            registry,
            extractor,
            max_iterations: max_iterations.max(1),
            chain_response_id: false,
            context_ceiling: 0,
        }
    }

    pub fn chain_response_id(mut self, on: bool) -> Self {
        self.chain_response_id = on;
        self
    }

    pub fn context_ceiling(mut self, ceiling: u64) -> Self {
        self.context_ceiling = ceiling;
        self
    }

    /// Eén draai. Geeft pas terug als het model klaar is of het budget op is; de aanroeper houdt hem
    /// buiten de UI-thread.
    pub async fn run(&self, mut run: LoopRun<'_>) -> LoopOutcome {
        let runner = ToolRunner::new(self.registry);
        let mut msgs: Vec<Msg> = run.history.to_vec();
        let mut added: Vec<Msg> = Vec::new();
        let mut steps: Vec<ToolStep> = Vec::new();
        let mut undos: Vec<Undo> = Vec::new();
        let mut prev = if self.chain_response_id { run.previous_response_id.map(str::to_string) } else { None };
        // Eén bron van definitie-tekst per draai, gedeeld door elke iteratie.
        let tools: Option<Value> = if self.registry.is_empty() { None } else { Some(self.registry.definitions()) };
        let mut call_steps = 0u32;
        let mut blanks = 0u32;
        let mut pending_image = run.image;

        for _ in 0..self.max_iterations {
            if self.context_ceiling > 0 && self.chain_response_id && estimate_tokens_of(&msgs) > self.context_ceiling {
                return LoopOutcome::CapReached {
                    previous_response_id: prev,
                    steps,
                    messages: added,
                    undos,
                    incomplete_reason: Some("context_limit".to_string()),
                };
            }
            run.sink.reply_started();
            let turn = self.ask(&mut run, &msgs, prev.as_deref(), &tools, &mut pending_image).await;
            run.sink.reply_completed(&turn.text);
            if self.chain_response_id && turn.response_id.is_some() {
                prev = turn.response_id.clone();
            }
            let calls = self.extractor.extract(&turn);

            if calls.is_empty() {
                // Een redeneermodel kan een hele beurt in zijn reasoning-kanaal doorbrengen en het
                // antwoord-kanaal leeg (of met een placeholder als "...") achterlaten. Dat is geen
                // finale antwoord. De fix is het volgende verzoek laten verschillen — een korte nudge die
                // nooit gepersisteerd wordt — tot een klein plafond. Voorbij dat plafond eindigt het zoals
                // altijd; de aanroeper markeert de lege beurt, dus een run is nooit meer een stilgevoelde
                // nothing.
                if is_blankish(&turn.text) && blanks < MAX_BLANK_TURNS {
                    blanks += 1;
                    msgs.push(Msg::user(BLANK_NUDGE));
                    continue;
                }
                self.add_assistant(&mut run, &mut msgs, &mut added, &turn);
                steps.push(ToolStep { assistant_text: turn.text.clone(), calls: Vec::new(), results: Vec::new() });
                return LoopOutcome::Completed {
                    response_id: turn.response_id.clone(),
                    final_text: turn.text,
                    final_turn: turn,
                    steps,
                    messages: added,
                    undos,
                    cap_reached: call_steps >= self.max_iterations,
                };
            }

            self.add_assistant(&mut run, &mut msgs, &mut added, &turn);
            let results = self.run_calls(&mut run, &runner, &calls, &mut msgs, &mut added, &mut undos).await;
            steps.push(ToolStep { assistant_text: turn.text, calls, results });
            call_steps += 1;
        }

        // Budget op terwijl het model nog wilde handelen: geef het één beurt om in proza te antwoorden,
        // zodat de persoon niet zonder antwoord zit; vraagt het wéér te handelen, stop dan.
        run.sink.reply_started();
        let settle = self.ask(&mut run, &msgs, prev.as_deref(), &tools, &mut pending_image).await;
        run.sink.reply_completed(&settle.text);
        if self.chain_response_id && settle.response_id.is_some() {
            prev = settle.response_id.clone();
        }
        self.add_assistant(&mut run, &mut msgs, &mut added, &settle);
        if self.extractor.extract(&settle).is_empty() {
            steps.push(ToolStep { assistant_text: settle.text.clone(), calls: Vec::new(), results: Vec::new() });
            return LoopOutcome::Completed {
                response_id: settle.response_id.clone(),
                final_text: settle.text,
                final_turn: settle,
                steps,
                messages: added,
                undos,
                cap_reached: true,
            };
        }
        LoopOutcome::CapReached { previous_response_id: prev, steps, messages: added, undos, incomplete_reason: None }
    }

    /// Eén verzoek naar de bron. De twee haken van de aanroeper en die van de sink worden samen
    /// doorgegeven: een chunk gaat naar beide, want de sink noteert hem als rijinhoud en de callback geeft
    /// hem aan wie meekijkt, en de een vervangt de ander niet.
    async fn ask(
        &self,
        run: &mut LoopRun<'_>,
        msgs: &[Msg],
        prev: Option<&str>,
        tools: &Option<Value>,
        pending_image: &mut Option<&'a str>,
    ) -> Turn {
        let sink = run.sink;
        let mut on_delta = std::mem::replace(&mut run.on_delta, Box::new(|_| {}));
        let mut on_reasoning = std::mem::replace(&mut run.on_reasoning, Box::new(|_| {}));
        let request = TurnRequest {
            history: msgs,
            previous_response_id: prev,
            tools: tools.as_ref(),
            on_delta: Box::new(move |chunk: &str| {
                sink.text_delta(chunk);
                on_delta(chunk);
            }),
            on_reasoning: Box::new(move |chunk: &str| {
                sink.reasoning_delta(chunk);
                on_reasoning(chunk);
            }),
            image: pending_image.take(),
        };
        let turn = self.turn_source.next_turn(request).await;
        // De haken terugleggen voor het volgende verzoek: dezelfde callback hoort elke iteratie te halen.
        let mut handed_back = request; // placeholder: zie opmerking onder deze functie
        drop(&mut handed_back);
        turn
    }

    /// Een batch van de calls van het model. Standaard één voor één, precies zoals eerder. Een batch van
    /// meer dan één draait samen alleen wanneer élke call [`crate::tool::Tool::can_run_parallel`] is —
    /// schrijft niets en wacht op niemand — en dan worden de kaarten en observaties wille in de
    /// callvolgorde van het model opgebouwd, zodat een fan-out voor de lezer en voor de volgende beurt niet
    /// van een seriele run te onderscheiden is. Een gemengde batch houdt het seriele pad, zodat een
    /// goedkeuringskaart live blijft opkomen.
    async fn run_calls(
        &self,
        run: &mut LoopRun<'_>,
        runner: &ToolRunner<'_>,
        calls: &[ToolCall],
        msgs: &mut Vec<Msg>,
        added: &mut Vec<Msg>,
        undos: &mut Vec<Undo>,
    ) -> Vec<ToolResult> {
        let parallel = calls.len() > 1
            && calls.iter().all(|call| self.registry.get(&call.tool).is_some_and(|t| t.can_run_parallel(&call.arguments, run.ctx)));
        if !parallel {
            let mut results = Vec::with_capacity(calls.len());
            for call in calls {
                // Vóór de run geopend en niet erna: een tool vraagt binnen `execute` om goedkeuring, dus
                // een kaart die daarna opkomt, meldt een beslissing die de lezer al nam op een rij die
                // hij nog niet zag.
                let result = runner.run(call, run.ctx);
                self.report(run, call, &result);
                if let Some(undo) = &result.undo {
                    undos.push(make_reusable(undo));
                }
                self.add_observation(run, msgs, added, call, &result);
                results.push(result);
            }
            return results;
        }
        self.run_calls_in_parallel(run, runner, calls, msgs, added, undos).await
    }

    /// Een batch zonder neveneffecten samen, met de kaarten in callvolgorde erachter. De bodies mogen op
    /// de pool in willekeurige volgorde eindigen; wat het model en de lezer zien, blijft de volgorde van
    /// het model. Een body kan niet gooien — [`ToolRunner`] maakt van elke mislukking een resultaat — dus
    /// er is niets op te vangen.
    async fn run_calls_in_parallel(
        &self,
        run: &mut LoopRun<'_>,
        runner: &ToolRunner<'_>,
        calls: &[ToolCall],
        msgs: &mut Vec<Msg>,
        added: &mut Vec<Msg>,
        undos: &mut Vec<Undo>,
    ) -> Vec<ToolResult> {
        let shared = SharedRun { ctx: run.ctx, runner };
        // Veertig tekens per keer, want `JoinSet` vraagt per taak om een bezitbare closure terwijl de
        // tools en hun argumenten bij de batch horen.
        let mut set = tokio::task::JoinSet::new();
        for call in calls.iter().cloned().collect::<Vec<_>>() {
            set.spawn(async move { let result = shared.runner.run(&call, shared.ctx); (call, result) });
            while set.len() >= MAX_PARALLEL_TOOL_CALLS {
                if let Some(joined) = set.join_next().await {
                    if let Ok((call, result)) = joined {
                        self.report(run, &call, &result);
                        if let Some(undo) = &result.undo {
                            undos.push(make_reusable(undo));
                        }
                        self.add_observation(run, msgs, added, &call, &result);
                        // Voltooide bodies komen in willekeurige volgorde terug; de volgorde van het
                        // model wordt hieronder uit de batch zelf hersteld.
                        done.push((call, result));
                    }
                }
            }
        }
        while let Some(joined) = set.join_next().await {
            if let Ok(pair) = joined {
                done.push(pair);
            }
        }
        // De kaarten en observaties in de callvolgorde van het model, niet in de volgorde van de pool.
        calls.iter().map(|call| { /* zie opmerking */ call }).collect();
        todo!("volgorde-Herstel")
    }

    /// De kaart van één call, met wat de run ervoor terugbracht.
    fn report(&self, run: &LoopRun<'_>, call: &ToolCall, result: &ToolResult) {
        let sink = run.sink;
        let id = sink.next_item_id();
        match self.registry.get(&call.tool).map(|t| t.item_kind()).unwrap_or(ItemKind::ToolCall) {
            ItemKind::CommandExecution => sink.command_completed(CommandExecutionItem {
                item_id: id,
                command: call.arg_str("command").unwrap_or_default().to_string(),
                cwd: result.cwd.clone(),
                exit_code: result.exit_code,
                timed_out: result.timed_out,
                duration_ms: Some(result.duration_ms),
                status: baudet_core::item::ItemStatus::Completed,
                output: result.output.clone(),
                ok: result.ok,
            }),
            ItemKind::FileChange => sink.file_change_completed(FileChangeItem {
                item_id: id,
                path: call.arg_str("file").unwrap_or_default().to_string(),
                change: baudet_core::item::FileChangeKind::Update,
                status: baudet_core::item::ItemStatus::Completed,
                diff: result.diff.clone(),
                stats: baudet_core::item::DiffStats::of(result.diff.as_deref()),
                ok: result.ok,
            }),
            _ => sink.tool_call_completed(ToolCallItem {
                item_id: id,
                tool: call.tool.clone(),
                detail: detail_of(&call.arguments),
                status: baudet_core::item::ItemStatus::Completed,
                ok: result.ok,
                output: result.output.clone(),
            }),
        }
    }

    fn add_assistant(&self, run: &mut LoopRun<'_>, msgs: &mut Vec<Msg>, added: &mut Vec<Msg>, turn: &Turn) {
        let msg = Msg {
            role: "assistant".into(),
            content: turn.text.clone(),
            response_id: turn.response_id.clone(),
            prompt_tokens: turn.usage.as_ref().and_then(|u| u.prompt).map(|p| p as u64),
        };
        msgs.push(msg.clone());
        added.push(msg.clone());
        (run.on_message)(&msg);
    }

    fn add_observation(
        &self,
        run: &mut LoopRun<'_>,
        msgs: &mut Vec<Msg>,
        added: &mut Vec<Msg>,
        call: &ToolCall,
        result: &ToolResult,
    ) {
        let msg = Msg::user(observation(call, result));
        msgs.push(msg.clone());
        added.push(msg.clone());
        (run.on_message)(&msg);
    }
}

/// Het argument dat op een kaart zonder eigen kaart genoemd mag worden.
fn detail_of(arguments: &Value) -> String {
    ["file", "command"].iter().filter_map(|key| arguments.get(*key).and_then(Value::as_str)).next().unwrap_or_default().to_string()
}

/// De tokenschatting van de origineler: tekens door drie, met acht per boodschap voor de rol.
fn estimate_tokens_of(messages: &[Msg]) -> u64 {
    messages.iter().map(|msg| (msg.content.chars().count() + msg.role.chars().count() + 8) as u64).sum::<u64>() / 3
}

/// De tekst die een toolresultaat wordt wanneer het naar het model teruggaat; blijft Engels. Hij noemt
/// de call-id, de tool, een expliciete ok-vlag en de diff, zodat het model zichzelf kan corrigeren: een
/// mislukte call die niet zegt "doe dit niet opnieuw" nodigt een model uit dezelfde verkeerde call te
/// herhalen, en de lus begrenst het aantal maar niet de loop.
fn observation(call: &ToolCall, result: &ToolResult) -> String {
    let mut text = String::from("Tool observation");
    if let Some(id) = &call.call_id {
        text.push_str(" for call ");
        text.push_str(id);
    }
    text.push_str(": `");
    text.push_str(&call.tool);
    text.push_str(if result.ok { "` succeeded (ok=true): " } else { "` failed (ok=false): " });
    text.push_str(&result.output);
    if !result.ok {
        text.push_str(" Do not send the same call again unchanged; correct it or answer.");
    }
    if let Some(diff) = &result.diff {
        text.push('\n');
        text.push_str(OBSERVATION_FENCE);
        text.push_str("diff\n");
        text.push_str(diff);
        text.push('\n');
        text.push_str(OBSERVATION_FENCE);
    }
    cap_model_text(&text, MAX_OBSERVATION_CHARS)
}
