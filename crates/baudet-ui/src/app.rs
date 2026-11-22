//! The window: a transcript, an input box, and the worker thread that owns the conversation.
//!
//! Shape of the data flow, which is the whole design of this file:
//!
//! ```text
//!   paint pass -- Submit ------------------>  worker thread
//!        ^                                        |
//!        |                                        v
//!        +---- Rows (snapshot) <----- AppState + pump() over a ScriptedBackend
//! ```
//!
//! The owner lives on the worker because eframe hands the paint closure `&mut self`: something
//! only reachable through that borrow cannot be shared with the thread draining a stream. So the
//! window sends *intents* and receives *snapshots*, and neither side can reach into the other.
//! `pump` is reused rather than rewritten because its contract is the one the turn-flow tests
//! already protect -- one reader, folded while draining, stops when the stream closes -- and a
//! frame clock that stops draining would otherwise be a second, subtly different turn loop.

use crate::palette;
use crate::panels::{self, Changes, SidebarIntent, Workspace};
use crate::rows::Row;
use crate::settings::{self, SettingsCommand, SettingsEvent, SettingsView};
use crate::sidebar::SidebarModel;
use baudet_app::session::pump;
use baudet_app::AppState;
use baudet_backend::protocol::Protocol;
use baudet_backend::scripted::{Script, ScriptedBackend};
use baudet_backend::transport::{Endpoint, ResponsesBackend};
use baudet_backend::Backend;
use baudet_core::{AgentMessageItem, Item, ItemStatus, ThreadEvent};
use baudet_settings::Provider;
use eframe::egui;
use egui::{CentralPanel, ColorImage, Frame, Id, Panel, ScrollArea, UiBuilder};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use tokio::sync::mpsc as ampcc;

/// What the window asks the worker to do. Kept to the three things the app can decide, so a frame
/// cannot invent a fourth state.
enum Intent {
    Submit(String),
    Stop,
    Quit,
    /// The panel chose a model or a reasoning level. Carried to the worker rather than applied on
    /// the paint side because `AppState`, which stamps them on a request, is owned there.
    Configure {
        model: Option<String>,
        reasoning: Option<String>,
        max_output_tokens: Option<u32>,
    },
    /// De sidebar koos een bestaand gesprek. De lading leest de worker, niet de paint-pass: in de
    /// origineler is dat dezelfde regel (`ChatPanel.loadSession` stuurt `buildSession` naar zijn
    /// `loadExecutor` en laat `adoptSession` op de EDT zien, ChatPanel.kt:457-478), om dezelfde
    /// reden — een lang gesprek mag het venster niet bevriezen, en een lezing mag nooit door de
    /// schrijfbewegingen van een beurt heen lopen.
    LoadSession { id: String, project: Option<String> },
    /// "Begin hier een nieuw gesprek" (`None` is de inbox). Zet alleen klaar: de origineler
    /// persisteert bewust niets (`AppState.startSessionIn`), want anders laat elke klik door de
    /// projectenboom een leeg gesprek achter. De rollout en de catalogusregel komen pas op de
    /// eerste vraag.
    NewSession(Option<String>),
    /// De lezer heeft de bovenrand van het gesprek bereikt en wil één ouder blok.
    LoadOlder,
}

/// Wat de worker over het gesprek meldt: wat er staat, en wat ermee bezig is.
///
/// Eén bundel in plaats van `(rijen, bezig)` met een losse vlag ernaast, omdat die drie statussen
/// in de origineler (`busy`, `loading`, en de statusregel die ze vertelt) anders uit twee bronnen
/// zouden moeten samenkomen: het venster tekent wat de werker zegt, en de werker is de enige die
/// weet of hij nog leest. `rows` is leeg bij een `reason` zonder lading — de rijen van toen blijven
/// dan op het scherm staan, precies zoals `loadSession` het vorige gesprek laat zien tot het nieuwe
/// er is ("no blank frame", ChatPanel.kt:453-455).
struct Snapshot {
    rows: Vec<Row>,
    /// Een beurt van dit gesprek heeft de sessie.
    busy: bool,
    /// De worker is een gesprek aan het lezen of ouder aan het halen.
    loading: bool,
    /// Dit gesprek wordt op schijf gezet.
    stored: bool,
    /// Wat er op schijf staat en wat er in het beeld staat, in die volgorde.
    turns: (usize, usize),
    /// Of de opslag van dit gesprek nog oudere regels houdt dan het beeld kent. Deze vlag is de
    /// reden dat het venster überhaupt om ouder mag vragen; zonder hem zou elk radwiel naar boven
    /// een lezing vragen van een gesprek dat helemaal aan het begin staat.
    has_more_older: bool,
    /// Een storing die de persoon mag lezen; `None` is "er is niets aan de hand".
    reason: Option<String>,
}

/// Which body the middle panel shows.
///
/// A choice rather than a boolean for "settings open", because the reference app's middle is a
/// tabbed pane — `Main.kt:130-134` adds Chat and Ingestellingen to one `JTabbedPane` — and a boolean
/// plus an overlay produced exactly the complaint this replaces: the form floated over the
/// transcript, and its status line and Save button fell outside the window. One state cannot be in
/// two tabs, so "is settings showing" and "is chat showing" are one question with two answers instead
/// of two flags that can both claim the same rectangle. Het models-tabblad van het voorbeeld is hier
/// bewust weggelaten: de Settings-plukknop toont ná een scan élk model al, dus een aparte
/// modellenlijst was tweede weergave van dezelfde lijst voor niets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Chat,
    Settings,
}

impl Tab {
    /// The order and the words of the strip. `const` so the tabstrip and a test enumerate the same
    /// two without either owning a copy of the list.
    pub const ALL: [Tab; 2] = [Tab::Chat, Tab::Settings];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Chat => "Chat",
            Tab::Settings => "Settings",
        }
    }
}

/// The app's own state: the last painted snapshot and the buttons' words. No conversation truth
/// lives here -- [`AppState`] holds that on the worker.
pub struct TranscriptApp {
    input: String,
    rows: Vec<Row>,
    /// Whether a turn owns the session. Drives the Stop button and disables Send, so the refusal
    /// in `AppState::send` stays a backstop rather than a user-visible surprise.
    busy: bool,
    status: String,
    intents: mpsc::Sender<Intent>,
    snapshots: ampcc::UnboundedReceiver<Snapshot>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// Where `--screenshot` must write its picture. `None` for a normal start.
    shot: Option<std::path::PathBuf>,
    /// Frames drawn so far in a screenshot run.
    frames: u32,
    /// Whether the picture has been written, so the run closes once instead of repeatedly.
    shot_done: bool,
    /// Whether to fire a model fetch on the first paint (set by `--fetch-models`). It is a flag, not
    /// an immediate send, because the fetch is asynchronous and a screenshot must be able to wait
    /// for its answer.
    fetch_models_on_start: bool,
    /// The question a screenshot run sends instead of the scripted demo (set by `--live --send`). It
    /// is a flag held until the first paint for the same reason as `fetch_models_on_start`: the turn
    /// is asynchronous and the picture must be able to wait for it to settle. `None` keeps the demo.
    shot_send: Option<String>,
    /// The left panel: the folder under inspection and its tree.
    workspace: Workspace,
    /// Het draft-veld van de map-rij. Het lééft hier en niet lokaal in `panels::file_tree_controls`, omdat
    /// `TextEdit::singleline(&mut draft)` zijn tekst uit de meegegeven string haalt en erin schrijft:
    /// `TextEditState` bewaart alleen cursor en offset, geen tekst (text_edit/builder.rs:551). Een
    /// lokaal `let mut draft = String::new();` bij elke paint wist dus elke toetsaanslag vóórdat de
    /// klik op `Open` of de Enter in een volgende frame arrives — dan is de rij leeg en opent er
    /// niets. Eén eigenaar van "wat staat er in het veld", en die overleeft de repaint.
    folder_draft: String,
    /// The right panel: the paths git reports for that folder.
    changes: Changes,
    /// Which body the middle panel shows. `Some` is a tab a person chose; `None` is the chat, which
    /// is what a window shows before anyone picks and is not a fourth state — see [`Tab`].
    active_tab: Option<Tab>,
    /// Why this window is not talking to a server, when `--live` asked it to. `None` for the normal
    /// scripted start, which is not a fallback and must not read as one.
    fell_back: Option<String>,
    /// The settings panel's own state: the view-model, the last listing, the last status line.
    settings: SettingsView,
    /// Commands out to the settings worker (a fetch, a save). The worker never reaches back into the
    /// window; it reports through `settings_events`, exactly as the conversation worker does.
    settings_tx: std::sync::mpsc::Sender<SettingsCommand>,
    /// Reports back from the settings worker, drained at the top of every paint.
    settings_events: std::sync::mpsc::Receiver<SettingsEvent>,
    /// The settings worker thread. Joined on drop so a save in flight finishes rather than dies.
    settings_worker: Option<std::thread::JoinHandle<()>>,
    /// De linkersidebar: de projectcatalogus plus wat er in die boom is gekozen. Het model woont
    /// hier en niet in `panels`, omdat het venster — niet de paint-pass — de eigenaar is van wat er
    /// in een vensterleven lang is gekozen.
    sidebar: SidebarModel,
    /// Het project van de selectie, onthouden tussen twee paint-passen door. Het model weet dit al,
    /// maar dit veld is wat de *rest* van het venster volgt — zo blijft de sidebar de schrijver en de
    /// bestandsboom de lezer, in één richting.
    current_project: Option<String>,
    /// Het sessie-id dat de sidebar het laatst koos.
    current_session: Option<String>,
    /// De data-map waarin gesprekken leven, of `None` als deze run puur in het geheugen tekent.
    ///
    /// `baudet_store::data_dir()` is de bron en de constructor leest hem één keer: wat een venster
    /// opent, is precies wat het opschrijft, en een paints-pass die de variabele opnieuw zou
    /// vragen, kon halverwege een run van map veranderen. `None` is geen fout maar de demo en de
    /// test-hygiëne — `new` heeft geen backend die antwoorden van disk hoefde te halen.
    data_root: Option<std::path::PathBuf>,
    /// Er is een lading onderweg. De worker kent deze vlag en de vensterkant leest hem via de
    /// statusregel; zolang hij staat, zijn `LoadSession` en `LoadOlder` afwijzend, want de
    /// origineler houdt precies één lezing tegelijk open (`loading`, ChatPanel.kt:461,494).
    loading: bool,
    /// Of de opslag nog oudere regels houdt dan het beeld kent. Komt mee met elke melding van
    /// de worker, want de werker is de enige die de opslag mag vragen.
    more_older: bool,
    /// Of het beeld onder aan het gesprek kleefde. Wordt gezet vóórdat een verzoek voor ouder
    /// werk uitgaat, zodat de terugsprong die een voorzet van above-the-fold-rijen maakt, het
    /// beeld niet naar beneden trekt.
    stick: bool,
    /// Of het linkervenster (projecten) staat getekend. De toggle in de kop zet dit om; een
    /// verborgen venster tekent zich gewoon niet, en `PanelState` onthoudt zijn breedte, dus hij
    /// keert op dezelfde breedte terug -- de herinnerde-breedte-bewaarnis van `sidebarToggle` in
    /// `Main.kt:247-269`, zonder diens divider-getreik.
    show_sidebar: bool,
    /// Of het rechtervenster (bestandboom/wijzigingen) staat getekend. Zelfde regel als hierboven.
    show_filetree: bool,
}

