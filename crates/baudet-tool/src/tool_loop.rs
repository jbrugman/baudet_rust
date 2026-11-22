//! De bestuurder: modelbeurt, dan uitvoeren, dan het resultaat teruggeven, dan opnieuw.
//!
//! Poort van `tool/ToolLoop.kt`. Hij orkestreert drie ingespoten naden — [`TurnSource`] voor het volgende
//! antwoord, [`CallExtractor`] om de calls te zien, [`ToolRunner`] om ze uit te voeren — en draagt zelf geen
//! transport, persistentie of UI, zodat élke afhankelijkheid een naad is die een test kan verwisselen.
//!
//! [`ItemSink`] is de vierde naad en de enige die hier nieuw is: de origineler haalt een `ItemSink` uit het
//! transcript-pakket, deze repo heeft nog geen transcript-laag, dus de loop definieert hem en laat de
//! persistentielaag hem straks implementeren. De loop persisteert dus zélf niets — hij vertelt alleen wat er
//! gebeurt, in dezelfde woorden die de rijen later krijgen.
//!
//! Model-gerichte tekst (`BLANK_NUDGE`, de observatie, de afkapmarker) blijft Engels, zoals in de origineler.

use crate::call_extractor::PromptedCallExtractor;
use crate::tool::{ToolCall, ToolContext, ToolRegistry, ToolResult, ToolRunner, Undo};
use crate::turn_source::{CallExtractor, TurnRequest, TurnSource};
use baudet_core::item::{
    CommandExecutionItem, FileChangeItem, FileChangeKind, ItemKind, ItemStatus, ToolCallItem,
};
use baudet_core::msg::Msg;
use baudet_core::protocol::Turn;
use serde_json::Value;
use std::sync::mpsc;
use std::thread;

/// Tool-calls die binnen één fan-out-batch samen mogen draaien. Twee, want deze app mikt op een lokale
/// bak: een paar read-only calls overlappen nuttig, terwijl een vloot alleen een hosted backend met
/// elastische workers helpt. Dit begrenst calls binnen één beurt, niet hele beurten — die andere grens is
/// `AppState.MAX_SAFE_PARALLEL`.
const MAX_PARALLEL_TOOL_CALLS: usize = 2;

/// Plafond op opeenvolgende lege, callloze beurten. Een redeneermodel dat zijn hele antwoord in zijn
/// reasoning-kanaal parkeert en op het antwoord-kanaal niets achterlaat, is niet klaar; één verzoek meer is
/// wat het in beweging brengt. Begrensd, zodat een model dat leeg blijft niet de lus kan laten draaien in
/// plaats van te antwoorden.
const MAX_BLANK_TURNS: u32 = 2;

/// Plafond op de tekst die één observatie op het model-kanaal mag zetten, in tekens. De volledige uitvoer
/// blijft in de transcriptierij — het sink-pad naar de kaart is onaangeroerd — en alleen wat het model te
/// zien krijgt, is begrensd. Een grote diff of grep die elke latere beurt wordt meegegeven, blaast anders
/// de hele historie op, wat een lange sessie traag maakt. Kop-gericht gehouden, zodat het model nog leest
/// hoe de call begon; de mens leest alles in de kaart.
const MAX_OBSERVATION_CHARS: usize = 32_000;

/// Het skelet met vaste vorm dat ruimte gereserveerd voor de marker, zodat kop + marker + staart onder het
/// plafond blijft; de echte marker ruilt alleen de nul-vullingen om voor cijfers van niet grotere breedte.
/// Eén vorm voor beide, anders is de gereserveerde breedte niet de breedte van wat erin komt.
const TRUNCATION_SKELETON: &str = "\n\n[... output truncated: keeping the first 0000 and last 0000 of 000000 characters; the full output stays in the transcript ...]\n\n";

/// Reden die de aanroeper krijgt wanneer het iteration-budget de lus sneed, niet het context-plafond. Uit
/// `LoopOutcome.kt:45`; de UI leest hieruit wat er gebeurde, dus de tekst is onderdeel van de naad.
const MAX_ITERATIONS_REASON: &str = "max_tool_iterations";

/// Reden die de lus geeft toen het context-plafond hem sneed, vóórdat er een verzoek uitging.
const CONTEXT_LIMIT_REASON: &str = "context_limit";

/// Een korte werkinstructie die aan de werkende conversatie wordt toegevoegd wanneer het model een beurt
/// leeg parkeert, zodat het volgende verzoek verschilt van het net lege. Een deterministisch lokaal model
/// speelt identieke boodschappen terug in een identiek leeg antwoord — de kale herbepaling veranderde dus
/// niets — dus alleen invoer die verschilde, kon het in beweging brengen. Blijft buiten de gepersisteerde
/// historie (nooit in `added`), zodat het transcript schoon blijft.
const BLANK_NUDGE: &str = "Your previous turn returned neither an answer nor a tool call. If you intend to use a tool, emit the call now; otherwise reply with your final answer.";

/// Of een callloze beurt niets draagt waarop een lezer kan handelen: leeg, of alleen placeholder-
/// leestekens zoals de "..." die een redeneermodel soms op het antwoord-kanaal achterlaat. Geen van beide is
/// een echt antwoord, dus beide verdienen één verzoek meer in plaats van een stille beurt.
fn is_blankish(text: &str) -> bool {
    text.chars().all(|c| c.is_whitespace() || c == '.' || c == '\u{2026}')
}

/// Begrens `text` tot ongeveer `max_chars` door het eerste drievijfde en het laatste tweevijfde om een
/// afkapmarker te houden. Modelgericht, dus Engels. Onder het plafond komt `text` ongeewijzigd terug; de
/// afgemaakte string overschrijdt `max_chars` nooit met meer dan de cijfers van de marker, dus de eigen
/// guard van een aanroeper op dat plafond houdt stand.
///
/// De snijpunten vallen op UTF-8-gransen, wat de origineler niet garandeert: zijn `substring` gooit op een
/// meerbaitig teken op precies die plek. Het granair wordt teruggeschoven naar het dichtstbijzijnde, zodat
/// één accent in een lange uitvoer geen crasht maar een iets kortere kop of staart.
pub fn cap_model_text(text: &str, max_chars: usize) -> String {
    let chars = text.chars().count();
    if max_chars == 0 || chars <= max_chars {
        return text.to_string();
    }
    let budget = max_chars.saturating_sub(TRUNCATION_SKELETON.chars().count());
    let head = budget * 3 / 5;
    let tail = budget - head;
    // Teken-inderingen naar byte-inderingen, altijd op een granair.
    let head_bytes = text.char_indices().nth(head).map(|(i, _)| i).unwrap_or(text.len());
    let tail_bytes = text.char_indices().nth(chars - tail).map(|(i, _)| i).unwrap_or(text.len());
    let marker = format!(
        "\n\n[... output truncated: keeping the first {head} and last {tail} of {chars} characters; the full output stays in the transcript ...]\n\n"
    );
    format!("{}{}{}", &text[..head_bytes], marker, &text[tail_bytes..])
}

/// De tokenschatting van de origineler: tekens door drie, met acht per boodschap voor de rol eromheen
/// (`SessionState.estimateTokensOf`). Het is een schatting en geen boekhouding, en hij wordt alleen als
/// grens gebruikt — de echte cijfers staan in [`baudet_core::usage::Usage`].
fn estimate_tokens_of(messages: &[Msg]) -> u64 {
    messages.iter().map(|msg| (msg.content.chars().count() + msg.role.chars().count() + 8) as u64).sum::<u64>() / 3
}

/// Het argument dat op een kaart genoemd mag worden die geen eigen kaart heeft.
fn detail_of(arguments: &Value) -> String {
    ["file", "command"].iter().filter_map(|key| arguments.get(*key).and_then(Value::as_str)).next().unwrap_or_default().to_string()
}

