//! ```text
//!   paint pass -- Submit ------------------>  worker thread
//!        ^                                        |
//!        |                                        v
//!        +---- Rows (snapshot) <----- AppState + pump() over a ScriptedBackend
//! ```

use crate::layout::{self, resolve_divider, worth_remembering, Dividers};
use crate::diff_view::DiffView;
use crate::palette;
use crate::panels::{self, Changes, SidebarIntent, Workspace, NO_FOLDER, NO_READ};
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
use baudet_i18n::{Key, Texts};
use baudet_settings::Provider;
use eframe::egui;
use egui::{CentralPanel, ColorImage, Frame, Id, Panel, ScrollArea, UiBuilder};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use tokio::sync::mpsc as ampcc;

#[cfg(test)]
#[path = "live_tests.rs"]
mod live_tests;

enum Intent {
    Submit(String, Option<String>),
    UseCatalog(baudet_projects::Catalog),
    Compact,
    Stop,
    Quit,
    Configure(Option<Result<Arc<dyn Backend>, String>>, Box<baudet_settings::Settings>),
    LoadSession {
        id: String,
        project: Option<String>,
    },
    NewSession(Option<String>),
    LoadOlder,
}

struct Snapshot {
    rows: Vec<Row>,
    busy: bool,
    loading: bool,
    stored: bool,
    turns: (usize, usize),
    has_more_older: bool,
    reason: Option<String>,
    rejected_draft: Option<(String, Option<String>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Chat,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 2] = [Tab::Chat, Tab::Settings];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Chat => Texts::get(Key::TabChat),
            Tab::Settings => Texts::get(Key::TabSettings),
        }
    }
}

pub struct TranscriptApp {
    attachment: Option<String>,
    thinking_hidden: bool,
    help: Option<bool>,
    input: String,
    rows: Vec<Row>,
    busy: bool,
    status: String,
    intents: mpsc::Sender<Intent>,
    snapshots: ampcc::UnboundedReceiver<Snapshot>,
    approvals: Arc<crate::approval::Approvals>,
    worker: Option<std::thread::JoinHandle<()>>,
    shot: Option<std::path::PathBuf>,
    frames: u32,
    shot_done: bool,
    fetch_models_on_start: bool,
    shot_send: Option<String>,
    workspace: Workspace,
    changes: Changes,
    diff_view: Option<DiffView>,
    active_tab: Option<Tab>,
    fell_back: Option<String>,
    server_beurt: bool,
    settings: SettingsView,
    settings_tx: std::sync::mpsc::Sender<SettingsCommand>,
    settings_events: std::sync::mpsc::Receiver<SettingsEvent>,
    settings_worker: Option<std::thread::JoinHandle<()>>,
    sidebar: SidebarModel,
    current_project: Option<String>,
    current_session: Option<String>,
    data_root: Option<std::path::PathBuf>,
    loading: bool,
    more_older: bool,
    stick: bool,
    show_sidebar: bool,
    show_filetree: bool,
    dividers: Dividers,
    side_widths: Option<[f32; 2]>,
    min_window_width: f32,
    geometry_saved: bool,
}

impl TranscriptApp {
    pub fn new(backend: Arc<dyn baudet_backend::Backend>) -> TranscriptApp {
        Self::new_inner(backend, false)
    }

    fn new_inner(backend: Arc<dyn Backend>, live_settings: bool) -> TranscriptApp {
        let (intent_tx, intent_rx) = mpsc::channel::<Intent>();
        let (snap_tx, snap_rx) = ampcc::unbounded_channel();
        let state = Arc::new(AppState::new(backend, "You are Baudet's Rust rewrite."));
        let root = Some(baudet_store::data_dir());
        let worker_root = root.clone();
        let approvals = Arc::new(crate::approval::Approvals::default());
        let worker_approvals = Arc::clone(&approvals);
        let sidebar = SidebarModel::open_default();
        // The sidebar and worker must see the same permission grants and session records.
        let catalog = sidebar.catalog().clone();
        let (settings_tx, settings_rx) = mpsc::channel::<SettingsCommand>();
        let worker_settings = settings_tx.clone();
        let worker = std::thread::spawn(move || {
            worker_loop(state, worker_root, intent_rx, snap_tx, worker_approvals, catalog, worker_settings)
        });
        let (report_tx, settings_events) = mpsc::channel::<SettingsEvent>();
        let configure = live_settings.then(|| intent_tx.clone());
        let settings_worker = std::thread::spawn(move || {
            settings_worker_loop(settings_rx, report_tx, configure, baudet_settings::SettingsFile::default_location())
        });
        let mut settings = SettingsView::new();
        settings.load(&baudet_settings::SettingsFile::default_location());
        Texts::set_language(settings.language());
        let dividers = settings.dividers();
        TranscriptApp {
            attachment: None,
            thinking_hidden: false,
            help: None,
            input: String::new(),
            rows: Vec::new(),
            busy: false,
            status: "idle".to_string(),
            intents: intent_tx,
            snapshots: snap_rx,
            approvals,
            worker: Some(worker),
            shot: None,
            frames: 0,
            shot_done: false,
            fetch_models_on_start: false,
            shot_send: None,
            workspace: Workspace::default(),
            changes: Changes::default(),
            diff_view: None,
            active_tab: None,
            fell_back: None,
            server_beurt: false,
            settings,
            settings_tx,
            settings_events,
            settings_worker: Some(settings_worker),
            sidebar,
            current_project: None,
            current_session: None,
            data_root: root,
            loading: false,
            more_older: false,
            stick: true,
            show_sidebar: true,
            show_filetree: true,
            dividers,
            side_widths: None,
            min_window_width: 520.0,
            geometry_saved: false,
        }
    }

    pub fn with_screenshot(mut self, path: impl Into<std::path::PathBuf>) -> TranscriptApp {
        self.shot = Some(path.into());
        self
    }

    pub fn with_workspace(mut self, path: impl AsRef<std::path::Path>) -> TranscriptApp {
        self.workspace.open(&path.as_ref().to_string_lossy());
        self
    }

    pub fn with_tab(mut self, tab: Tab) -> TranscriptApp {
        self.active_tab = Some(tab);
        self
    }

    fn with_status(mut self, reason: String) -> TranscriptApp {
        self.status = reason.clone();
        self.fell_back = Some(reason);
        self.server_beurt = false;
        self
    }

    fn with_server(mut self, notice: String) -> TranscriptApp {
        self.status = notice;
        self.fell_back = None;
        self.server_beurt = true;
        self
    }

    fn with_demo(mut self, notice: String) -> TranscriptApp {
        self.status = notice.clone();
        self.fell_back = Some(notice);
        self.server_beurt = false;
        self
    }

    pub fn with_fetch_models(mut self) -> TranscriptApp {
        self.fetch_models_on_start = true;
        self
    }

    pub fn with_shot_send(mut self, question: impl Into<String>) -> TranscriptApp {
        self.shot_send = Some(question.into());
        self
    }