impl TranscriptApp {
    /// Builds the window over whatever `Backend` it is handed. With a [`ScriptedBackend`] the app
    /// is fully interactive and needs no server, which is the point of phase one.
    pub fn new(backend: Arc<dyn baudet_backend::Backend>) -> TranscriptApp {
        let (intent_tx, intent_rx) = mpsc::channel::<Intent>();
        let (snap_tx, snap_rx) = ampcc::unbounded_channel();
        let state = Arc::new(AppState::new(backend, "You are Baudet's Rust rewrite."));
        // De data-map wordt hier gekozen en niet in de paint-pass: één keer vragen wat de
        // omgeving zegt, en dat hele vensterleven lang dezelfde map blijven dragen. Zonder map
        // (`BAUDET_HOME` wees op iets onbruikbaars, of een test die geen schijf wil) tekent het
        // venster zoals het altijd tekende — in het geheugen.
        let root = Some(baudet_store::data_dir());
        let worker_root = root.clone();
        let worker = std::thread::spawn(move || worker_loop(state, worker_root, intent_rx, snap_tx));
        // The settings worker owns the blocking calls — the listing and the file write — so a paint
        // never waits on a socket or a disk. It is a second, simpler copy of the conversation worker's
        // shape on purpose: one command channel in, one report channel out, a thread that drains.
        let (settings_tx, settings_rx) = mpsc::channel::<SettingsCommand>();
        let (report_tx, settings_events) = mpsc::channel::<SettingsEvent>();
        let settings_worker = std::thread::spawn(move || settings_worker_loop(settings_rx, report_tx));
        // Read the file once, now, so the panel opens already naming the datapad and a corrupt file
        // announces itself before a keystroke. A file read is not a paint concern, but it is also not
        // a thread's — it happens here, at construction, exactly once.
        let mut settings = SettingsView::new();
        settings.load(&baudet_settings::SettingsFile::default_location());
        TranscriptApp {
            input: String::new(),
            rows: Vec::new(),
            busy: false,
            status: "idle".to_string(),
            intents: intent_tx,
            snapshots: snap_rx,
            worker: Some(worker),
            shot: None,
            frames: 0,
            shot_done: false,
            fetch_models_on_start: false,
            shot_send: None,
            workspace: Workspace::default(),
            folder_draft: String::new(),
            changes: Changes::default(),
            active_tab: None,
            fell_back: None,
            settings,
            settings_tx,
            settings_events,
            settings_worker: Some(settings_worker),
            sidebar: SidebarModel::open_default(),
            current_project: None,
            current_session: None,
            data_root: root,
            loading: false,
            more_older: false,
            stick: true,
            show_sidebar: true,
            show_filetree: true,
        }
    }

    /// Point this window at a PNG file it must draw and then write. Used only by `--screenshot`.
    pub fn with_screenshot(mut self, path: impl Into<std::path::PathBuf>) -> TranscriptApp {
        self.shot = Some(path.into());
        self
    }

    /// Choose the folder the filetree panel shows, through the same `open` its folder field uses.
    /// Nothing else has to know this happened: `panels::file_tree` notices an unprobed folder
    /// and asks git, which is the refresh a typed path triggers.
    pub fn with_workspace(mut self, path: impl AsRef<std::path::Path>) -> TranscriptApp {
        self.workspace.open(&path.as_ref().to_string_lossy());
        self
    }

    /// Build the window over a backend that talks to a server.
    ///
    /// Separate from [`TranscriptApp::new`] rather than a flag on it, because the difference is not a
    /// preference but a different object: the scripted window is the picture the screenshots are taken
    /// of, and a window whose first turn reaches a network is a different promise. What the two share
    /// is everything that matters -- `AppState`, the reducer, the pump, the panels -- which is the
    /// point of the `Backend` seam.
    fn with_backend(backend: Arc<dyn Backend>) -> TranscriptApp {
        TranscriptApp::new(backend)
    }

    /// Show `tab` on startup. A scripted picture has no clicks, so this is the only way to draw a tab
    /// whose default is the chat; for a person it is the same state the strip's button reaches.
    pub fn with_tab(mut self, tab: Tab) -> TranscriptApp {
        self.active_tab = Some(tab);
        self
    }

    /// Say why the window is not live, in the header's own line.
    ///
    /// A setter rather than a second argument to [`TranscriptApp::new`], because the reason is known
    /// only to whoever chose the backend, and a window that silently answers from a script after
    /// being asked for a server is the failure this exists to make visible.
    fn with_status(mut self, reason: String) -> TranscriptApp {
        self.status = reason.clone();
        self.fell_back = Some(reason);
        self
    }

    /// Ask the endpoint for its models on the first paint, through the panel's own request path.
    /// Paired with a screenshot it makes the run capture a real listing (or a real failure) rather
    /// than an empty picker; for a person it is the same command the Fetch button sends.
    pub fn with_fetch_models(mut self) -> TranscriptApp {
        self.fetch_models_on_start = true;
        self
    }

    /// Ask `question` of the live server on the first paint, as if it were typed into the composer.
    /// Only reached from a `--live` window: the scripted window's demo answer is the picture the
    /// screenshots are made of and must not be replaced by a turn nobody sent to a server.
    pub fn with_shot_send(mut self, question: impl Into<String>) -> TranscriptApp {
        self.shot_send = Some(question.into());
        self
    }

    /// Point the panel's endpoint field at `address`, the same way `with_workspace` points the
    /// folder field: one assignment the field would make itself, so the fetch that follows uses it.
    pub fn with_endpoint(mut self, address: &str) -> TranscriptApp {
        self.settings.set_endpoint(address);
        self
    }

    /// The demo conversation the window ships with, so a first run shows a real transcript: a
    /// streamed answer, a tool row, a plan, and an answer that arrives as raw SSE lines.
    pub fn demo_backend() -> Arc<ScriptedBackend> {
        let answer = Script::new()
            .event(ThreadEvent::TurnStarted { thread_id: TH.into(), turn_id: T1.into() })
            .event(start(T1, agent(T1, "")))
            .event(delta(T1, "Here is what I found. "))
            .event(delta(T1, "The Kotlin front end and this Rust rewrite agree on the item vocabulary."))
            .event(ThreadEvent::ItemCompleted {
                thread_id: TH.into(),
                turn_id: T1.into(),
                item: finish(
                    T1,
                    "Here is what I found. The Kotlin front end and this Rust rewrite agree on the item vocabulary.",
                ),
            })
            .event(start(T1, item_tool(T1)))
            .event(start(T1, item_plan(T1)))
            // A plan is a row like any other, so it closes the same way: the turn does not end
            // while a row still claims to work. Leaving it open would paint a marker that never
            // stops under a header that already says idle.
            .event(ThreadEvent::ItemCompleted { thread_id: TH.into(), turn_id: T1.into(), item: finished_plan(T1) })
            .event(ThreadEvent::TurnCompleted {
                thread_id: TH.into(),
                turn_id: T1.into(),
                usage: None,
                response_id: None,
                final_event: None,
                incomplete_reason: None,
            });
        let echo = Script::new()
            .event(ThreadEvent::TurnStarted { thread_id: TH.into(), turn_id: T2.into() })
            .event(start(T2, agent(T2, "")))
            .event(delta(T2, "You said that back to a scripted player. The real transport comes next."))
            .event(ThreadEvent::ItemCompleted {
                thread_id: TH.into(),
                turn_id: T2.into(),
                item: finish(T2, "You said that back to a scripted player. The real transport comes next."),
            })
            .event(ThreadEvent::TurnCompleted {
                thread_id: TH.into(),
                turn_id: T2.into(),
                usage: None,
                response_id: None,
                final_event: None,
                incomplete_reason: None,
            });
        Arc::new(ScriptedBackend::new(vec![answer, echo]))
    }

