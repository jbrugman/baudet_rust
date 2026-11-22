//! Fold a stream of facts into rows.
//!
//! Pure on purpose: no backend, no clock, no widget. Given the same events, the same rows come
//! out, which is what lets a UI be verified against a script instead of against a GPU.

use baudet_core::{ApprovalDecision, CommandExecutionItem, Item, ItemKind, ItemStatus, ThreadEvent};
use std::collections::HashMap;

/// The rows of one conversation, in the order they appeared, plus the live index by id.
#[derive(Debug, Default)]
pub struct Rows {
    order: Vec<String>,
    items: HashMap<String, Item>,
}

impl Rows {
    pub fn new() -> Rows {
        Rows::default()
    }

    /// Rows in arrival order, oldest first.
    pub fn ordered(&self) -> Vec<&Item> {
        self.order.iter().filter_map(|id| self.items.get(id)).collect()
    }

    pub fn get(&self, item_id: &str) -> Option<&Item> {
        self.items.get(item_id)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn open(&mut self, item: Item) {
        let id = item.item_id().to_string();
        if !self.items.contains_key(&id) {
            self.order.push(id.clone());
        }
        self.items.insert(id, item);
    }

    /// Zet een regel vooraan, of hou haar op haar plaats als ze er al staat.
    ///
    /// Nodig voor lazy paging: een pagina die ouder is dan alles wat er staat, bestaat uit id's die
    /// `Rows` nog niet kent, en die horen vóór de KnownId's, niet achter ze. Een id die wél al
    /// staat, mag van positie blijven staan zoals hij stond - een scherm dat twee keer dezelfde
    /// rij tekent of haar verplaatst nadat de persoon haar al las, is erger dan een rij die niet
    /// is verdubbeld.
    /// Vervang een regel in tekst, op de plaats waar hij al stond.
    ///
    /// Voor een lading uit de opslag: die regels zijn af, en een scherm dat ze opnieuw tekent,
    /// mag ze niet verplaatsen en niet twee keer tonen.
    pub fn replace_for_screen(&mut self, item: Item) {
        let id = item.item_id().to_string();
        self.items.insert(id, item);
    }

    /// Alle rijen weggoooien, voor een gesprekwissel.
    pub fn clear(&mut self) {
        self.order.clear();
        self.items.clear();
    }

    pub fn prepend_for_screen(&mut self, item: Item) {
        let id = item.item_id().to_string();
        if self.items.contains_key(&id) {
            self.items.insert(id, item);
            return;
        }
        self.order.insert(0, id.clone());
        self.items.insert(id, item);
    }

    fn text_of(&self, item_id: &str) -> String {
        self.items
            .get(item_id)
            .and_then(|item| match item {
                Item::AgentMessage(i) => Some(i.text.clone()),
                Item::Reasoning(i) => Some(i.text.clone()),
                Item::UserMessage(i) => Some(i.text.clone()),
                Item::CommandExecution(i) => Some(i.output.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

}

/// De emitter van één beurt, vernoemd naar het gesprek: `ThreadEmitter` in `AppState.kt:800`.
///
/// Deux bestemmingen, één gebeurtenis per keer, en daarom deze plek:
///
///  * [`Rows`] — wat er nu op het scherm staat, incl. de redening van het moment;
///  * [`baudet_store::Session::items`] — het audit-spoor dat de opslag in gaat.
///
/// Die scheiding is de dragende regel van het sessiedomein en staat niet voor niets in de
/// doc van `baudet-store::session`: `items` is wat de persoon echt heeft gezien. De volgorde
/// hieronder is dus geen stylistische keuze: eerst de regel die de rode draad tekent, dan pas
/// de `items.push`, want de origineler doet precies dat (`AppState.kt:805` slaat een
/// voltooide regel op nadat hij hem heeft uitgezonden). En de redening blijft buiten het spoor
/// (`AppState.kt:775`: "Reasoning is display-only and deliberately never stored in a Msg") —
/// die is voor de dock, niet voor de transcript op schijf.
///
/// Zonder deze wrapper hadden de vlag ("is dit af") en de opslag ("gaat dit naar disk") twee
/// aparte plekken gekregen die elkaars resultaat konden verloochenen. Zij hebben er nu één.
#[derive(Debug)]
pub struct Emitter<'a> {
    rows: &'a mut Rows,
    items: Option<&'a mut Vec<Item>>,
    /// De boodschappen die deze beurt aan het model-verleden toevoegde. De aanroeper neemt ze over
    /// met [`Emitter::taken_messages`], of laat ze ledig als hij het gesprek niet zelf bijhoudt. De
    /// tool-lus brengt twee soorten voort: een kaart die een lezer leest, en een
    /// observatie die alleen het model nog ziet. Die tweede mag geen rij worden — gereedschaps-
    /// uitvoer is werk, geen prosa — maar moet wél mee naar de volgende vraag, anders vergeet het
    /// model wat hij zojuist las.
    messages: Vec<baudet_core::Msg>,
    /// De id's die deze beurt verdeelde. Eén beurt gebruikt op één draad hooguit enkele tientallen
    /// regels, dus deze teller is per definitie klein; hij bestaat omdat een tool-lus één id nodig
    /// heeft om een kaart te openen en later op datzelfde id te sluiten. `Rows` verdraagt geen
    /// dubbele id: de tweede tekening overschrijft de eerste en het audit-spoor krijgt twee regels
    /// met één naam.
    seq: usize,
    thread_id: String,
    turn_id: String,
}

impl<'a> Emitter<'a> {
    /// Emitter op de rijen van een beurt, zonder opslagdoel: voor een gesprek dat (nog) niet
    /// op disk hoeft — de scripted demo en elke test die alleen het scherm meet.
    pub fn display(rows: &'a mut Rows, thread_id: impl Into<String>, turn_id: impl Into<String>) -> Emitter<'a> {
        Emitter {
            rows,
            items: None,
            messages: Vec::new(),
            seq: 0,
            thread_id: thread_id.into(),
            turn_id: turn_id.into(),
        }
    }

    /// Emitter die vóór alles een opslagdoel krijgt: dezelfde regels gaan naar het scherm én
    /// naar het audit-spoor.
    pub fn storing(
        rows: &'a mut Rows,
        items: &'a mut Vec<Item>,
        thread_id: impl Into<String>,
        turn_id: impl Into<String>,
    ) -> Emitter<'a> {
        Emitter {
            rows,
            items: Some(items),
            messages: Vec::new(),
            seq: 0,
            thread_id: thread_id.into(),
            turn_id: turn_id.into(),
        }
    }

    /// De id waaronder deze beurt zijn regels uitzendt. De transport- en tests kant heeft hem
    /// nodig om dezelfde id te noemen als de emitter, anders vertoont een dedup-test iets wat
    /// op het scherm niet bestaat.
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    /// The id of the turn this emitter speaks for. Same reason as [`Emitter::thread_id`].
    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }

    /// Open a row: the row exists on screen, not yet in the store. De return is die van
    /// [`reduce`], zodat een ontvanger kan tellen of de gebeurtenis iets heeft geraakt - dezelfde
    /// informantie die `apply` doorgeeft en die een drainer nodig heeft om een stream te kunnen
    /// beoordelen zonder de rijen zelf te lezen.
    /// Het id van de volgende regel van deze beurt.
    ///
    /// Emitter-vorm en niet `Rows`-vorm: de beurt is wat de id's groepeert (`th_1/tu_1/i1`). De
    /// andere verdeler van id's in deze tree is `Session::next_item_seq`, die na een herstart
    /// `th_1/i1` oplevert; de twee kunnen niet botsen omdat ze verschillend heten en binnen één
    /// bron monotoon lopen.
    pub fn next_item_id(&mut self) -> String {
        self.seq += 1;
        format!("{}/{}/i{}", self.thread_id, self.turn_id, self.seq)
    }

    /// Teken een regel die nog loopt, en schuif haar niet in het audit-spoor.
    ///
    /// Een geopende regel is een belofte en geen feit: `items` is wat de persoon echt zag en
    /// afgerekend werd, dus een open rij daarin zou een herstart een half antwoord laten tonen alsof
    /// het klaar was — precies wat [`Emitter::completed`] verhindert. Wat wél meetekent, is de rij
    /// zelf: de goedkeuringsvraag die een tool halverwege `execute` stelt, moet op de kaart landen
    /// die de lezer dan al ziet, want een kaart die pas ná de run ontstaat, kan die vraag niet
    /// dragen.
    pub fn open(&mut self, item: Item) {
        self.started(item);
    }

    /// Draag een boodschap over aan het model-verleden zonder haar te tekenen.
    pub fn store_message(&mut self, msg: &baudet_core::Msg) {
        self.messages.push(msg.clone());
    }

    /// De boodschappen die deze beurt aan het model-verleden toevoegde, voor de aanroeper die ze
    /// overneemt wanneer hij het gesprek zelf bijhoudt.
    pub fn taken_messages(&mut self) -> Vec<baudet_core::Msg> {
        std::mem::take(&mut self.messages)
    }

    pub fn started(&mut self, item: Item) -> bool {
        reduce(
            self.rows,
            &ThreadEvent::ItemStarted {
                thread_id: self.thread_id.clone(),
                turn_id: self.turn_id.clone(),
                item,
            },
        )
    }

    /// Stream a delta into an open row. Noise by design: een tussenstand is geen feit, dus deze
    /// regel roept `items` nooit aan. `kind` draagt de aanroeper mee, precies zoals de transport
    /// dat doet — de emitter kent het soort van een id niet en moet het niet raden.
    pub fn delta(&mut self, item_id: &str, kind: ItemKind, delta: &str) -> bool {
        reduce(
            self.rows,
            &ThreadEvent::ItemDelta {
                thread_id: self.thread_id.clone(),
                turn_id: self.turn_id.clone(),
                item_id: item_id.to_string(),
                kind,
                delta: delta.to_string(),
            },
        )
    }

    /// Sluit een regel: die is pas een feit als ze voltooid is, en pas dán gaat ze het spoor in.
    pub fn completed(&mut self, item: Item) -> bool {
        let item_id = item.item_id().to_string();
        let store = item.kind() != ItemKind::Reasoning;
        let kept = reduce(
            self.rows,
            &ThreadEvent::ItemCompleted {
                thread_id: self.thread_id.clone(),
                turn_id: self.turn_id.clone(),
                item,
            },
        );
        if !store {
            return kept;
        }
        let Some(items) = self.items.as_deref_mut() else { return kept };
        // De voltooide regel zoals de reductor hem nu houdt: inclusief de tekst die een delta
        // heeft binnengekregen en die het sluitende exemplaar misschien niet draagt.
        let Some(closed) = self.rows.get(&item_id).cloned() else { return kept };
        // Eén keer per id, omdat het spoor een audit is en geen versiehistorie. `reduce` mag een
        // id upserten (de reducer vervangt hem op dezelfde plaats), het spoor niet.
        debug_assert!(
            items.iter().all(|stored| stored.item_id() != item_id),
            "{item_id} state twee keer opgeslaan"
        );
        items.push(closed);
        kept
    }
}


/// Apply one event. Returns `false` when the event named an item that was never opened, which a
/// caller may want to count: a delta for an unknown row means the stream was replayed from the
/// middle, and silently dropping it is how a transcript loses an answer without saying so.
pub fn reduce(rows: &mut Rows, event: &ThreadEvent) -> bool {
    match event {
        ThreadEvent::ItemStarted { item, .. } => {
            rows.open(item.clone());
            true
        }
        ThreadEvent::ItemCompleted { item, .. } => {
            // The completed item carries the final state, so a renderer never keeps its own copy
            // of the truth -- but a delta that arrived after an earlier snapshot must not be lost,
            // so an empty text on the close keeps what was streamed instead of erasing it.
            let id = item.item_id().to_string();
            let streamed = rows.text_of(&id);
            if streamed_text(item).is_empty() && !streamed.is_empty() {
                let mut kept = item.clone();
                kept.push_str(&streamed);
                rows.open(kept);
                return true;
            }
            rows.open(item.clone());
            true
        }
        ThreadEvent::ItemDelta { item_id, delta, .. } => match rows.items.get_mut(item_id) {
            Some(item) => {
                item.push_str(delta);
                true
            }
            None => false,
        },
        // Not row-bearing, but a turn's terminal failure deserves a row of its own so the reader
        // sees it where it happened rather than only in a status line.
        ThreadEvent::TurnCompleted { turn_id, incomplete_reason, .. } => {
            if let Some(reason) = incomplete_reason {
                rows.open(Item::Error(ErrorRow::build(turn_id, reason)));
            }
            true
        }
        // De vraag van een tool hoort op de kaart van de stap die hij tegenhoudt. De loop tekende
        // die kaart al vóór de run (zie [`Emitter::open`]); dit is wat hem in Ask-mode zichtbaar
        // maakt, want een regel met alleen een commando en een spin laat een lezer niet weten wat er
        // gevraagd werd. De vraag zelf blijft in de gebeurtenis wonen: de kaart heeft er geen veld
        // voor en krijgt er hier ook geen, want wat de UI moet tonen, staat al in het event dat haar
        // voorschotelt. Een vraag op een onbekende id krijgt wél een eigen kaart — wie gevraagd wil
        // worden, mag niet genegeerd worden.
        ThreadEvent::ApprovalRequested { item_id, .. } => {
            if rows.get(item_id).is_some() {
                true
            } else {
                rows.open(pending_card(item_id));
                true
            }
        }
        // Een beantwoorde vraag sluit de kaart: de knoppen verdwijnen omdat er een feit van is, en
        // dat feit (`approval`) staat al op de kaart zelf. Op een kaart die er nooit was is niets te
        // sluiten, en dat is geen fout maar een klik op een kaart van een beurt die al stopte.
        ThreadEvent::ApprovalResolved { item_id, decision, .. } => {
            match rows.items.get_mut(item_id) {
                Some(Item::CommandExecution(card)) => {
                    card.approval = Some(*decision);
                    true
                }
                Some(Item::FileChange(change)) => {
                    // Een patch-kaart heeft geen uitvoer waarin een beslissing past, en de beslissing
                    // verbergen is oneerlijker dan haar nergens zetten: de kijker mag zien dat hij
                    // weigerde, want dat verklaart waarom het bestand bleef zoals het was.
                    change.status = match decision {
                        ApprovalDecision::Accept | ApprovalDecision::AcceptForSession => ItemStatus::Completed,
                        ApprovalDecision::Decline | ApprovalDecision::Cancel => ItemStatus::Failed,
                    };
                    true
                }
                Some(_) => true,
                None => false,
            }
        }
        _ => true,
    }
}

/// De kaart voor een vraag die op geen enkele bestaande regel past: dan is de vraag zelf het enige
/// feit, en is een lege regel eerlijker dan een kaart die een uitkomst verzint.
fn pending_card(item_id: &str) -> Item {
    Item::CommandExecution(CommandExecutionItem {
        item_id: item_id.to_string(),
        command: String::new(),
        cwd: None,
        timeout_seconds: None,
        status: ItemStatus::InProgress,
        exit_code: None,
        timed_out: false,
        duration_ms: 0,
        output: String::new(),
        approval: None,
    })
}

/// De tekst van een regel, ongeacht welk soort het is.
///
/// Publiek omdat "wat staat er op dit scherm" een vraag is die buiten deze crate ook gesteld
/// wordt: een test die telt of een vraag na een herstart éénn keer terugkwam, en een statusregel
/// die het gesprek nummert. Binnen deze crate was het een hulp; naar buiten is het de enige
/// manier om een regel te lezen zonder elk item-soort te hoeven herkennen.
pub fn streamed_text(item: &Item) -> String {
    match item {
        Item::AgentMessage(i) => i.text.clone(),
        Item::Reasoning(i) => i.text.clone(),
        Item::UserMessage(i) => i.text.clone(),
        Item::CommandExecution(i) => i.output.clone(),
        _ => String::new(),
    }
}

/// Builds the error row a truncated turn leaves behind. Spelled out so the `interrupted`
/// distinction stays visible in one place.
struct ErrorRow;

impl ErrorRow {
    fn build(turn_id: &str, reason: &str) -> baudet_core::ErrorItem {
        baudet_core::ErrorItem {
            item_id: format!("{turn_id}/incomplete"),
            message: reason.to_string(),
            status: ItemStatus::Failed,
        }
    }
}