/// Een ganse-tal-argument tolerant gelezen, voor de timeout op een kaart. Een waarde die het model als
/// string stuurde, toont nog mee in plaats van stil weg te vallen; een waarde die geen getal is, blijft
/// afwezig, want een kaart mag geen limiet verzinnen die de tool niet heeft toegepast.
fn timeout_of(arguments: &Value) -> Option<i32> {
    match arguments.get("timeout_seconds")? {
        Value::Number(number) => number.as_i64().and_then(|v| i32::try_from(v).ok()),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// De tekst die een toolresultaat wordt wanneer het naar het model teruggaat; blijft Engels. Hij noemt de
/// call-id, de tool, een expliciete ok-vlag en de diff, zodat het model zichzelf kan corrigeren: een
/// mislukte call die niet zegt "doe dit niet opnieuw", nodigt een model uit dezelfde verkeerde call te
/// herhalen, en de lus begrenst het aantal maar niet de loop.
fn observation(call: &ToolCall, result: &ToolResult) -> String {
    let mut text = String::from("Tool observation");
    if let Some(id) = &call.call_id {
        text.push_str(" for call ");
        text.push_str(id);
    }
    text.push_str(": `");
    text.push_str(&call.tool);
    text.push('`');
    text.push_str(if result.ok { " succeeded (ok=true): " } else { " failed (ok=false): " });
    text.push_str(&result.output);
    if !result.ok {
        text.push_str(" Do not send the same call again unchanged; correct it or answer.");
    }
    if let Some(diff) = &result.diff {
        // Drie losse backticks uit een codepunt, zoals de origineler: zo staat de reeks nergens in de bron.
        let fence = "\u{60}\u{60}\u{60}";
        text.push('\n');
        text.push_str(fence);
        text.push_str("diff\n");
        text.push_str(diff);
        text.push('\n');
        text.push_str(fence);
    }
    cap_model_text(&text, MAX_OBSERVATION_CHARS)
}

/// Wat er tijdens een beurt op het scherm verschijnt.
///
/// De methods zijn uitdrukkelijk `&self` en niet `&mut self`: de streaming-haken van [`TurnRequest`] komen
/// vanaf de bron — in een echte backend vanaf diens eigen taak — dus een sink mag geen uniek eigendom van de
/// loop vergen. In de origineler *opent* en *sluit* een sink een rij (`commandStarted`/`commandCompleted`);
/// hier is één methode per afronding genoeg, omdat de rij-id uit [`ItemSink::next_item_id`] komt en de loop
/// een rij in één adempauze opent en sluit. De map-inhoud is dan identiek, en er is geen halve rij die een
/// afgebroken run achterlaat.
pub trait ItemSink: Send + Sync {
    /// Een uniek id voor één rij. De loop verzinnet ze niet zelf, want de persistentielaag is degene die
    /// weet welke ids er al bestaan.
    fn next_item_id(&self) -> String;
    /// Een modelantwoord is begonnen; deltas die volgen, behoren erbij tot [`Self::reply_completed`].
    fn reply_started(&self) {}
    /// Het antwoord van deze beurt is af, met de volledige tekst.
    fn reply_completed(&self, _text: &str) {}
    /// Deel van de antwoordtekst, respectievelijk van het redeneerwerk. Beide bestaan omdat de
    /// `onDelta`/`onReasoning`-callbacks van de aanroeper naast de sink blijven: de sink noteert de chunk
    /// als rijinhoud en de callback geeft hem aan wie meekijkt, en de een vervangt de ander niet.
    fn text_delta(&self, _chunk: &str) {}
    fn reasoning_delta(&self, _chunk: &str) {}
    /// Een commando-kaart, met wat de run er werkelijk van maakte. `cwd` komt uit het resultaat en niet uit
    /// de call: de tool lost het echte pad op, en via een symlink is dat niet het pad dat werd aangereikt.
    fn command_completed(&self, _item: CommandExecutionItem) {}
    /// Een schrijfkaart. `change` is [`FileChangeKind::Update`] en niet de `item_kind` van de tool:
    /// `edit_file` patched tekst in een bestand dat het eerst leest, dus de soorten komen overeen, en een
    /// afgewezen write wordt gedragen door de status van het item.
    fn file_change_completed(&self, _item: FileChangeItem) {}
    /// De generieke call-kaart: `read_file` en het alleen-lezen `git`.
    fn tool_call_completed(&self, _item: ToolCallItem) {}
}

/// De kaart van één commandorun, vanuit precies die run. `cwd` en `duration_ms` komen uit het resultaat en
/// niet uit de call: de tool lost het echte pad op, en via een symlink is dat niet het pad dat werd
/// aangereikt. Een `timeout_seconds` die geen getal is, blijft afwezig — een kaart mag geen limiet verzinnen
/// die de tool niet heeft toegepast.
fn command_card(item_id: String, call: &ToolCall, result: &ToolResult) -> CommandExecutionItem {
    CommandExecutionItem {
        item_id,
        command: call.arg_str("command").unwrap_or_default().to_string(),
        cwd: result.cwd.clone(),
        timeout_seconds: timeout_of(&call.arguments),
        status: if result.ok { ItemStatus::Completed } else { ItemStatus::Failed },
        exit_code: result.exit_code,
        timed_out: result.timed_out,
        duration_ms: result.duration_ms as i64,
        output: result.output.clone(),
        approval: None,
    }
}

/// De kaart van één schrijfpoging, met de preview-diff en de tellingen die eruit volgen. `change` is
/// [`FileChangeKind::Update`] en niet de `item_kind` van de tool: `edit_file` patched tekst in een bestand dat
/// het eerst leest, dus de soorten komen overeen, en een afgewezen write wordt gedragen door de status.
fn file_change_card(item_id: String, call: &ToolCall, result: &ToolResult) -> FileChangeItem {
    let stats = baudet_core::item::DiffStats::of(result.diff.as_deref());
    FileChangeItem {
        item_id,
        path: call.arg_str("file").unwrap_or_default().to_string(),
        change: FileChangeKind::Update,
        status: if result.ok { ItemStatus::Completed } else { ItemStatus::Failed },
        diff: result.diff.clone(),
        additions: stats.additions,
        deletions: stats.deletions,
    }
}

/// De kaart van een call zonder eigen kaart: `read_file` en het alleen-lezen `git`.
fn tool_call_card(item_id: String, call: &ToolCall, result: &ToolResult) -> ToolCallItem {
    ToolCallItem {
        item_id,
        tool: call.tool.clone(),
        detail: detail_of(&call.arguments),
        status: if result.ok { ItemStatus::Completed } else { ItemStatus::Failed },
        ok: result.ok,
        output: result.output.clone(),
    }
}

/// De sink die niets bijhoudt, voor een aanroeper die niet meekijkt.
pub struct NullSink;

impl ItemSink for NullSink {
    fn next_item_id(&self) -> String {
        String::new()
    }
}

/// Één stap van de lus: wat het model schreef, wat het vroeg, wat ervoor terugkwam.
#[derive(Debug)]
pub struct ToolStep {
    pub assistant_text: String,
    pub calls: Vec<ToolCall>,
    pub results: Vec<ToolResult>,
}

/// Waar een hele draai op eindigt. Slechts twee einden: het model stopte met vragen ([`LoopOutcome::Completed`])
/// of het plafond sneed hem af ([`LoopOutcome::CapReached`]). Geen van beide is een fout — een geweigerde of
/// mislukte tool gaat als observatie terug en het model kan doorgaan, dus tool-problemen verschijnen in
/// [`LoopOutcome::steps`], niet in het type.
///
/// `Debug` is met de hand geschreven: een handgreep is noch debugbaar noch vergelijkbaar, en wat een test
/// of een log van een draai wil weten, is hoeveel er landden en wat het model zag — precies dezelfde keuze die
/// `ToolResult` voor zijn eigen `undo`-veld maakt.
///
/// `messages` zijn alleeen de boodschappen die deze draai toevoegde (assistentbeurten plus de
/// tool-observaties), nooit de gebruikersboodschap van de aanroeper zelf, zodat die alleen de nieuwe beurt
/// persisteert. `undos` verzamelt de handgrepen van élke gelande write, en `response_id` is het id van het
/// laatste antwoord, voor wie een keten wil voortzetten of opslaan.
pub enum LoopOutcome {
    /// Het model gaf een finale zonder verder te vragen.
    Completed {
        final_text: String,
        /// Het antwoord dat de draai beëindigde, whole gehouden zodat de aanroeper usage en eindsignaal houdt.
        final_turn: Turn,
        response_id: Option<String>,
        steps: Vec<ToolStep>,
        messages: Vec<Msg>,
        undos: Vec<Undo>,
        /// `true` wanneer het antwoord pas na het iteration-budget kwam, zodat de aanroeper dat kan noteren.
        cap_reached: bool,
    },
    /// De draai stopte terwijl het model nog wilde handelen, vóór een finale. `incomplete_reason` zegt
    /// welke: het iteration-budget of het context-plafond dat een veroordeeld verzoek zou hebben gestuurd.
    CapReached {
        response_id: Option<String>,
        steps: Vec<ToolStep>,
        messages: Vec<Msg>,
        undos: Vec<Undo>,
        incomplete_reason: String,
    },
}

impl std::fmt::Debug for LoopOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoopOutcome::Completed { final_text, response_id, steps, messages, undos, cap_reached, .. } => f
                .debug_struct("Completed")
                .field("final_text", final_text)
                .field("response_id", response_id)
                .field("steps", steps)
                .field("messages", messages)
                .field("undos", &undos.len())
                .field("cap_reached", cap_reached)
                .finish(),
            LoopOutcome::CapReached { response_id, steps, messages, undos, incomplete_reason } => f
                .debug_struct("CapReached")
                .field("response_id", response_id)
                .field("steps", steps)
                .field("messages", messages)
                .field("undos", &undos.len())
                .field("incomplete_reason", incomplete_reason)
                .finish(),
        }
    }
}

impl LoopOutcome {
    /// De boodschappen die deze draai toevoegde, voor de aanroeper die ze persisteert.
    pub fn messages(&self) -> &[Msg] {
        match self {
            LoopOutcome::Completed { messages, .. } | LoopOutcome::CapReached { messages, .. } => messages,
        }
    }

    /// De handgrepen van de gelande writes, in de volgorde waarin ze landden.
    pub fn undos(&self) -> &[Undo] {
        match self {
            LoopOutcome::Completed { undos, .. } | LoopOutcome::CapReached { undos, .. } => undos,
        }
    }

    /// Het response-id van het laatste antwoord.
    pub fn response_id(&self) -> Option<&str> {
        match self {
            LoopOutcome::Completed { response_id, .. } | LoopOutcome::CapReached { response_id, .. } => {
                response_id.as_deref()
            }
        }
    }
}

/// Wat één Modelbeurt kostte, doorgegeven zoals de origineler het uit `Turn.usage` haalt.
fn prompt_tokens_of(turn: &Turn) -> Option<u64> {
    turn.usage.as_ref().and_then(|usage| usage.prompt).map(|prompt| prompt as u64)
}

/// De argumenten van één draai. De origineler draagt acht parameters met zes defaults; deze repo heeft die
/// defaults niet, dus het is één struct waarvan de verplichte velden hierboven staan en de rest leeg mag.
pub struct LoopRun<'a> {
    /// De lopende conversatie, nieuwste boodschap laatst. De loop kopiert haar en laat de ingang onaangeroerd.
    pub history: &'a [Msg],
    /// De rechten, goedkeuringen en pins van deze gebruiker op deze map.
    pub ctx: &'a ToolContext,
    /// De response-id van de vorige beurt, alleen bruikbaar wanneer [`ToolLoop::chain_response_id`] aan staat.
    pub previous_response_id: Option<&'a str>,
    /// Een beeld gaat mee op het eerste verzoek van deze draai; latere iteraties spelen tekst.
    pub image: Option<&'a str>,
    /// Callbacks van de aanroeper zelf. Ze blijven naast de sink bestaan (zie [`ItemSink`]). Het leven is
    /// `'a`, zodat de loop er zijn eigen samenvoeging van kan maken zonder er een `'static`-kopie van te hoeven
    /// lossen; de reden staat bij [`TurnRequest::on_delta`].
    pub on_delta: Box<dyn FnMut(&str) + Send + 'a>,
    pub on_reasoning: Box<dyn FnMut(&str) + Send + 'a>,
    /// Krijgt élke boodschap die de loop toevoegt, in de volgorde van toevoegen.
    pub on_message: Box<dyn FnMut(&Msg) + Send + 'a>,
    pub sink: &'a dyn ItemSink,
}

/// Een streaming-haak in een box met het leven van zijn leningen.
///
/// Deze hulp bestaat omdat een dynamisch boxtype het leven niet uit een closure haalt: de sluiting is
/// `FnMut(&str) + Send` zónder leven, dus een kale `Box::new(move |…| …)` op een veld van
/// `Box<dyn FnMut(&str) + Send + 'a>` krijgt `'static` en faalt daar. Met deze handtekening kiist de
/// compiler het leven uit de leningen. Bewezen in een wegwerpcrate: mét deze hulp compileert de
/// samenvoeging sink+callback, zónder niet. Het leven krijgt een naam (`'a`) en geen `'_`, want `'_` is
/// in een fn-signatuur gereserveerd.
fn boxed<'a, F>(f: F) -> Box<dyn FnMut(&str) + Send + 'a>
where
    F: FnMut(&str) + Send + 'a,
{
    Box::new(f)
}