    fn paint_layout(&mut self, ui: &mut egui::Ui) {
        // Side panels first. egui allocates a panel's strip from whatever the enclosing `Ui` still
        // has at the moment of the call, so a central panel painted first would take the whole
        // width and leave the flanks to overlap it.
        // Asking git is bounded by the probe's own timeout, so it happens on the turn an opening or
        // a switch commits rather than from a paint.
        // De linkersidebar vóór de rest: egui verdeelt de breedte in de volgorde van de aanroep,
        // dus de kolom die het eerst wordt gevraagd, krijgt zijn breedte uit de volle breedte.
        let intents = if self.show_sidebar { panels::project_sidebar(ui, &mut self.sidebar) } else { Vec::new() };
        // De intenties eerst verwerken en dan pas tekenen wat ernaast staat: de bestandsboom hoort
        // het project van de zojuist gemaakte keuze, niet dat van de vorige pass.
        self.apply_sidebar_intents(intents);
        // De bestandsboom staat rechts, naast het middenvlak — `[ ProjectSidebar | Chat/Settings ]
        // [ Files + Changes ]` zoals `Main.kt:161-170` het opzet. De mapkiezer en de git-lijst
        // delen één rechterstrook; er staat geen derde paneel naast de projectsidebar.
        if self.show_filetree
            && panels::file_tree(ui, &mut self.workspace, &mut self.folder_draft, &self.changes)
        {
            self.changes.refresh(self.workspace.cwd());
        }
        // Drain every worker report the settings panel asked for. Unlike transcript snapshots these
        // are not dropped: a fetch or save result is a single fact the person is waiting on, and
        // there is at most one in flight.
        while let Ok(event) = self.settings_events.try_recv() {
            // The session's choice is forwarded before the panel sees it, so a save and the model
            // the next turn asks cannot disagree even for one frame.
            if let SettingsEvent::Adopted { model, reasoning, max_output_tokens } = &event {
                let _ = self.intents.send(Intent::Configure {
                    model: model.clone(),
                    reasoning: reasoning.clone(),
                    max_output_tokens: *max_output_tokens,
                });
            }
            self.settings.apply_event(event);
        }

        // Drain every pending snapshot and paint the newest. Intermediate frames are dropped on
        // purpose: a row's value is its *current* folded text, so an old snapshot has nothing a
        // later one lacks, and skipping it is what keeps a burst of deltas cheap.
        let mut newest = None;
        while let Ok(snapshot) = self.snapshots.try_recv() {
            newest = Some(snapshot);
        }
        if let Some(report) = newest {
            // Een melding zonder lading draagt alleen een reden: het gesprek van toen blijft op
            // het scherm staan, want "kan dit niet openen" is geen reden om iets te laten zien dat
            // er niet is.
            // Een melding die zegt dat er niets meer ouder is, maakt de pager stil, ook als de
            // rijen van toen blijven staan: de vraag "kun ik nog verder" mag niet aan een
            // gesprek-id blijven kleven dat al is gewisseld.
            self.more_older = report.has_more_older;
            if !report.rows.is_empty() || report.reason.is_none() {
                self.rows = report.rows;
                self.busy = report.busy;
                self.loading = report.loading;
                self.more_older = report.has_more_older;
            }
            self.status = match (&report.reason, &self.fell_back, report.busy, report.loading) {
                (Some(reason), _, _, _) => reason.clone(),
                (None, Some(reason), false, false) => reason.clone(),
                (None, _, true, _) => "streaming".to_string(),
                (None, _, _, true) => "loading session".to_string(),
                (None, _, _, false) => self.status_line(report.stored, report.turns),
            };
        }

        Panel::top("header").show(ui, |ui| {
            ui.horizontal(|ui| {
                // Beide toggles ankeren op hun eigen rand van de kopstrook, WEST en EAST, precies
                // zoals de toolbar in `Main.kt:176-184` ze neerzet. Met `Ui::put` op een eigen rect
                // en niet als knop in deze rij: een wikkel hier zou de volle root-breedte erven,
                // dus zou zijn klikadres ná de kop vallen en onbereikbaar worden (gemeten). `put`
                // geeft hem een eigen vierkant op de rand, en dát adres bereikt een test.
                let band = ui.available_rect_before_wrap();
                let root = root_ui_id(ui.ctx());
                let west = root.with((panels::PANEL_TOGGLES, "sidebar"));
                if ui
                    .put(panels::toggle_rect(band, false), panels::Toggle::new(west, false, self.show_sidebar))
                    .clicked()
                {
                    self.show_sidebar = !self.show_sidebar;
                }
                let east = root.with((panels::PANEL_TOGGLES, "filetree"));
                if ui
                    .put(panels::toggle_rect(band, true), panels::Toggle::new(east, true, self.show_filetree))
                    .clicked()
                {
                    self.show_filetree = !self.show_filetree;
                }
                ui.heading("Baudet");
                ui.separator();
                ui.label(self.status.clone());
                ui.separator();
                ui.label(format!("{} rows", self.rows.len()));
            });
        });

        let tab = self.tab();
        // The composer is part of the chat, not of the window: `ChatPanel` *is* the chat tab in the
        // reference (`Main.kt:127-131`), so a box that sends a turn has no business standing under the
        // settings form, where Enter belongs to a field. `Panel::bottom` is still the right strip for
        // it — a bottom bar is where a person looks to type — it is just only painted for this tab.
        if tab == Tab::Chat {
            Panel::bottom("composer").show(ui, |ui| self.composer(ui));
        }

        CentralPanel::default().show(ui, |ui| {
            // The strip first, in its own strip rather than as the body's first row: a `JTabbedPane`
            // puts its tabs on the edge of its content for the same reason — three buttons that vanish
            // when a long transcript scrolls are not a way to leave a tab.
            Panel::top("tabs").show(ui, |ui| {
                // De knoppen ankeren op de wortel-Ui van het venster, niet op deze strook.
                // `Panel::top` en zijn `Frame` dragen op deze egui-build geen instelbaar adres, en de
                // `Ui` die ze doorgeven zit in de gemeten keten `wortel·kind·kind·tabs·kind`, dus van
                // buiten niet te reconstrueren; de wortel-Ui van het venster wél (zie `root_ui_id`).
                // Een `UiBuilder::id(..)`-wikkel met click-sense registreert zijn klik op exact dat
                // anker plus zijn zout — gemeten, twee passen later leesbaar — en dát is wat een test
                // terugvindt zonder de paneel-lay-out na te bouwen. De tekst erin is een kaal label:
                // de klik leeft op de wikkel, niet op de label.
                let root = root_ui_id(ui.ctx());
                ui.horizontal_wrapped(|ui| {
                    for candidate in Tab::ALL {
                        let clicked = ui
                            .scope_builder(
                                UiBuilder::new()
                                    .id(root.with((TABSTRIP, candidate.label())))
                                    .sense(egui::Sense::click()),
                                |ui| {
                                    ui.add(egui::Label::new(candidate.label()).selectable(false));
                                },
                            )
                            .response
                            .clicked();
                        if clicked {
                            self.active_tab = Some(candidate);
                        }
                    }
                });
            });
            // One scroll area for whichever body is showing. It is what makes the form's own height
            // a non-question, which is exactly what the overlay could not answer: a 900 pt form in a
            // 500 pt window scrolls instead of pushing its Save button off screen.
            let transcript = tab == Tab::Chat;
            ScrollArea::vertical()
                .id_salt("center-scroll")
                .auto_shrink([false, false])
                // `stick_to_bottom` only for the transcript: it is the body that grows by itself. A
                // form and a list must stay wherever the person left them when a repaint adds a line
                // under them, or a keystroke in a token field would jump the view.
                // `stick_to_bottom` only for the transcript, and only while the reader left it
                // there: a page of older rows added above the fold would otherwise yank the view
                // back to the newest row, which is exactly the jump the reference app holds steady
                // (`olderReady` re-adds the height it inserted, TranscriptView.kt:300-312).
                .stick_to_bottom(transcript && self.stick)
                .show(ui, |ui| match tab {
                    Tab::Chat => self.transcript(ui),
                    Tab::Settings => settings::body(ui, &mut self.settings, &self.settings_tx),
                });
            if transcript {
                self.ask_for_older(ui);
            }
            ui.output_mut(|o| {
                o.cursor_icon = if self.busy { egui::CursorIcon::Progress } else { egui::CursorIcon::Default }
            });
        });
    }
}

impl TranscriptApp {
    /// Which body the window shows. Public so a test can say which tab it painted; the paint pass
    /// reads the same field the strip writes, so this can never disagree with the screen.
    pub fn tab(&self) -> Tab {
        self.active_tab.unwrap_or(Tab::Chat)
    }

    /// De statusregel van het gesprek: hoeveel regels er staan en of ze op schijf staan.
    ///
    /// Drie zinnen op één bron, want de origineler telt dezelfde twee getallen ook
    /// (`statusLine`, ChatPanel.kt:540): "not stored" wanneer er geen handvat onder zit,
    /// "scroll up for older" wanneer de opslag nog meer inhoudt dan het beeld, en "saved"
    /// wanneer beeld en schijf gelijk zijn. `turns` is (op schijf, in beeld) — de volgorde
    /// die `Snapshot::turns` draagt.
    fn status_line(&self, stored: bool, turns: (usize, usize)) -> String {
        let (on_disk, on_screen) = turns;
        if !stored {
            return format!("{} rules, not stored", on_screen);
        }
        if on_disk > on_screen {
            return format!("{} of {} rules — scroll up for older", on_screen, on_disk);
        }
        format!("{} rules, saved", on_screen)
    }

    /// Frames drawn so far. Public for the same reason as [`TranscriptApp::paint`]: a test driving the
    /// paint pass has to know whether this is the first frame, which is the only frame the window's
    /// own repaint request has not made yet.
    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// De data-map die dit venster draagt, of `None` als het puur in het geheugen tekent.
    ///
    /// Een lezer op het veld dat de constructor één keer vulde, zodat "op welke schijf schrijft
    /// dit venster" een vraag is die een test en een latere `--home`-UI kunnen stellen zonder de
    /// map opnieuw aan de omgeving te vragen — wat een ander antwoord zou geven zodra iemand
    /// `BAUDET_HOME` onder het venster verandert.
    pub fn data_root(&self) -> Option<&std::path::Path> {
        self.data_root.as_deref()
    }

    /// De map die het rechterpane op dat moment toont, of `None` als er nog geen gekozen is.
    ///
    /// Een lezer, geen schrijver: `Workspace::cwd` houdt één eigenaar en `panels::file_tree` blijft de
    /// enige die er via `open` naar schrijft. Public zodat een test kan vragen "heeft de rij deze map
    /// geopend" zonder het pane-self te hoeven aanroepen — precies de vraag waarmee de draft-fix zich
    /// bewijst (een klik op `Open` na een getypt pad móet hier uitkomen).
    pub fn workspace_folder(&self) -> Option<&str> {
        self.workspace.cwd()
    }

    /// Wat de sidebar deze pass heeft besloten.
    ///
    /// Eén functie, drie gevallen, en géén sessie-lading: item D maakt van `OpenSession` een geladen
    /// gesprek. Zolang dat ontbreekt, onthoudt het venster het id en laat het de rest van het venster
    /// met rust — een chat die doet alsof hij een sessie laadt terwijl er niets staat, is precies de
    /// valse belofte die dit project al eerder deed.
    fn apply_sidebar_intents(&mut self, intents: Vec<SidebarIntent>) {
        for intent in intents {
            match intent {
                SidebarIntent::OpenSession(id) => {
                    if self.busy || self.loading {
                        // Een beurt of een lezing houdt het gesprek vast: de klik is gezien en
                        // geweigerd, niet genegeerd. `ChatPanel.loadSession` doet precies dat
                        // (`if (busy) return`), want laden door de schrijfbewegingen van een
                        // beurt heen is het spoor dat achteraf niet klopt.
                        self.status = "busy — finish the turn first".to_string();
                        continue;
                    }
                    self.current_session = Some(id.clone());
                    let project = self.current_project.clone();
                    let _ = self.intents.send(Intent::LoadSession { id, project });
                    self.loading = true;
                    self.status = "loading session".to_string();
                }
                SidebarIntent::StartChat(project) => {
                    if self.busy {
                        self.status = "busy — finish the turn first".to_string();
                        continue;
                    }
                    self.current_session = None;
                    self.current_project = project.clone();
                    let _ = self.intents.send(Intent::NewSession(project));
                    self.status = "new conversation".to_string();
                }
                SidebarIntent::Deleted => {
                    // Wat er in het midden stond, is weg uit de catalogus. De rijen van toen
                    // laten staan zou betekenen dat er iets openstaat dat niemand meer kan
                    // terugvinden, dus het beeld leegt en er begint een leeg gesprek — zonder
                    // niets op schijf verwijderen: `delete_session` haalt de indexregel weg, de
                    // rollout blijft, en dat is een besluit dat hier niet thuishoort.
                    self.current_session = None;
                    let _ = self.intents.send(Intent::NewSession(self.current_project.clone()));
                    self.status = "removed from the catalogue".to_string();
                }
            }
        }
        // Het project van de selectie volgen, niet nadat alles getekend is: de bestandsboom tekent
        // zichzelf uit `workspace.cwd()`, dus dit veld is wat de volgende pass aan het project vraagt.
        self.current_project = match self.sidebar.selection() {
            Some(crate::sidebar::Selection::Project(id)) => Some(id.clone()),
            Some(crate::sidebar::Selection::Session { project, .. }) => project.clone(),
            _ => None,
        };
    }

    /// Het project van de huidige selectie, of `None` (inbox, of niets gekozen).
    pub fn selected_project(&self) -> Option<&str> {
        self.current_project.as_deref()
    }

    /// Het gekozen sessie-id. Een lezer voor tests en voor item D; er hangt nog geen gedrag aan.
    pub fn selected_session(&self) -> Option<&str> {
        self.current_session.as_deref()
    }

    /// Of het linkervenster staat getekend. Dezelfde vlag die de kop-toggle omzet en de paint-pass
    /// leest, dus dit kan nooit van het scherm afwijken -- idem `tab()`.
    pub fn sidebar_visible(&self) -> bool {
        self.show_sidebar
    }

    /// Of het rechtervenster staat getekend. Idem `sidebar_visible`.
    pub fn filetree_visible(&self) -> bool {
        self.show_filetree
    }

    /// De linkersidebar zelf, voor tests die de catalogus of de rechtenstatus willen lezen zonder
    /// te raden welke pixel erbij hoort.
    pub fn sidebar(&self) -> &SidebarModel {
        &self.sidebar
    }

    /// The window's layout, as a plain paint pass.
    ///
    /// A thin public entry on the one layout function `App::ui` already calls, so the pass a test
    /// drives and the pass a person sees cannot drift apart. `App::ui` itself is not drivable from a
    /// test: it wants an `&mut eframe::Frame`, which only a running integration owns.
    pub fn paint(&mut self, ui: &mut egui::Ui) {
        self.paint_layout(ui);
    }
}

