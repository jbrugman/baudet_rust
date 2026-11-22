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

use crate::reducer::{reduce, Emitter, Rows};
use baudet_backend::{Backend, SendRequest, TurnHandle};
use baudet_core::{Item, ItemStatus, Msg, ThreadEvent};
use baudet_store::session::Session;
use baudet_store::Transcript;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// What a send call handed back: the live handle, a subscription to this turn's events, and the
/// join handle of the reader that folds them.
pub struct Sent {
    pub turn_id: String,
    /// Het aantal beurten dat het gesprek had vóórdat deze vraag erbij kwam. Een afgebroken beurt
    /// kan het model-verleden niet meer zuiver terugdraaien - de emitter sloot zijn regels al - en
    /// vraagt daarom om de grens in plaats van om een gok: alles erachter is antwoord dat er nooit
    /// een werd. Bestemd voor de aanroeper die van plan is af te breken; wie gewoon afrondt, laat
    /// het gesprek zoals het is.
    pub messages_before: usize,
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
    /// The model and reasoning level from settings, stamped on every request. `AppState` does not
    /// know what they mean -- it hands them to the backend, which does -- but the request is built
    /// here, so this is the only place they can enter it.
    ///
    /// Behind a `Mutex` for the same reason `rows` is: the value is written from the window's paint
    /// pass, when the settings panel saves, and read on the worker thread that builds the request.
    /// A plain field would make `AppState` `!Sync`, and every caller here already holds it in an
    /// `Arc`. Held for a clone only, never across a request.
    choices: Mutex<RequestChoices>,
    rows: Mutex<Rows>,
    /// De opslaglaag onder dit gesprek: `H2Store` is de origineler's ene lange aanraking op de
    /// schijf, dit is diezelfde aanraking. `Some` zodra een echte data-map te openen is; `None`
    /// houdt dit gesprek puur in het geheugen, wat de oorspronkelijke scripted demo en elke test
    /// die alleen het scherm meet, zijn gedrag laat. Bewust `Option` en niet "altijd op disk":
    /// `new` heeft geen map om in te schrijven, en een window dat niets mag schrijven, mag nog
    /// tekenen - dezelfde eerlijke teruggval die `SidebarModel::open_default` al kiest.
    transcript: Mutex<Option<Transcript>>,
    /// Het gesprek dat dit handvat bedient. Zolang `transcript` `None` is, leeft
    /// `active` alsnog: `messages` is sinds deze laag de kop van het gesprek, en de
    /// renderaar en de draad hebben die rijen nodig ongeacht of er geschreven wordt.
    active: Mutex<Session>,
    live_turns: AtomicUsize,
    turn_counter: AtomicUsize,
    live: Mutex<Option<Arc<Mutex<TurnHandle>>>>,
}