    pub fn with_endpoint(mut self, address: &str) -> TranscriptApp {
        self.settings.set_endpoint(address);
        self
    }

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

    fn save_geometry(&mut self, ctx: &egui::Context) {
        let stored = self.settings.dividers();
        let mut remembered = self.dividers;
        if let Some([left, right]) = self.side_widths {
            remembered.left = left.round() as i32;
            remembered.right_width = right.round() as i32;
        }
        remembered.files =
            worth_remembering(remembered.files as f32, ctx.viewport_rect().height(), stored.files);
        self.settings.set_dividers(&remembered);
        match self.settings.geometry_draft(&remembered) {
            Ok(draft) => {
                let _ = self.settings_tx.send(SettingsCommand::Save(draft));
            }
            Err(problem) => self.settings.report(problem),
        }
    }

    // Keep enough room for both visible side panels and the composer.
    fn update_min_window_width(&mut self, ctx: &egui::Context) {
        let Some([left, right]) = self.side_widths else { return };
        let minimum = layout::CENTER_MIN
            + if self.show_sidebar { left } else { 0.0 }
            + if self.show_filetree { right } else { 0.0 };
        let minimum = minimum.ceil().max(520.0);
        if minimum != self.min_window_width {
            self.min_window_width = minimum;
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(minimum, 360.0)));
            let viewport = ctx.viewport_rect();
            if viewport.width() < minimum {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(minimum, viewport.height())));
            }
        }
    }

    // Reserve the side panels before laying out the middle column.
    fn paint_layout(&mut self, ui: &mut egui::Ui) {
        let window_width = ui.available_width();
        let [mut left, mut right] = *self.side_widths.get_or_insert_with(|| {
            let left = resolve_divider(None, self.dividers.left, layout::LEFT_WIDTH);
            let right = if self.dividers.right_width > 0 {
                self.dividers.right_width as f32
            } else {
                window_width - resolve_divider(None, self.dividers.right, layout::CENTER_WIDTH)
            };
            [left.max(layout::LEFT_PANEL_MIN), right.round().max(layout::PANEL_MIN)]
        });

        // Lack of viewport space caps the drawn width without replacing the saved preference.
        let mut intents = if self.show_sidebar {
            let reserve_right = if self.show_filetree { right } else { 0.0 };
            let max = (window_width - reserve_right - layout::CENTER_MIN).max(layout::LEFT_PANEL_MIN);
            panels::project_sidebar(ui, &mut self.sidebar, &mut left, max)
        } else {
            Vec::new()
        };
        intents.extend(panels::ask_dialog(ui, &mut self.sidebar));
        self.apply_sidebar_intents(intents);
        if self.show_filetree {
            let max = (ui.available_width() - layout::CENTER_MIN).max(0.0);
            let (refresh, preview) =
                panels::file_tree(ui, &mut self.workspace, &self.changes, &mut self.dividers, &mut right, max);
            if refresh {
                self.changes.refresh(self.workspace.cwd());
            }
            if let Some(request) = preview {
                self.diff_view = Some(DiffView::open(ui.ctx(), request));
            }
        }
        self.side_widths = Some([left, right]);
        self.update_min_window_width(ui.ctx());

        while let Ok(event) = self.settings_events.try_recv() {
            if let SettingsEvent::ConnectionChanged(problem) = &event {
                self.server_beurt = problem.is_none();
                self.fell_back.clone_from(problem);
            }
            self.settings.apply_event(event);
        }

        let mut newest = None;
        while let Ok(mut snapshot) = self.snapshots.try_recv() {
            if let Some((mut text, image)) = snapshot.rejected_draft.take() {
                if !text.is_empty() && !self.input.is_empty() {
                    text.push('\n');
                }
                text.push_str(&self.input);
                self.input = text;
                if self.attachment.is_none() {
                    self.attachment = image;
                }
            }
            newest = Some(snapshot);
        }
        if let Some(report) = newest {
            self.loading = report.loading;
            self.more_older = report.has_more_older;
            self.busy = report.busy;
            if !report.rows.is_empty() || report.reason.is_none() {
                self.rows = report.rows;
            }
            self.status = match (&report.reason, &self.fell_back, report.busy, report.loading) {
                (Some(reason), _, _, _) => reason.clone(),
                (None, Some(reason), false, false) if !reason.is_empty() => reason.clone(),
                (None, _, true, _) => "streaming".to_string(),
                (None, _, _, true) => "loading session".to_string(),
                (None, _, _, false) => self.status_line(report.stored, report.turns),
            };
        }

        // egui paints the header separator on the parent. Clip it to the middle column.
        let clip = ui.clip_rect();
        ui.set_clip_rect(clip.intersect(ui.available_rect_before_wrap()));
        Panel::top("header").show(ui, |ui| {
            ui.horizontal(|ui| {
                let band = ui.available_rect_before_wrap();
                let root = root_ui_id(ui.ctx());
                let west = root.with((panels::PANEL_TOGGLES, "sidebar"));
                if ui.put(panels::toggle_rect(band, false), panels::Toggle::new(west, false)).clicked() {
                    self.show_sidebar = !self.show_sidebar;
                }
                ui.menu_button(Texts::get(Key::MenuHelp), |ui| {
                    if ui.button(Texts::get(Key::HelpGuide)).clicked() {
                        self.help = Some(false);
                        ui.close();
                    }
                    if ui.button(Texts::get(Key::AboutTitle)).clicked() {
                        self.help = Some(true);
                        ui.close();
                    }
                });
                let east = root.with((panels::PANEL_TOGGLES, "filetree"));
                if ui.put(panels::toggle_rect(band, true), panels::Toggle::new(east, true)).clicked() {
                    self.show_filetree = !self.show_filetree;
                }
                ui.heading("Baudet");
                ui.separator();
                ui.label(self.status.clone());
                ui.separator();
                ui.label(format!("{} rows", self.rows.len()));
            });
        });
        ui.set_clip_rect(clip);

        let tab = self.tab();
        if tab == Tab::Chat {
            Panel::bottom("composer").show(ui, |ui| self.composer(ui));
        }

        CentralPanel::default().show(ui, |ui| {
            Panel::top("tabs").show(ui, |ui| {
                let root = root_ui_id(ui.ctx());
                let tab_size = tab_size(ui);
                ui.style_mut().spacing.item_spacing.x = TAB_GAP;
                ui.horizontal_top(|ui| {
                    for candidate in Tab::ALL {
                        let slot = egui::Rect::from_min_size(ui.next_widget_position(), tab_size);
                        let clicked = ui
                            .scope_builder(
                                UiBuilder::new()
                                    .id(root.with((TABSTRIP, candidate.label())))
                                    .max_rect(slot)
                                    .layout(egui::Layout::top_down(egui::Align::Center)),
                                |ui| {
                                    ui.add_sized(
                                        tab_size,
                                        egui::Button::selectable(tab == candidate, candidate.label())
                                            .frame_when_inactive(true),
                                    )
                                    .clicked()
                                },
                            )
                            .inner;
                        if clicked {
                            self.active_tab = Some(candidate);
                        }
                    }
                });
            });
            let transcript = tab == Tab::Chat;
            ScrollArea::new([transcript && !self.settings.transcript_wrap(), true])
                .id_salt("center-scroll")
                .auto_shrink([false, false])
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
        if let Some(server) = self.approvals.show(ui.ctx()) {
            let _ = self.settings_tx.send(SettingsCommand::AllowMcp(server));
        }
        if let Some(view) = &mut self.diff_view {
            let (open, size) = view.show(ui.ctx(), self.settings.diff_window_size());
            if let Some(size) = size {
                self.settings.set_diff_window_size(size);
            }
            if !open {
                self.diff_view = None;
            }
        }
        self.show_help(ui.ctx());
    }
}

impl TranscriptApp {
    pub fn tab(&self) -> Tab {
        self.active_tab.unwrap_or(Tab::Chat)
    }

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

    pub fn frames(&self) -> u32 {
        self.frames
    }

    pub fn data_root(&self) -> Option<&std::path::Path> {
        self.data_root.as_deref()
    }

    pub fn workspace_folder(&self) -> Option<&str> {
        self.workspace.cwd()
    }

    fn apply_sidebar_intents(&mut self, intents: Vec<SidebarIntent>) {
        for intent in intents {
            match intent {
                SidebarIntent::OpenSession(id) => {
                    if self.busy || self.loading {
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
                    self.current_session = None;
                    let _ = self.intents.send(Intent::NewSession(self.current_project.clone()));
                    self.status = "removed from the catalogue".to_string();
                }
            }
        }
        self.current_project = match self.sidebar.selection() {
            Some(crate::sidebar::Selection::Project(id)) => Some(id.clone()),
            Some(crate::sidebar::Selection::Session { project, .. }) => project.clone(),
            _ => None,
        };
        match self.sidebar.selected_project() {
            Some(project) => match project.folder_path.as_deref().filter(|p| !p.trim().is_empty()) {
                None => self.workspace.show_message(NO_FOLDER),
                Some(folder) if project.is_granted(baudet_projects::FolderPermission::Read) => {
                    if self.workspace.cwd() != Some(folder) {
                        self.workspace.open(folder);
                    }
                }
                Some(_) => self.workspace.show_message(NO_READ),
            },
            None => self.workspace.show_message(NO_FOLDER),
        }
    }

    pub fn selected_project(&self) -> Option<&str> {
        self.current_project.as_deref()
    }

    pub fn selected_session(&self) -> Option<&str> {
        self.current_session.as_deref()
    }

    pub fn sidebar_visible(&self) -> bool {
        self.show_sidebar
    }

    pub fn filetree_visible(&self) -> bool {
        self.show_filetree
    }

    pub fn show_hidden_tree(&self) -> bool {
        self.workspace.show_hidden()
    }

    pub fn sidebar(&self) -> &SidebarModel {
        &self.sidebar
    }

    #[doc(hidden)]
    pub fn with_sidebar_model(mut self, model: SidebarModel) -> TranscriptApp {
        let _ = self.intents.send(Intent::UseCatalog(model.catalog().clone()));
        self.sidebar = model;
        self
    }

    #[doc(hidden)]
    pub fn workspace_message(&self) -> Option<&'static str> {
        self.workspace.message()
    }

    pub fn paint(&mut self, ui: &mut egui::Ui) {
        self.paint_layout(ui);
    }
}

pub const TABSTRIP: &str = "tabstrip";

pub const TAB_GAP: f32 = 10.0;

pub fn tab_size(ui: &egui::Ui) -> egui::Vec2 {
    let pad = ui.spacing().button_padding;
    let breedst = Tab::ALL
        .iter()
        .map(|candidate| {
            egui::WidgetText::from(candidate.label()).into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                egui::TextStyle::Button,
            )
        })
        .fold(0.0_f32, |breed, galley| breed.max(galley.size().x));
    egui::vec2(breedst + 2.0 * pad.x + 8.0, ui.spacing().interact_size.y.max(24.0))
}