/// Het zout van de tabknoppen, en dat van het vraagveld.
///
/// De adressen zijn gemeten, niet gekozen op gevoel. Met een losse proef op deze build:
///  * een widget met `.id_salt("veld")` krijgt `ui.id().with(IdSalt::new("veld"))` — een adres dat
///    meeschuift zodra de `Ui` zelf verschuift, dus alleen reproduceerbaar vanaf de wortel van het
///    venster. Dat kan hier: het vraagveld en het endpointveld staan in het middenvlak van een tab en
///    niet in een verspringende lijst;
///  * een `push_id(salt, ..)`-wikkel reproduceert dat adres van buitenaf níet — de knop erin erft de
///    auto-id van de kind-`Ui` (`Id::new(..)` plus een op-teller), en geen enkele combinatie van `salt`
///    met de wortel gaf hetzelfde adres: gemeten met `Id::new(salt)`, `root.with(salt)`,
///    `root.with(IdSalt::new(salt))` en `flat.with(salt)`.
///
/// De tabknoppen staan daarom in een `UiBuilder::id(..)`-wikkel met click-sense onder de wortel-`Ui`:
/// die wikkel registreert zijn klik op `root_ui_id(ctx).with((TABSTRIP, label))`, en dát is wat een
/// test terugvindt zonder de paneel-lay-out na te bouwen. Een label erin is alleen de tekst; de knop
/// zelf (`egui::Button`, `selectable_label`) zou een eigen auto-adres dragen dat van buiten niet te
/// reconstruëren is, dus de klik wordt op de wikkel geteld, niet op die knop.
pub const TABSTRIP: &str = "tabstrip";

/// Het stabiele zout van het vraagveld.
///
/// `.id_salt` geeft het veld een vast adres *relatief aan zijn ouder-`Ui`*, dus een toevallige
/// auto-id die zou meeschuiven zodra er boven in de composer iets bij komt. Het is géén van buiten
/// reconstrueerbaar root-adres zoals `TABSTRIP`: de composer zit in een `Panel::bottom`, en geen
/// enkele root-keten gaf dat adres terug (gemeten), dus tests/tabs.rs test de composer niet op adres
/// maar de tabblokken op klik. Het zout blijft nuttig voor de focusvolgorde binnen het venster.
pub const COMPOSER_FIELD: &str = "composer-field";

/// De `Ui` waarin deze vensters hun middenvlak tekenen, zoals een test die van buiten benoemt.
///
/// `run_ui` maakt hem zelf, met dit adres, en `App::ui` schildert het venster in die `Ui`. Een kaal
/// `.id_salt("veld")`-veld in het middenvlak eindigt dus op `root_ui_id(ctx).with(..)`, hoe breed of
/// smal het venster ook is — de keten hangt aan de viewport-id van het venster, niet aan de geometrie
/// van een paneel. Eén regel hier houdt test en app aan dezelfde keten.
pub fn root_ui_id(ctx: &egui::Context) -> Id {
    Id::new((ctx.viewport_id(), "__top_ui"))
}

impl TranscriptApp {
    /// The transcript rows.
    ///
    /// Split out of the paint pass rather than kept inline, because "which body" is now a choice and
    /// the choice should be one `match` in one place. Unchanged in every other respect, including
    /// that it inherits the style it is given: 24 rows each nudging the style would make the
    /// effective text size depend on what was painted last.
    fn transcript(&self, ui: &mut egui::Ui) {
        if self.rows.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label("No transcript yet. Type below and press Enter.");
            });
        }
        for row in &self.rows {
            paint_row(ui, row);
        }
    }

    /// Vraag om één ouder blok zodra de lezer bij de bovenrand van het gesprek rust.
    ///
    /// De trigger is een beweging naar boven, niet een positie: een radwiel omhoog of een sleep
    /// aan de balk. De origineler wachtte op rust bij de bovenrand (`value <= NEAR_TOP`,
    /// TranscriptView.kt:308); in een scroll area die door `stick_to_bottom` altijd onderaan
    /// begint, is een positie-drempel onbereikbaar zódat iemand actief omhoog beweegt, en dát is
    /// precies het moment waarop de vraag gesteld mag worden. De drempel zelf vervalt daarmee:
    /// `has_more_older` en één-lezing-tegelijk doen het werk dat `NEAR_TOP` deed.
    ///
    /// Drie dingen maken dit veilig: `has_more_older` komt uit het gesprek en zegt dat de opslag
    /// nog meer inhoudt (een vers geopend gesprek aan de staart raakt hier nooit), `loading`
    /// houdt er één onderweg — de `awaitingOlder`-vlag van de origineler, hier dezelfde vlag die
    /// de worker draagt — en `busy` want een beurt die schrijft, is niet het moment om van gesprek
    /// te wisselen. `stick` gaat uit vóórdat het verzoek gaat, zodat de voorzet het beeld niet
    /// naar beneden trekt.
    fn ask_for_older(&mut self, ui: &mut egui::Ui) {
        if self.busy || self.loading {
            return;
        }
        // Waartoe bewogen: het radwiel naar boven. Bewust een input-meting en geen
        // `Ui::interact` met `Sense::drag()` op het paneel: zo'n wikkel doet mee aan de
        // hit-test en legt zich over de tabstrip heen, waardoor een klik op Settings de
        // scroll-pagina vroeg en het tabblad nooit zette (gemeten: `tabs.rs` faalde erop).
        // een sleep op de balk zelf raakt damit buiten beeld; het radwiel en het trackpad
        // zijn op macOS de manier waarop iemand in een gesprek omhoog gaat.
        let scrolled_up = ui.input(|input| input.smooth_scroll_delta.y) > 0.0;
        if !scrolled_up {
            return;
        }
        if !self.has_more_older() {
            return;
        }
        self.stick = false;
        self.loading = true;
        let _ = self.intents.send(Intent::LoadOlder);
    }

    /// Of de opslag van dit gesprek nog regels houdt die het beeld niet kent.
    ///
    /// Gevraagd aan de handvatlaag via een snapshot in de origineler (`hasMoreOlder`, gezet door
    /// de gastheer); hier staat het antwoord in het venster omdat de werker het bij elke melding
    /// meedeelt, en een paint-pass mag nooit op de schijf kijken.
    fn has_more_older(&self) -> bool {
        self.more_older
    }


    fn composer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::multiline(&mut self.input)
                    // Het veld is het bewijs dat de chat-tab zijn composer heeft: zie `COMPOSER_FIELD`.
                    .id_salt(COMPOSER_FIELD)
                    .hint_text("Ask something…  (Enter sends)")
                    .desired_rows(2)
                    .lock_focus(false)
                    .desired_width(f32::INFINITY),
            );
            // Enter sends, Shift+Enter is a newline: the one shortcut the app cannot do
            // without, because the same box has to grow and submit.
            let send_requested =
                ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift) && response.has_focus();
            let send_clicked = ui.add_enabled(!self.busy, egui::Button::new("Send")).clicked();
            if (send_clicked || send_requested) && !self.busy && !self.input.trim().is_empty() {
                let text = std::mem::take(&mut self.input);
                let _ = self.intents.send(Intent::Submit(text));
                self.busy = true;
                self.status = "starting".to_string();
            }
            if ui.add_enabled(self.busy, egui::Button::new("Stop")).clicked() {
                let _ = self.intents.send(Intent::Stop);
            }
        });
    }
}

impl TranscriptApp {
    /// How many frames a `--screenshot` run draws before asking for the picture, so panels, fonts
    /// and the first-frame layout work have all settled. A picture taken earlier shows a window
    /// that is still arranging itself, which reads as a broken app rather than as an early frame.
    /// Frames a screenshot run may wait for a settled transcript, not a fixed count to wait out.
    ///
    /// The picture is taken once a turn has drained (see `take_screenshot`), so this is only a
    /// ceiling for the case where the stream never closes. It is generous because a screenshot is
    /// never on a person's critical path. On the timeout the window is still written, as-is, with
    /// the unfinished turn in it, and the miss is named on stderr: an honest picture of a turn that
    /// is still running is worth more to whoever asked for the picture than no picture at all, and
    /// the reason the ceiling exists at all is to bound a stream that never closes rather than a
    /// model that answers slowly.
    ///
    /// The number is measured (2026-11-21, a local gemma over `/chat/completions`). A shot run
    /// repaints at ~111 frames/s, so 600 frames was a ceiling of ~5.4 s. The same block asks gemma
    /// and drained a long answer in 3420 frames / 30.7 s, and a raw stream of an unbounded answer
    /// took 24.4 s (1180 deltas, first at 0.73 s) -- so 600 cut off every answer longer than a
    /// sentence and wrote a half-finished window while the stream was perfectly healthy.
    ///
    /// The ceiling cannot be the turn's watchdog, because the watchdog belongs to the transport:
    /// [`baudet_backend::transport::READ_TIMEOUT`] is 30 minutes per read, matching the reference
    /// app, and a stream that trickles is by design allowed to run. So this only has to outlast an
    /// answer no read timeout will end. 30 minutes of a streaming run is ~200 000 frames; 20 000
    /// outlasts a slow answer by a wide margin and still closes a window that froze without waiting
    /// on the transport.
    const SHOT_FRAMES: u32 = 20_000;