/// De context van deze draai, gedeeld door de draden van een fan-out.
///
/// [`ToolContext`] is `Send` maar níet `Sync`: de goedkeuringshaken en de twee providers zitten in een
/// `Box<dyn Fn … + Send>`, en `Box` erft de `Sync` van zijn contents niet. Een `&ToolContext` mag dus niet
/// door twee draden tegelijk geleerd worden, terwijl de runner van élke call diezelfde lening nodig heeft.
/// Daarom deze ene uitdrukkelijke wrapper: de garantie komt niet uit het type maar uit de eigenschap van
/// het parallelle pad zelf — élke call daar is [`crate::tool::Tool::can_run_parallel`], dus hij schrijft
/// niets aan de context en vraagt geen goedkeuring; hij leest alleen de rechten, en hoogstens
/// [`MAX_PARALLEL_TOOL_CALLS`] draden doen dat tegelijk. De `unsafe impl`s gelden alleen voor deze wrapper,
/// en `thread::scope` laat ze niet buiten de scope bestaan, dus er kan geen verwijzing de draaien overleven.
/// Bewezen in een wegwerpcrate: zónder deze wrapper wijst de compiler `&ToolContext` in beide poolvormen af
/// (`JoinSet::spawn` en `thread::spawn` eisen `Sync` van élke geleende waarde), mét wrapper compileert de
/// scope.
struct SharedContext<'a>(&'a ToolContext);

unsafe impl Send for SharedContext<'_> {}
unsafe impl Sync for SharedContext<'_> {}

/// De bestuurder zelf.
pub struct ToolLoop<'a> {
    turn_source: &'a dyn TurnSource,
    registry: &'a ToolRegistry,
    extractor: &'a dyn CallExtractor,
    runner: ToolRunner<'a>,
    /// Harde stop tegen een model dat eeuwig zou willen doorlopen: `maxToolIterations` uit de settings.
    pub max_iterations: u32,
    /// Spiegel van het sessiebeleid van de aanroeper: aan rijgt de loop het response-id van elk antwoord in
    /// het volgende verzoek om een server-side sessie te gebruiken; uit speelt hij elke beurt vanaf de client
    /// opnieuw af, de veilige default omdat een lokale backend die `previous_response_id` negeert dan tóch
    /// élke observatie ziet.
    pub chain_response_id: bool,
    /// Harde plafond op de tokenschatting van de lopende conversatie, alleen afgedwongen bij een server-sessie
    /// (waar de backend zijn eigen context laat groeien, wat de client niet kan begrenzen door wat hij stuurt
    /// in te korten). 0 zet het uit.
    pub context_ceiling: u64,
}

