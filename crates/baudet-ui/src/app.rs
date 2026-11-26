//! ```text
//!   paint pass -- Submit ------------------>  worker thread
//!        ^                                        |
//!        |                                        v
//!        +---- Rows (snapshot) <----- AppState + pump() over a ScriptedBackend
//! ```

use crate::layout::{self, worth_remembering, Dividers};
use crate::diff_view::DiffView;
use crate::folder_watch::FolderWatch;
use crate::palette;
use crate::panels::{self, Changes, SidebarIntent, Workspace, NO_FOLDER, NO_READ};
use crate::rows::Row;
use crate::settings::{self, SettingsCommand, SettingsEvent, SettingsView};
use crate::sidebar::SidebarModel;
use baudet_app::session::{pump, ContextStatus};
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
    LoadSession(String),
    CloseDeletedSession(String),
    DeleteSession { id: String, reply: mpsc::Sender<Result<DeletedConversation, String>> },
    LoadOlder,
}

struct DeletedConversation {
    id: String,
    project: Option<String>,
    was_active: bool,
    cleanup_warning: Option<String>,
}

struct Snapshot {
    rows: Vec<Row>,
    busy: bool,
    loading: bool,
    context: Option<ContextStatus>,
    has_more_older: bool,
    reason: Option<String>,
    rejected_draft: Option<(String, Option<String>)>,
    compaction: Option<String>,
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
    compaction: Option<String>,
    compacting: bool,
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
    folder_watch: Option<FolderWatch>,
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
    deletion: Option<mpsc::Receiver<Result<DeletedConversation, String>>>,
    data_root: Option<std::path::PathBuf>,
    loading: bool,
    more_older: bool,
    show_sidebar: bool,
    show_filetree: bool,
    dividers: Dividers,
    side_widths: Option<[f32; 2]>,
    geometry_saved: bool,
}

impl TranscriptApp {
    pub fn new(backend: Arc<dyn baudet_backend::Backend>) -> TranscriptApp {
        Self::new_inner(backend, false, false)
    }

    fn new_inner(backend: Arc<dyn Backend>, live_settings: bool, preview: bool) -> TranscriptApp {
        let app = Self::new_with_storage(
            backend, live_settings, preview, baudet_store::data_dir(), SidebarModel::open_default(),
        );
        Texts::set_language(app.settings.language());
        app
    }