    /// Ask for a picture once enough frames have been drawn, write it, then close.
    ///
    /// The pixels come back through egui as an event, and eframe's painter has already put the
    /// rows in the top-down order a PNG file wants. Nothing here waits: a run that never receives
    /// its picture simply keeps drawing, so a check fails by not producing a file rather than by
    /// hanging.
    fn take_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.shot.clone() else { return };
        if self.shot_done {
            return;
        }
        // One frame after `paint` has folded a snapshot where the turn is no longer live and the
        // transcript is not empty, the rows on screen are the settled transcript. `busy` only ever
        // falls in `paint`, so this cannot be satisfied by a stale frame.
        // A turn is settled once it is no longer live and something rendered. When a model fetch
        // was asked for, the picture also waits until that listing has landed — success or a named
        // failure — so a fetch the run requested cannot be cut out of its own picture. SHOT_FRAMES
        // still caps the wait, so a server that never answers fails by writing the window as-is.
        let listing_settled = !self.fetch_models_on_start || self.settings.listing_answered();
        let settled = !self.busy && !self.rows.is_empty() && listing_settled;
        if settled || self.frames >= Self::SHOT_FRAMES {
            if !settled {
                eprintln!(
                    "screenshot: the turn had not settled after {} frames; writing the window as-is",
                    self.frames
                );
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let picture = ctx.input(|i| {
            i.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(picture) = picture else { return };
        if let Err(error) = write_png(&path, &picture) {
            eprintln!("could not write the picture to {}: {error}", path.display());
            std::process::exit(1);
        }
        self.shot_done = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for TranscriptApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // A screenshot run receives no input -- an unfocused window gets none -- and nothing else
        // in this program asks for a redraw, so without this the window draws once and goes still
        // and the picture never arrives.
        if self.shot.is_some() {
            ctx.request_repaint();
            // No input reaches an unfocused window, so nothing would ever be sent. Rather than a
            // second data path, start the conversation exactly the way a keystroke in the composer
            // does -- `Intent::Submit` -- and let the worker drain the script through the same
            // `pump` the turn-flow tests already protect. Without this the picture would be an
            // honest cold start, which is true but shows nothing of the app.
            if self.frames == 0 {
                // A live --send window asks its own question; every other picture keeps the scripted
                // demo, which is the fixed text the reference screenshots are compared against.
                let question = self
                    .shot_send
                    .clone()
                    .unwrap_or_else(|| "Show me how the Rust rewrite renders a turn.".to_string());
                let _ = self.intents.send(Intent::Submit(question));
                if self.fetch_models_on_start {
                    self.settings.request_fetch(&self.settings_tx);
                }
            }
            self.frames += 1;
            self.take_screenshot(&ctx);
        }
        // Row spacing belongs to this Ui, set once before any row is painted. Per-row style
        // nudges would make the effective spacing depend on what was painted last.
        ui.style_mut().spacing.item_spacing.y = 6.0;
        self.paint_layout(ui);
        // A streaming turn must repaint on its own; egui otherwise waits for input and would show
        // a half-finished answer until the user moved the mouse.
        if self.busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }
}

impl Drop for TranscriptApp {
    fn drop(&mut self) {
        // The worker's snapshot send and its `rx.recv()` are both interruptible by a disconnect,
        // so quitting cannot strand the thread; joining proves it rather than assuming it.
        let _ = self.intents.send(Intent::Quit);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // The settings worker ends when its command channel closes. Replacing our sender with a
        // throwaway drops the only sender the worker sees, so its `recv` returns an error and the loop
        // ends — then joining guarantees a queued save lands rather than being abandoned. A fetch
        // cannot strand the join: the provider carries its own timeout, so the thread always returns.
        if let Some(worker) = self.settings_worker.take() {
            self.settings_tx = mpsc::channel::<SettingsCommand>().0;
            let _ = worker.join();
        }
    }
}

/// The worker: owns the conversation, folds its events, publishes snapshots.
fn worker_loop(
    state: Arc<AppState>,
    root: Option<std::path::PathBuf>,
    intents: mpsc::Receiver<Intent>,
    snapshots: ampcc::UnboundedSender<Snapshot>,
) {
    // Eén lezing tegelijk. De wachtrij van deze worker is het antwoord op de generatie-teller
    // van de origineler (`loadGeneration`, ChatPanel.kt:459/467): daar bracht een tweede draad
    // het resultaat terug op de UI-draad en moest een teller zeggen welk gesprek er intussen
    // openhoorde. Hier leest één draad de wachtrij op volgorde, dus een ouder antwoord bestaat
    // niet — de derde klik opent het derde gesprek, ná de tweede en vóór niets.
    let loading = Arc::new(AtomicBool::new(false));
    // Wat de schijf van dit gesprek draagt. Gelezen op de momenten waarop de schijf veranderde —
    // bij een opening, een pagina ouder en een afgeronde beurt — en niet in elke melding: het
    // getal vraagt een tellende lezing, en een stream die per delta meldt, zou die lezing
    // duizend keer voor hetzelfde antwoord vragen. `None` is "dit gesprek staat niet op schijf".
    let mut disk: Option<usize> = None;
    // De catalogus die de sidebar ook tekent, hier in handen van de werker: die is de enige die
    // weet wat er in een gesprek staat, en een indexregel die tijdens een paint-pass wordt
    // bijgewerkt, leest halverwege een beurt. `None` is een catalogus die niet schrijfbaar is —
    // dezelfde eerlijke terugval als `SidebarModel::open_default` al kiest.
    let catalog = baudet_projects::Catalog::open(&baudet_store::data_dir()).ok();
    let publish = |state: &AppState, busy: bool, disk: Option<usize>| {
        let rows = state.rows().iter().map(Row::from).collect::<Vec<_>>();
        let on_screen = rows.len();
        let _ = snapshots.send(Snapshot {
            rows,
            busy,
            loading: loading.load(Ordering::SeqCst),
            stored: state.persists(),
            // De screen-telling komt uit de rijen die er nu staan, niet uit `Session::items`:
            // een venster dat ouder voorzet, groeit vóórdat het gesprek zelf iets doet.
            turns: (disk.unwrap_or(0), on_screen),
            has_more_older: state.paging().0,
            reason: None,
        });
    };
    publish(&state, false, None);
    let draining = Arc::new(AtomicBool::new(false));
    while let Ok(intent) = intents.recv() {
        if matches!(intent, Intent::LoadSession { .. }) {
            loading.store(true, Ordering::SeqCst);
        }
        match intent {
            Intent::Quit => break,
            Intent::Submit(text) => {
                let Ok(sent) = state.send(&text) else {
                    publish(&state, state.live_turns() > 0, disk);
                    continue;
                };
                publish(&state, true, disk);
                let Some(handle) = sent.take_reader() else {
                    publish(&state, true, disk);
                    continue;
                };
                // Exactly one pump per turn: `take_reader` hands the stream over once, so a
                // second Submit while this one runs cannot split it. The pump thread is joined
                // before the next intent, which is what makes that guarantee structural.
                let folder = Arc::clone(&state);
                let flag = Arc::clone(&draining);
                let watched = Arc::clone(&draining);
                watched.store(true, Ordering::SeqCst);
                let (tx, _rx) = ampcc::unbounded_channel();
                let drain = std::thread::spawn(move || {
                    pump(&handle, |event| folder.apply(event), &tx);
                    flag.store(false, Ordering::SeqCst);
                    folder.finish();
                });
                // Pumped turns end by themselves, so the loop stays responsive to Stop by
                // publishing while the drain thread works. Quit is the one intent that must not
                // be skipped, so it waits for the drain instead of racing it.
                while watched.load(Ordering::SeqCst) {
                    publish(&state, true, disk);
                    match intents.recv_timeout(std::time::Duration::from_millis(32)) {
                        Ok(Intent::Stop) => state.cancel_turn(),
                        Ok(Intent::Submit(_)) => {
                            // Baudet's rule: the live turn keeps the session. Refused, not queued.
                        }
                        Ok(Intent::Quit) => {
                            state.cancel_turn();
                            drain.join().ok();
                            publish(&state, false, disk);
                            return;
                        }
                        Ok(Intent::Configure { .. }) => {
                            // Te laat voor de turn die loopt, en er is niets tussen wachend:
                            // de wachtlus hierboven pakt het op zodra deze drain is afgelopen.
                        }
                        Ok(Intent::LoadSession { .. } | Intent::NewSession(_) | Intent::LoadOlder) => {
                            // Een lopende beurt houdt het gesprek vast, dus deze drie zijn hier
                            // geweigerd in plaats van genegeerd — dezelfde regel die het venster
                            // al toepast vóórdat het ze stuurt (`if (busy) return`,
                            // ChatPanel.kt:461). Het intent is gezien; de worker boven deze lus
                            // handelt het af zodra de drain is afgelopen.
                        }
                        Err(_) => {}
                    }
                }
                drain.join().ok();
                // De beurt is pas af als hij op schijf staat, in dezelfde volgorde als de
                // origineler (`persistTo` na de drain, `AppState.kt:1122-1144`). Zonder deze
                // zet zou een venster dat uit de opslag opent, er nooit iets in vinden.
                disk = after_turn(&state, &catalog, &snapshots);
            }
            Intent::Stop => state.cancel_turn(),
            // Stamped before the next Submit can read them; a turn already running keeps the
            // request it already sent, which is why this is an intent and not a shared field.
            Intent::Configure { model, reasoning, max_output_tokens } => {
                state.set_request_choices(model, reasoning, max_output_tokens)
            }
            Intent::LoadSession { id, project } => {
                if !load_session(&state, root.as_deref(), &id, project.as_deref(), &loading, &snapshots) {
                    publish(&state, state.live_turns() > 0, disk);
                    continue;
                }
                disk = state.disk_items();
                publish(&state, state.live_turns() > 0, disk);
            }
            Intent::NewSession(project) => {
                // Een id van dezelfde fabrikant als sidebar en catalogus gebruiken, plus een
                // handvat op de data-map: `newSession` in de origineler deed precies die twee
                // dingen én schreef de indexregel (`SessionIds.new()` + `upsertSession`,
                // `AppState.kt:294-298`). De inhoud van gesprek en index blijft leeg tot de
                // eerste beurt ze vult — wat hier komt is de plek, niet de tekst.
                state.switch_project(project.clone());
                match root.as_deref() {
                    Some(root) => {
                        // `on_session` is een bouwer voor een handvat dat er nog niet is; dit
                        // handvat bestaat al en leeft in een `Arc`, dus de opening gaat via het
                        // paar dat voor die volgorde bestaat: lezen (`open_store`) en dan ruilen
                        // (`switch_to`). Met een leeg bestand is dat precies hetzelfde gesprek als
                        // een verse session, en het project blijft zoals de sidebar het aanwees.
                        let id = baudet_projects::new_session_id();
                        let model = state.model();
                        let opened = state.open_store(root, &id, &model, project.as_deref());
                        let _ = state.session_id();
                        if let Err(reason) = opened.map(|(transcript, session)| {
                            state.switch_to(transcript, session);
                        }) {
                            // Geen handvat betekent geen opslag onder dit gesprek; het scherm
                            // blijft zoals het was en de reden zegt waarom.
                            let _ = snapshots.send(Snapshot {
                                rows: Vec::new(),
                                busy: false,
                                loading: false,
                                stored: false,
                                turns: (0, 0),
                                has_more_older: state.paging().0,
                                reason: Some(reason),
                            });
                            continue;
                        }
                        if let Some(catalog) = catalog.as_ref() {
                            catalog.upsert_session(&id, Some(&model), project.as_deref(), 0).ok();
                        }
                    }
                    // Zonder data-map tekent dit venster alleen in het geheugen: hetzelfde lege
                    // gesprek, geen id en geen index, want er is geen schijf voor.
                    None => state.adopt(baudet_store::session::Session::new("", &state.model())),
                }
                disk = state.disk_items();
            }
            Intent::LoadOlder => {
                let (has_more_older, cursor, _) = state.paging();
                if has_more_older {
                    if let Some(before) = cursor {
                        // Een fout hier is geen melding waard die het gesprek overstelpt: de
                        // lezer staat boven en krijgt dezelfde bovenkant. `load_older` geeft het
                        // aantal rijen terug, en nul of een error is "er is niets nieuws".
                        // De schijf veranderde hier niet — alleen het beeld — dus `disk`
                        // blijft zoals het was; de statusregel ziet daardoor meer regels "van
                        // ouder" in beeld komen zonder dat er iets bij is geschreven.
                        state.load_older(before).ok();
                    }
                }
            }
        }
        publish(&state, state.live_turns() > 0, disk);
    }
}

/// Sluit een beurt af op schijf en zeg wat de schijf daarna draagt.
///
/// Drie zetten, in deze volgorde, omdat de index een gevolg van de inhoud is: eerst het gesprek
/// (`Transcript::persist`: alleen de staart die deze beurt toevoegde), dan de indexregel die zijn
/// getal noemt, dan de telling voor de statusregel. De origineler deed dezelfde drie in één
/// methode (`persistTo`: `appendMessages`, `appendItems`, `upsertSession`, `AppState.kt:1122-1145`).
///
/// Een schrijffout is een status en geen stilstand: het gesprek blijft op het scherm, de
/// schrijfranden (`persisted_messages`, `persisted_items_tail_id`) zijn alleen opgeschoven voor
/// wat de schijf echt heeft, en de volgende beurt probeert de stap opnieuw. De opbrengst is wat
/// de schijf draagt — `None` als er geen handvat onder zit, want dan is er niets te tellen.
fn after_turn(
    state: &AppState,
    catalog: &Option<baudet_projects::Catalog>,
    report: &ampcc::UnboundedSender<Snapshot>,
) -> Option<usize> {
    let id = state.session_id();
    if let Some(reason) = state.persist() {
        let _ = report.send(Snapshot {
            rows: Vec::new(),
            busy: false,
            loading: false,
            stored: state.persists(),
            turns: (state.disk_items().unwrap_or(0), 0),
            has_more_older: state.paging().0,
            reason: Some(reason),
        });
    }
    if let (Some(catalog), false) = (catalog.as_ref(), id.is_empty()) {
        // `last_seq` is het aantal beurten, niet het aantal regels: de titel van een rij in de
        // sidebar is "N beurten", en `transcript_len` geeft beide tellingen in die volgorde.
        let (_, turns) = state.transcript_len();
        catalog
            .upsert_session(&id, Some(&state.model()), state.project().as_deref(), turns as i32)
            .ok();
    }
    state.disk_items()
}


///
/// De lezing en de ruil zitten in één move omdat deze worker de enige eigenaar van `state` is:
/// de origineler moest ze splitsen (`buildSession` op de `loadExecutor`, `adoptSession` op de EDT)
/// omdat daar twee draden dezelfde gedeelde staat aanraakten. Hier is het splitsen wél nuttig
/// gebleven — `open_store` en `switch_to` zijn twee methodes — maar de volgorde is triviaal, en
/// wat telt is dat er niemand schrijft: `Submit` pompt zijn beurt af en roept `finish` vóórdat
/// deze lus het volgende intent leest, en `persist` is dan al gedaan.
///
/// Zonder `root` is er niets te openen. Dat antwoordt met `false` en laat het beeld staan, want
/// "er is geen opslag" is een feit over de run, niet over het gesprek.
fn load_session(
    state: &AppState,
    root: Option<&std::path::Path>,
    id: &str,
    project: Option<&str>,
    loading: &Arc<AtomicBool>,
    report: &ampcc::UnboundedSender<Snapshot>,
) -> bool {
    let Some(root) = root else {
        loading.store(false, Ordering::SeqCst);
        return false;
    };
    let model = state.model();
    let opened = state.open_store(root, id, &model, project);
    loading.store(false, Ordering::SeqCst);
    match opened {
        Ok((transcript, session)) => {
            state.switch_to(transcript, session);
            true
        }
        // De reden gaat mee naar de statusregel: "kan niet openen" zonder oorzaak is precies de
        // stilte die een persoon laat raden of het gesprek weg is of de schijf.
        Err(reason) => {
            let _ = report.send(Snapshot {
                rows: Vec::new(),
                busy: false,
                loading: false,
                stored: state.persists(),
                turns: state.transcript_len(),
                has_more_older: state.paging().0,
                reason: Some(reason),
            });
            false
        }
    }
}

/// The settings worker: the only place a settings command touches the network or the disk.
///
/// A fetch is blocking (the provider is sync by design) and a save is a file write, so neither belongs
/// in a paint pass. The worker takes one command at a time and reports the outcome; when both channels
/// close — the window dropped — `recv` returns an error and the thread ends. It holds no settings of
/// its own: it is a pair of hands for `baudet_settings`, nothing more, which is what lets the panel
/// stay the single owner of what is on screen.
fn settings_worker_loop(commands: mpsc::Receiver<SettingsCommand>, report: mpsc::Sender<SettingsEvent>) {
    while let Ok(command) = commands.recv() {
        match command {
            SettingsCommand::Fetch { endpoint, api_key } => {
                // `endpoint` travels with the report so the panel can tell a late answer for an old
                // address from an answer for the one now in the field. The fetch borrows it, so it is
                // still owned here when the report takes it by value. The key travels too, so a
                // gateway that wants one answers rather than 401-ing every fetch.
                let result = match Provider::with_key(&endpoint, api_key) {
                    Ok(provider) => provider.list_models(),
                    Err(error) => Err(error),
                };
                let _ = report.send(SettingsEvent::Listing(result, endpoint));
            }
            SettingsCommand::Probe { endpoint, api_key } => {
                let outcome = scan_endpoint(&endpoint, api_key);
                let _ = report.send(SettingsEvent::Probed(
                    outcome.map(|o| o.protocol).map_err(|problem| problem.to_string()),
                    endpoint,
                ));
            }
            SettingsCommand::Save(settings) => {
                let outcome = baudet_settings::SettingsFile::default_location().write(&settings);
                // The choices travel to the session with the file, because a person who saved a
                // model means the next turn to use it. Reported whatever the write did: a session
                // asking the model on screen is right even when the file refused, and the save's own
                // outcome still lands in the panel's status line.
                let _ = report.send(SettingsEvent::Adopted {
                    model: settings.model,
                    reasoning: settings.reasoning,
                    max_output_tokens: settings.max_output_tokens,
                });
                let _ = report.send(SettingsEvent::Saved(outcome));
            }
        }
    }
}

/// The localhost addresses worth asking, in the order they are asked.
///
/// Ordered by what this project actually runs against, not by which port a tool happened to pick
/// first: the reference app's own default is `127.0.0.1:8000` (`AppConfig.kt:18`), LM Studio serves
/// `1234`, Ollama `11434`, and llama.cpp's server `8080`. Loopback only -- a sweep that reached out
/// over the LAN would be a port scan of somebody else's network, which is not what "find my server"
/// means.
///
/// The list is a starting point, not a census, and the gap is worth stating plainly because the
/// default endpoint misleads on this machine: `127.0.0.1:8000` is what the reference app and
/// `Settings::default` name, yet the gateway answering it here is reached over the LAN (measured
/// 2026-11-21: loopback `8000` refuses, `192.168.1.2:8000` answers). A sweep does not go looking
/// for it: reaching such a server means naming an address, which the settings field is for, while
/// asking a whole subnet would be a port scan of somebody else's network.
pub fn local_candidates() -> Vec<String> {
    [8000u16, 1234, 11434, 8080].iter().map(|port| format!("http://127.0.0.1:{port}/v1")).collect()
}

/// Whether anything is listening, decided at the socket layer before any request is made.
///
/// This is the cheap half of the sweep and it is measured, though the cost it saves is paid on the
/// endpoint this project is developed against, which is not on loopback (see [`discover_local`] for
/// where that comes from): connecting to eight ports -- open and closed -- took 0.00 s, while the same
/// question asked over HTTP took 36 s for eighteen ports, because a port nobody listens on costs the
/// client's whole watchdog instead of a socket answer. Refused means nobody is there; a timeout is
/// read as absent, because a server that will not accept a connection cannot answer a listing either.
pub fn port_is_listening(endpoint: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(endpoint) else { return false };
    let Some(host) = url.host_str() else { return false };
    let port = url.port_or_known_default().unwrap_or(80);
    let address = format!("{host}:{port}");
    let Ok(address) = address.parse::<std::net::SocketAddr>() else {
        // A name rather than a literal: resolving it is a question of its own, and a sweep of
        // localhost candidates should not become a DNS dependency.
        return false;
    };
    std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(250)).is_ok()
}

/// Ask localhost what servers are here, and which dialect each answers.
///
/// Reuses [`baudet_backend::protocol::scan`] per candidate rather than deciding anything new: listing
/// first, one probe, the answer's body naming the dialect. `api_key` is offered to every candidate
/// because that is what the transport will do on the turn that follows -- and a gateway that wants a
/// key answers a listing `401` without one, which would otherwise be reported as "no server" when a
/// server is exactly what it is.
///
/// Loopback only, and that boundary is a decision rather than an oversight. The endpoint this project
/// is developed against is `127.0.0.1:8000`, the default in the reference app and in
/// `Settings::default`, but the server answering it on this machine is reached over the LAN
/// (measured 2026-11-21: loopback `8000` refuses, `192.168.1.2:8000` answers). A sweep does not go
/// looking for it: reaching a server this machine has to name explicitly is one setting away in the
/// panel, while a sweep that scanned a LAN would be a port scan of somebody else's network -- and the
/// `36 s` on [`port_is_listening`] was paid on exactly such a non-loopback gateway, which is why the
/// gate is a socket and not a request.
pub fn discover_local(api_key: Option<String>) -> baudet_backend::protocol::Discovery {
    let candidates = local_candidates();
    let scan_one = move |endpoint: &str| match Provider::with_key(endpoint, api_key.clone()) {
        Ok(provider) => baudet_backend::protocol::scan(
            provider.base_url(),
            || {
                provider
                    .list_models()
                    .map(|models| models.into_iter().map(|model| model.id).collect())
                    .map_err(|error| error.to_string())
            },
            |path, model, body| probe_dialect(&provider, provider.base_url(), path, model, body),
        ),
        Err(error) => baudet_backend::protocol::ScanOutcome {
            endpoint: endpoint.to_string(),
            protocol: None,
            models: Vec::new(),
            notes: vec![format!("not a usable address: {error}")],
        },
    };
    baudet_backend::protocol::discover(&candidates, port_is_listening, scan_one)
}

/// Say what a sweep did, in the order it did it.
pub fn describe_discovery(discovery: &baudet_backend::protocol::Discovery) -> String {
    let mut lines = Vec::new();
    for outcome in &discovery.tried {
        let verdict = match outcome.protocol {
            Some(protocol) => format!("answers {}", protocol.word()),
            None => "no".to_string(),
        };
        lines.push(format!("  {} — {verdict}", outcome.endpoint));
        for note in &outcome.notes {
            lines.push(format!("      {note}"));
        }
    }
    match &discovery.found {
        Some(found) => format!(
            "found a server at {} speaking {}\n{}\nmodels: {}",
            found.endpoint,
            found.protocol.expect("a found server names its dialect").word(),
            lines.join("\n"),
            found.models.join(", ")
        ),
        None => format!("no local server answered a turn\n{}", lines.join("\n")),
    }
}

/// Ask one endpoint what it is, on the settings worker's thread.
///
/// The rules themselves live in [`baudet_backend::protocol::scan`] — listing first, one probe, the
/// answer's body rather than its status deciding the dialect. What lives here is only the two sides
/// of it, because this is the crate holding the credential and the HTTP client: the listing is the
/// provider's own call, and the probe is the same POST the transport would make.
///
/// The listing's models arrive in the picker's order, so the model a probe names is the one a person
/// would see first. That is not tidiness: naming a model can make a server page it in, which on this
/// machine displaces the resident model, so a scan only ever names a server already answering.
fn scan_endpoint(
    endpoint: &str,
    api_key: Option<String>,
) -> Result<baudet_backend::protocol::ScanOutcome, baudet_settings::ListingError> {
    let provider = baudet_settings::Provider::with_key(endpoint, api_key)?;
    let address = provider.base_url().to_string();
    Ok(baudet_backend::protocol::scan(
        &address,
        || {
            provider
                .list_models()
                .map(|models| models.into_iter().map(|model| model.id).collect())
                .map_err(|error| error.to_string())
        },
        |path, model, body| probe_dialect(&provider, &address, path, model, body),
    ))
}

/// One probe, answered in the transport's own terms: the status and the body it carried.
///
/// `model` and `body` come from the scan, which is where the rules about what may be named live; this
/// function adds nothing to that decision, it only makes the request and reads the answer. The
/// credential is offered when one is known because that is what the transport will do on the turn
/// that follows a successful scan — a probe that answers without a key and a turn that sends one are
/// two different servers as far as a person is concerned.
fn probe_dialect(
    provider: &baudet_settings::Provider,
    address: &str,
    path: &str,
    _model: &str,
    body: &serde_json::Map<String, serde_json::Value>,
) -> (Option<baudet_backend::protocol::Protocol>, String) {
    let url = format!("{address}{path}");
    let client = match reqwest::blocking::Client::builder().timeout(baudet_backend::protocol::PROBE_TIMEOUT).build() {
        Ok(client) => client,
        Err(error) => return (None, format!("no request could be built: {error}")),
    };
    let body = serde_json::Value::Object(body.clone());
    let mut request = client.post(&url).json(&body);
    if let Some(key) = provider.api_key() {
        request = request.bearer_auth(key);
    }
    let answer = match request.send() {
        Ok(answer) => answer,
        Err(error) => return (None, format!("no answer from {url}: {error}")),
    };
    let status = answer.status().as_u16();
    // The body is read whatever the status, because the scan's whole rule is that a body settles the
    // dialect and a status does not: a refusal carrying `{"error": …}` at 200 is a refusal, and a
    // named server that answers 404 with a chat body would be a chat server.
    let text = answer.text().unwrap_or_default();
    let value = serde_json::from_str(&text).unwrap_or_else(|_| serde_json::Value::String(text));
    baudet_backend::protocol::judge_probe(baudet_backend::protocol::Protocol::ChatCompletions, status, &value)
}

/// The working indicator, on the row's own line: a thin line with a marker travelling over it.
///
/// `egui::Spinner` was tried and dropped. It allocates a square of `interact_size.y` = 18 pt,
/// draws an 11 pt ring with a 3 pt stroke inside it, and on a Retina screen the closed arc plus
/// its mitred corners read as a 10 x 8 pixel wedge beside the header -- a smudge rather than
/// motion, because a ring that small cannot carry a readable turn.
///
/// A stock progress bar was the second attempt and is also dropped: a full-width filled track
/// is the loudest thing in the panel, and it promises a fraction that is not known yet. A
/// streamed turn has no length to divide by, so what is honest is a *ruler with a marker on it*
/// -- movement without a claim -- which is also how the Kotlin panel showed a running command.
fn progress_indicator(ui: &mut egui::Ui) {
    let height = 12.0;
    let width = ui.available_width();
    if width < 8.0 {
        ui.add_space(height);
        return;
    }
    let (rect, _response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    ui.request_repaint();
    let time = ui.input(|i| i.time);
    // The line spans the row; the marker crosses it and turns around, so it never jumps.
    let line =
        egui::Rect::from_min_max(egui::pos2(rect.min.x, rect.center().y), egui::pos2(rect.max.x, rect.center().y));
    ui.painter().line_segment([line.min, line.max], egui::Stroke::new(1.0, palette::BORDER));
    let span = (width / 2.0 - 6.0).max(0.0);
    let half = span * (1.0 - (time * 0.7).cos() as f32) / 2.0;
    let centre = egui::pos2(rect.min.x + 6.0 + half, rect.center().y);
    ui.painter().circle_filled(centre, 2.5, palette::ACCENT);
}

/// One row. Prose reads as text; anything with a body of its own gets an expander so a long turn
/// stays skimmable, and every row carries a status that does not depend on colour alone.
fn paint_row(ui: &mut egui::Ui, row: &Row) {
    let label = kind_label(row);
    let frame = Frame::new().fill(row_background(ui, row)).corner_radius(6.0).inner_margin(8.0);
    frame.show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(label).strong().size(11.0));
            if let Some(subject) = &row.subject {
                ui.label(egui::RichText::new(subject).monospace().size(12.0));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(row.status_word()).size(11.0).color(status_color(ui, row)))
            });
        });
        if row.body.is_empty() {
            // Nothing of its own to show yet. While the item is live the spinner says so, but it
            // replaces the blank line rather than adding to it: an empty row beside a spinner reads
            // as a rendering glitch, and the spinner is already the whole message.
            // De spinner is een boog van acht punten met een sluitvlak; op Retina groeit die
            // uit tot een wiggetje van 10 x 8 px. Bij `interact_size.y` = 16 is de boog 8 px
            // en blijft hij als spinner herkenbaar.
            if row.in_progress() {
                progress_indicator(ui);
            }
            return;
        }
        if row.in_progress() {
            progress_indicator(ui);
        }
        if row.collapsed {
            // A disclosure that opens lazily is the difference between a transcript and a dump:
            // the reader asked for the diff, so the diff is where they are looking.
            ui.collapsing("details", |ui| {
                ui.label(egui::RichText::new(&row.body).monospace().size(12.0));
            });
        } else {
            ui.label(&row.body);
        }
    });
}

