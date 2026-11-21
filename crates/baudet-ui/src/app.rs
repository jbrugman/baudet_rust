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
use crate::panels::{self, Changes, Workspace};
use crate::rows::Row;
use crate::settings::{self, SettingsCommand, SettingsEvent, SettingsView};
use baudet_app::session::pump;
use baudet_app::AppState;
use baudet_backend::protocol::Protocol;
use baudet_backend::scripted::{Script, ScriptedBackend};
use baudet_backend::transport::{Endpoint, ResponsesBackend};
use baudet_backend::Backend;
use baudet_core::{AgentMessageItem, Item, ItemStatus, ThreadEvent};
use baudet_settings::Provider;
use eframe::egui;
use egui::{CentralPanel, ColorImage, Frame, Panel, ScrollArea};
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
    snapshots: ampcc::UnboundedReceiver<(Vec<Row>, bool)>,
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
    /// The left panel: the folder under inspection and its tree.
    workspace: Workspace,
    /// The right panel: the paths git reports for that folder.
    changes: Changes,
    /// Whether the settings panel is painted. Owned by the window and toggled from the header, so
    /// one boolean answers "is it showing" for both the header label and the panel.
    settings_open: bool,
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
}

impl TranscriptApp {
    /// Builds the window over whatever `Backend` it is handed. With a [`ScriptedBackend`] the app
    /// is fully interactive and needs no server, which is the point of phase one.
    pub fn new(backend: Arc<dyn baudet_backend::Backend>) -> TranscriptApp {
        let (intent_tx, intent_rx) = mpsc::channel::<Intent>();
        let (snap_tx, snap_rx) = ampcc::unbounded_channel();
        let state = Arc::new(AppState::new(backend, "You are Baudet's Rust rewrite."));
        let worker = std::thread::spawn(move || worker_loop(state, intent_rx, snap_tx));
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
            workspace: Workspace::default(),
            changes: Changes::default(),
            settings_open: false,
            fell_back: None,
            settings,
            settings_tx,
            settings_events,
            settings_worker: Some(settings_worker),
        }
    }

    /// Point this window at a PNG file it must draw and then write. Used only by `--screenshot`.
    pub fn with_screenshot(mut self, path: impl Into<std::path::PathBuf>) -> TranscriptApp {
        self.shot = Some(path.into());
        self
    }

    /// Choose the folder the workspace panel shows, through the same `open` the panel's own field
    /// uses. Nothing else has to know this happened: `panels::left` notices the folder is unprobed
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

    /// Open the settings panel on startup. A scripted picture has no clicks, so this is the only way
    /// to draw a panel whose default is closed; for a person it is the same state the header button
    /// reaches.
    pub fn with_settings_open(mut self) -> TranscriptApp {
        self.settings_open = true;
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

    /// Point the panel's endpoint field at `address`, the same way `with_workspace` points the
    /// folder field: one assignment the field would make itself, so the fetch that follows uses it.
    pub fn with_endpoint(mut self, address: &str) -> TranscriptApp {
        self.settings.set_endpoint(address);
        self
    }

    /// The demo conversation the window ships with, so a first run shows a real transcript: a
    /// streamed answer, a tool row, a plan, and an answer that arrives as raw SSE lines.
    pub(crate) fn demo_backend() -> Arc<ScriptedBackend> {
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

    fn paint(&mut self, ui: &mut egui::Ui) {
        // Side panels first. egui allocates a panel's strip from whatever the enclosing `Ui` still
        // has at the moment of the call, so a central panel painted first would take the whole
        // width and leave the flanks to overlap it.
        // Asking git is bounded by the probe's own timeout, so it happens on the turn an opening or
        // a switch commits rather than from a paint.
        if panels::left(ui, &mut self.workspace, self.changes.meta()) {
            self.changes.refresh(self.workspace.cwd());
        }
        panels::right(ui, &self.changes, self.workspace.cwd());
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
        if let Some((rows, busy)) = newest {
            self.rows = rows;
            self.busy = busy;
            self.status = match (&self.fell_back, busy) {
                (Some(reason), false) => reason.clone(),
                (_, busy) => if busy { "streaming" } else { "idle" }.to_string(),
            };
        }

        Panel::top("header").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Baudet");
                ui.separator();
                ui.label(self.status.clone());
                ui.separator();
                ui.label(format!("{} rows", self.rows.len()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // The one owner of "is the panel showing": this button and the flag. The panel
                    // itself never decides its own visibility, so the label can never lie about it.
                    if ui.selectable_label(self.settings_open, "Settings").clicked() {
                        self.settings_open = !self.settings_open;
                    }
                });
            });
        });

        Panel::bottom("composer").show(ui, |ui| {
            ui.horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::multiline(&mut self.input)
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
        });

        CentralPanel::default().show(ui, |ui| {
            // The body font is set once, in one place, rather than per row: 24 rows each nudging
            // the style would make the effective size depend on what was painted last.
            ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                if self.rows.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.label("No transcript yet. Type below and press Enter.");
                    });
                }
                for row in &self.rows {
                    paint_row(ui, row);
                }
            });
            ui.output_mut(|o| {
                o.cursor_icon = if self.busy { egui::CursorIcon::Progress } else { egui::CursorIcon::Default }
            });
        });

        // The settings panel floats over the transcript, painted last so it sits on top, and only
        // when open. It clears `settings_open` through the same flag the header button sets, so the
        // pane's own close button and the header can never disagree about whether it is showing.
        if self.settings_open {
            settings::panel(ui, &mut self.settings, &self.settings_tx, &mut self.settings_open);
        }
    }
}

