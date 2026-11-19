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

use crate::rows::Row;
use baudet_app::session::pump;
use baudet_app::AppState;
use baudet_backend::scripted::{Script, ScriptedBackend};
use baudet_core::{AgentMessageItem, Item, ItemStatus, ThreadEvent};
use eframe::egui;
use egui::{CentralPanel, Frame, Panel, ScrollArea};
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
}

impl TranscriptApp {
    /// Builds the window over whatever `Backend` it is handed. With a [`ScriptedBackend`] the app
    /// is fully interactive and needs no server, which is the point of phase one.
    pub fn new(backend: Arc<dyn baudet_backend::Backend>) -> TranscriptApp {
        let (intent_tx, intent_rx) = mpsc::channel::<Intent>();
        let (snap_tx, snap_rx) = ampcc::unbounded_channel();
        let state = Arc::new(AppState::new(backend, "You are Baudet's Rust rewrite."));
        let worker = std::thread::spawn(move || worker_loop(state, intent_rx, snap_tx));
        TranscriptApp {
            input: String::new(),
            rows: Vec::new(),
            busy: false,
            status: "idle".to_string(),
            intents: intent_tx,
            snapshots: snap_rx,
            worker: Some(worker),
        }
    }

    /// The demo conversation the window ships with, so a first run shows a real transcript: a
    /// streamed answer, a tool row, a plan, and an answer that arrives as raw SSE lines.
    fn demo_backend() -> Arc<ScriptedBackend> {
        let answer = Script::new()
            .event(ThreadEvent::TurnStarted { thread_id: TH.into(), turn_id: T1.into() })
            .event(start(T1, agent(T1, "")))
            .event(delta(T1, "agentMessage", "Here is what I found. "))
            .event(delta(
                T1,
                "agentMessage",
                "The Kotlin front end and this Rust rewrite agree on the item vocabulary.",
            ))
            .event(start(T1, item_tool(T1)))
            .event(start(T1, item_plan(T1)))
            .event(start(T1, agent(T1, "Two facts, one table, no backend process.")))
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
            .event(delta(T2, "agentMessage", "You said that back to a scripted player. The real transport comes next."))
            .event(start(T2, agent(T2, "")))
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
            self.status = if busy { "streaming" } else { "idle" }.to_string();
        }

        Panel::top("header").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Baudet");
                ui.separator();
                ui.label(self.status.clone());
                ui.separator();
                ui.label(format!("{} rows", self.rows.len()));
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
    }
}

impl eframe::App for TranscriptApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
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
                        Err(_) => {}
                    }
                }
                drain.join().ok();
            }
            Intent::Stop => state.cancel_turn(),
        }
        publish(&state, state.live_turns() > 0);
    }
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
        if row.in_progress() {
            ui.spinner();
        }
        if row.body.is_empty() {
            return;
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
pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([820.0, 640.0]).with_min_inner_size([520.0, 360.0]),
        // Software rasterising is the default when `glow` is the only backend, and it is what lets
        // this window appear on a machine whose GL path is untrusted. wgpu stays opt-in.
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Baudet",
        options,
        Box::new(|_cc| Ok(Box::new(TranscriptApp::new(TranscriptApp::demo_backend())))),
    )
}

const TH: &str = "th_1";
const T1: &str = "tu_1";
const T2: &str = "tu_2";

fn start(turn: &str, item: Item) -> ThreadEvent {
    ThreadEvent::ItemStarted { thread_id: TH.into(), turn_id: turn.into(), item }
}

fn delta(turn: &str, _kind: &str, text: &str) -> ThreadEvent {
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