    fn new_with_storage(
        backend: Arc<dyn Backend>,
        live_settings: bool,
        preview: bool,
        root: std::path::PathBuf,
        sidebar: SidebarModel,
    ) -> TranscriptApp {
        let (intent_tx, intent_rx) = mpsc::channel::<Intent>();
        let (snap_tx, snap_rx) = ampcc::unbounded_channel();
        let state = Arc::new(AppState::new(backend, baudet_settings::DEFAULT_SYSTEM_PROMPT));
        let settings_file = baudet_settings::SettingsFile::at(root.join("settings.json"));
        let root = Some(root);
        let worker_root = if preview { None } else { root.clone() };
        let approvals = Arc::new(crate::approval::Approvals::default());
        let worker_approvals = Arc::clone(&approvals);
        // The sidebar and worker must see the same permission grants and session records.
        let catalog = sidebar.catalog().clone();
        let (settings_tx, settings_rx) = mpsc::channel::<SettingsCommand>();
        let worker_settings = settings_tx.clone();
        let worker = std::thread::spawn(move || {
            worker_loop(state, worker_root, intent_rx, snap_tx, worker_approvals, catalog, worker_settings)
        });
        let (report_tx, settings_events) = mpsc::channel::<SettingsEvent>();
        let configure = live_settings.then(|| intent_tx.clone());
        let writable_settings = settings_file.clone();
        let settings_worker = std::thread::spawn(move || {
            settings_worker_loop(settings_rx, report_tx, configure, writable_settings)
        });
        let mut settings = SettingsView::new();
        settings.load(&settings_file);
        let dividers = settings.dividers();
        TranscriptApp {
            attachment: None,
            thinking_hidden: !settings.show_reasoning(),
            help: None,
            input: String::new(),
            rows: Vec::new(),
            busy: false,
            status: "idle".to_string(),
            compaction: None,
            compacting: false,
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
            folder_watch: None,
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
            deletion: None,
            data_root: root,
            loading: false,
            more_older: false,
            show_sidebar: true,
            show_filetree: true,
            dividers,
            side_widths: None,
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
            remembered.left = layout::encode_width(left);
            remembered.right_width = layout::encode_width(right);
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

    fn remember_chat_layout(&mut self) {
        self.settings.set_dividers(&self.dividers);
        let _ = self.settings_tx.send(SettingsCommand::SaveChatLayout {
            composer: self.dividers.composer,
            thinking: self.dividers.thinking,
        });
    }

    fn paint_layout(&mut self, ui: &mut egui::Ui) {
        let menu_frame = egui::Frame::side_top_panel(ui.style()).fill(palette::FIELD_ALT);
        Panel::top("menu-bar").frame(menu_frame).show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
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
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let messages = self.rows.iter().filter(|row| {
                        matches!(row.kind, baudet_core::ItemKind::UserMessage | baudet_core::ItemKind::AgentMessage)
                    }).count();
                    ui.label(egui::RichText::new(Texts::fmt(Key::MessagesCount, &[&messages])).size(16.0).strong());
                });
            });
        });
        let window_width = ui.available_width();
        let [mut left, mut right] = *self.side_widths.get_or_insert_with(|| {
            let left = layout::decode_width(self.dividers.left, layout::LEFT_WIDTH);
            let default_right = if self.dividers.right > 0 {
                (window_width - self.dividers.right as f32).max(0.0)
            } else {
                layout::RIGHT_WIDTH
            };
            let right = layout::decode_width(self.dividers.right_width, default_right);
            [left, right]
        });

        let budget = (window_width - layout::CENTER_MIN).max(0.0);
        let requested = if self.show_sidebar { left } else { 0.0 }
            + if self.show_filetree { right } else { 0.0 };
        let scale = if requested > budget { budget / requested } else { 1.0 };
        let visible_left = (left * scale).floor();
        let visible_right = (right * scale).floor();
        let mut intents = if self.show_sidebar {
            let max = budget - if self.show_filetree { visible_right } else { 0.0 };
            panels::project_sidebar(ui, &mut self.sidebar, &mut left, visible_left, max)
        } else {
            Vec::new()
        };
        intents.extend(panels::ask_dialog(ui, &mut self.sidebar));
        self.apply_sidebar_intents(intents);
        self.sync_folder_watch(ui.ctx());
        if self.show_filetree {
            let max = (ui.available_width() - layout::CENTER_MIN).max(0.0);
            let (refresh, preview) =
                panels::file_tree(ui, &mut self.workspace, &self.changes, &mut self.dividers, &mut right, visible_right, max);
            if refresh {
                if let Some(watch) = &self.folder_watch {
                    watch.refresh();
                }
            }
            if let Some(request) = preview {
                self.diff_view = Some(DiffView::open(ui.ctx(), request));
            }
        }
        self.side_widths = Some([left, right]);

        while let Ok(event) = self.settings_events.try_recv() {
            if let SettingsEvent::ConnectionChanged(problem) = &event {
                self.server_beurt = problem.is_none();
                self.fell_back.clone_from(problem);
            }
            self.settings.apply_event(event);
        }

        let mut newest = None;
        while let Ok(mut snapshot) = self.snapshots.try_recv() {
            if let Some(result) = snapshot.compaction.take() {
                self.compaction = Some(result);
                self.compacting = false;
            } else if snapshot.reason.is_some() && !self.compacting {
                self.compaction = None;
            }
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
            self.loading = report.loading || self.compacting;
            self.more_older = report.has_more_older;
            self.busy = report.busy;
            if !report.rows.is_empty() || report.reason.is_none() {
                self.rows = report.rows;
            }
            self.status = match (&report.reason, &self.fell_back, report.busy, report.loading) {
                (Some(reason), _, _, _) => reason.clone(),
                (None, Some(reason), false, false) if !reason.is_empty() => reason.clone(),
                (None, _, true, _) => Texts::get(Key::Streaming).to_owned(),
                (None, _, _, true) => Texts::get(Key::LoadingSession).to_owned(),
                (None, _, _, false) => report.context.as_ref().map_or_else(|| self.status.clone(), Self::status_line),
            };
        }
        self.poll_deletion();

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
                let east = root.with((panels::PANEL_TOGGLES, "filetree"));
                if ui.put(panels::toggle_rect(band, true), panels::Toggle::new(east, true)).clicked() {
                    self.show_filetree = !self.show_filetree;
                }
            });
        });
        ui.set_clip_rect(clip);

        let tab = self.tab();
        let needs_approval = self.approvals.is_pending();
        if tab == Tab::Chat {
            self.honesty_footer(ui);
            self.chat_status(ui);
            let reserve = if needs_approval { crate::approval::CARD_HEIGHT } else { 0.0 };
            let max = (ui.available_height() - 36.0 - reserve).max(0.0);
            let height = self.composer_height(max);
            Panel::bottom("composer").exact_size(height).show_separator_line(false)
                .show(ui, |ui| self.composer(ui, max));
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
            match tab {
                Tab::Chat => {
                    if needs_approval {
                        let granted = Panel::bottom("tool-approval").exact_size(crate::approval::CARD_HEIGHT)
                            .frame(egui::Frame::NONE).show(ui, |ui| self.approvals.show(ui)).inner;
                        if let Some(grant) = granted {
                            let command = match grant {
                                crate::approval::SavedGrant::Command(key) => SettingsCommand::AllowCommand(key),
                                crate::approval::SavedGrant::Mcp(server) => SettingsCommand::AllowMcp(server),
                            };
                            let _ = self.settings_tx.send(command);
                        }
                    }
                    ScrollArea::new([!self.settings.transcript_wrap(), true])
                        .id_salt("center-scroll")
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show(ui, |ui| self.transcript(ui));
                    self.ask_for_older(ui);
                }
                Tab::Settings => settings::body(ui, &mut self.settings, &self.settings_tx),
            }
            if self.busy {
                ui.output_mut(|o| {
                    if o.cursor_icon == egui::CursorIcon::Default {
                        o.cursor_icon = egui::CursorIcon::Progress;
                    }
                });
            }
        });
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

    fn status_line(status: &ContextStatus) -> String {
        let reading = status.reading;
        let compact = if !reading.trimming && reading.percent > 80 {
            Texts::get(Key::CompactRecommended)
        } else {
            ""
        };
        let context = if reading.budget == 0 {
            format!("~{} / ? tokens (?%){compact}", reading.used)
        } else {
            format!("~{} / {} tokens ({}%){compact}", reading.used, reading.budget, reading.percent)
        };
        if let Some(turn) = &status.last_turn {
            let id = turn.response_id.as_deref().unwrap_or("?");
            let tokens = |value: Option<i64>| value.map_or_else(|| "?".to_owned(), |value| value.to_string());
            let usage = turn.usage.unwrap_or_default();
            let event = Texts::fmt(Key::LastEvent, &[&turn.final_event.as_deref().unwrap_or("?")]);
            format!("id={id} · prompt={} · completion={} · {event} · {context}", tokens(usage.prompt), tokens(usage.completion))
        } else {
            let mode = status.server_session.as_deref().map_or_else(
                || Texts::get(Key::ClientReplay).to_owned(),
                |id| Texts::fmt(Key::ServerSession, &[&id]),
            );
            format!("{} · {context} · {mode}", Texts::fmt(Key::MessagesCount, &[&status.messages]))
        }
    }

    pub fn frames(&self) -> u32 {
        self.frames
    }

    pub fn data_root(&self) -> Option<&std::path::Path> {
        self.data_root.as_deref()
    }

    fn sync_folder_watch(&mut self, ctx: &egui::Context) {
        let root = self.workspace.cwd().filter(|_| self.show_filetree);
        if self.folder_watch.as_ref().map(FolderWatch::root) != root {
            self.folder_watch = root.map(|root| FolderWatch::start(root, ctx));
            self.changes = Changes::default();
        }
        if let Some(changes) = self.folder_watch.as_ref().and_then(FolderWatch::latest) {
            self.changes = changes;
        }
    }

    pub fn workspace_folder(&self) -> Option<&str> {
        self.workspace.cwd()
    }

    fn apply_sidebar_intents(&mut self, intents: Vec<SidebarIntent>) {
        for intent in intents {
            match intent {
                SidebarIntent::OpenSession(id) => {
                    if self.busy || self.loading || self.deletion.is_some() {
                        self.status = "busy — finish the turn first".to_string();
                        continue;
                    }
                    self.compaction = None;
                    self.current_session = Some(id.clone());
                    let _ = self.intents.send(Intent::LoadSession(id));
                    self.loading = true;
                    self.status = Texts::get(Key::LoadingSession).to_owned();
                }
                SidebarIntent::DeleteSession(id) => {
                    if self.deletion.is_none() {
                        if !self.compacting {
                            self.compaction = None;
                        }
                        let (reply, result) = mpsc::channel();
                        if self.intents.send(Intent::DeleteSession { id, reply }).is_ok() {
                            self.deletion = Some(result);
                            self.status = "deleting conversation".into();
                        } else {
                            self.status = "could not start conversation deletion".into();
                        }
                    }
                }
                SidebarIntent::ProjectDeleted => {
                    let Some(id) = self.current_session.as_deref() else { continue };
                    if self.sidebar.catalog().session_assignment(id).is_none() {
                        let _ = self.intents.send(Intent::CloseDeletedSession(id.to_owned()));
                        self.current_session = None;
                        self.compaction = None;
                    }
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

    fn poll_deletion(&mut self) {
        let Some(result) = self.deletion.as_ref().map(mpsc::Receiver::try_recv) else { return };
        let result = match result {
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("conversation deletion was interrupted".into()),
            Ok(result) => result,
        };
        self.deletion = None;
        match result {
            Ok(deleted) => {
                if self.sidebar.selected_session_id() == Some(&deleted.id) {
                    self.sidebar.select_project(deleted.project.as_deref());
                }
                if deleted.was_active {
                    self.current_session = None;
                    self.compaction = None;
                }
                self.status = deleted.cleanup_warning.unwrap_or_else(|| "conversation deleted".into());
            }
            Err(error) => self.status = format!("could not delete conversation: {error}"),
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
        if self.rows.is_empty() && self.current_session.is_none() && self.shot.is_none() {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label(Texts::get(Key::ChooseSession));
            });
        }
        for row in self.rows.iter().filter(|row| row.kind != baudet_core::ItemKind::Reasoning) {
            paint_row(ui, row, self.current_session.as_deref(), self.settings.transcript_wrap());
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

    fn reasoning_rows(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter().rev()
            .take_while(|row| row.kind != baudet_core::ItemKind::UserMessage)
            .filter(|row| row.kind == baudet_core::ItemKind::Reasoning && !row.body.is_empty())
    }

    fn reasoning_toggle(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let toggle = if self.thinking_hidden { Key::ShowReasoning } else { Key::HideReasoning };
        let button = ui.button(Texts::get(toggle)).on_hover_text(Texts::get(Key::TipThinkingToggle));
        if button.clicked() {
            self.thinking_hidden = !self.thinking_hidden;
            let show = !self.thinking_hidden;
            self.settings.set_show_reasoning(show);
            let _ = self.settings_tx.send(SettingsCommand::SaveReasoningVisibility(show));
            ui.request_repaint();
        }
        button
    }

    fn thinking_dock(&mut self, ui: &mut egui::Ui) {
        if self.thinking_hidden {
            return;
        }
        let thought: Vec<&str> = self.reasoning_rows().map(|row| row.body.as_str()).collect();
        if thought.is_empty() {
            return;
        }
        let separation = panels::SPLIT_GRIP_HEIGHT + 2.0 * ui.spacing().item_spacing.y;
        let max = (ui.available_height() - separation - 32.0).max(0.0);
        if max < 20.0 {
            return;
        }
        let height = layout::resolve_divider(None, self.dividers.thinking, 64.0).clamp(20.0, max);
        ScrollArea::vertical()
            .id_salt("reasoning-dock")
            .max_height(height)
            .min_scrolled_height(0.0)
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for text in thought.into_iter().rev() {
                    crate::markdown::show(ui, text, true);
                }
            });
        if let Some(delta) = panels::released_split(ui, "thinking-split", 20.0 - height..=max - height) {
            self.dividers.thinking = (height + delta).round() as i32;
            self.remember_chat_layout();
            ui.request_repaint();
        }
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

    fn start_compaction(&mut self) {
        self.compaction = Some(match self.intents.send(Intent::Compact) {
            Ok(()) => {
                self.loading = true;
                self.compacting = true;
                Texts::get(Key::Summarising).to_owned()
            }
            Err(error) => Texts::fmt(Key::CompactionFailed, &[&error]),
        });
    }

    fn composer_height(&self, max: f32) -> f32 {
        layout::resolve_divider(None, self.dividers.composer, 150.0).clamp(88.0_f32.min(max), max)
    }

    fn honesty_footer(&self, ui: &mut egui::Ui) {
        if self.settings.honesty_footer() {
            let text = Texts::get(Key::LocalHonestyNote);
            let font = egui::FontId::proportional(15.0);
            let frame = egui::Frame::side_top_panel(ui.style());
            let width = (ui.available_width() - frame.total_margin().sum().x).max(1.0);
            let galley = ui.painter().layout(text.to_owned(), font, palette::TEXT, width);
            Panel::bottom("honesty-footer").frame(frame).show_separator_line(false)
                .exact_size(galley.size().y + frame.total_margin().sum().y)
                .show(ui, |ui| {
                    ui.label(egui::RichText::new(text).size(15.0).color(palette::TEXT));
                });
        }
    }

    fn chat_status(&self, ui: &mut egui::Ui) {
        let busy = self.compacting || self.busy || self.loading || self.deletion.is_some();
        let waiting = self.approvals.is_pending();
        let message = if waiting {
            Texts::get(Key::ApprovalWaiting)
        } else if self.compacting || !busy {
            self.compaction.as_deref().unwrap_or(&self.status)
        } else {
            &self.status
        };
        Panel::bottom("chat-status").exact_size(28.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                if busy && !waiting {
                    ui.add(egui::Spinner::new().size(16.0).color(palette::ACCENT));
                }
                ui.add(egui::Label::new(message).truncate()).on_hover_text(message);
            });
        });
    }

    fn composer(&mut self, ui: &mut egui::Ui, max: f32) {
        let height = self.composer_height(max);
        let min = 88.0_f32.min(max);
        if let Some(delta) = panels::released_split(ui, "composer-split", height - max..=height - min) {
            self.dividers.composer = (height - delta).round() as i32;
            self.remember_chat_layout();
            ui.request_repaint();
        }
        if self.attachment.is_some() {
            ui.horizontal_wrapped(|ui| {
                ui.label(Texts::get(Key::ImageAttached));
                if ui.small_button(Texts::get(Key::ImageRemoveButton)).clicked() {
                    self.attachment = None;
                }
            });
        }
        let can_start = self.server_beurt && !self.busy && !self.loading && self.deletion.is_none();
        let (send_clicked, compact_clicked, stop_clicked) = ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(9.0, 8.0);
            let stop_text = Texts::get(Key::StopButton);
            let font = egui::TextStyle::Button.resolve(ui.style());
            let text_width = ui.painter().layout_no_wrap(stop_text.to_owned(), font, palette::TEXT).size().x;
            let stop_width = (text_width + 2.0 * ui.spacing().button_padding.x)
                .max(ui.spacing().interact_size.x);
            let left_width = (ui.available_width() - stop_width - ui.spacing().item_spacing.x).max(0.0);
            let (send, compact) = ui.allocate_ui_with_layout(
                egui::vec2(left_width, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(left_width);
                    ui.horizontal_wrapped(|ui| {
                        let send = ui.add_enabled(can_start, egui::Button::new(Texts::get(Key::SendButton))).clicked();
                        let compact = ui.add_enabled(can_start, egui::Button::new(Texts::get(Key::CompactButton))).clicked();
                        self.reasoning_toggle(ui);
                        (send, compact)
                    }).inner
                },
            ).inner;
            let stop = ui.add_enabled(self.busy, egui::Button::new(stop_text).min_size(egui::vec2(stop_width, 0.0))).clicked();
            (send, compact, stop)
        }).inner;
        if compact_clicked {
            self.start_compaction();
            ui.request_repaint();
        }

        self.thinking_dock(ui);
        let input_height = ui.available_height().max(1.0);
        let composer_id = ui.make_persistent_id(COMPOSER_FIELD);
        let send_requested = can_start
            && !self.loading
            && ui.memory(|memory| memory.has_focus(composer_id))
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Enter));
        let response = ScrollArea::vertical().id_salt("composer-scroll")
            .max_height(input_height).min_scrolled_height(0.0).auto_shrink([false, false]).show(ui, |ui| {
                ui.add_enabled(
                    self.server_beurt,
                    egui::TextEdit::multiline(&mut self.input)
                        .id(composer_id)
                        .hint_text("Ask something…  (Ctrl+Enter sends)")
                        .desired_rows(1)
                        .min_size(egui::vec2(0.0, (input_height - 8.0).max(0.0)))
                        .lock_focus(false)
                        .desired_width(f32::INFINITY),
                )
            }).inner;
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
            self.compaction = None;
            let text = std::mem::take(&mut self.input);
            let _ = self.intents.send(Intent::Submit(text, self.attachment.take()));
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
        if self.busy || self.loading || self.deletion.is_some() {
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

    fn isolated_app() -> (tempfile::TempDir, TranscriptApp) {
        let root = tempfile::tempdir().unwrap();
        let catalog = baudet_projects::Catalog::open(root.path()).unwrap();
        let app = TranscriptApp::new_with_storage(
            TranscriptApp::demo_backend(), false, false, root.path().to_owned(), SidebarModel::new(catalog),
        );
        (root, app)
    }

    #[test]
    fn streaming_follows_the_bottom_again_after_loading_older_messages() {
        let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
        let (_reports, snapshots) = ampcc::unbounded_channel();
        app.snapshots = snapshots;
        app.more_older = true;
        let (intents, requests) = mpsc::channel();
        app.intents = intents;
        let history = |start, end| {
            (start..end).map(|number| Row::from(&Item::UserMessage(baudet_core::UserMessageItem {
                created_at_ms: None,
                item_id: format!("history/{number}"),
                text: format!("History {number}"),
                status: ItemStatus::Completed,
            }))).collect::<Vec<_>>()
        };
        app.rows = history(10, 35);
        let ctx = egui::Context::default();
        let mut time = 0.0;
        let mut paint = |app: &mut TranscriptApp, wheel: f32| {
            time += 0.05;
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 620.0))),
                time: Some(time),
                ..Default::default()
            };
            if wheel != 0.0 {
                input.events = vec![
                    egui::Event::PointerMoved(egui::pos2(600.0, 240.0)),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point, phase: egui::TouchPhase::Move,
                        delta: egui::vec2(0.0, wheel), modifiers: egui::Modifiers::NONE,
                    },
                ];
            }
            let output = ctx.run_ui(input, |ui| app.paint(ui));
            let visible = output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some((text, shape.clip_rect)),
                _ => None,
            }).flat_map(|(text, clip)| {
                text.galley.rows.iter()
                    .filter(move |row| clip.contains_rect(row.rect().translate(text.pos.to_vec2())))
                    .map(|row| row.text())
            }).collect::<Vec<_>>();
            output.drop_without_applying_deltas();
            visible
        };
        let has = |labels: &[String], label| labels.iter().any(|text| text == label);
        paint(&mut app, 0.0);
        assert!(has(&paint(&mut app, 0.0), "History 34"));

        for _ in 0..4 {
            paint(&mut app, 4000.0);
        }
        assert!(matches!(requests.try_recv(), Ok(Intent::LoadOlder)));
        let mut older = history(0, 10);
        older.append(&mut app.rows);
        app.rows = older;
        app.loading = false;
        app.more_older = false;
        assert!(has(&paint(&mut app, 0.0), "History 0"));

        app.busy = true;
        app.rows.push(Row::from(&agent("streaming", "First streamed text")));
        let reading_history = paint(&mut app, 0.0);
        assert!(has(&reading_history, "History 0"));
        assert!(!has(&reading_history, "First streamed text"));

        for _ in 0..4 {
            paint(&mut app, -4000.0);
        }
        // Let the simulated wheel gesture settle before the next streamed update.
        for _ in 0..20 {
            paint(&mut app, 0.0);
        }
        assert!(has(&paint(&mut app, 0.0), "First streamed text"));
        let body = &mut app.rows.last_mut().unwrap().body;
        body.push_str(&"\n\nAnother streamed paragraph.".repeat(30));
        body.push_str("\n\nLatest streamed text");
        paint(&mut app, 0.0);
        let latest = paint(&mut app, 0.0);
        assert!(has(&latest, "Latest streamed text"), "{latest:?}");
    }

    #[test]
    fn transcript_details_click_independently_and_keep_session_state_separate() {
        let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
        app.current_session = Some("first-session".into());
        app.rows = [("first", "first tool output"), ("second", "second tool output")]
            .into_iter()
            .map(|(id, output)| {
                let mut row = Row::from(&item_tool(id));
                row.body = output.into();
                row
            })
            .collect();
        let ctx = egui::Context::default();
        palette::install(&ctx);
        ctx.global_style_mut(|style| style.animation_time = 0.0);
        let paint = |app: &TranscriptApp, events| {
            let input = egui::RawInput { events, ..Default::default() };
            let mut output = ctx.run_ui(input, |ui| app.transcript(ui));
            output.textures_delta.clear();
            output
        };
        let label = |shape: &egui::epaint::ClippedShape| match &shape.shape {
            egui::Shape::Text(text) => Some((text.galley.text().to_owned(), text.visual_bounding_rect().center())),
            _ => None,
        };
        let buttons = |output: &egui::FullOutput| {
            output.shapes.iter().filter_map(label).filter_map(|(text, pos)| (text == "Details").then_some(pos))
                .collect::<Vec<_>>()
        };
        let visible = |output: &egui::FullOutput| {
            output.shapes.iter().filter_map(label).map(|(text, _)| text)
                .filter(|text| text.ends_with("tool output")).collect::<Vec<_>>()
        };
        let click = |app: &TranscriptApp, pos| {
            for pressed in [true, false] {
                paint(app, vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                    },
                ]);
            }
            paint(app, Vec::new())
        };

        let initial = paint(&app, Vec::new());
        let positions = buttons(&initial);
        assert_eq!(positions.len(), 2);
        assert!(visible(&initial).is_empty());
        for pos in &positions {
            assert!(initial.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Rect(rect) if rect.fill == palette::BUTTON && rect.rect.contains(*pos)
            )));
        }

        let first_open = click(&app, positions[0]);
        assert_eq!(first_open.platform_output.cursor_icon, egui::CursorIcon::PointingHand);
        assert_eq!(visible(&first_open), ["first tool output"]);
        let both_open = click(&app, buttons(&first_open)[1]);
        assert_eq!(visible(&both_open), ["first tool output", "second tool output"]);
        let second_open = click(&app, buttons(&both_open)[0]);
        assert_eq!(visible(&second_open), ["second tool output"]);

        app.current_session = Some("second-session".into());
        assert!(visible(&paint(&app, Vec::new())).is_empty());
        app.current_session = Some("first-session".into());
        assert_eq!(visible(&paint(&app, Vec::new())), ["second tool output"]);
    }

    #[test]
    fn enter_inserts_a_newline_and_ctrl_enter_submits_the_draft_once() {
        let mut app = TranscriptApp::new(TranscriptApp::demo_backend()).with_server(String::new());
        app.thinking_hidden = true;
        let (tx, intents) = mpsc::channel();
        app.intents = tx;
        app.input = "first line".into();
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let id = ui.make_persistent_id(COMPOSER_FIELD);
            ui.memory_mut(|memory| memory.request_focus(id));
            app.composer(ui, 700.0);
        }).drop_without_applying_deltas();

        for modifiers in [egui::Modifiers::NONE, egui::Modifiers::CTRL] {
            let raw = egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
                ..Default::default()
            };
            ctx.run_ui(raw, |ui| app.composer(ui, 700.0)).drop_without_applying_deltas();
            if modifiers == egui::Modifiers::NONE {
                assert_eq!(app.input, "first line\n");
                assert!(matches!(intents.try_recv(), Err(mpsc::TryRecvError::Empty)));
            }
        }
        assert!(matches!(intents.try_recv(), Ok(Intent::Submit(text, None)) if text == "first line\n"));
        assert!(matches!(intents.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert!(app.input.is_empty());
        assert!(app.thinking_hidden);
    }

    #[test]
    fn reasoning_can_be_collapsed_and_reopened() {
        let (_root, mut app) = isolated_app();
        app.thinking_hidden = true;
        let (settings_tx, saved) = mpsc::channel();
        app.settings_tx = settings_tx;
        app.rows.push(Row::from(&Item::Reasoning(baudet_core::ReasoningItem {
            item_id: "test/reasoning".into(),
            text: "Thinking summary".into(),
            status: ItemStatus::Completed,
        })));
        let ctx = egui::Context::default();
        let mut paint = |events| {
            let raw = egui::RawInput { events, ..Default::default() };
            let mut pos = egui::Pos2::ZERO;
            let output = ctx.run_ui(raw, |ui| {
                pos = app.reasoning_toggle(ui).rect.center();
                app.thinking_dock(ui);
            });
            let visible = output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Thinking summary")
            });
            output.drop_without_applying_deltas();
            (pos, visible)
        };

        assert!(!paint(Vec::new()).1);
        for show in [true, false, true] {
            let (pos, _) = paint(Vec::new());
            for pressed in [true, false] {
                paint(vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        pressed,
                        button: egui::PointerButton::Primary,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]);
            }
            assert_eq!(paint(Vec::new()).1, show);
            assert!(matches!(saved.try_recv(), Ok(SettingsCommand::SaveReasoningVisibility(value)) if value == show));
        }
    }

    #[test]
    fn a_no_op_compaction_stays_visible_after_another_snapshot() {
        let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
        app.start_compaction();
        assert_eq!(app.compaction.as_deref(), Some(Texts::get(Key::Summarising)));
        app.intents.send(Intent::LoadOlder).unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while app.snapshots.len() < 3 {
            assert!(std::time::Instant::now() < deadline, "compaction did not publish its result");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0))),
            ..Default::default()
        };
        ctx.run_ui(input.clone(), |ui| app.paint(ui)).drop_without_applying_deltas();
        assert_eq!(app.compaction.as_deref(), Some(Texts::get(Key::TooFewTurns)));
        let output = ctx.run_ui(input, |ui| app.paint(ui));
        let visible = output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == Texts::get(Key::TooFewTurns))
        });
        output.drop_without_applying_deltas();
        assert!(visible, "the composer must show why nothing was compacted");
        assert!(!app.loading);
    }

    #[test]
    fn status_line_matches_kotlin_and_uses_live_turn_metadata() {
        let state = AppState::new(TranscriptApp::demo_backend(), "");
        let mut settings = baudet_settings::Settings {
            context_budget_tokens: 24_576,
            ..Default::default()
        };
        let context = state.context_status(&settings, false);
        assert_eq!(
            TranscriptApp::status_line(&context),
            "0 messages · ~0 / 24576 tokens (0%) · client-side replay"
        );
        let mut session = baudet_store::session::Session::new("fixture", "");
        session.messages.push(baudet_core::Msg::user("Explain this function."));
        session.previous_response_id = Some("resp_previous".into());
        state.adopt(session);
        let context = state.context_status(&settings, false);
        assert!(TranscriptApp::status_line(&context).starts_with("1 messages · ~"));
        assert!(context.reading.used > 0);
        settings.context_strategy = baudet_settings::config::ContextStrategy::StopAtLimit;
        settings.use_server_session = true;
        settings.use_responses_api = true;
        settings.store_responses = true;
        settings.protocol = Some("responses".into());
        assert!(TranscriptApp::status_line(&state.context_status(&settings, true)).ends_with("server session resp_previous"));
        assert!(TranscriptApp::status_line(&state.context_status(&settings, false)).ends_with("client-side replay"));

        state.apply(&ThreadEvent::TurnCompleted {
            thread_id: "th_1".into(), turn_id: "tu_1".into(), response_id: Some("resp_current".into()),
            usage: Some(baudet_core::Usage::new(Some(48), None, None)),
            final_event: Some("response.completed".into()), incomplete_reason: None,
        });
        let line = TranscriptApp::status_line(&state.context_status(&settings, true));
        assert!(line.starts_with("id=resp_current · prompt=48 · completion=? · last=response.completed · ~"));
        state.clear_session();
        assert!(state.context_status(&settings, true).last_turn.is_none());
    }

    #[test]
    fn compact_click_shows_progress_and_result_in_the_fixed_chat_status() {
        let mut app = TranscriptApp::new(TranscriptApp::demo_backend())
            .with_server("Ready for a local model".into());
        let (intents, submitted) = mpsc::channel();
        app.intents = intents;
        let (snapshots, updates) = ampcc::unbounded_channel();
        app.snapshots = updates;
        let ctx = egui::Context::default();
        let mut paint = |events| {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 360.0))),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| app.paint(ui));
            out.textures_delta.clear();
            out
        };
        let panel = |id| egui::containers::PanelState::load(&ctx, egui::Id::new(id)).unwrap().outer_rect;
        let text = |out: &egui::FullOutput, label: &str| {
            out.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => Some(text.visual_bounding_rect()),
                _ => None,
            }).expect("the chat text is visible")
        };

        let out = paint(Vec::new());
        let status = panel("chat-status");
        let composer = panel("composer");
        assert!(status.top() >= composer.bottom());
        assert!(status.contains_rect(text(&out, "Ready for a local model")));
        let pos = text(&out, Texts::get(Key::CompactButton)).center();
        for pressed in [true, false] {
            paint(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos, pressed, button: egui::PointerButton::Primary, modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        assert!(matches!(submitted.try_recv(), Ok(Intent::Compact)));
        let spinner = |out: &egui::FullOutput| {
            out.shapes.iter().any(|shape| matches!(
                &shape.shape,
                egui::Shape::Path(path) if path.stroke.width == 3.0 && path.points.iter().all(|&p| status.contains(p))
            ))
        };
        let out = paint(Vec::new());
        assert!(status.contains_rect(text(&out, Texts::get(Key::Summarising))));
        assert!(spinner(&out));

        let result = Texts::fmt(Key::CompactionDone, &[&364]);
        let snapshot = |busy, reason, compaction| Snapshot {
            rows: Vec::new(), busy, loading: false, context: None,
            has_more_older: false, reason, rejected_draft: None, compaction,
        };
        snapshots.send(snapshot(false, None, Some(result.clone()))).unwrap();
        let out = paint(Vec::new());
        assert!(status.contains_rect(text(&out, &result)));
        assert!(!spinner(&out));
        assert_eq!(panel("composer"), composer);

        snapshots.send(snapshot(true, None, None)).unwrap();
        let out = paint(Vec::new());
        assert!(status.contains_rect(text(&out, Texts::get(Key::Streaming))));
        assert!(spinner(&out));

        let error = "The local model server did not answer";
        snapshots.send(snapshot(false, Some(error.into()), None)).unwrap();
        let out = paint(Vec::new());
        assert!(status.contains_rect(text(&out, error)));
        assert!(!spinner(&out));
        assert_eq!(panel("composer"), composer);
    }

    #[test]
    fn command_approval_buttons_stay_visible_in_chat_and_save_the_exact_grant() {
        use baudet_core::{ApprovalDecision, ApprovalRequest};

        let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
        let (_updates, snapshots) = ampcc::unbounded_channel();
        app.snapshots = snapshots;
        let (settings_tx, saved) = mpsc::channel();
        app.settings_tx = settings_tx;
        let approvals = Arc::clone(&app.approvals);
        let pin = "fixture-exact-command";
        let (result, decision) = mpsc::channel();
        std::thread::spawn(move || {
            let outcome = approvals.request(ApprovalRequest::RunCommand {
                command: "git add -- README.md".into(),
                cwd: Some("/fixture/project".into()),
                timeout_seconds: Some(30),
                always_key: Some(pin.into()),
            }, &AtomicBool::new(false));
            let _ = result.send(outcome);
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !app.approvals.is_pending() {
            assert!(std::time::Instant::now() < deadline, "no pending approval reached the chat");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let ctx = egui::Context::default();
        palette::install(&ctx);
        let mut paint = |events| {
            let mut out = ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(650.0, 360.0))),
                events,
                ..Default::default()
            }, |ui| app.paint(ui));
            out.textures_delta.clear();
            out
        };
        paint(Vec::new());
        let out = paint(Vec::new());
        let card = egui::containers::PanelState::load(&ctx, egui::Id::new("tool-approval")).unwrap().outer_rect;
        let locate_button = |key| out.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == Texts::get(key) => {
                let rect = text.visual_bounding_rect();
                (card.contains_rect(rect) && shape.clip_rect.contains_rect(rect)).then_some(rect.center())
            }
            _ => None,
        });
        for key in [Key::AllowOnce, Key::AlwaysAllow, Key::DenyButton] {
            assert!(locate_button(key).is_some(), "the chat card must show {}", Texts::get(key));
        }
        assert!(matches!(decision.try_recv(), Err(mpsc::TryRecvError::Empty)));
        let pos = locate_button(Key::AlwaysAllow).unwrap();
        for pressed in [true, false] {
            paint(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
            }]);
        }
        assert_eq!(decision.recv_timeout(std::time::Duration::from_secs(1)).unwrap(), ApprovalDecision::AcceptForSession);
        assert!(matches!(saved.try_recv(), Ok(SettingsCommand::AllowCommand(key)) if key == pin));
    }

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
            let output = ctx.run_ui(egui::RawInput::default(), |ui| app.honesty_footer(ui));
            let visible = output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == Texts::get(Key::LocalHonestyNote))
            });
            output.drop_without_applying_deltas();
            assert_eq!(visible, enabled);
        }
    }

    #[test]
    fn composer_and_thinking_grips_resize_independently() {
        let (_root, mut app) = isolated_app();
        app.dividers = Dividers::default();
        app.thinking_hidden = false;
        app.rows.push(Row::from(&Item::Reasoning(baudet_core::ReasoningItem {
            item_id: "test/reasoning".into(),
            text: "Thinking summary".into(),
            status: ItemStatus::Completed,
        })));
        let ctx = egui::Context::default();
        let (settings_tx, saved) = mpsc::channel();
        app.settings_tx = settings_tx;
        for (grip, delta, expected) in [
            ("composer-split", -30.0, [180, 0]),
            ("thinking-split", 30.0, [180, 94]),
        ] {
            let before = [app.dividers.composer, app.dividers.thinking];
            let mut id = egui::Id::NULL;
            ctx.run_ui(egui::RawInput::default(), |ui| {
                id = ui.make_persistent_id(grip);
                app.composer(ui, 700.0);
            }).drop_without_applying_deltas();
            let start = ctx.read_response(id).unwrap().rect.center();
            let end = start + egui::vec2(0.0, delta);
            for (pos, pressed) in [(start, None), (start, Some(true)), (end, None), (end, Some(false))] {
                let mut events = vec![egui::Event::PointerMoved(pos)];
                if let Some(pressed) = pressed {
                    events.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                let raw = egui::RawInput { events, ..Default::default() };
                ctx.run_ui(raw, |ui| app.composer(ui, 700.0)).drop_without_applying_deltas();
                if pressed != Some(false) {
                    assert_eq!([app.dividers.composer, app.dividers.thinking], before);
                    assert!(matches!(saved.try_recv(), Err(mpsc::TryRecvError::Empty)));
                }
            }
            assert_eq!([app.dividers.composer, app.dividers.thinking], expected);
            assert!(matches!(
                saved.try_recv(),
                Ok(SettingsCommand::SaveChatLayout { composer, thinking }) if [composer, thinking] == expected
            ));
        }
    }

    #[test]
    fn reasoning_split_keeps_the_composer_visible_in_the_full_chat_layout() {
        let mut app = TranscriptApp::new(TranscriptApp::demo_backend()).with_server(String::new());
        app.thinking_hidden = false;
        let (_snapshot_tx, snapshots) = ampcc::unbounded_channel();
        app.snapshots = snapshots;
        app.dividers.composer = 150;
        app.rows.push(Row::from(&Item::Reasoning(baudet_core::ReasoningItem {
            item_id: "test/long-reasoning".into(),
            text: "A long reasoning block.\n\n".repeat(50),
            status: ItemStatus::Completed,
        })));
        let ctx = egui::Context::default();
        let (settings_tx, _) = mpsc::channel();
        app.settings_tx = settings_tx;
        let paint = |app: &mut TranscriptApp, height, events| {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, height))),
                events,
                ..Default::default()
            };
            ctx.run_ui(raw, |ui| app.paint(ui))
        };
        let controls = |out: &egui::FullOutput| {
            for label in [Texts::get(Key::SendButton), Texts::get(Key::CompactButton), Texts::get(Key::StopButton),
                "Ask something…  (Ctrl+Enter sends)"] {
                assert!(out.shapes.iter().any(|shape| matches!(
                    &shape.shape,
                    egui::Shape::Text(text) if text.galley.text() == label
                        && shape.clip_rect.contains_rect(text.visual_bounding_rect())
                )), "{label} is clipped in the full chat layout");
            }
            let button = |key| out.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == Texts::get(key) =>
                    Some(text.visual_bounding_rect()),
                _ => None,
            });
            let compact = button(Key::CompactButton).unwrap();
            let stop = button(Key::StopButton).unwrap();
            let reasoning = button(Key::HideReasoning).or_else(|| button(Key::ShowReasoning)).unwrap();
            assert!((compact.top() - reasoning.top()).abs() < 2.0);
            assert!(reasoning.left() > compact.right() + 8.0);
            assert!(stop.left() > reasoning.right());
        };
        let grips = |out: &egui::FullOutput| {
            let panel = egui::containers::PanelState::load(&ctx, egui::Id::new("composer")).unwrap().outer_rect;
            let mut grips: Vec<_> = out.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::Shape::LineSegment { points, stroke }
                    if stroke.color == palette::CHECK_BORDER && points[0].y == points[1].y
                        && panel.contains(points[0]) && panel.contains(points[1]) =>
                {
                    Some(points[0].lerp(points[1], 0.5))
                }
                _ => None,
            }).collect();
            grips.sort_by(|a, b| a.y.total_cmp(&b.y));
            grips
        };
        for height in [760.0, 360.0] {
            let out = paint(&mut app, height, Vec::new());
            controls(&out);
            let before = grips(&out);
            out.drop_without_applying_deltas();
            assert_eq!(before.len(), 2, "grips at height {height}: {before:?}");

            let start = before[1];
            let end = start - egui::vec2(0.0, 14.0);
            for (pos, pressed) in [(start, None), (start, Some(true)), (end, None), (end, Some(false))] {
                let mut events = vec![egui::Event::PointerMoved(pos)];
                if let Some(pressed) = pressed {
                    events.push(egui::Event::PointerButton {
                        pos, pressed, button: egui::PointerButton::Primary, modifiers: egui::Modifiers::NONE,
                    });
                }
                paint(&mut app, height, events).drop_without_applying_deltas();
            }
            let out = paint(&mut app, height, Vec::new());
            controls(&out);
            let after = grips(&out);
            out.drop_without_applying_deltas();
            assert_eq!(after.len(), 2);
            assert_eq!(after[0], before[0]);
            assert!(after[1].y < before[1].y, "inner grip should move up when the input grows");

            app.thinking_hidden = true;
            let out = paint(&mut app, height, Vec::new());
            controls(&out);
            assert_eq!(grips(&out), [before[0]]);
            out.drop_without_applying_deltas();
            app.thinking_hidden = false;
            app.dividers.thinking = 0;
        }
    }

    #[test]
    fn narrow_viewport_does_not_replace_the_chosen_panel_widths() {
        let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
        app.side_widths = Some([93.0, 107.0]);
        let ctx = egui::Context::default();
        for (width, expected) in [(300.0, [37.0, 42.0]), (1200.0, [93.0, 107.0])] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 360.0))),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| app.paint(ui));
            let widths = ["projects", "filetree"].map(|id| {
                egui::containers::PanelState::load(&ctx, egui::Id::new(id)).unwrap().size().x
            });
            assert_eq!(widths, expected);
            assert_eq!(app.side_widths, Some([93.0, 107.0]));
            assert!(!output.viewport_output.values().flat_map(|viewport| &viewport.commands).any(
                |command| matches!(command, egui::ViewportCommand::InnerSize(_) | egui::ViewportCommand::MinInnerSize(_))
            ));
            output.drop_without_applying_deltas();
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
    let folder = tempfile::tempdir().unwrap();
    let state = Arc::new(AppState::new(Arc::new(BackendWithEndpoint(ScriptedBackend::new(vec![script]))), "test")
        .with_persistence(folder.path(), "default-model").unwrap());
    state.set_request_choices(Some("chosen-model".into()), None, None);
    let catalog = baudet_projects::Catalog::open(folder.path()).unwrap();
    catalog.upsert_session(&state.session_id(), Some("default-model"), None, 0).unwrap();
    let root = folder.path().to_owned();
    let (intents, worker_intents) = mpsc::channel();
    let (snapshots, _reports) = ampcc::unbounded_channel();
    let (settings, commands) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        worker_loop(state, Some(root), worker_intents, snapshots, Arc::default(), catalog, settings);
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
    let mut catalog = Some(catalog);
    let publish = |state: &AppState, busy: bool, catalog: &Option<baudet_projects::Catalog>, reason, rejected_draft| {
        let rows = state.rows().iter().map(Row::from).collect::<Vec<_>>();
        let loading = loading.load(Ordering::SeqCst);
        let context = (!busy && !loading).then(|| {
            let current = settings_file.read();
            let settings = current.settings();
            let project = state.project().and_then(|id| catalog.as_ref()?.project(&id));
            state.context_status(settings, AppState::tools_active(settings, project.as_ref()))
        });
        let _ = snapshots.send(Snapshot {
            rows,
            busy,
            loading,
            context,
            has_more_older: state.paging().0,
            reason: if busy && state.is_auto_compacting() {
                Some(Texts::get(Key::AutoCompacting).to_owned())
            } else { reason },
            rejected_draft,
            compaction: None,
        });
    };
    publish(&state, false, &catalog, None, None);
    let mut learned_windows = std::collections::BTreeMap::new();
    let servers = Arc::new(std::sync::Mutex::new(baudet_tool::mcp::McpServers::default()));
    let mut deferred = std::collections::VecDeque::new();
    while let Some(intent) = deferred.pop_front().or_else(|| intents.recv().ok()) {
        if matches!(intent, Intent::LoadSession(_)) {
            loading.store(true, Ordering::SeqCst);
        }
        match intent {
            Intent::Quit => break,
            Intent::UseCatalog(replacement) => catalog = Some(replacement),
            Intent::Compact => {
                use baudet_app::compaction::{CompactionOutcome, CompactionPlan};
                let result = connection_problem.clone().unwrap_or_else(|| {
                    let settings = settings_file.read();
                    let plan = CompactionPlan::from(&settings.settings().context_limits());
                    match state.compact(&plan) {
                        Ok(CompactionOutcome::NothingToFold) => Texts::get(Key::TooFewTurns).to_owned(),
                        Ok(CompactionOutcome::Compacted { summary, .. }) => {
                            Texts::fmt(Key::CompactionDone, &[&summary.chars().count()])
                        }
                        Ok(CompactionOutcome::OversizedTurn { max_chars }) => {
                            Texts::fmt(Key::CompactionOversizedTurn, &[&max_chars])
                        }
                        Ok(CompactionOutcome::SummaryRefused(reason)) => Texts::fmt(Key::CompactionRefused, &[&reason]),
                        Err(error) => Texts::fmt(Key::CompactionFailed, &[&error]),
                    }
                });
                let rows: Vec<Row> = state.rows().iter().map(Row::from).collect();
                let _ = snapshots.send(Snapshot {
                    rows,
                    busy: false,
                    loading: false,
                    context: None,
                    has_more_older: state.paging().0,
                    reason: None,
                    rejected_draft: None,
                    compaction: Some(result),
                });
                continue;
            }
            Intent::Submit(text, image) => {
                if let Some(reason) = &connection_problem {
                    publish(&state, false, &catalog, Some(reason.clone()), Some((text, image)));
                    continue;
                }
                if root.is_some() && catalog.as_ref().and_then(|catalog| catalog.session_assignment(&state.session_id())).is_none() {
                    publish(&state, false, &catalog, Some(Texts::get(Key::ChooseSession).to_owned()), Some((text, image)));
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
                let tools_active = AppState::tools_active(&settings, project.as_ref());
                let auto_compact = settings.context_strategy == baudet_settings::config::ContextStrategy::AutoCompact;
                let handle = if !tools_active && !auto_compact {
                    use baudet_app::session::SendError;
                    let sent = match state.send_with_context(&text, image.clone(), &settings) {
                        Ok(sent) => sent,
                        Err(SendError::ContextLimit) => {
                            publish(&state, false, &catalog, Some(Texts::get(Key::ContextTooLong).to_owned()), Some((text, image)));
                            continue;
                        }
                        Err(SendError::Busy(_)) => {
                            publish(&state, state.live_turns() > 0, &catalog, None, None);
                            continue;
                        }
                    };
                    sent.take_reader()
                } else {
                    None
                };
                publish(&state, true, &catalog, None, None);
                let folder = Arc::clone(&state);
                let watched = Arc::new(AtomicBool::new(true));
                let flag = Arc::clone(&watched);
                let cancelled = Arc::new(AtomicBool::new(false));
                let tool_cancelled = Arc::clone(&cancelled);
                let approvals = Arc::clone(&approvals);
                let servers = Arc::clone(&servers);
                let drain = std::thread::spawn(move || {
                    let failure = if tools_active || auto_compact {
                        let approval_cancelled = Arc::clone(&tool_cancelled);
                        let run = baudet_app::session::tool_turn::ToolTurn {
                            project: project.as_ref(),
                            settings: &settings,
                            servers: &mut servers.lock().unwrap(),
                            approval: Arc::new(move |request| approvals.request(request, &approval_cancelled)),
                            cancelled: tool_cancelled,
                        };
                        if tools_active {
                            folder.run_tools(&text, image.as_deref(), run)
                        } else {
                            folder.run_auto_chat(&text, image.as_deref(), run)
                        }.ok().flatten()
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
                    publish(&state, true, &catalog, None, None);
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
                            publish(&state, false, &catalog, None, None);
                            return;
                        }
                        Ok(Intent::Configure(backend, settings)) => {
                            let backend = backend.or_else(|| pending_configuration.take().and_then(|(backend, _)| backend));
                            pending_configuration = Some((backend, settings));
                        }
                        Ok(intent @ (Intent::DeleteSession { .. } | Intent::CloseDeletedSession(_))) => deferred.push_back(intent),
                        Ok(Intent::LoadSession(_) | Intent::LoadOlder) => {
                        }
                        Err(_) => {}
                    }
                }
                let failure = drain.join().ok().flatten();
                after_turn(&state, &catalog, &snapshots);
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
                    publish(&state, false, &catalog, Some(Texts::get(Key::ContextOverflow).to_owned()), None);
                    continue;
                }
                publish(&state, false, &catalog, failure, None);
                continue;
            }
            Intent::Stop => state.cancel_turn(),
            Intent::Configure(backend, settings) => {
                apply_configuration(&state, backend, *settings, &mut connection_problem);
            }
            Intent::LoadSession(id) => {
                let Some(project) = catalog.as_ref().and_then(|catalog| catalog.session_assignment(&id)) else {
                    loading.store(false, Ordering::SeqCst);
                    publish(&state, false, &catalog, Some("conversation no longer exists in the catalogue".into()), None);
                    continue;
                };
                if !load_session(&state, root.as_deref(), &id, project.as_deref(), &loading, &snapshots) {
                    publish(&state, state.live_turns() > 0, &catalog, None, None);
                    continue;
                }
                publish(&state, state.live_turns() > 0, &catalog, None, None);
            }
            Intent::CloseDeletedSession(id) => {
                if state.session_id() == id {
                    state.switch_project(None);
                    state.clear_session();
                }
            }
            Intent::DeleteSession { id, reply } => {
                let result = (|| {
                    let root = root.as_deref().ok_or_else(|| "conversation storage unavailable".to_owned())?;
                    let catalog = catalog.as_ref().ok_or_else(|| "project catalogue unavailable".to_owned())?;
                    let project = catalog.project_of_session(&id);
                    let cleanup_warning = match catalog.delete_session(&id, root) {
                        Ok(()) => None,
                        Err(error @ baudet_projects::CatalogError::SessionCleanup(_)) => Some(error.to_string()),
                        Err(error) => return Err(error.to_string()),
                    };
                    let was_active = state.session_id() == id;
                    if was_active {
                        state.switch_project(project.clone());
                        state.clear_session();
                    }
                    Ok(DeletedConversation { id, project, was_active, cleanup_warning })
                })();
                publish(&state, false, &catalog, None, None);
                let _ = reply.send(result);
                continue;
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
        publish(&state, state.live_turns() > 0, &catalog, None, None);
    }
}

fn after_turn(
    state: &AppState,
    catalog: &Option<baudet_projects::Catalog>,
    report: &ampcc::UnboundedSender<Snapshot>,
) {
    let id = state.session_id();
    if let Some(reason) = state.persist() {
        let _ = report.send(Snapshot {
            rows: Vec::new(),
            busy: false,
            loading: false,
            context: None,
            has_more_older: state.paging().0,
            reason: Some(reason),
            rejected_draft: None,
            compaction: None,
        });
    }
    if let (Some(catalog), false) = (catalog.as_ref(), id.is_empty()) {
        let (_, turns) = state.transcript_len();
        catalog.update_session_progress(&id, &state.model(), turns as i32).ok();
    }
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
        Ok((transcript, mut session)) => {
            session.project_id = project.map(str::to_owned);
            state.switch_project(session.project_id.clone());
            state.switch_to(transcript, session);
            true
        }
        Err(reason) => {
            let _ = report.send(Snapshot {
                rows: Vec::new(),
                busy: false,
                loading: false,
                context: None,
                has_more_older: state.paging().0,
                reason: Some(reason),
                rejected_draft: None,
                compaction: None,
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
                    outcome.map(|o| o.protocols).map_err(|problem| problem.to_string()),
                    endpoint,
                ));
            }
            SettingsCommand::SaveReasoningVisibility(show) => {
                let current = file.read();
                let mut settings = current.settings().clone();
                settings.show_reasoning = show;
                if let Err(error) = file.write(&settings) {
                    let _ = report.send(SettingsEvent::Saved(Err(error)));
                }
            }
            SettingsCommand::SaveChatLayout { composer, thinking } => {
                let current = file.read();
                let mut settings = current.settings().clone();
                settings.composer_height = composer;
                settings.thinking_height = thinking;
                if let Err(error) = file.write(&settings) {
                    let _ = report.send(SettingsEvent::Saved(Err(error)));
                }
            }
            SettingsCommand::Save(mut settings) => {
                let current = file.read();
                // MCP settings have dedicated commands on this worker. Preserve their latest values.
                settings.mcp_servers = current.settings().mcp_servers.clone();
                settings.mcp_allow_servers = current.settings().mcp_allow_servers.clone();
                settings.auto_allow_commands = current.settings().auto_allow_commands.clone();
                settings.show_reasoning = current.settings().show_reasoning;
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
            SettingsCommand::AllowCommand(key) => mutate_and_save(&|settings| {
                if !settings.auto_allow_commands.contains(&key) {
                    settings.auto_allow_commands.push(key.clone());
                }
            }, 0, &report),
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
            |dialect, _, body| probe_dialect(&provider, dialect, body),
        ),
        Err(error) => baudet_backend::protocol::ScanOutcome {
            endpoint: endpoint.to_string(),
            protocols: Vec::new(),
            models: Vec::new(),
            notes: vec![format!("not a usable address: {error}")],
        },
    };
    baudet_backend::protocol::discover(&candidates, port_is_listening, scan_one)
}