impl<'a> ToolLoop<'a> {
    /// De ingebouwde lezer van [`PromptedCallExtractor`] is zo vaak de juiste dat hij hiermee mee mag; een
    /// afwijkende lezer — een native function-calling-backend — verdient [`ToolLoop::with_extractor`].
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
            runner: ToolRunner::new(registry),
            // `coerceAtLeast(1)`: een budget van nul zou een draai zonder enkel verzoek maken.
            max_iterations: max_iterations.max(1),
            chain_response_id: false,
            context_ceiling: 0,
        }
    }

    pub fn chain_response_id(mut self, on: bool) -> ToolLoop<'a> {
        self.chain_response_id = on;
        self
    }

    pub fn context_ceiling(mut self, ceiling: u64) -> ToolLoop<'a> {
        self.context_ceiling = ceiling;
        self
    }

    /// Eén draai. Geeft pas terug als het model klaar is of het plafond hem sneed; de aanroeper houdt hem
    /// buiten de UI-thread, want hij blokkeert op élke beurt en op élke goedkeuring.
    pub async fn run(&self, run: &mut LoopRun<'_>) -> LoopOutcome {
        let mut msgs: Vec<Msg> = run.history.to_vec();
        let mut added: Vec<Msg> = Vec::new();
        let mut steps: Vec<ToolStep> = Vec::new();
        let mut undos: Vec<Undo> = Vec::new();
        let mut prev = if self.chain_response_id { run.previous_response_id.map(str::to_string) } else { None };
        // Eén bron van definitie-tekst per draai, gedeeld door élke iteratie, zodat een registry die tussendoor
        // wijzigt (dat mag niet, maar zo'n fout wil je niet belonen) niet twee vormen door één conversatie stuurt.
        let tools: Option<Value> = if self.registry.is_empty() { None } else { Some(self.registry.definitions()) };
        let mut pending_image = run.image;
        let mut call_steps = 0u32;
        let mut blanks = 0u32;

        for _ in 0..self.max_iterations {
            if self.context_ceiling > 0 && self.chain_response_id && estimate_tokens_of(&msgs) > self.context_ceiling {
                return LoopOutcome::CapReached {
                    response_id: prev,
                    steps,
                    messages: added,
                    undos,
                    incomplete_reason: CONTEXT_LIMIT_REASON.to_string(),
                };
            }
            run.sink.reply_started();
            let turn = self.ask(run, &mut prev, &msgs, tools.as_ref(), &mut pending_image).await;
            run.sink.reply_completed(&turn.text);
            if self.chain_response_id && turn.response_id.is_some() {
                prev = turn.response_id.clone();
            }
            let calls = self.extractor.extract(&turn);

            if calls.is_empty() {
                // Een redeneermodel kan een hele beurt in zijn reasoning-kanaal doorbrengen en het
                // antwoord-kanaal leeg (of met een placeholder als "...") achterlaten, zonder extractionele call.
                // Dat is geen finale antwoord. Een kale herbepaling hielp alleen bij een server-sessie: dezelfde
                // boodschappen in een deterministisch lokaal model geven hetzelfde lege antwoord terug. De fix
                // is het volgende verzoek laten verschillen — een korte nudge die nooit gepersisteerd wordt —
                // tot een klein plafond. Voorbij dat plafond eindigt het zoals altijd; de aanroeper markeert de
                // lege beurt, dus een run is nooit meer een stilgevoelde niets.
                if is_blankish(&turn.text) && blanks < MAX_BLANK_TURNS {
                    blanks += 1;
                    // Alleen in de werkende kop, nooit in `added`: het transcript blijft schoon.
                    msgs.push(Msg::user(BLANK_NUDGE));
                    continue;
                }
                self.add_assistant(run, &mut msgs, &mut added, &turn);
                steps.push(ToolStep { assistant_text: turn.text.clone(), calls: Vec::new(), results: Vec::new() });
                return LoopOutcome::Completed {
                    final_text: turn.text.clone(),
                    final_turn: turn.clone(),
                    response_id: turn.response_id.clone(),
                    steps,
                    messages: added,
                    undos,
                    cap_reached: call_steps >= self.max_iterations,
                };
            }

            self.add_assistant(run, &mut msgs, &mut added, &turn);
            let results = self.run_calls(run, &calls, &mut msgs, &mut added, &mut undos).await;
            steps.push(ToolStep { assistant_text: turn.text, calls, results });
            call_steps += 1;
        }

        // Budget op terwijl het model nog wilde handelen: geef het één beurt om in proza te antwoorden, zodat
        // de persoon niet zonder antwoord zit; vraagt het wéér te handelen, stop dan.
        run.sink.reply_started();
        let settle = self.ask(run, &mut prev, &msgs, tools.as_ref(), &mut pending_image).await;
        run.sink.reply_completed(&settle.text);
        if self.chain_response_id && settle.response_id.is_some() {
            prev = settle.response_id.clone();
        }
        self.add_assistant(run, &mut msgs, &mut added, &settle);
        if self.extractor.extract(&settle).is_empty() {
            steps.push(ToolStep { assistant_text: settle.text.clone(), calls: Vec::new(), results: Vec::new() });
            return LoopOutcome::Completed {
                final_text: settle.text.clone(),
                final_turn: settle.clone(),
                response_id: settle.response_id.clone(),
                steps,
                messages: added,
                undos,
                cap_reached: true,
            };
        }
        LoopOutcome::CapReached {
            response_id: prev,
            steps,
            messages: added,
            undos,
            incomplete_reason: MAX_ITERATIONS_REASON.to_string(),
        }
    }

    /// Eén verzoek naar de bron. `prev` wordt bijgewerkt zoals in de loop, zodat de keten én de uitkomst
    /// hetzelfde id dragen; `pending_image` leegt hier, want een beeld speelt niet twee keer mee.
    async fn ask<'r>(
        &self,
        run: &'r mut LoopRun<'_>,
        prev: &'r mut Option<String>,
        msgs: &'r [Msg],
        tools: Option<&'r Value>,
        pending_image: &'r mut Option<&'a str>,
    ) -> Turn {
        // Eén sluiting per kanaal: de sink noteert de chunk als rijinhoud en de callback van de aanroeper
        // geeft hem aan wie meekijkt. Twee aparte sluitingen, want één `FnMut`-box kan niet door twee
        // eigenaars geleerd worden; de sink en de callbacks zelf zijn gedeeld, dus de volgorde binnen één
        // chunk blijft sink-eerst, precies zoals de origineler `emitText` en `emitReasoning` bouwt.
        let sink = run.sink;
        let on_delta = &mut *run.on_delta;
        let on_reasoning = &mut *run.on_reasoning;
        let request = TurnRequest {
            history: msgs,
            previous_response_id: prev.as_deref(),
            tools,
            on_delta: boxed(move |chunk: &str| {
                sink.text_delta(chunk);
                on_delta(chunk);
            }),
            on_reasoning: boxed(move |chunk: &str| {
                sink.reasoning_delta(chunk);
                on_reasoning(chunk);
            }),
            image: pending_image.take(),
        };
        let turn = self.turn_source.next_turn(request).await;
        if self.chain_response_id && turn.response_id.is_some() {
            *prev = turn.response_id.clone();
        }
        turn
    }

    /// Een batch van de calls van het model. Standaard één voor één, precies zoals eerder. Een batch van meer
    /// dan één draait samen, maar alleen wanneer élke call [`crate::tool::Tool::can_run_parallel`] is —
    /// schrijft niets en wacht op niemand — en dan worden de kaarten en observaties wille in de callvolgorde
    /// van het model opgebouwd, zodat een fan-out voor de lezer en voor de volgende beurt niet van een serieuze
    /// run te onderscheiden is. Een gemengde batch houdt het serieuze pad, zodat een goedkeuringskaart live
    /// blijft opkomen.
    async fn run_calls(
        &self,
        run: &mut LoopRun<'_>,
        calls: &[ToolCall],
        msgs: &mut Vec<Msg>,
        added: &mut Vec<Msg>,
        undos: &mut Vec<Undo>,
    ) -> Vec<ToolResult> {
        let parallel = calls.len() > 1
            && calls.iter().all(|call| {
                self.registry.get(&call.tool).is_some_and(|tool| tool.can_run_parallel(&call.arguments, run.ctx))
            });
        if parallel {
            return self.run_calls_in_parallel(run, calls, msgs, added, undos).await;
        }
        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            // Vóór de run gemeld en niet erna: een tool vraagt binnen `execute` om goedkeuring, dus een kaart
            // die daarna opkomt, meldt een beslissing die de lezer al nam op een rij die hij nog niet zag.
            let mut result = self.runner.run(call, run.ctx);
            self.report(run, call, &result);
            if let Some(undo) = result.undo.take() {
                undos.push(undo);
            }
            self.add_observation(run, msgs, added, call, &result);
            results.push(result);
        }
        results
    }

    /// Een batch zonder neveneffecten samen, met de kaarten en observaties in de callvolgorde van het model
    /// erachter. De bodies mogen in willekeurige volgorde eindigen; wat het model en de lezer zien, blijft de
    /// volgorde van het model, en dat is de eigenschap die een fan-out van een serieuze run ononderscheidbaar
    /// maakt. Een body kan niet gooien — [`ToolRunner`] maakt van élke mislukking een resultaat — maar een
    /// resultaat dat de pool niet produced, wordt geweigerd in plaats van geraden: een lege plek is een nette
    /// ontkenning voor het model, geen crash en geen fantoom-uitkomst.
    ///
    /// De pool is een scoped stel `std::thread`s en geen tokio-taak, om twee gemeten redenen. De consument
    /// telt de openstaande draden af en sluit zijn eigen zendreferentie vóór hij gaat wachten; zonder die
    /// sluiting wordt `recv` van geen enkele kant wakker na de laatste call, wat deze functie in een eerdere
    /// opzet liet hangen. De naden die de
    /// loop leent, zijn `Send` maar niet `Sync`: [`ItemSink`] en [`TurnSource`] eisen dat niet, en
    /// [`ToolContext`]s goedkeuringen zijn `Box<dyn Fn … + Send>`, waarvan `Box` de `Sync` niet erft. Zowel
    /// `JoinSet::spawn` als `thread::spawn` eist `Sync` van élke geleende waarde, dus de enige eerlijke vorm is
    /// een pool die zijn leningen zélf begrenst: [`thread::scope`] laat de draden niet de context overleven,
    /// en [`SharedContext`] maakt het delen ervan uitdrukkelijk — hoogstens [`MAX_PARALLEL_TOOL_CALLS`] tegelijk.
    /// Daarnaast is [`crate::tool::Tool::execute`] blokkerende code (een proces afwachten, op een goedkeuring
    /// wachten) die een runtime-werkernaar niet mag bezetten. De origineler doet hetzelfde met een gewone
    /// executor.
    async fn run_calls_in_parallel(
        &self,
        run: &mut LoopRun<'_>,
        calls: &[ToolCall],
        msgs: &mut Vec<Msg>,
        added: &mut Vec<Msg>,
        undos: &mut Vec<Undo>,
    ) -> Vec<ToolResult> {
        let runner = &self.runner;
        let ctx = SharedContext(run.ctx);
        let (sender, receiver) = mpsc::channel::<(usize, ToolResult)>();
        let mut slots: Vec<Option<ToolResult>> = (0..calls.len()).map(|_| None).collect();

        thread::scope(|scope| {
            // Een draad die één call uitvoert en zijn resultaat in de wachtstapel legt.
            //
            // De handgreep gaat niet mee: deze pad krijgt alleen calls die niets schrijven, dus er valt niets
            // te dragen. Wat er toch aanzit, wordt genegeerd en niet nagemaakt — `LoopOutcome.undos` is de
            // lijst waarmee de persoon de beurt terugdraait, en een handgreep die de loop niet kan bewijzen,
            // hoort daar niet in. Het `send` mag falen: het kanaal dicht zodra de laatste draad terug is en
            // deze consumer zijn eigen zendreferentie losliet, en dan luistert er niemand meer.
            let mut next = 0usize;
            let mut outstanding = 0usize;
            while next < calls.len() && outstanding < MAX_PARALLEL_TOOL_CALLS {
                let call = &calls[next];
                let slot = next;
                next += 1;
                outstanding += 1;
                let sender = sender.clone();
                let ctx = &ctx;
                scope.spawn(move || {
                    let mut result = runner.run(call, ctx.0);
                    let undo = result.undo.take();
                    debug_assert!(undo.is_none(), "een paralelle call schrijft niets en heeft dus geen handgreep");
                    let _ = sender.send((slot, result));
                });
            }
            // De consumer houdt zijn eigen zendreferentie niet vast. `recv` wordt wakker van twee kanten: een
            // bericht, of een kanaal zonder levende zenders. Met deze referentie in leven is die tweede kant
            // er nooit, dus na de laatste call bleef `recv` wachten en ving de batch geen resultaat — gemeten:
            // één call die "produced no result" terugkreeg. Eerst de eerste lading wegspringen en de
            // referentie dán pas loslaten: `Sender::clone` mag tegen een gesloten kanaal, de kloon bereikt dan
            // alleen niets meer, dus de nabevulling hieronder heeft geen open kanaal nodig.
            // De nabevulling heeft nog een eigen zender nodig: `drop` consumeren is een verplaatsing, dus
            // wat hieronder staat, leest niet meer uit `sender` zelf.
            let refill = sender;
            while outstanding > 0 {
                match receiver.recv() {
                    Ok((slot, result)) => {
                        slots[slot] = Some(result);
                        outstanding -= 1;
                    }
                    // Niemand kan nog zenden: er komt niets meer. Een leeg slot wordt hierna geweigerd in
                    // plaats van geraden.
                    Err(_) => break,
                }
                // Pas hier opnieuw vullen, want het aantal lopende draden is geen signaal dat er iets klaar is.
                while next < calls.len() && outstanding < MAX_PARALLEL_TOOL_CALLS {
                    let call = &calls[next];
                    let slot = next;
                    next += 1;
                    outstanding += 1;
                    let sender = refill.clone();
                    let ctx = &ctx;
                    scope.spawn(move || {
                        let mut result = runner.run(call, ctx.0);
                        let undo = result.undo.take();
                        debug_assert!(undo.is_none(), "een paralelle call schrijft niets en heeft dus geen handgreep");
                        let _ = sender.send((slot, result));
                    });
                }
            }
        });

        let mut ordered = Vec::with_capacity(calls.len());
        for (index, call) in calls.iter().enumerate() {
            let mut result = slots[index].take().unwrap_or_else(|| {
                ToolResult::fail(format!("tool `{}` produced no result", call.tool))
            });
            self.report(run, call, &result);
            if let Some(undo) = result.undo.take() {
                undos.push(undo);
            }
            self.add_observation(run, msgs, added, call, &result);
            ordered.push(result);
        }
        ordered
    }

    /// De kaart van één call, met wat de run ervoor terugbracht. De kaart komt uit de `item_kind` van de
    /// tool zelf, zodat een hernoeming een run niet stil in de verkeerde rij zet. Een tool die niet te
    /// resolven is, krijgt de generieke kaart: een call die de loop niet kon dispatchen, is precies het
    /// evenement dat de mens mag zien, en de runner maakt er al een mislukt resultaat van — verbergen zou de
    /// mens een mislukking laten zien die het model zag, zonder uitleg.
    fn report(&self, run: &LoopRun<'_>, call: &ToolCall, result: &ToolResult) {
        let sink = run.sink;
        let id = sink.next_item_id();
        match self.registry.get(&call.tool).map(|tool| tool.item_kind()).unwrap_or(ItemKind::ToolCall) {
            ItemKind::CommandExecution => sink.command_completed(command_card(id, call, result)),
            ItemKind::FileChange => sink.file_change_completed(file_change_card(id, call, result)),
            _ => sink.tool_call_completed(tool_call_card(id, call, result)),
        }
        // Een tool die een beeld terugbracht, krijgt in de origineler een eigen rij ná zijn een-regelige
        // resultaat, zodat het beeld inline verschijnt. Die adapter (MCP) bestaat in deze repo nog niet en
        // `ToolResult` draagt er geen veld voor, dus er is hier niets te melden; de rijsoort staat wel al in
        // `ItemKind::Image`. Wie het beeld-poort poort, voegt hier de regel toe en nergens anders.
    }

    /// De assistant-boodschap van één beurt: tekst, response-id en de prompt-kosten die de backend meldde.
    fn add_assistant(&self, run: &mut LoopRun<'_>, msgs: &mut Vec<Msg>, added: &mut Vec<Msg>, turn: &Turn) {
        let msg = Msg {
            role: "assistant".to_string(),
            content: turn.text.clone(),
            response_id: turn.response_id.clone(),
            prompt_tokens: prompt_tokens_of(turn),
        };
        msgs.push(msg.clone());
        added.push(msg.clone());
        (run.on_message)(&msg);
    }

    /// De observatie van één call, als gebruikersboodschap: de origineler voert een toolresultaat terug in
    /// dezelfde rol als een menselijke opmerking, want voor het model is het simpelweg volgende invoer.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::Tool;
    use crate::turn_source::TurnFuture;
    use baudet_core::protocol::FunctionCall;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Het aantal tekens van het observatie-plafond, opgevraagd op één plek zodat geen enkele test een
    /// drempel zelf raadt — de eigenschap die getest wordt, is het plafond zélf.
    const CAP: usize = MAX_OBSERVATION_CHARS;

    /// Een afgebakende call, zoals de ingebouwde lezer hem produceert (die zet ook een call-id neer).
    fn call(tool: &str, arguments: &Value, id: Option<&str>) -> ToolCall {
        ToolCall { tool: tool.to_string(), arguments: arguments.clone(), call_id: id.map(str::to_string) }
    }

    // ---- de model-tekst van één observatie ----------------------------------------------

    #[test]
    fn an_observation_names_the_call_and_its_verdict() {
        let text = observation(
            &call("read_file", &json!({"file": "a.txt"}), Some("call_7")),
            &ToolResult::ok("inhoud"),
        );
        // De call-id, de toolnaam en de ok-vlag moeten er letterlijk in staan: dat is wat een model nodig
        // heeft om zijn eigen eerdere call te herkennen in plaats van te raden.
        assert!(text.contains("for call call_7"), "{text}");
        assert!(text.contains(": `read_file`"), "{text}");
        assert!(text.contains(" succeeded (ok=true): inhoud"), "{text}");
        assert!(!text.contains("ok=false"), "{text}");
        assert!(!text.contains("Do not send the same call again"), "een geslaagde call hoeft niet te corrigeren");
    }

    #[test]
    fn a_failed_observation_tells_the_model_not_to_repeat() {
        let text = observation(
            &call("execute", &json!({"command": "false"}), None),
            &ToolResult::fail("exit code: 1"),
        );
        assert!(text.contains(" failed (ok=false): exit code: 1"), "{text}");
        assert!(text.contains("Do not send the same call again unchanged; correct it or answer."), "{text}");
        // Zonder backend-id blijft er een observatie zonder call-verwijzing, geen lege id.
        assert!(!text.contains("for call "), "{text}");
    }

    #[test]
    fn an_observation_carries_its_diff_in_a_fence() {
        let mut result = ToolResult::ok("g written");
        result.diff = Some("--- a\n+++ b\n+line".to_string());
        let text = observation(&call("edit_file", &json!({"file": "g"}), Some("c1")), &result);
        // De mens-kaart toont de diff apart; wat hier telt, is dat het model hem in een fenced diff-blok krijgt.
        let fence = "\u{60}\u{60}\u{60}";
        assert!(text.contains(&format!("\n{fence}diff\n--- a\n+++ b\n+line\n{fence}")), "{text}");
    }

    // ---- cap_model_text ---------------------------------------------------------------

    #[test]
    fn text_under_the_cap_comes_back_unchanged() {
        let text = "een regel\nmet\nmeerdere regels";
        assert_eq!(cap_model_text(text, CAP), text);
        assert_eq!(cap_model_text("", CAP), "");
    }

    #[test]
    fn a_capped_observation_stays_within_the_cap_plus_the_marker_digits() {
        let huge = "x".repeat(CAP * 3);
        let capped = cap_model_text(&huge, CAP);
        let width = capped.chars().count() - CAP;
        // De gereserveerde breedte in het skelet is de breedte van wat erin komt: de cijfers van kop, staart
        // en totaal zijn nooit breder dan de vier, vier en zes nullen waarvoor ruimte is gereserveerd.
        assert!(width <= 14, "ouder dan het plafond met {width} tekens");
        assert!(capped.contains("output truncated"), "{capped}");
        assert!(capped.starts_with('x') && capped.ends_with('x'));
        assert!(capped.chars().count() < huge.chars().count());
    }

    #[test]
    fn capping_never_splits_a_character() {
        // Met `é` (twee bytes) snijdt de origineler op een byte-indering en gooit zijn substring; de snij-
        // punten hier vallen op granen, dus één accent in een lange uitvoer mag nooit een run crashing.
        let text = "é".repeat(CAP * 3);
        let capped = cap_model_text(&text, CAP);
        assert!(capped.chars().all(|c| c == 'é' || c.is_ascii()), "geen vervangingstekens");
        assert!(!capped.contains('\u{fffd}'), "{capped}");
        // De snijpunten staan ruim binnen de reeks, dus beide kanten zijn echt afgeknipt.
        assert!(capped.chars().count() < text.chars().count());
    }

    // ---- de hulpen waarmee de loop zonder transport en zonder echte tools draait ------

    /// Een sink die bijhoudt wat er op het scherm verscheen, in de volgorde van melden, plus de nummers die
    /// hij uitdeelde. [`ItemSink`] is `Send + Sync`, dus de fan-out meldt er ook op en de volgorde is de
    /// eigenschap die dan getest wordt.
    #[derive(Default)]
    struct RecordingSink {
        lines: Mutex<Vec<String>>,
        ids: AtomicUsize,
    }

    impl RecordingSink {
        fn record(&self, line: impl Into<String>) {
            self.lines.lock().unwrap().push(line.into());
        }

        fn lines(&self) -> Vec<String> {
            self.lines.lock().unwrap().clone()
        }

        fn count(&self, needle: &str) -> usize {
            self.lines().iter().filter(|line| line.contains(needle)).count()
        }
    }

    impl ItemSink for RecordingSink {
        fn next_item_id(&self) -> String {
            format!("item-{}", self.ids.fetch_add(1, Ordering::SeqCst) + 1)
        }
        fn reply_started(&self) {
            self.record("reply-started");
        }
        fn reply_completed(&self, text: &str) {
            self.record(format!("reply-completed {text}"));
        }
        fn text_delta(&self, chunk: &str) {
            self.record(format!("delta {chunk}"));
        }
        fn reasoning_delta(&self, chunk: &str) {
            self.record(format!("reasoning {chunk}"));
        }
        fn command_completed(&self, item: CommandExecutionItem) {
            self.record(format!("command {} {}", item.item_id, item.command));
        }
        fn file_change_completed(&self, item: FileChangeItem) {
            self.record(format!("filechange {} {}", item.item_id, item.path));
        }
        fn tool_call_completed(&self, item: ToolCallItem) {
            self.record(format!("toolcall {} {}", item.item_id, item.tool));
        }
    }

    /// Een tool met één gedrag, in plaats van een replica van een echte handler.
    struct StubTool {
        name: &'static str,
        item: ItemKind,
        parallel: bool,
        delay: Option<std::time::Duration>,
        gate: Option<std::sync::Arc<Gate>>,
        /// Zet zijn spoor in `overlap` en wacht `delay`; zo is een echte gelijktijdigheid meetbaar en niet
        /// aangenomen.
        barrier: bool,
    }

    impl StubTool {
        fn new(name: &'static str) -> StubTool {
            StubTool { name, item: ItemKind::ToolCall, parallel: false, delay: None, gate: None, barrier: false }
        }
    }

    impl Tool for StubTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "stub"
        }
        fn parameters(&self) -> Value {
            json!({"type": "object"})
        }
        fn item_kind(&self) -> ItemKind {
            self.item
        }
        fn can_run_parallel(&self, _args: &Value, _ctx: &ToolContext) -> bool {
            self.parallel
        }
        fn execute(&self, args: &Value, _ctx: &ToolContext) -> ToolResult {
            if self.barrier {
                let gate = self.gate.clone().expect("een barrier-call draagt een poort");
                gate.arrive();
                gate.wait();
            }
            if let Some(delay) = self.delay {
                std::thread::sleep(delay);
            }
            let file = args.get("file").and_then(Value::as_str).unwrap_or("-");
            ToolResult::ok(format!("({} {file})", self.name))
        }
    }

    /// Een tool die schrijft: eigen kaart, `can_run_parallel` uit (de default) en een handgreep die hij
    /// daadwerkelijk afgeeft — dat laatste is wat de loop in `undos` moet verzamelen. De teller staat in een
    /// `Arc`, want een handgreep is `'static` en mag dus geen lening van de tool meedragen.
    struct WriterTool {
        writes: std::sync::Arc<AtomicUsize>,
    }

    impl Tool for WriterTool {
        fn name(&self) -> &str {
            "edit_file"
        }
        fn description(&self) -> &str {
            "stub writer"
        }
        fn parameters(&self) -> Value {
            json!({"type": "object"})
        }
        fn item_kind(&self) -> ItemKind {
            ItemKind::FileChange
        }
        fn execute(&self, args: &Value, _ctx: &ToolContext) -> ToolResult {
            let file = args.get("file").and_then(Value::as_str).unwrap_or("-").to_string();
            let mut result = ToolResult::ok(format!("written {file}"));
            let writes = self.writes.clone();
            result.undo = Some(Box::new(move || {
                writes.fetch_add(1, Ordering::SeqCst);
            }));
            result
        }
    }

    /// Twee draden die elk hun komst melden en wachten tot élkom er is, plus een telling van hoeveel er
    /// tegelijk binnen waren. Zonder deze poort is "paralel" niet van "snel serieel" te onderscheiden: een
    /// seriele loop zou hier tot in den treure blijven wachten. De release is éénmalig (`released`) en niet
    /// "er zijn er minder dan nodig", want de telling van binnenzijn-denkt-buiten is een telfout: gemeten met
    /// de eerste versie, die de tweede draden weer in de wacht duwde en de test liet hangen.
    struct Gate {
        state: Mutex<GateState>,
        changed: std::sync::Condvar,
        needed: usize,
    }

    #[derive(Default)]
    struct GateState {
        inside: usize,
        max_concurrent: usize,
        released: bool,
    }

    impl Gate {
        fn new(needed: usize) -> Gate {
            Gate { state: Mutex::new(GateState::default()), changed: std::sync::Condvar::new(), needed }
        }

        fn arrive(&self) {
            let mut state = self.state.lock().unwrap();
            state.inside += 1;
            state.max_concurrent = state.max_concurrent.max(state.inside);
            if state.inside >= self.needed {
                state.released = true;
            }
            self.changed.notify_all();
        }

        fn wait(&self) {
            let mut state = self.state.lock().unwrap();
            while !state.released {
                state = self.changed.wait(state).unwrap();
            }
            state.inside -= 1;
        }

        fn peak(&self) -> usize {
            self.state.lock().unwrap().max_concurrent
        }
    }

    /// De bron: geeft per beurt één antwoord uit een script en telt de beurten. Een beurt vraagt om een call
    /// door er een native function-call op te zetten — de lezer der loop haalt er de call uit alsof het een
    /// prompted model was — en zet verder zijn tekst neer.
    struct StubSource {
        script: Mutex<Vec<(String, Vec<(String, Value)>)>>,
        calls: AtomicUsize,
        seen: Mutex<Vec<(Option<String>, Option<String>, usize)>>,
        /// De schatting van élk verzoek dat de bron echt zag, in beurtenvolgorde. De lus meet zijn plafond op
        /// deze schatting, dus dit — en niet een narekening met de hand — is het getal dat een plafondbewijs
        /// moet tonen.
        estimates: Mutex<Vec<u64>>,
        last_id: Mutex<Option<String>>,
    }

    impl StubSource {
        /// Eén beurt met alleen proza.
        fn prose(text: impl Into<String>) -> (String, Vec<(String, Value)>) {
            (text.into(), Vec::new())
        }

        /// Eén beurt die deze calls vraagt.
        fn calls_for(text: impl Into<String>, calls: &[(&str, Value)]) -> (String, Vec<(String, Value)>) {
            (text.into(), calls.iter().map(|(name, args)| (name.to_string(), args.clone())).collect())
        }
    }

    impl TurnSource for StubSource {
        fn next_turn<'r>(&'r self, mut request: TurnRequest<'r>) -> TurnFuture<'r> {
            Box::pin(async move {
                let index = self.calls.fetch_add(1, Ordering::SeqCst);
                self.seen.lock().unwrap().push((
                    request.previous_response_id.map(str::to_string),
                    request.image.map(str::to_string),
                    request.history.len(),
                ));
                self.estimates.lock().unwrap().push(estimate_tokens_of(request.history));
                if let Some(delta) = request.history.last() {
                    (request.on_delta)(&delta.content);
                }
                (request.on_reasoning)("denk");
                let (text, wanted) = {
                    let script = self.script.lock().unwrap();
                    script.get(index).cloned().unwrap_or_else(|| Self::prose("einde"))
                };
                // Eén oplopend id per antwoord, zodat een keten meetbaar is zonder backend.
                let id = format!("resp-{index}");
                *self.last_id.lock().unwrap() = Some(id.clone());
                let turns = wanted
                    .into_iter()
                    .enumerate()
                    .map(|(n, (name, args))| FunctionCall {
                        call_id: Some(format!("call-{index}-{n}")),
                        name,
                        arguments: args.to_string(),
                    })
                    .collect();
                Turn { text, response_id: Some(id), function_calls: turns, ..Turn::text(String::new()) }
            })
        }
    }

    /// Een registry met één tool.
    fn one(tool: StubTool) -> ToolRegistry {
        ToolRegistry::new(vec![Box::new(tool)])
    }

    /// Een registry met twee alleen-lezen tools die samen mogen draaien.
    fn two_readers() -> ToolRegistry {
        let mut reader_a = StubTool::new("read_file");
        reader_a.parallel = true;
        let mut reader_b = StubTool::new("grep");
        reader_b.parallel = true;
        ToolRegistry::new(vec![Box::new(reader_a), Box::new(reader_b)])
    }

    /// Een registry met één schrijver, voor het seriele pad en de handgrepen.
    fn writer_only() -> ToolRegistry {
        ToolRegistry::new(vec![Box::new(WriterTool { writes: std::sync::Arc::new(AtomicUsize::new(0)) })])
    }

    /// Eén draai: een loop over deze registry en bron, met een eigen sink die de test daarna uitleest. De
    /// callbacks van de aanroeper zijn hier leeg — wat de sink noteert, is wat getest wordt; dat ze naast de
    /// sink bestaan, test `streaming_reaches_both_the_sink_and_the_caller`.
    async fn drive(
        registry: &ToolRegistry,
        source: &StubSource,
        history: &[Msg],
        ctx: &ToolContext,
        max_iterations: u32,
        configure: impl FnOnce(ToolLoop<'_>) -> ToolLoop<'_>,
    ) -> (LoopOutcome, RecordingSink) {
        let sink = RecordingSink::default();
        let outcome = drive_into(registry, source, history, ctx, max_iterations, configure, &sink).await;
        (outcome, sink)
    }

    /// Hetzelfde, maar met een sink die de aanroeper zelf bezit, zodat die hem na de run kan uitlezen zonder
    /// dat de run nog leeft.
    async fn drive_into(
        registry: &ToolRegistry,
        source: &StubSource,
        history: &[Msg],
        ctx: &ToolContext,
        max_iterations: u32,
        configure: impl FnOnce(ToolLoop<'_>) -> ToolLoop<'_>,
        sink: &RecordingSink,
    ) -> LoopOutcome {
        let mut run = LoopRun {
            history,
            ctx,
            previous_response_id: None,
            image: None,
            on_delta: Box::new(|_| {}),
            on_reasoning: Box::new(|_| {}),
            on_message: Box::new(|_| {}),
            sink,
        };
        // Via `configure`, zodat een test die zelf al een keten zet (`without_a_chain_…`) die niet
        // overschreven krijgt: een onvoorwaardelijke `chain_response_id(true)` hier zou het uit-pad
        // stil aanzetten en die test op de verkeerde plek laten falen.
        let loop_ = configure(ToolLoop::new(source, registry, max_iterations));
        loop_.run(&mut run).await
    }

    /// Hetzelfde, maar met een keten-start: `chain_response_id` aan plus dit response-id van de vorige
    /// beurt. Het context-plafond geldt alleen bij een server-sessie, en zo'n sessie begint ergens —
    /// zonder start-id zou elke keten-test toevallig op `None` draaien.
    async fn drive_chained(
        registry: &ToolRegistry,
        source: &StubSource,
        history: &[Msg],
        ctx: &ToolContext,
        max_iterations: u32,
        configure: impl FnOnce(ToolLoop<'_>) -> ToolLoop<'_>,
        sink: &RecordingSink,
        previous: Option<&str>,
    ) -> LoopOutcome {
        let mut run = LoopRun {
            history,
            ctx,
            previous_response_id: previous,
            image: None,
            on_delta: Box::new(|_| {}),
            on_reasoning: Box::new(|_| {}),
            on_message: Box::new(|_| {}),
            sink,
        };
        let loop_ = configure(ToolLoop::new(source, registry, max_iterations).chain_response_id(true));
        loop_.run(&mut run).await
    }

    /// Een bron met dit script.
    fn source(script: Vec<(String, Vec<(String, Value)>)>) -> StubSource {
        StubSource {
            script: Mutex::new(script),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
            estimates: Mutex::new(Vec::new()),
            last_id: Mutex::new(None),
        }
    }

    /// Het aantal beurten dat de bron echt zag.
    fn turns(source: &StubSource) -> usize {
        source.calls.load(Ordering::SeqCst)
    }

    /// Wat de bron per beurt aangereikt kreeg: het vorige response-id, het beeld, het aantal boodschappen.
    fn seen(source: &StubSource) -> Vec<(Option<String>, Option<String>, usize)> {
        source.seen.lock().unwrap().clone()
    }

    fn ctx() -> ToolContext {
        ToolContext::for_folder("/", true, true, true)
    }


    // ---- de loop ---------------------------------------------------------------------

    #[tokio::test]
    async fn a_prose_turn_ends_the_run_without_calling_a_tool() {
        let reg = two_readers();
        let src = source(vec![StubSource::prose("het antwoord")]);
        let (outcome, sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        let LoopOutcome::Completed { final_text, cap_reached, steps, .. } = &outcome else {
            panic!("verwacht een finale, got {outcome:?}");
        };
        assert_eq!(final_text, "het antwoord");
        assert!(!*cap_reached, "een beurt zonder calls is geen kap");
        assert_eq!(steps.len(), 1);
        assert!(steps[0].calls.is_empty());
        assert_eq!(turns(&src), 1);
        // De sink zag één geopende en één gesloten rij, met de tekst erin.
        assert_eq!(sink.count("reply-started"), 1);
        assert_eq!(sink.count("reply-completed het antwoord"), 1);
    }

    #[tokio::test]
    async fn one_tool_call_returns_and_the_next_turn_finishes() {
        let reg = two_readers();
        let src = source(vec![
            StubSource::calls_for(" ik lees even", &[("read_file", json!({"file": "a.txt"}))]),
            StubSource::prose("klaar"),
        ]);
        let (outcome, sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        let LoopOutcome::Completed { steps, messages, cap_reached, .. } = &outcome else {
            panic!("verwacht een finale, got {outcome:?}");
        };
        assert!(!*cap_reached);
        assert_eq!(steps.len(), 2, "één call-stap en één finale");
        assert_eq!(steps[0].calls.len(), 1);
        assert_eq!(steps[0].results[0].output, "(read_file a.txt)");
        // De observatie gaat als gebruikersboodschap terug en is het model-tekstpad.
        let obs = messages.iter().find(|m| m.role == "user").expect("observatie ontbreekt");
        assert!(obs.content.contains(" succeeded (ok=true): (read_file a.txt)"), "{}", obs.content);
        assert!(obs.content.contains("call-0-0"), "de call-id van de bron staat erin");
        // Eén generieke kaart, in de callvolgorde, met het id dat de sink uitdeelde. De volgorde is wat de
        // loop belooft en wat de UI nodig heeft: de beurt Gaat open, zijn tekst stroomt, de rij sluit, en pas
        // daarna komen de kaarten van de calls die hij vroeg, vóór de rij van de volgende beurt. Een kaart
        // binnen de slotrij zou een beslissing tonen die er nog niet was.
        assert_eq!(
            sink.lines(),
            vec![
                "reply-started",
                "delta vraag",
                "reasoning denk",
                "reply-completed  ik lees even",
                "toolcall item-1 read_file",
                "reply-started",
                "delta Tool observation for call call-0-0: `read_file` succeeded (ok=true): (read_file a.txt)",
                "reasoning denk",
                "reply-completed klaar",
            ]
        );
    }

    #[tokio::test]
    async fn the_nudge_fights_a_blank_turn_but_never_reaches_the_transcript() {
        let reg = two_readers();
        // Een deterministische bron die drie keer "..." parkeert en dan pas antwoordt: precies het
        // redeneermodel-gedrag waar de nudge voor bedoeld is.
        let src = source(vec![
            StubSource::prose("..."),
            StubSource::prose("."),
            StubSource::prose("nu wel"),
        ]);
        let (outcome, _sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 6, |l| l).await;
        let LoopOutcome::Completed { final_text, messages, .. } = &outcome else {
            panic!("verwacht een finale, got {outcome:?}");
        };
        assert_eq!(final_text, "nu wel");
        assert_eq!(turns(&src), 3, "twee nudges en dan de echte beurt");
        // De nudges verschillen: elke verzoek-geschiedenis is langer dan de vorige, want de nudge gaat mee
        // in de werkende kop maar nooit in het transcript.
        let counts: Vec<usize> = seen(&src).iter().map(|(_, _, len)| *len).collect();
        // Eerlijk over wat hier gemeten wordt: de lege beurten worden niét aan de kop toegevoegd, dus alleen
        // de nudges laten hem groeien. De nudge is dezelfde tekst in beide rondes, en een latere native
        // function-calling-backend zou dus nog één herbepaling kunnen uitsparen; deze loop wint er niets door.
        assert_eq!(counts, vec![1, 2, 3], "de werkende kop groeit met alleen de nudge erbij");
        assert!(!messages.iter().any(|m| m.content == BLANK_NUDGE), "de nudge blijft buiten het transcript");
        assert_eq!(messages.len(), 1, "alleen de echte finale wordt gepersisteerd");
    }

    #[tokio::test]
    async fn a_source_that_stays_blank_ends_at_the_blank_ceiling() {
        let reg = two_readers();
        // Een model dat elk verzoek met blanco tekst beantwoordt: voorbij het nudge-plafond moet de lus
        // gewoon eindigen, anders ruilt een stil model een stille beurt in voor een oneindige.
        let blanks = StubSource::prose(" ");
        let src = source(vec![blanks.clone(), blanks.clone(), blanks.clone(), blanks]);
        let (outcome, _sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 9, |l| l).await;
        // Twee nudges, dan geeft hij op zoals altijd: de derde lege beurt is de finale.
        assert_eq!(turns(&src), 3);
        let LoopOutcome::Completed { final_text, .. } = &outcome else { panic!("got {outcome:?}") };
        assert_eq!(final_text, " ");
    }

    #[tokio::test]
    async fn an_empty_tool_turn_settles_in_prose_before_the_cap() {
        let reg = two_readers();
        // Een model dat in elke beurt een call vraagt: het budget snijdt de lus, maar de aanroeper krijgt
        // eerst nog één beurt om in proza te antwoorden.
        let asking = StubSource::calls_for("werk", &[("read_file", json!({"file": "a"}))]);
        let src = source(vec![asking.clone(), asking, StubSource::prose("genoeg gedaan")]);
        let (outcome, _sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 2, |l| l).await;
        let LoopOutcome::Completed { cap_reached, steps, messages, final_text, .. } = &outcome else {
            panic!("verwacht een finale, got {outcome:?}");
        };
        assert!(*cap_reached, "het antwoord kwam pas na het budget");
        assert_eq!(steps.len(), 3, "twee call-stappen en de settle-beurt");
        assert_eq!(turns(&src), 3, "twee beurten plus de settle-poging");
        assert!(steps[2].calls.is_empty(), "de settle-stap vraagt niets");
        // De aanroeper krijgt de settle-tekst als finale, zodat hij de beurt niet als antwoordloos noteert.
        assert_eq!(final_text, "genoeg gedaan");
        assert_eq!(messages.last().map(|m| m.content.as_str()), Some("genoeg gedaan"));
    }

    #[tokio::test]
    async fn a_model_that_keeps_asking_ends_as_cap_reached() {
        let reg = two_readers();
        let asking = StubSource::calls_for("werk", &[("read_file", json!({"file": "a"}))]);
        let src = source(vec![asking.clone(), asking.clone(), asking.clone(), asking]);
        let (outcome, _sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 3, |l| l).await;
        let LoopOutcome::CapReached { incomplete_reason, steps, .. } = &outcome else { panic!("verwacht een kap, got {outcome:?}") };
        assert_eq!(incomplete_reason, MAX_ITERATIONS_REASON);
        assert_eq!(steps.len(), 3, "alleen de call-stappen; de settle-beurt vroeg weer een call");
        assert_eq!(turns(&src), 4, "drie beurten plus de settle-poging");
    }

    #[tokio::test]
    async fn an_empty_registry_offers_the_model_no_tools() {
        // Een registry zonder tools mag geen `tools`-veld meesturen: de Responses API verwerpt een leeg
        // array, en de loop hoort dat verschil zelf te kennen in plaats van de aanroeper.
        let reg = ToolRegistry::default();
        assert!(reg.is_empty());
        let src = source(vec![StubSource::prose("antwoord")]);
        let (outcome, _sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 3, |l| l).await;
        assert!(matches!(outcome, LoopOutcome::Completed { .. }));
        assert_eq!(turns(&src), 1);
    }

    #[tokio::test]
    async fn the_card_kind_comes_from_the_tool_and_undos_are_carried() {
        let reg = writer_only();
        let src = source(vec![
            StubSource::calls_for("schrijf", &[("edit_file", json!({"file": "g.txt"}))]),
            StubSource::prose("gedaan"),
        ]);
        let (outcome, sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        // De kaartsoort komt uit de tool, niet uit de naam: een schrijver krijgt een fileChange-rij.
        let lines = sink.lines();
        assert!(lines.iter().any(|l| l == "filechange item-1 g.txt"), "{lines:?}");
        assert!(!lines.iter().any(|l| l.starts_with("toolcall")), "niet de generieke kaart: {lines:?}");
        // De handgreep van de gelande write wordt gedragen, niet weggegooid.
        let LoopOutcome::Completed { undos, .. } = &outcome else { panic!("got {outcome:?}") };
        assert_eq!(undos.len(), 1);
        (undos[0])();
    }

    #[tokio::test]
    async fn an_unknown_tool_is_reported_as_a_failed_observation() {
        // Een call op een naam die niet bestaat, mag de loop niet laten crashen: de runner maakt er een
        // mislukt resultaat van en de mens ziet de mislukking die het model zag.
        let reg = two_readers();
        let src = source(vec![
            StubSource::calls_for("vreemd", &[("bestaan_niet", json!({}))]),
            StubSource::prose("ik stel bij"),
        ]);
        let (outcome, sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        assert!(matches!(outcome, LoopOutcome::Completed { .. }));
        assert!(sink.lines().iter().any(|l| l.starts_with("toolcall item-1 bestaan_niet")), "{:?}", sink.lines());
        let obs = outcome.messages().iter().find(|m| m.role == "user").expect("observatie");
        assert!(obs.content.contains("failed (ok=false): no tool named"), "{}", obs.content);
    }

    #[tokio::test]
    async fn streaming_reaches_both_the_sink_and_the_caller() {
        let reg = two_readers();
        let src = source(vec![StubSource::prose("antwoord")]);
        let rights = ctx();
        let sink = RecordingSink::default();
        let deltas = std::sync::Arc::new(Mutex::new(Vec::new()));
        let reasonings = std::sync::Arc::new(Mutex::new(Vec::new()));
        let seen_deltas = deltas.clone();
        let seen_reasonings = reasonings.clone();
        let mut run = LoopRun {
            history: &[Msg::user("losse tekst")],
            ctx: &rights,
            previous_response_id: None,
            image: None,
            on_delta: Box::new(move |chunk| deltas.lock().unwrap().push(chunk.to_string())),
            on_reasoning: Box::new(move |chunk| reasonings.lock().unwrap().push(chunk.to_string())),
            on_message: Box::new(|_| {}),
            sink: &sink,
        };
        let outcome = ToolLoop::new(&src, &reg, 3).run(&mut run).await;
        assert!(matches!(outcome, LoopOutcome::Completed { .. }));
        // De bron stuurde de laatste boodschap als delta en "denk" als redeneerwerk: beide kanalen moeten
        // beide afnemers bereiken — de sink voor de rijinhoud, de callback voor wie meekijkt.
        assert_eq!(*seen_deltas.lock().unwrap(), vec!["losse tekst".to_string()]);
        assert_eq!(*seen_reasonings.lock().unwrap(), vec!["denk".to_string()]);
        assert_eq!(sink.count("delta losse tekst"), 1);
        assert_eq!(sink.count("reasoning denk"), 1);
    }

    #[tokio::test]
    async fn an_image_plays_once_and_a_chain_rides_only_when_asked() {
        let reg = two_readers();
        let asking = StubSource::calls_for("werk", &[("read_file", json!({"file": "a"}))]);
        let src = source(vec![asking, StubSource::prose("klaar")]);
        let rights = ctx();
        let sink = RecordingSink::default();
        let mut run = LoopRun {
            history: &[Msg::user("bekijk dit")],
            ctx: &rights,
            previous_response_id: Some("resp-uit-de-sessie"),
            image: Some("base64-afbeelding"),
            on_delta: Box::new(|_| {}),
            on_reasoning: Box::new(|_| {}),
            on_message: Box::new(|_| {}),
            sink: &sink,
        };
        let outcome = ToolLoop::new(&src, &reg, 4).chain_response_id(true).run(&mut run).await;
        assert!(matches!(outcome, LoopOutcome::Completed { .. }));
        let looked = seen(&src);
        // Eén beeld, op de eerste beurt, nooit op de tweede.
        assert_eq!(looked[0].1.as_deref(), Some("base64-afbeelding"));
        assert_eq!(looked[1].1, None, "een beeld speelt niet twee keer mee");
        // Aan: het id uit het eerste antwoord rijgt het tweede.
        assert_eq!(looked[0].0.as_deref(), Some("resp-uit-de-sessie"));
        assert_eq!(looked[1].0.as_deref(), Some("resp-0"));
        assert_eq!(outcome.response_id(), Some("resp-1"));
    }

    #[tokio::test]
    async fn without_a_chain_every_turn_starts_from_the_client() {
        let reg = two_readers();
        let asking = StubSource::calls_for("werk", &[("read_file", json!({"file": "a"}))]);
        let src = source(vec![asking, StubSource::prose("klaar")]);
        let (outcome, _sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        assert!(matches!(outcome, LoopOutcome::Completed { .. }));
        // De veilige default: een lokale backend die previous_response_id negeert, moet tóch élke
        // observatie zien, dus er wordt geen id doorgegeven.
        let looked = seen(&src);
        assert_eq!(looked[0].0, None);
        assert_eq!(looked[1].0, None);
        // Het response-id van het laatste antwoord gaat wél mee naar de aanroeper.
        assert_eq!(outcome.response_id(), Some("resp-1"));
    }

    #[tokio::test]
    async fn a_chained_run_stops_before_a_request_it_cannot_afford() {
        // Poort-getrouw aan de origineler: bij een server-sessie speelt de backend zijn eigen historie voor
        // en laat hij die groeien, dus de client kan een veroordeeld verzoek niet wegtrimmen — de lus moet
        // zélf stoppen vóórdat hij het verstuurt. De transcriptie is hier al te lang (1 004 tekenschatting
        // tegen plafond 10), dus er mag geen enkele beurt uitgaan.
        let reg = two_readers();
        let src = source(vec![StubSource::calls_for("had nooit mogen starten", &[("read_file", json!({"file": "a"}))])]);
        let sink = RecordingSink::default();
        let history = [Msg::user("x".repeat(3_000))];
        // Met de formule van de origineler (~3 tekens per token, plus 8 per boodschap) ruim boven het
        // plafond, dus de eerste beoordeling van de schatting kapte de draai al af.
        assert!(estimate_tokens_of(&history) > 10, "de kop moet het plafond overschrijden");
        let outcome = drive_chained(
            &reg,
            &src,
            &history,
            &ctx(),
            4,
            |l| l.context_ceiling(10),
            &sink,
            Some("resp-vorige-beurt"),
        )
        .await;
        let LoopOutcome::CapReached { incomplete_reason, steps, .. } = &outcome else {
            panic!("verwacht een kap, got {outcome:?}");
        };
        assert_eq!(incomplete_reason, CONTEXT_LIMIT_REASON);
        assert!(steps.is_empty(), "geen enkele stap: het verzoek is nooit verstuurd");
        assert_eq!(turns(&src), 0, "de bron heeft geen enkele beurt gezien");
        assert!(sink.lines().is_empty(), "er is niets op het scherm gezet: {:?}", sink.lines());
    }

    #[tokio::test]
    async fn a_chained_run_keeps_going_while_the_ceiling_leaves_room() {
        // De keerzijde van dat plafond binnen dezelfde schepping: een keten met ruimte moet doorspelen en
        // pas uitstappen op het juiste moment. De schatting groeit per beurt (gemeten in de afdruk hieronder,
        // in de volgorde van de draai): het verzoek van de eerste beurt past, dat van de tweede ook — 44 van
        // de 60 — maar na de observatie van de tweede call staat de kop op 82, en dáár stapt de loop uit,
        // vóór het derde, veroordeelde verzoek. Zonder deze test bewijst de kap alleen dat er iets kapging,
        // niet dat het plafond het juiste moment koos.
        let reg = two_readers();
        let asking = StubSource::calls_for("werk", &[("read_file", json!({"file": "a"}))]);
        let src = source(vec![asking.clone(), asking, StubSource::prose("had af kunnen sluiten")]);
        let history = [Msg::user("vraag")];
        assert!(estimate_tokens_of(&history) <= 60, "de eerste beurt moet door het plafond heen passen");
        let outcome = drive_chained(
            &reg,
            &src,
            &history,
            &ctx(),
            5,
            |l| l.context_ceiling(60),
            &RecordingSink::default(),
            Some("resp-vorige-beurt"),
        )
        .await;
        let LoopOutcome::CapReached { incomplete_reason, steps, .. } = &outcome else {
            panic!("verwacht een kap, got {outcome:?}");
        };
        assert_eq!(incomplete_reason, CONTEXT_LIMIT_REASON);
        assert_eq!(steps.len(), 2, "twee betaalbare beurten draaiden, het derde verzoek niet");
        assert_eq!(turns(&src), 2, "het derde verzoek is nooit verstuurd");
        // De afdruk is geen test maar bewijs: wat de bron per beurt aangereikt kreeg, gemeten door dezelfde
        // schatter waar het plafond op rekent. Bij een andere observatietekst — of een andere schatter dan de
        // origineler — zijn dit de getallen om naar te kijken vóór de asserties hierboven aangepast worden.
        let verzocht = src.estimates.lock().unwrap().clone();
        assert_eq!(verzocht.len(), 2, "de bron zag precies twee verzoeken");
        assert!(verzocht.iter().all(|e| *e <= 60), "élk verzoek dat de bron zag, pastte: {verzocht:?}");
        println!("schatting per verzoek dat de bron zag: {verzocht:?} van plafond 60");
    }

    #[tokio::test]
    async fn the_ceiling_is_only_enforced_for_a_server_session() {
        // Zonder keten groeit de servercontext niet: de client stuurt per beurt alles zelf, dus een plafond
        // mag dan geen enkele draai afkappen.
        let reg = two_readers();
        let asking = StubSource::calls_for("werk", &[("read_file", json!({"file": "a"}))]);
        let src = source(vec![asking, StubSource::prose("klaar")]);
        let (outcome, _sink) =
            drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 5, |l| l.context_ceiling(1)).await;
        assert!(matches!(outcome, LoopOutcome::Completed { .. }), "geen keten, dus geen plafond: {outcome:?}");
        assert_eq!(turns(&src), 2);
    }

    #[tokio::test]
    async fn a_batch_that_must_run_serially_stays_serial() {
        // Een gemengde batch — één schrijver ertussen — houdt het serieuze pad, want een write mag niet
        // racen en een goedkeuringskaart moet live opkomen.
        let mut reader = StubTool::new("read_file");
        reader.parallel = true;
        reader.delay = Some(std::time::Duration::from_millis(60));
        let reg = ToolRegistry::new(vec![
            Box::new(reader),
            Box::new(WriterTool { writes: std::sync::Arc::new(AtomicUsize::new(0)) }),
        ]);
        let src = source(vec![
            StubSource::calls_for("lees en schrijf", &[("read_file", json!({"file": "a"})), ("edit_file", json!({"file": "b"}))]),
            StubSource::prose("klaar"),
        ]);
        let started = std::time::Instant::now();
        let (outcome, sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        let elapsed = started.elapsed();
        assert!(matches!(outcome, LoopOutcome::Completed { .. }));
        // Serieel: de traagste call bepaalt de tijd, en de kaarten volgen de callvolgorde van het model.
        assert!(elapsed >= std::time::Duration::from_millis(55), "serialiteit moet meetbaar zijn: {elapsed:?}");
        let lines = sink.lines();
        let read = lines.iter().position(|l| l.starts_with("toolcall")).unwrap();
        let write = lines.iter().position(|l| l.starts_with("filechange")).unwrap();
        assert!(read < write, "de kaarten volgen het model, niet de afloop: {lines:?}");
    }

    #[tokio::test]
    async fn a_parallel_batch_overlaps_yet_reports_in_model_order() {
        let gate = std::sync::Arc::new(Gate::new(2));
        let mut reader_a = StubTool::new("read_file");
        reader_a.parallel = true;
        reader_a.barrier = true;
        reader_a.gate = Some(gate.clone());
        let mut reader_b = StubTool::new("grep");
        reader_b.parallel = true;
        reader_b.barrier = true;
        reader_b.gate = Some(gate.clone());
        let reg = ToolRegistry::new(vec![Box::new(reader_a), Box::new(reader_b)]);
        let src = source(vec![
            StubSource::calls_for("twee lezingen", &[("grep", json!({"file": "twee"})), ("read_file", json!({"file": "een"}))]),
            StubSource::prose("klaar"),
        ]);
        let (outcome, sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        let LoopOutcome::Completed { steps, .. } = &outcome else { panic!("got {outcome:?}") };
        // Zonder deze poort zouden beide draden elkaar opwachten en zou de test hangen: de piek van twee
        // bewijst dat ze echt binnen waren, dus de loop kan niet serieel gedraaid hebben.
        assert_eq!(gate.peak(), 2, "de calls hebben elkander werkelijk gepasseerd");
        // Devolgorde van het model blijft leidend voor wat de lezer en het model zien.
        assert_eq!(steps[0].calls[0].tool, "grep");
        assert_eq!(steps[0].results[0].output, "(grep twee)");
        assert_eq!(steps[0].results[1].output, "(read_file een)");
        let lines = sink.lines();
        let positions: Vec<usize> = ["toolcall item-1 grep", "toolcall item-2 read_file"]
            .iter()
            .map(|needle| lines.iter().position(|l| l.starts_with(needle)).unwrap_or_else(|| panic!("missing {needle} in {lines:?}")))
            .collect();
        assert_eq!(positions, vec![positions[0], positions[0] + 1], "kaarten direct na elkaar, in modelvolgorde: {lines:?}");
        // De observaties in de werkende kop volgen dezelfde volgorde.
        let observations: Vec<&str> = outcome.messages().iter().filter(|m| m.role == "user").map(|m| m.content.as_str()).collect();
        assert_eq!(observations.len(), 2);
        assert!(observations[0].contains("`grep`"), "{}", observations[0]);
        assert!(observations[1].contains("`read_file`"), "{}", observations[1]);
    }

    #[tokio::test]
    async fn a_batch_larger_than_the_pool_finishes_every_call() {
        // Vier calls, een pool van er twee: de wachtrij mag geen call verliezen en geen verdubbelen.
        let mut reader = StubTool::new("read_file");
        reader.parallel = true;
        reader.delay = Some(std::time::Duration::from_millis(30));
        let reg = one(reader);
        let asks: Vec<(&str, Value)> =
            (1..=4).map(|n| ("read_file", json!({"file": format!("f{n}")}))).collect();
        let src = source(vec![StubSource::calls_for("vier keer", &asks), StubSource::prose("klaar")]);
        let (outcome, sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        let LoopOutcome::Completed { steps, .. } = &outcome else { panic!("got {outcome:?}") };
        assert_eq!(steps[0].results.len(), 4);
        assert_eq!(steps[0].results.iter().filter(|r| r.ok).count(), 4);
        assert_eq!(sink.count("toolcall item-"), 4);
        // De modelvolgorde blijft de requestvolgorde, ook al komen de resultaten in willekeurige volgorde binnen.
        let files: Vec<String> = steps[0].results.iter().map(|r| r.output.clone()).collect();
        assert_eq!(files, vec!["(read_file f1)", "(read_file f2)", "(read_file f3)", "(read_file f4)"]);
    }

    #[tokio::test]
    async fn a_sixteen_thousand_char_observation_keeps_the_head_and_tail() {
        // De observatietekst is het kanaal dat een lange sessie opblaast: de grens hiervóór is exact, dus
        // hier alleen het gedrag eromheen — een grote run blijft in de kop, het model krijgt de kop.
        let output = "y".repeat(CAP * 2);
        let mut big = StubToolWithOutput { inner: StubTool::new("read_file"), output };
        big.inner.parallel = true;
        let reg = ToolRegistry::new(vec![Box::new(big)]);
        let src = source(vec![
            StubSource::calls_for("grote uitvoer", &[("read_file", json!({"file": "groot"}))]),
            StubSource::prose("klaar"),
        ]);
        let (outcome, _sink) = drive(&reg, &src, &[Msg::user("vraag")], &ctx(), 4, |l| l).await;
        let obs = outcome.messages().iter().find(|m| m.role == "user").expect("observatie");
        assert!(obs.content.contains("output truncated"), "{}", &obs.content[..120]);
        assert!(obs.content.starts_with("Tool observation"), "kop blijft vooraan");
        assert!(obs.content.ends_with('y'), "staart blijft bewaard");
        assert!(obs.content.chars().count() <= CAP + 14, "ouder dan het plafond: {}", obs.content.chars().count());
        // De mens-kaart blijft de volledige uitvoer dragen — alleen het model-kanaal is begrensd.
    }

    /// Een stub die vaste uitvoer teruggeeft, om de observatie-grens zonder echte tools te raken.
    struct StubToolWithOutput {
        inner: StubTool,
        output: String,
    }

    impl Tool for StubToolWithOutput {
        fn name(&self) -> &str {
            self.inner.name
        }
        fn description(&self) -> &str {
            "stub"
        }
        fn parameters(&self) -> Value {
            json!({"type": "object"})
        }
        fn item_kind(&self) -> ItemKind {
            self.inner.item
        }
        fn can_run_parallel(&self, _args: &Value, _ctx: &ToolContext) -> bool {
            self.inner.parallel
        }
        fn execute(&self, _args: &Value, _ctx: &ToolContext) -> ToolResult {
            ToolResult::ok(self.output.clone())
        }
    }

}