pub const COMPOSER_FIELD: &str = "composer-field";

// Use the root address for toolbar controls and tests.
pub fn root_ui_id(ctx: &egui::Context) -> Id {
    Id::new((ctx.viewport_id(), "__top_ui"))
}

impl TranscriptApp {
    fn transcript(&self, ui: &mut egui::Ui) {
        if self.rows.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label("No transcript yet. Type below and press Enter.");
            });
        }
        for row in self.rows.iter().filter(|row| row.kind != baudet_core::ItemKind::Reasoning) {
            paint_row(ui, row, self.settings.transcript_wrap());
        }
    }

    fn show_help(&mut self, ctx: &egui::Context) {
        let Some(about) = self.help else { return };
        let (title, body) = if about {
            (Key::AboutTitle, Key::AboutBody)
        } else if self.tab() == Tab::Settings {
            (Key::HelpGuide, Key::HelpBodySettings)
        } else {
            (Key::HelpGuide, Key::HelpBodyChat)
        };
        let modal = egui::Modal::new(Id::new("baudet-help")).show(ctx, |ui| {
            ui.set_width(620.0_f32.min((ctx.content_rect().width() - 32.0).max(1.0)));
            ui.heading(Texts::get(title));
            if about {
                ui.label(Texts::fmt(Key::AboutVersion, &[&env!("CARGO_PKG_VERSION")]));
            }
            ScrollArea::vertical().max_height((ctx.content_rect().height() - 120.0).clamp(80.0, 490.0)).show(ui, |ui| {
                crate::markdown::show(ui, Texts::get(body), true);
            });
            ui.button(Texts::get(Key::CloseButton)).clicked()
        });
        if modal.inner || modal.should_close() {
            self.help = None;
        }
    }

    fn thinking_dock(&mut self, ui: &mut egui::Ui) {
        if self.thinking_hidden {
            return;
        }
        let thought: Vec<&str> = self.rows.iter().rev()
            .take_while(|row| row.kind != baudet_core::ItemKind::UserMessage)
            .filter(|row| row.kind == baudet_core::ItemKind::Reasoning && !row.body.is_empty())
            .map(|row| row.body.as_str()).collect();
        if thought.is_empty() {
            return;
        }
        ui.horizontal(|ui| {
            ui.strong(Texts::get(Key::ReasoningLabel));
            if ui.small_button(Texts::get(Key::CloseButton)).on_hover_text(Texts::get(Key::TipThinkingToggle)).clicked() {
                self.thinking_hidden = true;
            }
        });
        ScrollArea::vertical().id_salt("reasoning-dock").max_height(135.0).stick_to_bottom(true)
            .show(ui, |ui| { for text in thought.into_iter().rev() { crate::markdown::show(ui, text, true); } });
    }

    fn ask_for_older(&mut self, ui: &mut egui::Ui) {
        if self.busy || self.loading {
            return;
        }
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

    fn has_more_older(&self) -> bool {
        self.more_older
    }

    pub fn composer_enabled(&self) -> bool {
        self.server_beurt
    }

    pub fn header_status(&self) -> &str {
        &self.status
    }

    pub fn with_status_balk(mut self, balk: String) -> TranscriptApp {
        self.status = balk;
        self
    }

    fn composer(&mut self, ui: &mut egui::Ui) {
        self.thinking_dock(ui);
        if self.attachment.is_some() {
            ui.horizontal_wrapped(|ui| {
                ui.label(Texts::get(Key::ImageAttached));
                if ui.small_button(Texts::get(Key::ImageRemoveButton)).clicked() {
                    self.attachment = None;
                }
            });
        }
        let can_start = self.server_beurt && !self.busy && !self.loading;
        let (send_clicked, compact_clicked, stop_clicked) = ui
            .horizontal_wrapped(|ui| {
                let send = ui.add_enabled(can_start, egui::Button::new(Texts::get(Key::SendButton))).clicked();
                let compact = ui.add_enabled(can_start, egui::Button::new(Texts::get(Key::CompactButton))).clicked();
                let stop = ui.add_enabled(self.busy, egui::Button::new(Texts::get(Key::StopButton))).clicked();
                (send, compact, stop)
            })
            .inner;
        if compact_clicked {
            let _ = self.intents.send(Intent::Compact);
            self.loading = true;
            self.status = Texts::get(Key::Summarising).to_owned();
        }

        let response = ui.add_enabled(
            self.server_beurt,
            egui::TextEdit::multiline(&mut self.input)
                .id_salt(COMPOSER_FIELD)
                .hint_text("Ask something…  (Enter sends)")
                .desired_rows(2)
                .lock_focus(false)
                .desired_width(f32::INFINITY),
        );
        if self.settings.honesty_footer() {
            ui.add(egui::Label::new(egui::RichText::new(Texts::get(Key::LocalHonestyNote)).small().weak()).wrap());
        }
        let send_requested =
            ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift) && response.has_focus();
        if self.server_beurt
            && response.has_focus()
            && ui.input(|i| {
                ((i.modifiers.command || i.modifiers.ctrl) && i.key_pressed(egui::Key::V))
                    || i.events.iter().any(|event| matches!(event, egui::Event::Paste(_)))
            })
        {
            if let Some(image) = crate::clipboard::image_data_url() {
                self.attachment = Some(image);
                ui.request_repaint();
            }
        }
        if (send_clicked || send_requested)
            && can_start
            && !self.loading
            && (!self.input.trim().is_empty() || self.attachment.is_some())
        {
            let text = std::mem::take(&mut self.input);
            let _ = self.intents.send(Intent::Submit(text, self.attachment.take()));
            self.thinking_hidden = false;
            self.busy = true;
            self.status = "starting".to_string();
        }
        if stop_clicked {
            let _ = self.intents.send(Intent::Stop);
        }
    }
}