pub fn describe_discovery(discovery: &baudet_backend::protocol::Discovery) -> String {
    let names = |protocols: &[Protocol]| protocols.iter().map(Protocol::word).collect::<Vec<_>>().join(", ");
    let mut lines = Vec::new();
    for outcome in &discovery.tried {
        let verdict = if outcome.usable() {
            format!("answers {}", names(&outcome.protocols))
        } else {
            "no".to_string()
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
            names(&found.protocols),
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
        |dialect, _, body| probe_dialect(&provider, dialect, body),
    ))
}

fn probe_dialect(
    provider: &baudet_settings::Provider,
    dialect: Protocol,
    body: &serde_json::Map<String, serde_json::Value>,
) -> (Option<baudet_backend::protocol::Protocol>, String) {
    let url = format!("{}{}", provider.base_url(), dialect.path());
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
    baudet_backend::protocol::judge_probe(dialect, status, &value)
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

fn paint_row(ui: &mut egui::Ui, row: &Row, session: Option<&str>, wrap: bool) {
    let label = kind_label(row);
    let frame = Frame::new().fill(row_background(ui, row)).corner_radius(6.0).inner_margin(8.0);
    frame.show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(label).strong().size(11.0));
            if let Some(time) = row.message_timestamp.and_then(chrono::DateTime::from_timestamp_millis) {
                let local = time.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string();
                ui.label(egui::RichText::new(local).weak().size(11.0));
            }
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
            egui::CollapsingHeader::new("Details")
                .id_salt(("row-details", session, &row.item_id))
                .show_background(true)
                .icon(panels::compact_chevron)
                .show(ui, |ui| {
                    let label = egui::Label::new(egui::RichText::new(&row.body).monospace().size(12.0)).selectable(true);
                    ui.add(if wrap { label.wrap() } else { label.extend() });
                })
                .header_response.on_hover_cursor(egui::CursorIcon::PointingHand);
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
    let icon = image::load_from_memory_with_format(include_bytes!("../assets/baudet.png"), image::ImageFormat::Png)
        .expect("valid embedded Baudet icon")
        .into_rgba8();
    let (width, height) = icon.dimensions();
    let mut builder = egui::ViewportBuilder::default()
        .with_app_id("dev.baudet.desktop")
        .with_icon(egui::IconData { rgba: icon.into_raw(), width, height })
        .with_inner_size([1040.0, 720.0])
        .with_min_inner_size([0.0, 360.0]);
    if startup.screenshot.is_some() {
        builder = builder.with_active(false);
    }
    #[cfg(target_os = "windows")]
    if startup.screenshot.is_some() {
        builder = builder.with_window_level(egui::WindowLevel::AlwaysOnBottom);
    }
    let options = eframe::NativeOptions {
        viewport: builder,
        renderer: eframe::Renderer::Glow,
        #[cfg(target_os = "macos")]
        event_loop_builder: if startup.screenshot.is_some() {
            use winit::platform::macos::EventLoopBuilderExtMacOS as _;
            Some(Box::new(|builder| {
                builder.with_activate_ignoring_other_apps(false);
            }))
        } else {
            None
        },
        ..Default::default()
    };
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
            let mut app = TranscriptApp::new_inner(backend, !demo, screenshot.is_some());
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
        Ok(outcome) => Protocol::preferred(&outcome.protocols, settings.use_responses_api).map(|protocol| protocol.word().to_string()),
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
        thinking_template: settings.thinking_template,
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
        created_at_ms: None,
        item_id: format!("{turn}/msg"),
        text: text.into(),
        status: ItemStatus::InProgress,
    })
}

fn finish(turn: &str, text: &str) -> Item {
    Item::AgentMessage(AgentMessageItem {
        created_at_ms: None,
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