fn kind_label(row: &Row) -> &'static str {
    use baudet_core::ItemKind as K;
    match row.kind {
        K::UserMessage => "you",
        K::AgentMessage => "baudet",
        K::Reasoning => "thinking",
        K::CommandExecution => "command",
        K::FileChange => "files",
        K::Plan => "plan",
        K::Error => "error",
        K::ToolCall => "tool",
        K::Image => "image",
    }
}

/// Colour plus a word, never colour alone: a status that only reads as red or green is
/// invisible to colour-blind readers and to a grey screenshot.
fn status_color(ui: &egui::Ui, row: &Row) -> egui::Color32 {
    let palette = &ui.visuals().noninteractive().fg_stroke.color;
    match row.status {
        ItemStatus::InProgress => ui.visuals().strong_text_color(),
        ItemStatus::Completed => *palette,
        ItemStatus::Failed => ui.visuals().error_fg_color,
        ItemStatus::Interrupted => ui.visuals().warn_fg_color,
    }
}

fn row_background(ui: &egui::Ui, row: &Row) -> egui::Color32 {
    use baudet_core::ItemKind as K;
    match row.kind {
        K::UserMessage => ui.visuals().selection.bg_fill.gamma_multiply(0.25),
        K::Error => ui.visuals().error_fg_color.gamma_multiply(0.18),
        _ => ui.visuals().faint_bg_color,
    }
}