impl TranscriptApp {
    const SHOT_FRAMES: u32 = 20_000;

    fn take_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.shot.clone() else { return };
        if self.shot_done {
            return;
        }
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
        if self.shot.is_some() {
            ctx.request_repaint();
            if self.frames == 0 {
                let question = self
                    .shot_send
                    .clone()
                    .unwrap_or_else(|| "Show me how the Rust rewrite renders a turn.".to_string());
                let _ = self.intents.send(Intent::Submit(question, None));
                if self.fetch_models_on_start {
                    self.settings.request_fetch(&self.settings_tx);
                }
            }
            self.frames += 1;
            self.take_screenshot(&ctx);
        }
        ui.style_mut().spacing.item_spacing.y = 6.0;
        self.paint_layout(ui);
        // Save while the settings worker still accepts commands.
        if self.shot.is_none() && !self.geometry_saved && ctx.input(|input| input.viewport().close_requested()) {
            self.geometry_saved = true;
            self.save_geometry(&ctx);
        }
        if self.busy || self.loading {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }
}

impl Drop for TranscriptApp {
    fn drop(&mut self) {
        let _ = self.intents.send(Intent::Quit);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(worker) = self.settings_worker.take() {
            self.settings_tx = mpsc::channel::<SettingsCommand>().0;
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::*;

    #[test]
    fn the_composer_footer_follows_the_setting() {
        let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
        let folder = tempfile::tempdir().unwrap();
        let file = baudet_settings::SettingsFile::at(folder.path().join("settings.json"));
        let ctx = egui::Context::default();

        for enabled in [true, false] {
            let settings = baudet_settings::Settings { honesty_footer: enabled, ..Default::default() };
            file.write(&settings).unwrap();
            app.settings.load(&file);
            let output = ctx.run_ui(egui::RawInput::default(), |ui| app.composer(ui));
            let visible = output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == Texts::get(Key::LocalHonestyNote))
            });
            output.drop_without_applying_deltas();
            assert_eq!(visible, enabled);
        }
    }

    #[test]
    fn opgeslagen_breedtes_openen_op_dezelfde_maat_in_een_ander_venster() {
        let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
        let dir = tempfile::tempdir().unwrap();
        let file = baudet_settings::SettingsFile::at(dir.path().join("settings.json"));
        app.settings.load(&file);
        app.side_widths = Some([245.0, 365.0]);
        let (tx, rx) = mpsc::channel();
        app.settings_tx = tx;
        app.save_geometry(&egui::Context::default());
        let SettingsCommand::Save(settings) = rx.try_recv().unwrap() else {
            panic!("sluiten moet de paneelbreedtes opslaan");
        };
        file.write(&settings).unwrap();

        let mut reopened = TranscriptApp::new(TranscriptApp::demo_backend());
        reopened.settings.load(&file);
        reopened.dividers = reopened.settings.dividers();
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1500.0, 800.0))),
            ..Default::default()
        };
        ctx.run_ui(input, |ui| reopened.paint(ui)).drop_without_applying_deltas();
        for (id, width) in [("projects", 245.0), ("filetree", 365.0)] {
            let panel = egui::containers::PanelState::load(&ctx, egui::Id::new(id)).unwrap();
            assert_eq!(panel.size().x, width);
        }
    }
}