impl TranscriptApp {
    /// How many frames a `--screenshot` run draws before asking for the picture, so panels, fonts
    /// and the first-frame layout work have all settled. A picture taken earlier shows a window
    /// that is still arranging itself, which reads as a broken app rather than as an early frame.
    /// Frames a screenshot run may wait for a settled transcript, not a fixed count to wait out.
    ///
    /// The picture is taken once the demo turn has drained (see `take_screenshot`), so this is only
    /// a ceiling for the case where the stream never closes. It is generous because a screenshot is
    /// never on a person's critical path. On the timeout the window is still written, as-is, with
    /// the unfinished turn in it, and the miss is named on stderr: an honest picture of a turn that
    /// is still running is worth more to whoever asked for the picture than no picture at all, and
    /// the reason the ceiling exists at all is to bound a stream that never closes rather than a
    /// model that answers slowly.
    const SHOT_FRAMES: u32 = 600;

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
                let _ = self.intents.send(Intent::Submit("Show me how the Rust rewrite renders a turn.".to_string()));
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
        self.paint(ui);
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
    intents: mpsc::Receiver<Intent>,
    snapshots: ampcc::UnboundedSender<(Vec<Row>, bool)>,
) {
    let publish = |state: &AppState, busy: bool| {
        let rows = state.rows().iter().map(Row::from).collect::<Vec<_>>();
        let _ = snapshots.send((rows, busy));
    };
    publish(&state, false);
    let draining = Arc::new(AtomicBool::new(false));
    while let Ok(intent) = intents.recv() {
        match intent {
            Intent::Quit => break,
            Intent::Submit(text) => {
                let Ok(sent) = state.send(&text) else {
                    publish(&state, state.live_turns() > 0);
                    continue;
                };
                publish(&state, true);
                let Some(handle) = sent.take_reader() else {
                    publish(&state, true);
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
                    publish(&state, true);
                    match intents.recv_timeout(std::time::Duration::from_millis(32)) {
                        Ok(Intent::Stop) => state.cancel_turn(),
                        Ok(Intent::Submit(_)) => {
                            // Baudet's rule: the live turn keeps the session. Refused, not queued.
                        }
                        Ok(Intent::Quit) => {
                            state.cancel_turn();
                            drain.join().ok();
                            publish(&state, false);
                            return;
                        }
                        Ok(Intent::Configure { .. }) => {
                            // Te laat voor de turn die loopt, en er is niets tussen wachend:
                            // de wachtlus hierboven pakt het op zodra deze drain ended is.
                        }
                        Err(_) => {}
                    }
                }
                drain.join().ok();
            }
            Intent::Stop => state.cancel_turn(),
            // Stamped before the next Submit can read them; a turn already running keeps the
            // request it already sent, which is why this is an intent and not a shared field.
            Intent::Configure { model, reasoning, max_output_tokens } => {
                state.set_request_choices(model, reasoning, max_output_tokens)
            }
        }
        publish(&state, state.live_turns() > 0);
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
    /// `Workspace::open` followed by the git probe `panels::left` asks for. A scripted picture needs
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
            } else if arg == "--endpoint" {
                match args.next() {
                    Some(value) => out.endpoint = Some(std::path::PathBuf::from(value)),
                    None => return Err(format!("{USAGE}\n--endpoint needs an address")),
                }
            } else if let Some(rest) = arg.strip_prefix("--endpoint=") {
                out.endpoint = Some(std::path::PathBuf::from(rest));
            } else {
                return Err(format!("{USAGE}\nunknown option: {arg}"));
            }
        }
        Ok(out)
    }
}

const USAGE: &str =
    "usage: baudet-ui [--screenshot=<file.png>] [--workspace=<folder>] [--settings] [--fetch-models] [--endpoint=<address>] [--live]";
const HELP: &str = "
Baudet - a local-first coding assistant

usage: baudet-ui [option]

  --screenshot <file>   draw the window, save a picture of it to <file> and exit,
  --screenshot=<file>   without taking focus from what you are doing
  --workspace <folder>  open <folder> in the workspace panel on startup
  --settings            open the settings panel on startup
  --fetch-models        on startup, ask the endpoint for its models (with --settings,
                        the picture waits for the listing so it cannot miss the fetch)
  --endpoint <address>  point the panel at <address> on startup, instead of the stored endpoint
  --endpoint=<address>  (needed with --fetch-models whenever the server is not the default)
  --live                send turns to the stored endpoint instead of the scripted player;
                        it needs a protocol in the settings file, which Scan endpoint writes
  -h, --help            show this text";

/// Start the window with `startup`'s arguments.
///
/// The window is 1040x720, which is the size the original opens at; the minimum is ours, sized so
/// that a transcript row and the composer both stay reachable on a small laptop screen.
pub fn run_from(startup: Startup) -> eframe::Result {
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
                app = app.with_settings_open();
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