/// Opens the window. `app_creator` is where a real transport gets wired in: it is the one place
/// holding a `CreationContext`, so it is the honest seam, and no placeholder is needed beside it.
/// Start the window.
pub fn run() -> eframe::Result {
    run_from(Startup::default())
}

/// How the window should start.
#[derive(Debug, Clone, Default)]
pub struct Startup {
    /// Where `--screenshot` writes its picture; `None` opens a window as usual.
    pub screenshot: Option<std::path::PathBuf>,
    /// A folder to open on startup. It travels the same road as a path typed into the panel:
    /// `Workspace::open` followed by the git probe `panels::file_tree` asks for. A scripted picture needs
    /// it because an unfocused window receives no keystrokes, and it is not a second way of choosing
    /// a folder -- it is the same one, entered by a flag.
    pub workspace: Option<std::path::PathBuf>,
    /// Open the settings panel on startup. Same reason as `workspace`: a headless `--screenshot` gets
    /// no keystrokes, so the only way to draw a panel that starts closed is to start it open.
    pub settings: bool,
    /// Ask the endpoint for its models on startup, as if Fetch were clicked. A headless run has no
    /// click, so this is the only way to capture the *result* of a fetch rather than its empty panel.
    pub fetch_models: bool,
    /// The endpoint to point the panel at on startup, through the same path as a path typed into the
    /// field — it replaces the stored base URL for this run. It exists for the same reason as
    /// `workspace`: a headless run cannot type an address, so without it `--fetch-models` could only
    /// ever probe the default, which is usually not where a server actually is.
    pub endpoint: Option<std::path::PathBuf>,
    /// Ask a server instead of the scripted player. Opt-in, because the two are different promises:
    /// the scripted window draws the same picture on every machine, and a window that reaches a
    /// network answers with whatever that machine happens to be running. Without a stored protocol
    /// there is nothing to send to, so the flag alone does not invent an endpoint.
    pub live: bool,
    /// The question a `--screenshot` run sends on its own. A headless window receives no keystrokes,
    /// so without a typed question it can only show its scripted demo. `Some` only means something
    /// beside `live`: it puts a real turn on the wire so the picture can capture a server answering,
    /// which is the point of `--live`. It reuses the same `Intent::Submit` a keystroke uses.
    pub send: Option<String>,
    /// Report which local server answers, then stop. A flag rather than a startup step for the
    /// reason in [`discover_local`]: the sweep costs seconds, and a window that costs seconds before
    /// it appears is a window that looks broken.
    pub discover: bool,
}

impl Startup {
    /// Read the process's own arguments.
    pub fn from_env() -> Result<Startup, String> {
        Startup::parse(std::env::args())
    }

    /// Parse `args`, where `args[0]` is the program name.
    ///
    /// An unknown option is an error rather than something to ignore: a mistyped flag that is
    /// quietly dropped starts a window nobody asked for, which in a scripted check hangs instead
    /// of failing.
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Startup, String> {
        let mut out = Startup::default();
        let mut args = args.into_iter();
        let _program = args.next();
        while let Some(arg) = args.next() {
            if arg == "--help" || arg == "-h" {
                return Err(HELP.trim_start().to_string());
            }
            if arg == "--screenshot" {
                match args.next() {
                    Some(path) => out.screenshot = Some(std::path::PathBuf::from(path)),
                    None => return Err(format!("{USAGE}\n--screenshot needs a file name")),
                }
            } else if let Some(rest) = arg.strip_prefix("--screenshot=") {
                out.screenshot = Some(std::path::PathBuf::from(rest));
            } else if arg == "--workspace" {
                match args.next() {
                    Some(path) => out.workspace = Some(std::path::PathBuf::from(path)),
                    None => return Err(format!("{USAGE}\n--workspace needs a folder")),
                }
            } else if let Some(rest) = arg.strip_prefix("--workspace=") {
                out.workspace = Some(std::path::PathBuf::from(rest));
            } else if arg == "--settings" {
                out.settings = true;
            } else if arg == "--fetch-models" {
                out.fetch_models = true;
            } else if arg == "--live" {
                out.live = true;
            } else if arg == "--send" {
                match args.next() {
                    Some(text) => out.send = Some(text),
                    None => return Err(format!("{USAGE}\n--send needs a question")),
                }
            } else if let Some(rest) = arg.strip_prefix("--send=") {
                out.send = Some(rest.to_string());
            } else if arg == "--endpoint" {
                match args.next() {
                    Some(value) => out.endpoint = Some(std::path::PathBuf::from(value)),
                    None => return Err(format!("{USAGE}\n--endpoint needs an address")),
                }
            } else if let Some(rest) = arg.strip_prefix("--endpoint=") {
                out.endpoint = Some(std::path::PathBuf::from(rest));
            } else if arg == "--discover" {
                out.discover = true;
            } else {
                return Err(format!("{USAGE}\nunknown option: {arg}"));
            }
        }
        Ok(out)
    }
}

const USAGE: &str =
    "usage: baudet-ui [--screenshot=<file.png>] [--workspace=<folder>] [--settings] [--fetch-models] [--endpoint=<address>] [--live] [--send=<question>] [--discover]";
const HELP: &str = "
Baudet - a local-first coding assistant

usage: baudet-ui [option]

  --screenshot <file>   draw the window, save a picture of it to <file> and exit,
  --screenshot=<file>   without taking focus from what you are doing
  --workspace <folder>  open <folder> in the filetree panel on startup
  --settings            open the settings panel on startup
  --fetch-models        on startup, ask the endpoint for its models (with --settings,
                        the picture waits for the listing so it cannot miss the fetch)
  --endpoint <address>  point the panel at <address> on startup, instead of the stored endpoint
  --endpoint=<address>  (needed with --fetch-models whenever the server is not the default)
  --live                send turns to the stored endpoint instead of the scripted player;
                        it needs a protocol in the settings file, which Scan endpoint writes
  --send <question>     on startup, ask <question> of the live server (with --screenshot, the
                        picture waits for that turn to settle); it needs --live
  --send=<question>     (a window with --live but no --send shows its scripted demo)
  --discover            list the local servers that answer a turn, say which dialect each
                        answers in, and exit without opening a window
  -h, --help            show this text";