#[cfg(test)]
#[test]
fn the_worker_learns_from_the_endpoint_and_model_that_received_the_turn() {
    use baudet_backend::{BackendError, SendRequest, TurnHandle};

    struct BackendWithEndpoint(ScriptedBackend);

    impl Backend for BackendWithEndpoint {
        fn send(&self, request: SendRequest) -> Result<TurnHandle, BackendError> {
            self.0.send(request)
        }

        fn context_endpoint(&self) -> Option<(&str, &str)> {
            Some(("http://test-endpoint/v1", "default-model"))
        }
    }

    let script = Script::new().event(ThreadEvent::TurnCompleted {
        thread_id: "th_1".into(),
        turn_id: "tu_1".into(),
        response_id: None,
        usage: None,
        final_event: None,
        incomplete_reason: Some("Maximum context length is 8,192 tokens".into()),
    });
    let state = Arc::new(AppState::new(Arc::new(BackendWithEndpoint(ScriptedBackend::new(vec![script]))), "test"));
    state.set_request_choices(Some("chosen-model".into()), None, None);
    let folder = tempfile::tempdir().unwrap();
    let catalog = baudet_projects::Catalog::open(folder.path()).unwrap();
    let (intents, worker_intents) = mpsc::channel();
    let (snapshots, _reports) = ampcc::unbounded_channel();
    let (settings, commands) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        worker_loop(state, None, worker_intents, snapshots, Arc::default(), catalog, settings);
    });

    intents.send(Intent::Submit("question".into(), None)).unwrap();
    let learned = commands.recv_timeout(std::time::Duration::from_secs(3));
    intents.send(Intent::Quit).unwrap();
    worker.join().unwrap();
    assert!(matches!(
        learned,
        Ok(SettingsCommand::LearnContext { key, tokens: 8192 }) if key == "http://test-endpoint/v1|chosen-model"
    ));
}

// Only this worker starts a turn or switches sessions.
fn worker_loop(
    state: Arc<AppState>,
    root: Option<std::path::PathBuf>,
    intents: mpsc::Receiver<Intent>,
    snapshots: ampcc::UnboundedSender<Snapshot>,
    approvals: Arc<crate::approval::Approvals>,
    catalog: baudet_projects::Catalog,
    settings_tx: mpsc::Sender<SettingsCommand>,
) {
    let loading = Arc::new(AtomicBool::new(false));
    let settings_file = root.as_ref().map(|path| baudet_settings::SettingsFile::at(path.join("settings.json")))
        .unwrap_or_else(baudet_settings::SettingsFile::default_location);
    let mut connection_problem: Option<String> = None;
    let mut disk: Option<usize> = None;
    let mut catalog = Some(catalog);
    let publish = |state: &AppState, busy: bool, disk: Option<usize>, reason, rejected_draft| {
        let rows = state.rows().iter().map(Row::from).collect::<Vec<_>>();
        let on_screen = rows.len();
        let _ = snapshots.send(Snapshot {
            rows,
            busy,
            loading: loading.load(Ordering::SeqCst),
            stored: state.persists(),
            turns: (disk.unwrap_or(0), on_screen),
            has_more_older: state.paging().0,
            reason,
            rejected_draft,
        });
    };
    publish(&state, false, None, None, None);
    let mut learned_windows = std::collections::BTreeMap::new();
    let servers = Arc::new(std::sync::Mutex::new(baudet_tool::mcp::McpServers::default()));
    while let Ok(intent) = intents.recv() {
        if matches!(intent, Intent::LoadSession { .. }) {
            loading.store(true, Ordering::SeqCst);
        }
        match intent {
            Intent::Quit => break,
            Intent::UseCatalog(replacement) => catalog = Some(replacement),
            Intent::Compact => {
                use baudet_app::compaction::{CompactionOutcome, CompactionPlan};
                if let Some(reason) = &connection_problem {
                    publish(&state, false, disk, Some(reason.clone()), None);
                    continue;
                }
                let settings = settings_file.read();
                let plan = CompactionPlan::from(&settings.settings().context_limits());
                let reason = match state.compact(&plan) {
                    Ok(CompactionOutcome::NothingToFold) => Texts::get(Key::TooFewTurns).to_owned(),
                    Ok(CompactionOutcome::Compacted { summary, .. }) => {
                        Texts::fmt(Key::CompactionDone, &[&summary.chars().count()])
                    }
                    Ok(CompactionOutcome::OversizedTurn { max_chars }) => {
                        Texts::fmt(Key::CompactionOversizedTurn, &[&max_chars])
                    }
                    Ok(CompactionOutcome::SummaryRefused(reason)) => Texts::fmt(Key::CompactionRefused, &[&reason]),
                    Err(error) => Texts::fmt(Key::CompactionFailed, &[&error]),
                };
                let rows: Vec<Row> = state.rows().iter().map(Row::from).collect();
                let _ = snapshots.send(Snapshot {
                    turns: (disk.unwrap_or(0), rows.len()),
                    rows,
                    busy: false,
                    loading: false,
                    stored: state.persists(),
                    has_more_older: state.paging().0,
                    reason: Some(reason),
                    rejected_draft: None,
                });
            }
            Intent::Submit(text, image) => {
                if let Some(reason) = &connection_problem {
                    publish(&state, false, disk, Some(reason.clone()), Some((text, image)));
                    continue;
                }
                let current = settings_file.read();
                let mut settings = current.settings().clone();
                let context_key = state.context_key();
                if let Some(key) = context_key.as_ref() {
                    if let Some(&tokens) = learned_windows.get(key) {
                        settings.record_context_window(key.clone(), tokens);
                    }
                }
                let project = state.project().and_then(|id| catalog.as_ref()?.project(&id));
                let tool_project = project.filter(|project| AppState::tools_active(&settings, Some(project)));
                let handle = if tool_project.is_none() {
                    use baudet_app::session::SendError;
                    let sent = match state.send_with_context(&text, image.clone(), &settings) {
                        Ok(sent) => sent,
                        Err(SendError::ContextLimit) => {
                            publish(&state, false, disk, Some(Texts::get(Key::ContextTooLong).to_owned()), Some((text, image)));
                            continue;
                        }
                        Err(SendError::Busy(_)) => {
                            publish(&state, state.live_turns() > 0, disk, None, None);
                            continue;
                        }
                    };
                    sent.take_reader()
                } else {
                    None
                };
                publish(&state, true, disk, None, None);
                let folder = Arc::clone(&state);
                let watched = Arc::new(AtomicBool::new(true));
                let flag = Arc::clone(&watched);
                let cancelled = Arc::new(AtomicBool::new(false));
                let tool_cancelled = Arc::clone(&cancelled);
                let approvals = Arc::clone(&approvals);
                let servers = Arc::clone(&servers);
                let drain = std::thread::spawn(move || {
                    let failure = if let Some(project) = tool_project {
                        let approval_cancelled = Arc::clone(&tool_cancelled);
                        folder.run_tools(&text, image.as_deref(), baudet_app::session::tool_turn::ToolTurn {
                            project: &project,
                            settings: &settings,
                            servers: &mut servers.lock().unwrap(),
                            approval: Arc::new(move |request| approvals.request(request, &approval_cancelled)),
                            cancelled: tool_cancelled,
                        }).ok().flatten()
                    } else if let Some(handle) = handle {
                        let mut failure = None;
                        let (tx, _rx) = ampcc::unbounded_channel();
                        pump(&handle, |event| {
                            if let ThreadEvent::TurnCompleted { incomplete_reason, .. } = event {
                                failure.clone_from(incomplete_reason);
                            }
                            folder.apply(event)
                        }, &tx);
                        folder.finish();
                        failure
                    } else {
                        None
                    };
                    flag.store(false, Ordering::SeqCst);
                    failure
                });
                let mut pending_configuration = None;
                while watched.load(Ordering::SeqCst) {
                    publish(&state, true, disk, None, None);
                    match intents.recv_timeout(std::time::Duration::from_millis(32)) {
                        Ok(Intent::UseCatalog(replacement)) => catalog = Some(replacement),
                        Ok(Intent::Stop) => {
                            cancelled.store(true, Ordering::Relaxed);
                            state.cancel_turn();
                        }
                        Ok(Intent::Submit(..) | Intent::Compact) => {
                        }
                        Ok(Intent::Quit) => {
                            cancelled.store(true, Ordering::Relaxed);
                            state.cancel_turn();
                            drain.join().ok();
                            publish(&state, false, disk, None, None);
                            return;
                        }
                        Ok(Intent::Configure(backend, settings)) => {
                            let backend = backend.or_else(|| pending_configuration.take().and_then(|(backend, _)| backend));
                            pending_configuration = Some((backend, settings));
                        }
                        Ok(Intent::LoadSession { .. } | Intent::NewSession(_) | Intent::LoadOlder) => {
                        }
                        Err(_) => {}
                    }
                }
                let failure = drain.join().ok().flatten();
                disk = after_turn(&state, &catalog, &snapshots);
                if let Some((backend, settings)) = pending_configuration {
                    apply_configuration(&state, backend, *settings, &mut connection_problem);
                }
                if baudet_core::error::looks_like_context_overflow(failure.as_deref()) {
                    if let (Some(key), Some(tokens)) =
                        (context_key, baudet_core::error::claimed_context_tokens_of(failure.as_deref()))
                    {
                        learned_windows.insert(key.clone(), tokens);
                        let _ = settings_tx.send(SettingsCommand::LearnContext { key, tokens });
                    }
                    publish(&state, false, disk, Some(Texts::get(Key::ContextOverflow).to_owned()), None);
                    continue;
                }
            }
            Intent::Stop => state.cancel_turn(),
            Intent::Configure(backend, settings) => {
                apply_configuration(&state, backend, *settings, &mut connection_problem);
            }
            Intent::LoadSession { id, project } => {
                if !load_session(&state, root.as_deref(), &id, project.as_deref(), &loading, &snapshots) {
                    publish(&state, state.live_turns() > 0, disk, None, None);
                    continue;
                }
                disk = state.disk_items();
                publish(&state, state.live_turns() > 0, disk, None, None);
            }
            Intent::NewSession(project) => {
                state.switch_project(project.clone());
                match root.as_deref() {
                    Some(root) => {
                        let id = baudet_projects::new_session_id();
                        let model = state.model();
                        let opened = state.open_store(root, &id, &model, project.as_deref());
                        let _ = state.session_id();
                        if let Err(reason) = opened.map(|(transcript, session)| {
                            state.switch_to(transcript, session);
                        }) {
                            let _ = snapshots.send(Snapshot {
                                rows: Vec::new(),
                                busy: false,
                                loading: false,
                                stored: false,
                                turns: (0, 0),
                                has_more_older: state.paging().0,
                                reason: Some(reason),
                                rejected_draft: None,
                            });
                            continue;
                        }
                        if let Some(catalog) = catalog.as_ref() {
                            catalog.upsert_session(&id, Some(&model), project.as_deref(), 0).ok();
                        }
                    }
                    None => state.adopt(baudet_store::session::Session::new("", state.model())),
                }
                disk = state.disk_items();
            }
            Intent::LoadOlder => {
                let (has_more_older, cursor, _) = state.paging();
                if has_more_older {
                    if let Some(before) = cursor {
                        state.load_older(before).ok();
                    }
                }
            }
        }
        publish(&state, state.live_turns() > 0, disk, None, None);
    }
}

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
            rejected_draft: None,
        });
    }
    if let (Some(catalog), false) = (catalog.as_ref(), id.is_empty()) {
        let (_, turns) = state.transcript_len();
        catalog.upsert_session(&id, Some(&state.model()), state.project().as_deref(), turns as i32).ok();
    }
    state.disk_items()
}

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
        Err(reason) => {
            let _ = report.send(Snapshot {
                rows: Vec::new(),
                busy: false,
                loading: false,
                stored: state.persists(),
                turns: state.transcript_len(),
                has_more_older: state.paging().0,
                reason: Some(reason),
                rejected_draft: None,
            });
            false
        }
    }
}