/// What the settings panel chose for the next request.
#[derive(Debug, Clone, Default)]
pub struct RequestChoices {
    pub model: Option<String>,
    pub reasoning: Option<String>,
    /// The ceiling every subsequent turn may spend, from the same panel. `None` leaves the decision
    /// to the endpoint, which is what an empty field means and why this is an option rather than a
    /// number with a default: inventing a budget here would cap an answer nobody capped.
    pub max_output_tokens: Option<u32>,
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

/// Everything a turn may remember, newest first, in the shape the wire reads.
///
/// De kop van het gesprek is [`Session::messages`] - hetzelfde vlak als `SessionState.messages` in
/// `AppState.kt:806` - en niet langer de schermrijen. Dat is meer dan een verhuizing van bron: een
/// boodschap uit de opslag is al een feit, terwijl een rij alleen bestaat als het scherm haar ooit
/// tekende. Een gesprek dat van disk wordt geladen, had met de rows-versie een verleden zonder
/// verleden.
///
/// De vorm blijft `Vec<ThreadEvent>`, want [`baudet_backend::protocol::wire_history`] is het enige
/// punt dat `Msg` in wire-volgorde zet en die functie slikt gebeurtenissen. De lijst staat
/// nieuwst-last opgeschreven en `wire_history` keert haar om en appendt de vraag van de aanroeper
/// daarna, daarom ontbreekt die vraag hier.
fn history_of(session: &Session) -> Vec<ThreadEvent> {
    session
        .messages
        .iter()
        .filter(|msg| !msg.content.trim().is_empty())
        .rev()
        .map(|msg| {
            let item: Item = if msg.role == "user" {
                Item::UserMessage(baudet_core::UserMessageItem {
                    item_id: "history/user".to_string(),
                    text: msg.content.clone(),
                    status: ItemStatus::Completed,
                })
            } else {
                Item::AgentMessage(baudet_core::AgentMessageItem {
                    item_id: "history/agent".to_string(),
                    text: msg.content.clone(),
                    status: ItemStatus::Completed,
                })
            };
            ThreadEvent::ItemCompleted { thread_id: String::new(), turn_id: String::new(), item }
        })
        .collect()
}

/// De `Msg`-vorm van een regel die opende, voor zover hij in het model-verleden thuishoort.
///
/// Alleen vraag en antwoord, en alleen leds: wat een model wordt gevraagd te onthouden is het
/// gesprek, niet zijn redening, zijn shell-output, zijn plan of een foutmelding. Een openings-exemplaar
/// zonder tekst is géén ledige boodschap maar een aanhef, en die blijft buiten beschouwing tot de
/// sluiting hem vult - `apply` vervangt hem dan, niet voegt toe.
fn msg_of(item: &Item) -> Option<Msg> {
    match item {
        Item::UserMessage(message) if !message.text.trim().is_empty() => Some(Msg::user(message.text.clone())),
        Item::AgentMessage(message) if !message.text.trim().is_empty() => Some(Msg::assistant(message.text.clone())),
        _ => None,
    }
}

impl AppState {
    pub fn new(backend: Arc<dyn Backend>, system: impl Into<String>) -> AppState {
        AppState {
            backend,
            system: system.into(),
            choices: Mutex::new(RequestChoices::default()),
            rows: Mutex::new(Rows::new()),
            transcript: Mutex::new(None),
            active: Mutex::new(Session::new("", "")),
            live_turns: AtomicUsize::new(0),
            turn_counter: AtomicUsize::new(0),
            live: Mutex::new(None),
        }
    }