/// Start the window with `startup`'s arguments.
///
/// The window is 1040x720, which is the size the original opens at; the minimum is ours, sized so
/// that a transcript row and the composer both stay reachable on a small laptop screen.
pub fn run_from(startup: Startup) -> eframe::Result {
    if startup.discover {
        // Print and stop. The sweep is the whole answer, and it is measured to cost seconds, so
        // opening a window behind it would make a question about the machine look like an app that
        // is slow to start. Status quo: without the flag nothing here runs and no port is asked.
        // The credential the app would offer, read the way the settings panel reads it: a typed
        // key in the file first, then the variable named there. Without it a gateway that wants a
        // key answers 401, which is a server, and the sweep would report it as no server.
        let key = baudet_settings::SettingsFile::default_location().read().settings().resolved_key();
        println!("{}", describe_discovery(&discover_local(key)));
        return Ok(());
    }
    let mut builder =
        egui::ViewportBuilder::default().with_inner_size([1040.0, 720.0]).with_min_inner_size([520.0, 360.0]);
    // A screenshot must not steal focus from what the person is doing. `with_active` is a no-op on
    // X11/Wayland, and the window only goes to the back on Windows, where that is the documented
    // way; on macOS and Linux lowering a window can stop it drawing at all, so it is not lowered
    // there.
    if startup.screenshot.is_some() {
        builder = builder.with_active(false);
    }
    #[cfg(target_os = "windows")]
    if startup.screenshot.is_some() {
        builder = builder.with_window_level(egui::WindowLevel::AlwaysOnBottom);
    }
    let mut options = eframe::NativeOptions {
        viewport: builder,
        // Software rasterising is the default when `glow` is the only backend, and it is what lets
        // this window appear on a machine whose GL path is untrusted. wgpu stays opt-in.
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    // On macOS the flag that keeps the app from activating sits on winit's event-loop builder,
    // which eframe only exposes through this hook. It is set last because a `NativeOptions` clone
    // drops the hook.
    #[cfg(target_os = "macos")]
    if startup.screenshot.is_some() {
        use winit::platform::macos::EventLoopBuilderExtMacOS as _;
        options.event_loop_builder = Some(Box::new(|builder| {
            builder.with_activate_ignoring_other_apps(false);
        }));
    }
    let screenshot = startup.screenshot.clone();
    let workspace = startup.workspace.clone();
    let settings = startup.settings;
    let fetch_models = startup.fetch_models;
    let endpoint = startup.endpoint.clone();
    let live = startup.live;
    let shot_send = startup.send.clone();
    // Chosen here rather than inside the closure, because the answer comes from the settings file and
    // nothing in the creation context, and because `startup` is moved into that closure below.
    let (backend, fell_back) = chosen_backend(&startup);
    eframe::run_native(
        "Baudet",
        options,
        Box::new(move |cc| {
            palette::install(&cc.egui_ctx);
            let mut app = TranscriptApp::with_backend(backend);
            if let Some(reason) = fell_back {
                app = app.with_status(reason);
            }
            if let Some(path) = &workspace {
                app = app.with_workspace(path);
            }
            if settings {
                app = app.with_tab(Tab::Settings);
            }
            if let Some(address) = &endpoint {
                app = app.with_endpoint(&address.to_string_lossy());
            }
            if live {
                // The panel's choices reach the session over the same channel a save uses, so a
                // window opened with --live asks the stored model from its first turn instead of
                // leaving that to the endpoint. Only for a live window: the scripted player's answer
                // is the picture the screenshots are taken of, and it must not be stamped with a
                // model nobody sent it to.
                let choices = app.settings.draft().ok();
                let _ = app.intents.send(Intent::Configure {
                    model: choices.as_ref().and_then(|draft| draft.model.clone()),
                    reasoning: choices.as_ref().and_then(|draft| draft.reasoning.clone()),
                    max_output_tokens: choices.as_ref().and_then(|draft| draft.max_output_tokens),
                });
            }
            if fetch_models {
                app = app.with_fetch_models();
            }
            if let Some(path) = &screenshot {
                app = app.with_screenshot(path);
                // A live window drawing a picture asks the server its own question, so the shot can
                // capture a real turn settling. Without --live the demo stands; without a question
                // there is nothing to ask, and inventing one is the endpoint's owner's decision.
                if live {
                    if let Some(question) = &shot_send {
                        app = app.with_shot_send(question.clone());
                    }
                }
            }
            Ok(Box::new(app))
        }),
    )
}

/// The backend this window will answer with: a server when `--live` can name one, the scripted
/// player otherwise.
///
/// Returns the fallback's reason beside the backend rather than returning an error, because "no
/// server tonight" is not a reason to refuse to start a window: the scripted transcript is what the
/// screenshots and the demo are made of, and a person who forgot to scan an endpoint still gets the
/// app. What it must not get is the impression the answers came from a model, which is what the
/// reason in the header is for.
fn chosen_backend(startup: &Startup) -> (Arc<dyn Backend>, Option<String>) {
    if !startup.live {
        return (TranscriptApp::demo_backend(), None);
    }
    // Read the file the window itself would read, rather than trusting a copy: the dialect and the
    // endpoint have to be the ones a save would write, or the header could claim a server the
    // settings panel does not show.
    let file = baudet_settings::SettingsFile::default_location();
    let settings = file.read().settings().clone();
    match endpoint_from_settings(&settings).and_then(|endpoint| {
        let reasoning = settings.reasoning.clone();
        ResponsesBackend::new(endpoint, reasoning)
            .map(|backend| Arc::new(backend) as Arc<dyn Backend> as _)
            .map_err(|error| error.to_string())
    }) {
        Ok(backend) => (backend, None),
        Err(problem) => (TranscriptApp::demo_backend(), Some(format!("scripted player: {problem}"))),
    }
}

/// Translate the settings file into the transport's endpoint.
///
/// This lives in the UI crate rather than in `baudet-backend` because of the direction of the arrow:
/// `baudet-settings` already knows the provider and the credential, while the backend crate knows
/// neither and must stay that way — a transport that reads a settings file could no longer be driven
/// by a test that hands it an endpoint. The window is the only crate in the graph that sees both
/// sides, so the translation happens here and the backend keeps taking a plain [`Endpoint`].
///
/// An unset dialect is an error rather than a default for the reason stated on
/// [`baudet_settings::Settings::protocol`]: guessing a dialect sends a turn built for one server to
/// another, and a wrong answer is worse than an absent one.
fn endpoint_from_settings(settings: &baudet_settings::Settings) -> Result<Endpoint, String> {
    let key = settings.resolved_key();
    let provider =
        baudet_settings::Provider::with_key(&settings.base_url, key.clone()).map_err(|error| error.to_string())?;
    let dialect = settings
        .protocol_word()
        .and_then(Protocol::from_word)
        .ok_or("no protocol chosen; use Scan endpoint in settings")?;
    Ok(Endpoint {
        base_url: provider.base_url().to_string(),
        credential: match key {
            Some(value) => baudet_backend::transport::Credential::typed(value),
            None => baudet_backend::transport::Credential::none(),
        },
        protocol: dialect,
        model: settings.model.clone().unwrap_or_default(),
        // No chaining: the reference app replays its own transcript, and a chat-completions server
        // keeps nothing between turns. Responses stays opt-in until a server is measured to chain.
        store: false,
    })
}

/// Write egui's picture to `path` as an 8-bit RGBA PNG.
///
/// `to_srgba_unmultiplied` rather than `to_array`: they agree for an opaque pixel, and the former
/// is the documented way to get bytes that mean sRGB, which is what an image file declares.
fn write_png(path: &std::path::Path, picture: &ColorImage) -> Result<(), String> {
    let [width, height] = picture.size;
    if width == 0 || height == 0 {
        return Err("the window drew nothing".to_string());
    }
    let bytes: Vec<u8> = picture.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let file = std::fs::File::create(path).map_err(|e| format!("create: {e}"))?;
    let mut writer = png::Encoder::new(file, width as u32, height as u32);
    writer.set_color(png::ColorType::Rgba);
    writer.set_depth(png::BitDepth::Eight);
    let mut stream = writer.write_header().map_err(|e| format!("header: {e}"))?;
    stream.write_image_data(&bytes).map_err(|e| format!("pixels: {e}"))?;
    stream.finish().map_err(|e| format!("finish: {e}"))
}

const TH: &str = "th_1";
const T1: &str = "tu_1";
const T2: &str = "tu_2";

fn start(turn: &str, item: Item) -> ThreadEvent {
    ThreadEvent::ItemStarted { thread_id: TH.into(), turn_id: turn.into(), item }
}

fn delta(turn: &str, text: &str) -> ThreadEvent {
    ThreadEvent::ItemDelta {
        thread_id: TH.into(),
        turn_id: turn.into(),
        item_id: format!("{turn}/msg"),
        kind: baudet_core::ItemKind::AgentMessage,
        delta: text.into(),
    }
}

fn agent(turn: &str, text: &str) -> Item {
    Item::AgentMessage(AgentMessageItem {
        item_id: format!("{turn}/msg"),
        text: text.into(),
        status: ItemStatus::InProgress,
    })
}

/// The completed form of the turn's answer. A streaming message stays `InProgress` until its
/// `ItemCompleted` arrives -- `TurnCompleted` builds no closing row of its own -- so this is what
/// turns "working" into "done" once the whole answer is on screen.
fn finish(turn: &str, text: &str) -> Item {
    Item::AgentMessage(AgentMessageItem {
        item_id: format!("{turn}/msg"),
        text: text.into(),
        status: ItemStatus::Completed,
    })
}

fn item_tool(turn: &str) -> Item {
    Item::ToolCall(baudet_core::ToolCallItem {
        item_id: format!("{turn}/tool"),
        tool: "read_file".into(),
        detail: "src/main/resources/static/index.html".into(),
        status: ItemStatus::Completed,
        ok: true,
        output: "24443 lines of front end, which is why the web route stayed on the table.".into(),
    })
}

/// The same plan, closed. A streamed row keeps its own status until its `ItemCompleted` arrives,
/// so the demo has to carry the plan's closing item the way [`finish`] carries the answer's.
/// The entry markers stay as they were: closing a row says the plan is no longer being streamed,
/// not that every step of it is now true.
fn finished_plan(turn: &str) -> Item {
    let mut item = item_plan(turn);
    if let Item::Plan(plan) = &mut item {
        plan.status = ItemStatus::Completed;
    }
    item
}

fn item_plan(turn: &str) -> Item {
    Item::Plan(baudet_core::PlanItem {
        item_id: format!("{turn}/plan"),
        entries: vec![
            baudet_core::PlanEntry::new("groene kern: core, backend, app", baudet_core::PlanEntryStatus::Completed),
            baudet_core::PlanEntry::new("transcript-UI op egui 0.36", baudet_core::PlanEntryStatus::InProgress),
            baudet_core::PlanEntry::new("echte transport: reqwest + SSE", baudet_core::PlanEntryStatus::Pending),
        ],
        status: ItemStatus::InProgress,
    })
}