fn apply_configuration(
    state: &AppState,
    backend: Option<Result<Arc<dyn Backend>, String>>,
    settings: baudet_settings::Settings,
    connection_problem: &mut Option<String>,
) {
    if let Some(backend) = backend {
        match backend {
            Ok(backend) => {
                state.replace_backend(backend);
                *connection_problem = None;
            }
            Err(reason) => *connection_problem = Some(reason),
        }
    }
    state.set_request_choices(settings.model, settings.reasoning, settings.max_output_tokens);
}

fn settings_worker_loop(
    commands: mpsc::Receiver<SettingsCommand>,
    report: mpsc::Sender<SettingsEvent>,
    configure: Option<mpsc::Sender<Intent>>,
    file: baudet_settings::SettingsFile,
) {
    while let Ok(command) = commands.recv() {
        match command {
            command @ (SettingsCommand::ApplyMcp { .. } | SettingsCommand::ClearMcpAllow | SettingsCommand::AllowMcp(_)) => {
                update_mcp(command, &report);
            }
            SettingsCommand::Fetch { endpoint, api_key } => {
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
            SettingsCommand::Save(mut settings) => {
                let current = file.read();
                // MCP settings have dedicated commands on this worker. Preserve their latest values.
                settings.mcp_servers = current.settings().mcp_servers.clone();
                settings.mcp_allow_servers = current.settings().mcp_allow_servers.clone();
                settings.learned_context_windows = current.settings().learned_context_windows.clone();
                let outcome = file.write(&settings);
                if outcome.is_ok() {
                    if let Some(configure) = &configure {
                        let old = current.settings();
                        let changed = endpoint_from_settings(old) != endpoint_from_settings(&settings)
                            || old.reasoning != settings.reasoning
                            || old.temperature != settings.temperature
                            || old.top_p != settings.top_p;
                        let backend = changed.then(|| {
                            endpoint_from_settings(&settings).and_then(|endpoint| configured_backend(&settings, endpoint))
                        });
                        let connection = backend.as_ref().map(|result| result.as_ref().err().cloned());
                        let _ = configure.send(Intent::Configure(backend, Box::new(settings)));
                        if let Some(problem) = connection {
                            let _ = report.send(SettingsEvent::ConnectionChanged(problem));
                        }
                    }
                }
                let _ = report.send(SettingsEvent::Saved(outcome));
            }
            SettingsCommand::LearnContext { key, tokens } => {
                let mut settings = file.read().settings().clone();
                if settings.learned_window(&key) != Some(tokens) {
                    settings.record_context_window(key.clone(), tokens);
                    if let Err(error) = file.write(&settings) {
                        let _ = report.send(SettingsEvent::Saved(Err(error)));
                        continue;
                    }
                }
                let _ = report.send(SettingsEvent::ContextLearned { key, tokens });
            }
            SettingsCommand::ApplyTrusted(lines) => {
                let mut kept: Vec<String> = Vec::new();
                let mut afgevallen = 0usize;
                for raw in &lines {
                    match baudet_tool::command_signature::trusted_entry(raw) {
                        None => afgevallen += 1,
                        Some(key) => {
                            if !kept.iter().any(|seen| seen == &key) {
                                kept.push(key);
                            }
                        }
                    }
                }
                mutate_and_save(&|settings| settings.trusted_commands = kept.clone(), afgevallen, &report);
            }
            SettingsCommand::ClearTrusted => mutate_and_save(&|settings| settings.trusted_commands.clear(), 0, &report),
            SettingsCommand::ClearAlways => {
                mutate_and_save(&|settings| settings.auto_allow_commands.clear(), 0, &report)
            }
        }
    }
}

fn update_mcp(command: SettingsCommand, report: &mpsc::Sender<SettingsEvent>) {
    use baudet_core::mcp::McpServerConfig;
    use std::collections::HashSet;

    let file = baudet_settings::SettingsFile::default_location();
    let mut settings = file.read().settings().clone();
    let mut refresh_servers = false;
    let status = match command {
        SettingsCommand::ApplyMcp { servers, allowed } => {
            refresh_servers = true;
            let mut ids = HashSet::new();
            let mut dropped = 0;
            settings.mcp_servers = servers.lines().map(str::trim).filter(|line| !line.is_empty()).filter_map(|line| {
                match serde_json::from_str::<McpServerConfig>(line) {
                    Ok(server) if server.valid() && ids.insert(server.id.clone()) => Some(server),
                    _ => { dropped += 1; None }
                }
            }).collect();
            let mut seen = HashSet::new();
            let mut ignored = 0;
            settings.mcp_allow_servers = allowed.lines().map(str::trim).filter(|line| !line.is_empty()).filter_map(|id| {
                if ids.contains(id) {
                    seen.insert(id).then(|| id.to_owned())
                } else {
                    ignored += 1;
                    None
                }
            }).collect();
            format!("{} · {}", Texts::fmt(Key::McpDropped, &[&dropped]), Texts::fmt(Key::McpAllowApplied, &[&ignored]))
        }
        SettingsCommand::ClearMcpAllow => {
            settings.mcp_allow_servers.clear();
            Texts::fmt(Key::McpAllowApplied, &[&0])
        }
        SettingsCommand::AllowMcp(id) => {
            if settings.mcp_servers.iter().any(|server| server.id == id) && !settings.mcp_allow_servers.contains(&id) {
                settings.mcp_allow_servers.push(id);
            }
            Texts::fmt(Key::McpAllowApplied, &[&0])
        }
        _ => unreachable!("only MCP commands are routed here"),
    };
    let outcome = file.write(&settings);
    let _ = report.send(SettingsEvent::McpUpdated {
        servers: settings.mcp_servers, allowed: settings.mcp_allow_servers, refresh_servers, status, outcome,
    });
}

fn mutate_and_save(
    change: &dyn Fn(&mut baudet_settings::Settings),
    dropped: usize,
    report: &mpsc::Sender<SettingsEvent>,
) {
    let file = baudet_settings::SettingsFile::default_location();
    let mut settings = file.read().settings().clone();
    change(&mut settings);
    let outcome = file.write(&settings);
    let _ = report.send(SettingsEvent::TrustedApplied {
        trusted: settings.trusted_commands,
        always: settings.auto_allow_commands,
        dropped,
        outcome,
    });
}

pub fn local_candidates() -> Vec<String> {
    [8000u16, 1234, 11434, 8080].iter().map(|port| format!("http://127.0.0.1:{port}/v1")).collect()
}

pub fn port_is_listening(endpoint: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(endpoint) else { return false };
    let Some(host) = url.host_str() else { return false };
    let port = url.port_or_known_default().unwrap_or(80);
    let address = format!("{host}:{port}");
    let Ok(address) = address.parse::<std::net::SocketAddr>() else {
        return false;
    };
    std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(250)).is_ok()
}

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
    let text = answer.text().unwrap_or_default();
    let value = serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
    baudet_backend::protocol::judge_probe(baudet_backend::protocol::Protocol::ChatCompletions, status, &value)
}

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
    let line =
        egui::Rect::from_min_max(egui::pos2(rect.min.x, rect.center().y), egui::pos2(rect.max.x, rect.center().y));
    ui.painter().line_segment([line.min, line.max], egui::Stroke::new(1.0, palette::BORDER));
    let span = (width / 2.0 - 6.0).max(0.0);
    let half = span * (1.0 - (time * 0.7).cos() as f32) / 2.0;
    let centre = egui::pos2(rect.min.x + 6.0 + half, rect.center().y);
    ui.painter().circle_filled(centre, 2.5, palette::ACCENT);
}