    /// Set the model and reasoning level every subsequent request names.
    ///
    /// A setter rather than a constructor argument because these are the settings panel's values, and
    /// the panel can change them while a conversation is open; a turn already running keeps whatever
    /// it was sent with, since its request has already left.
    pub fn set_request_choices(
        &self,
        model: Option<String>,
        reasoning: Option<String>,
        max_output_tokens: Option<u32>,
    ) {
        *self.choices.lock().unwrap() = RequestChoices { model, reasoning, max_output_tokens };
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

        // Everything the model is allowed to remember, read before this turn's question is added
        // below: the request's transcript must not carry the question twice, because `user_text` is
        // appended after it by both request builders.
        let question = Item::UserMessage(baudet_core::UserMessageItem {
            item_id: format!("{turn_id}/user"),
            text: user_text.to_string(),
            status: ItemStatus::Completed,
        });
        let history;
        {
            // Eén slot, drie feiten van dezelfde beurt: het verleden wordt gelezen vóórdat deze
            // vraag meetelt (de request-bouwers appenden `user_text` zelf, dus meenemen zou de
            // vraag tweemaal maken), de vraag gaat het model-verleden in, en dezelfde vraag wordt
            // regel - op het scherm én in het spoor - vóórdat hij de deur uitgaat. Een beurt die
            // nooit antwoordt, laat dus zien wat er werd gevraagd.
            let mut active = self.active.lock().unwrap();
            history = history_of(&active);
            active.messages.push(Msg::user(user_text));
            let mut rows = self.rows.lock().unwrap();
            let mut emitter =
                Emitter::storing(&mut rows, &mut active.items, &thread_id, &turn_id);
            emitter.started(question.clone());
            // De vraag is geen open regel die nog gevuld moet worden zoals een antwoord: hij is op
            // het moment van vragen al een feit. Alleen `started` zou hem op het scherm zetten en
            // buiten het audit-spoor houden, want de emitter schuift een regel pas in `items` als
            // hij voltooid is (de reducer laat een geopende regel leeg). De sluiting doet precies
            // wat `ThreadEmitter.userMessage` doet - `started` en dan `completed` (`.kt:61-65)` -
            // zodat een herstart dezelfde vraag teruggeeft die nu op het scherm staat.
            emitter.completed(question);
        }

        let choices = self.choices.lock().unwrap().clone();
        let request = SendRequest {
            system: self.system.clone(),
            user_text: user_text.to_string(),
            history,
            previous_response_id: None,
            tools: None,
            image: None,
            max_output_tokens: choices.max_output_tokens,
            model: choices.model.clone(),
            reasoning: choices.reasoning.clone(),
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
        Ok(Sent {
            turn_id,
            messages_before: self.active.lock().unwrap().messages.len() - 1,
            handle: Arc::clone(&handle),
            reader: Mutex::new(Some(handle)),
        })
    }

    /// Vouw één gebeurtenis toe, of werk een bestaande regel bij.
    ///
    /// Het verschil met [`Self::apply`] is de bedoeling van de gebeurtenis, niet zijn vorm: `apply`
    /// vouwt de stream van een beurt (elk id hoort bij die beurt, dus `started` is daar letterlijk
    /// een opening), terwijl een lading uit de opslag regels opnieuw **laat zien** die bij een
    /// eerdere beurt hoorden. Een lading door `apply` sturen, zou elke herstelde regel als nieuw
    /// in het model-verleden zetten en het gesprek daarmee twee keer laten bestaan.
    ///
    /// Een id die het scherm nog niet kent, is hier dus geen opening maar een regel die terugkomt;
    /// een id die het wél kent, blijft op zijn plaats en wordt in tekst bijgewerkt.
    pub fn emit(&self, item: &Item) {
        let mut active = self.active.lock().unwrap();
        let mut rows = self.rows.lock().unwrap();
        if rows.get(item.item_id()).is_some() {
            rows.replace_for_screen(item.clone());
            return;
        }
        let mut emitter = Emitter::storing(&mut rows, &mut active.items, "", "");
        emitter.started(item.clone());
        if item.status() == ItemStatus::Completed {
            emitter.completed(item.clone());
        }
        if let Some(msg) = msg_of(item) {
            active.messages.push(msg);
        }
    }

    /// Vouw één gebeurtenis in het gesprek: de rijen van het scherm, en - alleen een voltooid feit -
    /// het spoor dat naar de opslag gaat. `ThreadEmitter` in `AppState.kt:800-806` is de origineler
    /// en zijn twee regels gelden hier onveranderd: een regel gaat pas het spoor in als hij voltooid
    /// is, en redening gaat er nooit in.
    ///
    /// De emitter staat hier en niet in de renderaar, want de vlag ("is dit af") en de bestemming
    /// ("gaat dit naar disk") mogen niet uit elkaar kunnen lopen. Eén gebeurtenis, twee bestemmingen,
    /// één volgorde.
    pub fn apply(&self, event: &ThreadEvent) -> bool {
        let mut active = self.active.lock().unwrap();
        let mut rows = self.rows.lock().unwrap();
        let mut emitter = Emitter::storing(&mut rows, &mut active.items, "", "");
        match event {
            ThreadEvent::ItemStarted { item, .. } => {
                let kept = emitter.started(item.clone());
                if let Some(msg) = msg_of(item) {
                    active.messages.push(msg);
                }
                kept
            }
            ThreadEvent::ItemDelta { item_id, kind, delta, .. } => emitter.delta(item_id, *kind, delta),
            ThreadEvent::ItemCompleted { item, .. } => {
                let kept = emitter.completed(item.clone());
                // De voltooide regel zoals de reductor hem houdt, niet het sluitende exemplaar dat
                // de stream bracht: de transport-laag sluit een rij met ledige tekst en laat de
                // delta's het werk doen (openai.rs `close_open_rows`), dus het exemplaar van de draad
                // is niet de waarheid. Het scherm en het model-verleden mogen er niet over
                // verschillen - één gesprek, één tekst. `Emitter::completed` bewees zojuist dat deze
                // id bekend is, dus dit is geen gok.
                let Some(row) = rows.get(item.item_id()) else { return kept };
                if let Some(msg) = msg_of(row) {
                    // Voltooien in plaats van toevoegen: `started` zette de aanhef van deze regel al
                    // in het model-verleden, en die aanhef vervangen is zuiverder dan er een tweede
                    // regel onder zetten. Van achteren zoeken houdt de gesprekvolgorde intact als een
                    // beurt meer dan één regel van hetzelfde soort opent.
                    match active.messages.iter_mut().rev().find(|held| held.role == msg.role) {
                        Some(held) => held.content = msg.content,
                        None => active.messages.push(msg),
                    }
                }
                kept
            }
            other => reduce(&mut rows, other),
        }
    }

    /// Of dit scherm ouder bezit heeft dat het nog niet tekent, waar de cursor staat en hoeveel
    /// rijen er nu staan: de drie getallen die lazy paging vraagt vóórdat het laadt.
    ///
    /// De cursor is een volgorde-nummer en geen indeks - `oldest_loaded_seq` zegt of de opslag nog
    /// rijen houdt die dit beeld niet kent - en een vorgere pagina voorzetten mag de schrijfgrens
    /// niet verschuiven, anders herschrijft `persist` het halve verleden dat het scherm nooit zag.
    pub fn paging(&self) -> (bool, Option<u64>, u64) {
        let active = self.active.lock().unwrap();
        (active.has_more_older(), active.oldest_loaded_seq, active.items.len() as u64)
    }

    /// Hoeveel rijen en beurten dit gesprek draagt, voor de statusregel die het gesprek nummert.
    pub fn transcript_len(&self) -> (usize, usize) {
        let active = self.active.lock().unwrap();
        (active.items.len(), active.messages.len())
    }

    /// Lees één venster ouder en zet het vooraan; het aantal regels dat het scherm echt kreeg.
    ///
    /// De lezing is read-only en mag op de worker, de voorzet hier is het moment waarop het scherm
    /// verandert - dezelfde splitsing die de origineler in `loadOlderItems` en `ChatPanel.loadOlder`
    /// hanteert. De aanroeper kiest de cursor en krijgt het aantal terug, zodat "kan ik nog verder"
    /// en "zijn er regels bij" dezelfde bron hebben.
    ///
    /// De cursor is een **regelnummer in de opslag**, geen teller van wat het scherm draagt: de
    /// opslag knipt op regel (`item_rows`: de reeks is de plaats in het bestand, want bytes zijn
    /// geen regelnummers). Wat `prepend_older` als `older_seq` terugkrijgt, is het laagste
    /// regelnummer dat deze voorzet bevat, en dat zet het beeld als nieuwe knip - niet de lengte
    /// van `active.items`, die ook live-rijen van een lopende beurt zou tellen en daarmee de
    /// volgende knip zou miszetten.
    pub fn load_older(&self, before_seq: u64) -> Result<usize, String> {
        let window = {
            let transcript = self.transcript.lock().unwrap();
            let transcript = transcript.as_ref().ok_or("dit gesprek heeft geen opslag-map")?;
            transcript
                .items_window(baudet_store::repository::page::SCROLL, Some(before_seq))
                .map_err(|error| error.to_string())?
        };
        Ok(self.prepend_older(window.items, window.older_seq))
    }

    /// Zet een ouder venster vóór de rijen die er al staan, en verzet de cursor.
    ///
    /// Deze buigt om `Rows` en om de emitter heen, en dat is de reden, niet een bekorting:
    /// `Rows::open` kent één volgorde per id en zou een vorgere rij die hier al stond op zijn
    /// oude plaats laten staan, en de emitter zou ouder goedgoed in het audit-spoor backappen.
    /// Ouder laden is lezen, en lezen raakt de schrijfgrens niet: `persisted_items_tail_id` blijft
    /// waar hij was, zodat de eerstvolgende beurt nog steeds alleen zijn eigen staart aanhangt.
    ///
    /// `older_seq` is de cursor die de opslag teruggaf: `None` zegt dat de opslag niets ouder meer
    /// houdt, en dan eindigt het scrollen hier in plaats van eindeloos dezelfde ledige pagina te
    /// vragen.
    pub fn prepend_older(&self, mut older: Vec<Item>, older_seq: Option<u64>) -> usize {
        let mut active = self.active.lock().unwrap();
        let held: Vec<String> = active.items.iter().map(|row| row.item_id().to_string()).collect();
        older.retain(|row| !held.iter().any(|id| id == row.item_id()));
        let new_ids: Vec<String> = older.iter().map(|row| row.item_id().to_string()).collect();
        let added = older.len();
        if added == 0 {
            active.oldest_loaded_seq = older_seq;
            return 0;
        }
        // Gemeten: de opslag levert per beurt `tu_N/msg` vóór `tu_N/user` (de emitter sluit de
        // beurt in die volgorde af, en `persist` appendt zoals hij ze kreeg). `active.items` is
        // chronologisch omgekeerd daarvan - de vraag van een beurt staat er langer. De opslag-pagina
        // is chronologisch; omkeren en vooraan-inserten maakt er de items-volgorde van. De cursor
        // (`older_seq`) komt rechtstreeks van de opslag en is onafhankelijk van deze lijstvolgorde.
        for row in older.into_iter().rev() {
            active.items.insert(0, row);
        }
        // De cursor gaat mee met wat de opslag aanwees: `older_seq` is het laagste positie-nummer
        // dat deze voorzet echt bevat, precies de knip voor de volgende `load_older`. `has_more_older`
        // had de opslag al in `older_seq` verwerkt (ledig venster -> `None`), dus `None` hier betekent
        // dat er niets ouder meer is en het scrollen eindigt - niet dat er een nieuwe knip nodig is.
        active.oldest_loaded_seq = older_seq;
        let mut rows = self.rows.lock().unwrap();
        // `new_ids` is chronologisch, precies zoals `active.items` en het scherm eruit horen te
        // zien. Prepanden van de eerste naar de laatste zet de pagina vooraan in die volgorde;
        // `prepend_for_screen` laat een id die al stond op zijn plaats, dus een grenspagina
        // verdubbelt of verplaatst niets.
        for id in &new_ids {
            let Some(row) = active.items.iter().find(|held| held.item_id() == id) else { continue };
            rows.prepend_for_screen(row.clone());
        }
        added
    }

    /// Maak een gesprek dat ergens anders werd gelezen, het gesprek van dit handvat.
    ///
    /// De lezing gebeurt op de worker (`build_session`), de ruil hier, en die splitsing is de
    /// reden dat ze twee functies zijn: in de origineler (`AppState.adoptSession`) mutateert de
    /// ruil gedeelde staat en mag hij daarom niet tussen de schrijfbewegingen van een lopende
    /// beurt komen. Hier is die scheiding geen formaliteit: deze methode is het enige punt waar
    /// een venster van het ene naar het andere gesprek springt, en zij gooit de rijen van het
    /// vorige gesprek weg - iets dat een paint-pass niet bij elkaar mag rapen.
    ///
    /// De rijen gaan via [`Self::draw`] op het scherm, niet via [`Self::emit`]: een gesprek dat
    /// van disk komt, is geen stroom en geen nieuw feit. `build` heeft de regels al gelezen en de
    /// schrijfranden (`persisted_messages`, `persisted_items_tail_id`) uit die lezing gezet, dus de
    /// schijf draagt het spoor al. `emit` zou dezelfde id's als opening van een nieuwe regel
    /// behandelen en het spoor opnieuw willen schrijven, en dat botst met de dedup-eis van het
    /// spoor ("twee keer opgeslaan", `reducer.rs`). De origineler scheidt dezelfde twee bewegingen:
    /// `AppState.buildSession` las en `adoptSession` liet alleen zien (`AppState.kt:320,344`).
    pub fn adopt(&self, session: Session) {
        let items = session.items.clone();
        *self.active.lock().unwrap() = session;
        self.draw(&items);
    }

    /// Zet een lading regels op het scherm, in de volgorde waarin de opslag ze gaf.
    ///
    /// Alleen tekenen: geen spoor en geen model-verleden, want beide dragen die regels al. Na
    /// [`Rows::clear`] kent `Rows` geen enkele id, dus elk item gaat vooraan en de lijst groeit zo,
    /// in de volgorde van de lading, tot het scherm oud-naar-nieuw is.
    fn draw(&self, items: &[Item]) {
        let mut rows = self.rows.lock().unwrap();
        rows.clear();
        for item in items {
            rows.prepend_for_screen(item.clone());
        }
    }

    /// Lees één gesprek uit de opslag in een verse staat, zonder de huidige aan te raken.
    ///
    /// Veilig buiten de UI-thread aan te roepen: er gebeurt alleen een schijflezing en een kopie
    /// van het gekozen model, precies zoals `AppState.buildSession` alleen las en `adoptSession`
    /// de gedeelde velden liet zetten. Zonder handvat is er niets te lezen en zegt deze functie
    /// dat, in plaats van een leeg gesprek te laten lijken op een geladen gesprek.
    pub fn build_session(&self) -> Result<Session, String> {
        let transcript = self.transcript.lock().unwrap();
        let transcript = transcript.as_ref().ok_or("dit gesprek heeft geen opslag-map")?;
        let mut session = transcript.build(&self.model()).map_err(|error| error.to_string())?;
        // De opslag is leidend voor het model, tenzij de persoon in dit venster al een model
        // koos: een herstart mag een bewuste keuze niet stilletjes terugdraaien.
        if session.model.trim().is_empty() {
            session.model = self.model();
        }
        Ok(session)
    }

    /// Het model dat de volgende aanvraag noemt, voor de titel van een laden-dialoog.
    pub fn model(&self) -> String {
        self.choices.lock().unwrap().model.clone().unwrap_or_default()
    }

    /// Het sessie-id dat dit handvat bedient, voor de titel van een dialoog en voor de vraag
    /// "welk gesprek staat er open". Een lege snaar betekent dat er geen opslag onder zit.
    pub fn session_id(&self) -> String {
        self.active.lock().unwrap().session_id.clone()
    }

    /// Of dit handvat op schijf schrijft. Een statusregel die "opgeslagen" zegt zonder dat er een
    /// handvat is, loog tegen de persoon.
    pub fn persists(&self) -> bool {
        self.transcript.lock().unwrap().is_some()
    }

    /// Schrijf wat deze beurt toevoegde. Zonder handvat gebeurt er niets, en dat is géén fout: een
    /// puur-geheugen gesprek tekent gewoon door, precies zoals de niet-beschrijfbare catalogus die
    /// `open_default` ook accepteert.
    pub fn persist(&self) -> Option<String> {
        let mut transcript = self.transcript.lock().unwrap();
        let transcript = transcript.as_mut()?;
        let mut active = self.active.lock().unwrap();
        transcript.persist(&mut active).err().map(|error| error.to_string())
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

    /// Sluit dit gesprek aan op een data-map op schijf: de ene handgreep waarmee een venster
    /// zegt "mijn transcript overleeft een herstart".
    ///
    /// De sessie-id komt uit `baudet_projects::new_session_id` (`s-` plus acht hex-tekens),
    /// dezelfde fabrikant als de sidebar en de catalogus gebruiken, zodat één gesprek één id
    /// heeft over index en rollout. Als de map niet te openen of te schrijven is, blijft het
    /// gesprek in het geheugen en meldt deze functie de oorzaak: de persoon tekent gewoon door,
    /// en de statusregel draagt het feit - niet de paniek - net als de niet-beschrijfbare
    /// catalogus die `open_default` ook accepteert.
    pub fn with_persistence(self, root: &std::path::Path, model: &str) -> Result<AppState, String> {
        self.on_session(root, &baudet_projects::new_session_id(), model)
    }

    /// Sluit aan op één bepaalde sessie-id: hetzelfde handvat, geen nieuw gesprek.
    ///
    /// De id is hier een argument en niet een verassing, want de map van de opslag is de id
    /// (`Rollout::open(root, session_id)`), en de sidebar toont id's die de catalogus kent. Een
    /// venster dat "open dit gesprek" vraagt, moet dat id kunnen noemen; met
    /// [`Self::with_persistence`] alone zou elke aanvraag een leeg gesprek krijgen met een id die
    /// niemand zocht.
    ///
    /// Bestaat de sessie, dan wordt zijn inhoud de staat van dit handvat - de identiteit van de
    /// schijf wint, want een herstart mag het model van het gesprek niet terugdraaien naar wat de
    /// CLI-ingave toevallig noemde. Bestaat hij niet, dan wordt de id aangemaakt met `model` als
    /// herkomst, en begint een nieuw gesprek op de plek waar de persoon erom vroeg.
    pub fn on_session(
        self,
        root: &std::path::Path,
        session_id: &str,
        model: &str,
    ) -> Result<AppState, String> {
        let opened = Transcript::open_or_create(root, session_id, Some(model), None)
            .map_err(|error| error.to_string())?;
        let state = self;
        let mut active = match opened.build(model) {
            Ok(session) => session,
            Err(_) => Session::new(session_id, model),
        };
        active.project_id = None;
        let items = active.items.clone();
        *state.transcript.lock().unwrap() = Some(opened);
        *state.active.lock().unwrap() = active;
        state.draw(&items);
        Ok(state)
    }

    /// Let the next turn be claimed.
    pub fn finish(&self) {
        self.live_turns.fetch_sub(1, Ordering::SeqCst);
        *self.live.lock().unwrap() = None;
    }
}