fn paint_row(ui: &mut egui::Ui, row: &Row, wrap: bool) {
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
        if let Some((id, image)) = &row.image {
            crate::clipboard::show_image(ui, *id, image);
            return;
        }
        if row.body.is_empty() {
            if row.in_progress() {
                progress_indicator(ui);
            }
            return;
        }
        if row.in_progress() {
            progress_indicator(ui);
        }
        if row.collapsed {
            ui.collapsing("details", |ui| {
                let label = egui::Label::new(egui::RichText::new(&row.body).monospace().size(12.0)).selectable(true);
                ui.add(if wrap { label.wrap() } else { label.extend() });
            });
        } else if row.kind == baudet_core::ItemKind::AgentMessage {
            crate::markdown::show(ui, &row.body, wrap);
        } else {
            let label = egui::Label::new(&row.body).selectable(true);
            ui.add(if wrap { label.wrap() } else { label.extend() });
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

pub fn run() -> eframe::Result {
    run_from(Startup::default())
}

#[derive(Debug, Clone, Default)]
pub struct Startup {
    pub screenshot: Option<std::path::PathBuf>,
    pub workspace: Option<std::path::PathBuf>,
    pub data_dir: Option<std::path::PathBuf>,
    pub settings: bool,
    pub fetch_models: bool,
    pub endpoint: Option<std::path::PathBuf>,
    pub demo: bool,
    pub send: Option<String>,
    pub discover: bool,
}

impl Startup {
    pub fn from_env() -> Result<Startup, String> {
        Startup::parse(std::env::args())
    }

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
            } else if arg == "--home" || arg.starts_with("--home=") {
                let path = match arg.strip_prefix("--home=") {
                    Some(path) => Some(path.to_owned()),
                    None => args.next().filter(|path| !path.starts_with("--")),
                };
                let path = path.filter(|path| !path.is_empty())
                    .ok_or_else(|| format!("{USAGE}\n--home needs a folder"))?;
                out.data_dir = Some(std::path::PathBuf::from(path));
            } else if arg == "--settings" {
                out.settings = true;
            } else if arg == "--fetch-models" {
                out.fetch_models = true;
            } else if arg == "--demo" {
                out.demo = true;
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
        Ok(out)    }
}

const USAGE: &str =
    "usage: baudet-ui [--screenshot=<file.png>] [--workspace=<folder>] [--home=<folder>] [--settings] [--fetch-models] [--endpoint=<address>] [--demo] [--send=<question>] [--discover]";
const HELP: &str = "
Baudet - a local-first coding assistant

usage: baudet-ui [option]

  --screenshot <file>   draw the window, save a picture of it to <file> and exit,
  --screenshot=<file>   without taking focus from what you are doing
  --workspace <folder>  open <folder> in the filetree panel on startup
  --home <folder>       store settings and conversations in <folder>, overriding BAUDET_HOME
  --settings            open the settings panel on startup
  --fetch-models        on startup, ask the endpoint for its models (with --settings,
                        the picture waits for the listing so it cannot miss the fetch)
  --endpoint <address>  point the panel at <address> on startup, instead of the stored endpoint
  --endpoint=<address>  (needed with --fetch-models whenever the server is not the default)
  --demo                answer from the scripted player instead of the stored endpoint. The
                        window talks to the stored endpoint by default; this flag is the
                        exception and exists for headless pictures, which must look the same
                        everywhere
  --send <question>     on startup, ask <question> (with --screenshot, the picture waits for
                        that turn to settle); a --demo window keeps its own fixed demo text
  --send=<question>
  --discover            list the local servers that answer a turn, say which dialect each
                        answers in, and exit without opening a window
  -h, --help            show this text";

pub fn run_from(startup: Startup) -> eframe::Result {
    if let Some(path) = startup.data_dir.clone() {
        baudet_store::set_data_dir(path).map_err(|error| eframe::Error::AppCreation(Box::new(error)))?;
    }
    if startup.discover {
        let key = baudet_settings::SettingsFile::default_location().read().settings().resolved_key();
        println!("{}", describe_discovery(&discover_local(key)));
        return Ok(());
    }
    let mut builder =
        egui::ViewportBuilder::default().with_inner_size([1040.0, 720.0]).with_min_inner_size([520.0, 360.0]);
    if startup.screenshot.is_some() {
        builder = builder.with_active(false);
    }
    #[cfg(target_os = "windows")]
    if startup.screenshot.is_some() {
        builder = builder.with_window_level(egui::WindowLevel::AlwaysOnBottom);
    }
    let mut options = eframe::NativeOptions {
        viewport: builder,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
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
    let demo = startup.demo;
    let shot_send = startup.send.clone();
    let (backend, fell_back) = chosen_backend(&startup);
    eframe::run_native(
        "Baudet",
        options,
        Box::new(move |cc| {
            palette::install(&cc.egui_ctx);
            let mut app = TranscriptApp::new_inner(backend, !demo);
            let balk = status_balk(
                startup.demo,
                fell_back.as_deref(),
                endpoint.as_deref().map(|address| address.to_string_lossy().into_owned()),
            );
            if screenshot.is_none() {
                eprintln!("baudet: {balk}");
            }
            app = app.with_status_balk(balk.clone());
            app = with_chosen_backend(app, startup.demo, fell_back);
            if let Some(path) = &workspace {
                app = app.with_workspace(path);
            }
            if settings {
                app = app.with_tab(Tab::Settings);
            }
            if let Some(address) = &endpoint {
                app = app.with_endpoint(&address.to_string_lossy());
            }
            if !demo {
                let file = baudet_settings::SettingsFile::default_location();
                let _ = app.intents.send(Intent::Configure(None, Box::new(file.read().settings().clone())));
            }
            if fetch_models {
                app = app.with_fetch_models();
            }
            if let Some(path) = &screenshot {
                app = app.with_screenshot(path);
                if !demo {
                    if let Some(question) = &shot_send {
                        app = app.with_shot_send(question.clone());
                    }
                }
            }
            Ok(Box::new(app))
        }),
    )
}

pub fn status_balk(demo: bool, fell_back: Option<&str>, endpoint: Option<String>) -> String {
    if demo {
        return "scripted player: asked for with --demo".to_string();
    }
    match fell_back {
        Some(reason) if !reason.is_empty() => reason.to_string(),
        _ => match endpoint {
            Some(address) if !address.is_empty() => format!("server: {address}"),
            _ => "server: the stored endpoint".to_string(),
        },
    }
}

pub fn with_chosen_backend(app: TranscriptApp, demo: bool, fell_back: Option<String>) -> TranscriptApp {
    if demo {
        app.with_demo("scripted player: asked for with --demo".to_string())
    } else {
        match fell_back {
            Some(reason) => app.with_status(reason),
            None => app.with_server(String::new()),
        }
    }
}

pub fn chosen_backend(startup: &Startup) -> (Arc<dyn Backend>, Option<String>) {
    if startup.demo {
        return (TranscriptApp::demo_backend(), None);
    }
    let file = baudet_settings::SettingsFile::default_location();
    let mut settings = file.read().settings().clone();
    match endpoint_from_settings(&settings) {
        Ok(endpoint) => return live_backend(&settings, endpoint, None),
        Err(reason) => {
            if settings.protocol_word().is_some() {
                return (TranscriptApp::demo_backend(), Some(format!("scripted player: {reason}")));
            }
        }
    }
    let address = settings.base_url.trim().to_string();
    let dialect = match scan_endpoint(&address, settings.resolved_key()) {
        Ok(outcome) => outcome.protocol.map(|word| word.word().to_string()),
        Err(_) => None,
    };
    let Some(word) = dialect else {
        return (
            TranscriptApp::demo_backend(),
            Some(format!("scripted player: no protocol and no server answered on {address}")),
        );
    };
    settings.protocol = Some(word.clone());
    let _ = file.write(&settings);
    match endpoint_from_settings(&settings) {
        Ok(endpoint) => live_backend(&settings, endpoint, None),
        Err(reason) => (TranscriptApp::demo_backend(), Some(format!("scripted player: {reason}"))),
    }
}

fn live_backend(
    settings: &baudet_settings::Settings,
    endpoint: Endpoint,
    notice: Option<String>,
) -> (Arc<dyn Backend>, Option<String>) {
    match configured_backend(settings, endpoint) {
        Ok(backend) => (backend, notice),
        Err(error) => (TranscriptApp::demo_backend(), Some(format!("scripted player: {error}"))),
    }
}

fn configured_backend(settings: &baudet_settings::Settings, endpoint: Endpoint) -> Result<Arc<dyn Backend>, String> {
    ResponsesBackend::with_sampling(endpoint, settings.reasoning.clone(), Some(settings.temperature), settings.top_p)
        .map(|backend| Arc::new(backend) as Arc<dyn Backend>)
        .map_err(|error| error.to_string())
}

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
        store: false,
    })
}

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
