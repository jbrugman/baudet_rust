
use crate::app::root_ui_id;
use crate::layout::{self};
use crate::palette;
use crate::sidebar::{Confirm, NameAsk, PermissionState, Selection, SidebarModel, UNASSIGNED};
use baudet_core::{probe_changes, probe_git, ChangeKind, ChangedFile, GitMeta};
use baudet_i18n::{Key, Texts};
use egui::{Color32, Panel, ScrollArea, Ui};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const IGNORED_DIRS: &[&str] = &["target", "node_modules", "build", "dist", "out"];

pub const NO_FOLDER: &str = "Select a project with a linked folder to see its files.";

pub const NO_READ: &str = "Read access is not granted for this project's folder.";

const MAX_DEPTH: usize = 4;

const MAX_ENTRIES_PER_DIR: usize = 200;

const KEEP_TAIL: usize = 22;

#[derive(Default)]
pub struct Workspace {
    cwd: String,
    message: Option<&'static str>,
    show_hidden: bool,

    probed: bool,
}

pub struct DirListing {
    pub dirs: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
    pub truncated: bool,
}

impl DirListing {
    fn unreadable() -> DirListing {
        DirListing { dirs: Vec::new(), files: Vec::new(), truncated: false }
    }
}

impl Workspace {
    pub fn open(&mut self, path: &str) {
        self.cwd = path.trim().to_string();
        self.probed = false;
        self.message = None;
    }

    pub fn show_message(&mut self, text: &'static str) {
        self.cwd.clear();
        self.probed = false;
        self.message = Some(text);
    }

    pub fn message(&self) -> Option<&'static str> {
        self.message
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    pub fn set_show_hidden(&mut self, show: bool) {
        self.show_hidden = show;
    }

    pub fn cwd(&self) -> Option<&str> {
        (!self.cwd.is_empty()).then_some(self.cwd.as_str())
    }

    pub fn tree_at(&mut self, dir: &Path) -> DirListing {
        let Ok(read_dir) = std::fs::read_dir(dir) else { return DirListing::unreadable() };
        let mut names: Vec<(String, PathBuf, bool)> = Vec::new();
        for entry in read_dir.flatten() {
            let Ok(file_type) = entry.file_type() else { continue };
            let is_dir = file_type.is_dir() && !file_type.is_symlink();
            let name = entry.file_name().to_string_lossy().into_owned();
            if (!self.show_hidden && name.starts_with('.')) || (is_dir && IGNORED_DIRS.contains(&name.as_str())) {
                continue;
            }
            names.push((name, entry.path(), is_dir));
        }
        names.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        let truncated = names.len() > MAX_ENTRIES_PER_DIR;
        if truncated {
            names.truncate(MAX_ENTRIES_PER_DIR);
        }
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for (_, path, is_dir) in names {
            if is_dir {
                dirs.push(path);
            } else {
                files.push(path);
            }
        }
        DirListing { dirs, files, truncated }
    }
}

#[derive(Default)]
pub struct Changes {
    meta: Option<GitMeta>,
    paths: Vec<ChangedFile>,
    unavailable: bool,
}

impl Changes {
    pub fn refresh(&mut self, path: Option<&str>) {
        self.meta = None;
        self.paths.clear();
        self.unavailable = false;
        let Some(cwd) = path else { return };
        let Some(meta) = probe_git(cwd) else { return };
        match probe_changes(cwd) {
            Some(paths) => {
                self.meta = Some(meta);
                self.paths = paths;
            }
            None => self.unavailable = true,
        }
    }

    pub fn meta(&self) -> Option<&GitMeta> {
        self.meta.as_ref()
    }

    pub fn paths(&self) -> &[ChangedFile] {
        &self.paths
    }
}

fn file_tree_controls(
    ui: &mut Ui,
    workspace: &mut Workspace,
    meta: Option<&GitMeta>,
    changes: &[ChangedFile],
    actions: &mut FileRowActions,
) -> bool {
    let mut requested: Option<String> = None;
    {
        let root = crate::app::root_ui_id(ui.ctx());
        let refresh_clicked = ui
            .scope_builder(
                egui::UiBuilder::new().id(root.with((TREE_REFRESH, "refresh"))).sense(egui::Sense::click()),
                |ui| {
                    ui.add(egui::Button::new(Texts::get(Key::FileTreeRefreshButton)).sense(egui::Sense::hover()))
                        .on_hover_text(Texts::get(Key::TipFileTreeRefresh));
                },
            )
            .response
            .clicked();
        let mut hidden = workspace.show_hidden();
        ui.horizontal(|ui| {
            if ui.checkbox(&mut hidden, Texts::get(Key::FileTreeHidden)).changed() {
                workspace.set_show_hidden(hidden);
            }
        });
        if refresh_clicked && workspace.cwd().is_some() {
            requested = workspace.cwd().map(str::to_string);
        }
        ui.separator();
        let chosen = workspace.cwd().map(|s| s.to_string());
        let status = FileStatus::build(chosen.as_deref(), changes);
        match &chosen {
            None => {
                let text = workspace.message().unwrap_or(NO_FOLDER);
                ui.label(dim(text));
            }
            Some(cwd) => {
                let root = PathBuf::from(cwd);
                ui.label(folder_name(cwd)).on_hover_text(cwd.clone());
                if let Some(meta) = meta {
                    branch_line(ui, meta);
                }
                ui.add(egui::Separator::default().spacing(0.0));
                ScrollArea::both().min_scrolled_height(0.0).auto_shrink([false, false]).show(ui, |ui| {
                    if !root.is_dir() {
                        ui.label(dim(NO_READ));
                        return;
                    }
                    egui::CollapsingHeader::new(folder_name(cwd))
                        .id_salt(cwd.clone())
                        .default_open(true)
                        .show(ui, |ui| tree(ui, workspace, &root, 1, &status, &root, actions));
                });
            }
        }
    }
    if requested.is_some() {
        workspace.open(requested.as_deref().unwrap().trim());
    }
    if workspace.cwd().is_some() && !workspace.probed {
        workspace.probed = true;
        return true;
    }
    false
}


pub const SIDEBAR_ADD: &str = "sidebar-add-project";

pub const ASK_OK: &str = "sidebar-ask-ok";
pub const CONFIRM_YES: &str = "sidebar-confirm-yes";

pub const PERMISSIONS_TITLE: &str = "Folder permissions";
pub const FOLDER_LABEL: &str = "folder:";
pub const FOLDER_NONE: &str = "no folder linked";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarIntent {
    OpenSession(String),
    StartChat(Option<String>),
    DeleteSession(String),
    Deleted,
}

// Isolate content measurement from the width owned by the resize handle.
fn side_panel(
    ui: &mut Ui,
    panel: Panel,
    id: &str,
    width: &mut f32,
    min: f32,
    max: f32,
    body: impl FnOnce(&mut Ui),
) {
    let max = max.min(ui.available_width()).floor().max(0.0);
    let visible = width.min(max);
    let id = egui::Id::new(id);
    if let Some(mut state) = egui::containers::PanelState::load(ui.ctx(), id) {
        if (state.size().x - visible).abs() > 0.01 {
            state.outer_rect.max.x = state.outer_rect.min.x + visible;
            ui.ctx().data_mut(|data| data.insert_persisted(id, state));
        }
    }
    let response = panel
        .resizable(true)
        .default_size(visible)
        .size_range(min.min(max)..=max)
        .frame(egui::Frame::side_top_panel(ui.style()))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            let mut content = ui.new_child(egui::UiBuilder::new().id_salt("content"));
            content.set_clip_rect(ui.clip_rect().intersect(content.max_rect()));
            body(&mut content);
        });
    // A temporary viewport cap must not change the chosen width.
    if *width <= max {
        *width = response.response.rect.width().round();
    }
}

pub fn project_sidebar(ui: &mut Ui, model: &mut SidebarModel, width: &mut f32, max: f32) -> Vec<SidebarIntent> {
    let mut intents = Vec::new();
    side_panel(ui, Panel::left("projects"), "projects", width, layout::LEFT_PANEL_MIN, max, |ui| {
        let root = crate::app::root_ui_id(ui.ctx());
        if ui
            .scope_builder(
                egui::UiBuilder::new().id(root.with((SIDEBAR_ADD, "add"))).sense(egui::Sense::click()),
                |ui| {
                    ui.add(egui::Button::new(Texts::get(Key::AddProjectButton)).sense(egui::Sense::hover()))
                        .on_hover_text(Texts::get(Key::TipAdd));
                },
            )
            .response
            .clicked()
        {
            model.cancel_confirm();
            model.start_name(NameAsk::NewProject, None);
        }

        // Reserve space for project permissions before allocating the scrolling tree.
        let footer_id = egui::Id::new("project-details");
        let previous = egui::containers::PanelState::load(ui.ctx(), footer_id);
        let footer = Panel::bottom(footer_id).frame(egui::Frame::NONE).show(ui, |ui| {
            match model.permission_state() {
                None => {
                    ui.horizontal(|ui| {
                        ui.label(FOLDER_LABEL);
                        ui.label(dim(FOLDER_NONE));
                    });
                }
                Some(state) => {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.strong(Texts::get(Key::FolderPermissionsTitle));
                        ScrollArea::horizontal().id_salt("project-folder").show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(FOLDER_LABEL);
                                ui.add(egui::Label::new(state.folder.as_deref().unwrap_or(FOLDER_NONE)).extend());
                            });
                        });
                        if let Some(capability) = &state.disk_capability {
                            ui.label(dim(capability.clone()));
                        }
                        permission_boxes(ui, &state, model);
                    });
                }
            }
        });
        if previous.map(|state| state.size().y) != Some(footer.response.rect.height()) {
            ui.ctx().request_repaint();
        }

        ui.add(egui::Separator::default().spacing(0.0));

        ScrollArea::both().id_salt("project-tree").min_scrolled_height(0.0).auto_shrink([false, false]).show(ui, |ui| {
            let projects = model.catalog().list_projects();
            for project in &projects {
                let sessions = model.catalog().sessions_in(Some(&project.id));
                let response = egui::CollapsingHeader::new(project_label(&project.name, project.archived))
                    .id_salt(project.id.clone())
                    .default_open(true)
                    .show(ui, |ui| {
                        for session in &sessions {
                            session_row(ui, model, Some(project.id.clone()), session, &mut intents);
                        }
                    });
                if response.header_response.clicked() {
                    model.select_project(Some(&project.id));
                    intents.push(SidebarIntent::StartChat(Some(project.id.clone())));
                }
                row_menu(ui, &response.header_response, model, Some(project.id.clone()), None, &mut intents);
            }

            let inbox = model.catalog().sessions_in(None);
            let inbox_label = if model.selection() == Some(&Selection::Unassigned) {
                egui::RichText::new(Texts::get(Key::InboxNode)).strong()
            } else {
                egui::RichText::new(Texts::get(Key::InboxNode))
            };
            let response =
                egui::CollapsingHeader::new(inbox_label).id_salt(UNASSIGNED).default_open(true).show(ui, |ui| {
                    for session in &inbox {
                        session_row(ui, model, None, session, &mut intents);
                    }
                });
            if response.header_response.clicked() && !inbox.is_empty() {
                model.select_project(None);
                intents.push(SidebarIntent::StartChat(None));
            }
            row_menu(ui, &response.header_response, model, None, None, &mut intents);
        });
    });
    intents
}

fn session_row(
    ui: &mut Ui,
    model: &mut SidebarModel,
    project: Option<String>,
    session: &baudet_projects::SessionSummary,
    intents: &mut Vec<SidebarIntent>,
) {
    let selected = model.selected_session_id() == Some(session.id.as_str());
    let row = ui
        .push_id(session.id.clone(), |ui| {
            let row =
                ui.add(egui::Button::new(truncate_head(&session.title, KEEP_TAIL)).frame(false).selected(selected));
            row.on_hover_text(session.title.clone())
        })
        .inner;
    if row.clicked() {
        model.select_session(&session.id);
        intents.push(SidebarIntent::OpenSession(session.id.clone()));
    }
    row_menu(ui, &row, model, project, Some(session.id.clone()), intents);
}

fn permission_boxes(ui: &mut Ui, state: &PermissionState, model: &mut SidebarModel) {
    let mut read = state.read;
    let mut write = state.write;
    let mut execute = state.execute;
    ui.horizontal_wrapped(|ui| {
        let previous = ui.style().visuals.widgets.clone();
        let style = ui.style_mut();
        for widget in
            [&mut style.visuals.widgets.inactive, &mut style.visuals.widgets.hovered, &mut style.visuals.widgets.active]
        {
            widget.bg_fill = palette::CHECK;
            widget.weak_bg_fill = palette::CHECK;
        }
        style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, palette::CHECK_BORDER);
        style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, palette::ACCENT);
        style.visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, palette::ACCENT);
        ui.add_enabled_ui(state.checkbox_enabled, |ui| {
            ui.checkbox(&mut read, Texts::get(Key::PermRead));
            ui.checkbox(&mut write, Texts::get(Key::PermWrite));
            ui.checkbox(&mut execute, Texts::get(Key::PermExecute));
        });
        ui.style_mut().visuals.widgets = previous;
    });
    if (read, write, execute) != (state.read, state.write, state.execute) {
        let _ = model.set_permissions(read, write, execute);
    }
}

pub fn ask_dialog(ui: &mut Ui, model: &mut SidebarModel) -> Vec<SidebarIntent> {
    let mut intents = Vec::new();
    if model.ask_name.is_none() && model.confirm.is_none() {
        return intents;
    }
    let (hoek, afmeting) = vraag_vensterhoek(ui.ctx());
    egui::Window::new(Texts::get(ask_title(model)))
        .id(egui::Id::new("ask-dialog"))
        .current_pos(hoek)
        .default_size(afmeting)
        .movable(false)
        .resizable(false)
        .collapsible(false)
        .title_bar(false)
        .auto_sized()
        .frame(egui::Frame::popup(ui.style()))
        .show(ui.ctx(), |ui| {
            ask_and_confirm(ui, model, &mut intents);
        });
    intents
}

fn ask_title(model: &SidebarModel) -> Key {
    match &model.confirm {
        Some(Confirm::DeleteProject(..)) => Key::DeleteProjectButton,
        Some(Confirm::DeleteSession(..)) => Key::DeleteConversationButton,
        None => match &model.ask_name {
            Some(ask) => ask.prompt_key(),
            None => Key::ProjectNamePrompt,
        },
    }
}

pub fn vraag_vensterhoek(ctx: &egui::Context) -> (egui::Pos2, egui::Vec2) {
    let afmeting = egui::vec2(280.0, 76.0);
    let vlak = ctx.viewport_rect();
    let links = vlak.min.x;
    let rechts = (vlak.max.x - layout::PANEL_MIN).max(links);
    let x = (links + (rechts - links) / 2.0 - afmeting.x / 2.0).clamp(links, rechts);
    (egui::pos2(x, vlak.center().y - afmeting.y / 2.0), afmeting)
}

const VRAAG_VELD_BREEDTE: f32 = 90.0;

fn ask_and_confirm(ui: &mut Ui, model: &mut SidebarModel, intents: &mut Vec<SidebarIntent>) {
    if let Some(ask) = model.ask_name.clone() {
        ui.horizontal_wrapped(|ui| {
            ui.label(ask.prompt());
            let field = ui.add(
                egui::TextEdit::singleline(&mut model.name_entry).hint_text("naam").desired_width(VRAAG_VELD_BREEDTE),
            );
            field.request_focus();
            let submitted = field.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let ok = ui
                .scope_builder(
                    egui::UiBuilder::new().id(root_ui_id(ui.ctx()).with((ASK_OK, "ok"))).sense(egui::Sense::click()),
                    |ui| {
                        ui.add(egui::Button::new(Texts::get(Key::Yes)).sense(egui::Sense::hover()));
                    },
                )
                .response
                .clicked();
            if (ok || submitted) && model.answer_name().is_some() {
                intents.push(SidebarIntent::Deleted);
            }
            if ui.button(Texts::get(Key::No)).clicked() {
                model.cancel_name();
            }
        });
        return;
    }
    if let Some(confirm) = model.confirm.clone() {
        ui.horizontal_wrapped(|ui| {
            ui.label(confirm.prompt(model));
            let yes = ui
                .scope_builder(
                    egui::UiBuilder::new()
                        .id(root_ui_id(ui.ctx()).with((CONFIRM_YES, "yes")))
                        .sense(egui::Sense::click()),
                    |ui| {
                        ui.add(egui::Button::new(Texts::get(Key::DeleteButton)).sense(egui::Sense::hover()));
                    },
                )
                .response
                .clicked();
            if yes {
                match model.answer_confirm(true) {
                    Some(crate::sidebar::SidebarOutcome::DeleteSession(id)) => intents.push(SidebarIntent::DeleteSession(id)),
                    Some(crate::sidebar::SidebarOutcome::DeletedProject) => intents.push(SidebarIntent::Deleted),
                    None => {}
                }
            }
            if ui.button(Texts::get(Key::No)).clicked() {
                model.cancel_confirm();
            }
        });
    }
}

fn project_label(name: &str, archived: bool) -> String {
    if archived {
        format!("{name} (archived)")
    } else {
        name.to_string()
    }
}

fn row_menu(
    _ui: &mut Ui,
    row: &egui::Response,
    model: &mut SidebarModel,
    project: Option<String>,
    session: Option<String>,
    intents: &mut Vec<SidebarIntent>,
) {
    let picked = egui::Popup::context_menu(row)
        .show(|ui: &mut Ui| {
            ui.scope_builder(egui::UiBuilder::new().id(crate::app::root_ui_id(ui.ctx()).with(ROW_MENU)), |ui| {
                menu_item(ui, model, project.as_deref(), session.as_deref())
            })
            .inner
        })
        .and_then(|response| response.inner);
    match picked {
        Some(MenuAction::NewSession) => {
            if let Ok(created) = model.add_session(project.as_deref()) {
                intents.push(SidebarIntent::OpenSession(created.id));
            }
        }
        Some(MenuAction::Open) => {
            if let Some(session) = &session {
                model.select_session(session);
                intents.push(SidebarIntent::OpenSession(session.clone()));
            }
        }
        Some(MenuAction::Rename) => match (&project, &session) {
            (_, Some(session)) => {
                let title = model.session_title(session);
                model.start_name(NameAsk::RenameSession(title), Some(session.clone()));
            }
            (Some(project), None) => {
                let name = model.project_name(project).unwrap_or_default();
                model.start_name(NameAsk::RenameProject(name), Some(project.clone()));
            }
            (None, None) => {}
        },
        Some(MenuAction::ToggleArchive) => {
            if let Some(project) = &project {
                let archived = model.project_archived(project).unwrap_or(false);
                let _ = model.set_archived(project, !archived);
            }
        }
        Some(MenuAction::LinkFolder) => {
            let start = project.as_deref().and_then(|id| model.project_folder(id));
            if let (Some(project), Some(chosen)) = (&project, pick_folder(start.as_deref())) {
                let _ = model.link_folder(project, Some(&chosen));
                model.select_project(Some(project));
            }
        }
        Some(MenuAction::UnlinkFolder) => {
            if let Some(project) = &project {
                let _ = model.link_folder(project, None);
                model.select_project(Some(project));
            }
        }
        Some(MenuAction::DeleteProject) => {
            if let Some(project) = &project {
                let count = model.session_count(project);
                model.start_confirm(Confirm::DeleteProject(project.clone(), count));
            }
        }
        Some(MenuAction::DeleteSession) => {
            if let Some(session) = &session {
                model.start_confirm(Confirm::DeleteSession(session.clone()));
            }
        }
        None => {}
    }
}

enum MenuAction {
    NewSession,
    Open,
    Rename,
    ToggleArchive,
    LinkFolder,
    UnlinkFolder,
    DeleteProject,
    DeleteSession,
}

fn menu_item(ui: &mut Ui, model: &SidebarModel, project: Option<&str>, session: Option<&str>) -> Option<MenuAction> {
    match (project, session) {
        (Some(project), None) => {
            if ui.button(Texts::get(Key::NewSessionButton)).clicked() {
                return Some(MenuAction::NewSession);
            }
            if ui.button(Texts::get(Key::RenameButton)).clicked() {
                return Some(MenuAction::Rename);
            }
            let archived = model.project_archived(project).unwrap_or(false);
            let archive_label =
                if archived { Texts::get(Key::UnarchiveProjectButton) } else { Texts::get(Key::ArchiveProjectButton) };
            if ui.button(archive_label).clicked() {
                return Some(MenuAction::ToggleArchive);
            }
            let link = ui
                .push_id(ROW_MENU_LINK, |ui| ui.add(egui::Button::new(Texts::get(Key::LinkFolderButton))))
                .inner;
            if link.clicked() {
                return Some(MenuAction::LinkFolder);
            }
            if model.project_folder(project).is_some() && ui.button(Texts::get(Key::UnlinkFolderButton)).clicked() {
                return Some(MenuAction::UnlinkFolder);
            }
            ui.separator();
            if ui.button(Texts::get(Key::DeleteProjectButton)).clicked() {
                return Some(MenuAction::DeleteProject);
            }
            None
        }
        (_, Some(_)) => {
            if ui.button(Texts::get(Key::OpenSessionButton)).clicked() {
                return Some(MenuAction::Open);
            }
            if ui.button(Texts::get(Key::RenameButton)).clicked() {
                return Some(MenuAction::Rename);
            }
            if ui.button(Texts::get(Key::DeleteConversationButton)).clicked() {
                return Some(MenuAction::DeleteSession);
            }
            None
        }
        (None, None) => {
            if ui.button(Texts::get(Key::NewSessionButton)).clicked() {
                return Some(MenuAction::NewSession);
            }
            None
        }
    }
}

pub const TREE_REFRESH: &str = "file-tree-refresh";

fn pick_folder(start: Option<&str>) -> Option<String> {
    let mut dialog = rfd::FileDialog::new().set_title(Texts::get(Key::LinkFolderButton));
    if let Some(path) = start.filter(|p| Path::new(p).is_dir()) {
        dialog = dialog.set_directory(path);
    }
    dialog.pick_folder().map(|path| path.display().to_string())
}

pub const ROW_MENU: &str = "row-menu";

pub const ROW_MENU_LINK: &str = "link";

fn copy_path_menu(row: &egui::Response, root: Option<&Path>, path: Option<&Path>) -> Option<String> {
    let text = crate::clipboard::display_path(root, path);
    let enabled = crate::clipboard::copyable(path);
    let chosen = egui::Popup::context_menu(row)
        .show(|ui: &mut Ui| {
            let item =
                ui.add_enabled(enabled, egui::Button::new(egui::RichText::new(crate::clipboard::COPY_PATH).weak()));
            item.clicked().then_some(())
        })
        .and_then(|response| response.inner);
    chosen.map(|()| text)
}

#[derive(Default)]
struct FileRowActions {
    copy: Option<String>,
    preview: Option<PathBuf>,
}

impl FileRowActions {
    fn interact(&mut self, ui: &mut Ui, id: egui::Id, root: &Path, path: &Path) {
        ui.set_min_width(ui.available_width().max(ui.min_rect().width()));
        let row = ui.interact(ui.min_rect(), id, egui::Sense::click());
        if row.double_clicked_by(egui::PointerButton::Primary) {
            self.preview = Some(path.to_owned());
        }
        if let Some(text) = copy_path_menu(&row, Some(root), Some(path)) {
            self.copy = Some(text);
        }
    }
}

fn tree(
    ui: &mut Ui,
    workspace: &mut Workspace,
    dir: &Path,
    depth: usize,
    status: &FileStatus,
    root: &Path,
    actions: &mut FileRowActions,
) {
    let listing = workspace.tree_at(dir);
    for path in &listing.files {
        file_row(ui, path, status.get(path), root, actions);
    }
    if listing.truncated {
        ui.label(dim(format!("… cut at {MAX_ENTRIES_PER_DIR} entries")));
    }
    for path in &listing.dirs {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        egui::CollapsingHeader::new(truncate_head(&name, KEEP_TAIL)).id_salt(path.display().to_string()).show(
            ui,
            |ui| {
                if depth >= MAX_DEPTH {
                    ui.label(dim("…"));
                } else {
                    tree(ui, workspace, path, depth + 1, status, root, actions);
                }
            },
        );
    }
    if listing.files.is_empty() && listing.dirs.is_empty() {
        ui.label(dim("empty folder"));
    }
}

fn file_row(ui: &mut Ui, path: &Path, change: Option<ChangeKind>, root: &Path, actions: &mut FileRowActions) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let colour = change.map_or(palette::DIM, ink_for);
    ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::hover()), |ui| {
        ui.horizontal(|ui| {
            ui.add(
                egui::Label::new(egui::RichText::new(truncate_head(&name, KEEP_TAIL)).color(colour)).selectable(false),
            )
            .on_hover_text(path.display().to_string());
        });
        actions.interact(ui, egui::Id::new(("file-row", path)), root, path);
    });
}

// Reserve the bottom list before laying out the file tree.
fn changes_band(
    ui: &mut Ui,
    changes: &Changes,
    chosen: Option<&str>,
    dividers: &mut layout::Dividers,
    actions: &mut FileRowActions,
) {
    let column = ui.available_height();
    let stored = if dividers.files > 0 { dividers.files as f32 } else { layout::CHANGES_HEIGHT };
    let header = ui.text_style_height(&egui::TextStyle::Heading) + ui.spacing().item_spacing.y;
    let max = (column - 100.0).max(0.0);
    let floor = (layout::CHANGES_MIN + header).min(max);
    let band = stored.clamp(floor, max);
    Panel::bottom("filetree-changes")
        .exact_size(band)
        .frame(egui::Frame::NONE)
        .show_separator_line(false)
        .show(ui, |ui| {
            let strip = ui.allocate_space(egui::vec2(ui.available_width(), 6.0)).1;
            let grip = ui.interact(strip, egui::Id::new("files-split-handle"), egui::Sense::drag());
            if grip.dragged() {
                dividers.files = (band - grip.drag_delta().y).clamp(floor, max).round() as i32;
                ui.ctx().request_repaint();
            }
            let hot = grip.hovered() || grip.dragged();
            ui.painter().hline(
                strip.x_range(),
                strip.center().y,
                egui::Stroke::new(if hot { 2.0 } else { 1.0 }, palette::BORDER),
            );
            changes_body(ui, changes, chosen, actions);
        });
}

fn changes_body(ui: &mut Ui, changes: &Changes, chosen: Option<&str>, actions: &mut FileRowActions) {
    ui.strong(if chosen.is_none() {
        " ".to_string()
    } else if changes.unavailable {
        "git did not answer".to_string()
    } else if changes.meta.is_none() {
        "not a git repository".to_string()
    } else if changes.paths.is_empty() {
        "no changes".to_string()
    } else {
        format!("changes ({})", changes.paths.len())
    });
    let Some(root) = chosen.map(Path::new) else { return };
    ScrollArea::both().min_scrolled_height(0.0).auto_shrink([false, false]).show(ui, |ui| {
        if changes.unavailable || changes.meta.is_none() {
            return;
        }
        for file in &changes.paths {
            change_row(ui, file, root, actions);
        }
    });
}

pub(crate) fn file_tree(
    ui: &mut Ui,
    workspace: &mut Workspace,
    changes: &Changes,
    dividers: &mut layout::Dividers,
    width: &mut f32,
    max: f32,
) -> (bool, Option<crate::diff_view::Request>) {
    let chosen = workspace.cwd().map(str::to_owned);
    let mut refresh = false;
    let mut actions = FileRowActions::default();
    side_panel(ui, Panel::right("filetree"), "filetree", width, layout::PANEL_MIN, max, |ui| {
        changes_band(ui, changes, chosen.as_deref(), dividers, &mut actions);
        refresh = file_tree_controls(ui, workspace, changes.meta(), changes.paths(), &mut actions);
    });
    if let Some(text) = actions.copy {
        let _ = crate::clipboard::copy(&text);
    }

    let preview = actions.preview.zip(chosen).and_then(|(path, root)| {
        let relative = relative_to(&root, &path)?;
        let change = changes.paths.iter().find(|file| file.path == relative);
        Some(crate::diff_view::Request {
            root,
            relative,
            in_repo: changes.meta.is_some() || changes.unavailable,
            old_path: change.and_then(|file| file.old_path.clone()),
        })
    });
    (refresh, preview)
}

fn change_row(ui: &mut Ui, file: &ChangedFile, root: &Path, actions: &mut FileRowActions) {
    let ink = ink_for(file.change);
    let body_ink = if file.change.exists_on_disk() { palette::TEXT } else { palette::DIM };
    ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::hover()), |ui| {
        ui.horizontal(|ui| {
            ui.add(
                egui::Label::new(egui::RichText::new(format!("{:4}", file.change.word())).color(ink).size(11.0))
                    .selectable(false),
            )
            .on_hover_text(&file.path);
            ui.add(
                egui::Label::new(egui::RichText::new(truncate_head(&file.name(), KEEP_TAIL)).color(body_ink).strong())
                    .selectable(false),
            )
            .on_hover_text(&file.path);
            if let Some(old) = &file.old_path {
                let old =
                    Path::new(old).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| old.clone());
                ui.label(dim(truncate_head(&format!("← {old}"), KEEP_TAIL)));
            }
            if let (Some(a), Some(d)) = (file.additions, file.deletions) {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if d > 0 {
                        ui.label(egui::RichText::new(format!("−{d}")).color(palette::DIFF_REMOVED).size(11.0));
                    }
                    if a > 0 {
                        ui.label(egui::RichText::new(format!("+{a}")).color(palette::DIFF_ADDED).size(11.0));
                    }
                });
            }
        });
        actions.interact(ui, egui::Id::new(("change-row", &file.path)), root, &root.join(&file.path));
    });
}

fn branch_line(ui: &mut Ui, meta: &GitMeta) {
    match (&meta.branch, &meta.commit_hash) {
        (Some(branch), Some(hash)) => {
            ui.label(format!("{branch}@{}", short(hash)));
        }
        (Some(branch), None) => {
            ui.label(format!("{branch} (no commits yet)"));
        }
        (None, Some(hash)) => {
            ui.label(format!("detached@{}", short(hash)));
        }
        (None, None) => {
            ui.label(dim("no branch"));
        }
    }
    if let Some(url) = &meta.repository_url {
        ui.label(dim(truncate_head(url, KEEP_TAIL))).on_hover_text(url.clone());
    }
}

fn ink_for(change: ChangeKind) -> Color32 {
    match change {
        ChangeKind::Added | ChangeKind::Modified | ChangeKind::Renamed | ChangeKind::Copied => palette::GIT_ADDED,
        ChangeKind::Deleted | ChangeKind::Removed => palette::GIT_DELETED,
        ChangeKind::Untracked => palette::GIT_UNTRACKED,
        ChangeKind::Unmerged | ChangeKind::Other => palette::DIM,
    }
}

fn relative_to(root: &str, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .and_then(Path::to_str)
        .map(|rel| rel.replace(std::path::MAIN_SEPARATOR, "/"))
}

struct FileStatus {
    root: Option<String>,
    statuses: HashMap<String, ChangeKind>,
}

impl FileStatus {
    fn build(cwd: Option<&str>, files: &[ChangedFile]) -> Self {
        let mut statuses = HashMap::with_capacity(files.len());
        for file in files {
            statuses.insert(file.path.clone(), file.change);
        }
        Self { root: cwd.filter(|_| !statuses.is_empty()).map(str::to_owned), statuses }
    }

    fn get(&self, path: &Path) -> Option<ChangeKind> {
        let rel = relative_to(self.root.as_deref()?, path)?;
        self.statuses.get(&rel).copied()
    }
}


pub const PANEL_TOGGLES: &str = "panel-toggles";

pub const TOGGLE_SIZE: f32 = 28.0;

pub const TOGGLE_WIDTH: f32 = 34.0;

pub fn toggle_rect(band: egui::Rect, right_side: bool) -> egui::Rect {
    let height = band.height().min(TOGGLE_SIZE);
    let x = if right_side { band.max.x - TOGGLE_WIDTH } else { band.min.x };
    let min = egui::pos2(x, band.min.y);
    egui::Rect::from_min_size(min, egui::vec2(TOGGLE_WIDTH, height))
}

pub const TOGGLE_ICON: egui::Vec2 = egui::vec2(20.0, 18.0);

pub fn toggle_rail_fraction(right_side: bool) -> f32 {
    if right_side {
        0.66
    } else {
        0.34
    }
}

pub fn toggle_icon_rect(band: egui::Rect) -> egui::Rect {
    egui::Rect::from_center_size(band.center(), TOGGLE_ICON)
}

pub struct Toggle {
    id: egui::Id,
    right_side: bool,
}

impl Toggle {
    pub fn new(id: egui::Id, right_side: bool) -> Self {
        Self { id, right_side }
    }
}

impl egui::Widget for Toggle {
    fn ui(self, ui: &mut Ui) -> egui::Response {
        let right_side = self.right_side;
        let response = ui
            .scope_builder(egui::UiBuilder::new().id(self.id).sense(egui::Sense::click()), |ui| {
                ui.add(
                    egui::Button::new("").min_size(egui::vec2(TOGGLE_WIDTH, TOGGLE_SIZE)).sense(egui::Sense::hover()),
                );
            })
            .response;
        paint_toggle_icon(ui, toggle_icon_rect(response.rect), right_side);
        response.on_hover_text(if right_side {
            Texts::get(Key::ToggleFileTree)
        } else {
            Texts::get(Key::ToggleSidebar)
        })
    }
}

fn paint_toggle_icon(ui: &Ui, icon: egui::Rect, right_side: bool) {
    let state = if ui.rect_contains_pointer(icon) {
        egui::widget_style::WidgetState::Hovered
    } else {
        egui::widget_style::WidgetState::Inactive
    };
    let colour = ui.style().visuals.widgets.state(state).fg_stroke.color;
    let line = egui::Stroke::new(1.5, colour);
    let painter = ui.painter();
    let min = icon.min + egui::vec2(1.0, 1.0);
    let box_rect = egui::Rect::from_min_max(min, min + egui::vec2(icon.width() - 2.0, icon.height() - 2.0));
    painter.rect_stroke(box_rect, 3.0, line, egui::StrokeKind::Inside);
    let rail_x = box_rect.min.x + (box_rect.width() * toggle_rail_fraction(right_side)).trunc();
    painter.line_segment([egui::pos2(rail_x, box_rect.min.y), egui::pos2(rail_x, box_rect.max.y)], line);
}

fn dim(text: impl Into<String>) -> egui::WidgetText {
    egui::RichText::new(text.into()).color(palette::DIM).into()
}

fn folder_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

fn short(hash: &str) -> String {
    hash.chars().take(7).collect()
}

fn truncate_head(text: &str, keep_tail: usize) -> String {
    let count = text.chars().count();
    if count <= keep_tail {
        return text.to_string();
    }
    let skip = count - keep_tail;
    let tail: String = text.chars().skip(skip).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_of(path: &Path) -> String {
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    #[test]
    fn git_kleuren_zijn_de_waarden_van_de_origineler() {
        assert_eq!(palette::GIT_ADDED, Color32::from_rgb(46, 160, 67));
        assert_eq!(palette::GIT_DELETED, Color32::from_rgb(210, 70, 70));
        assert_eq!(palette::GIT_UNTRACKED, Color32::from_rgb(130, 170, 220));
    }

    #[test]
    fn git_staten_lezen_als_de_kleurentabel() {
        assert_eq!(ink_for(ChangeKind::Added), palette::GIT_ADDED);
        assert_eq!(ink_for(ChangeKind::Modified), palette::GIT_ADDED);
        assert_eq!(ink_for(ChangeKind::Renamed), palette::GIT_ADDED);
        assert_eq!(ink_for(ChangeKind::Copied), palette::GIT_ADDED);
        assert_eq!(ink_for(ChangeKind::Deleted), palette::GIT_DELETED);
        assert_eq!(ink_for(ChangeKind::Removed), palette::GIT_DELETED);
        assert_eq!(ink_for(ChangeKind::Untracked), palette::GIT_UNTRACKED);
        assert_eq!(ink_for(ChangeKind::Unmerged), palette::DIM);
        assert_eq!(ink_for(ChangeKind::Other), palette::DIM);
    }

    #[test]
    fn een_pad_krijgt_zijn_relatieve_naam_of_geen() {
        assert_eq!(relative_to("/repo", Path::new("/repo/src/app.rs")).as_deref(), Some("src/app.rs"));
        assert_eq!(relative_to("/repo", Path::new("/repo")).as_deref(), Some(""));
        assert_eq!(relative_to("/repo", Path::new("/other/src/app.rs")), None);
    }

    #[test]
    fn de_status_index_zoekt_op_git_naam() {
        let files = vec![ChangedFile {
            path: "src/app.rs".to_string(),
            additions: None,
            deletions: None,
            change: ChangeKind::Modified,
            old_path: None,
        }];
        let status = FileStatus::build(Some("/repo"), &files);
        assert_eq!(status.get(Path::new("/repo/src/app.rs")), Some(ChangeKind::Modified));
        assert_eq!(status.get(Path::new("/repo/src/other.rs")), None);
        assert_eq!(status.get(Path::new("/elsewhere/src/app.rs")), None);
    }

    #[test]
    fn a_name_that_fits_is_left_alone() {
        assert_eq!(truncate_head("main.rs", KEEP_TAIL), "main.rs");
    }

    #[test]
    fn a_long_path_keeps_its_tail_and_says_so() {
        assert_eq!(truncate_head("crates/baudet-ui/src/app.rs", KEEP_TAIL), "…s/baudet-ui/src/app.rs");
        assert_eq!(truncate_head("crates/baudet-core/src/context.rs", 12), "…c/context.rs");
    }

    #[test]
    fn the_cut_counts_characters_not_bytes() {
        assert_eq!(truncate_head("naam_met_éééën_tot_lang_pad.rs", 8), "…g_pad.rs");
    }

    #[test]
    fn a_folder_without_a_path_has_nothing_to_show() {
        let mut workspace = Workspace::default();
        assert_eq!(workspace.cwd(), None);
        let missing = workspace.tree_at(Path::new("/no/such/folder/at/all"));
        assert!(missing.dirs.is_empty() && missing.files.is_empty(), "nothing chosen means nothing listed");
        assert!(!missing.truncated, "an unreadable level does not claim to have been cut");
        let changes = Changes::default();
        assert!(changes.meta().is_none());
        assert!(!changes.unavailable);
    }

    #[test]
    fn moving_to_an_absent_folder_leaves_no_stale_listing() {
        let mut workspace = Workspace::default();
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("bron.rs"), "x\n").unwrap();
        workspace.open(&dir.path().to_string_lossy());
        let here = workspace.tree_at(dir.path());
        assert_eq!(here.files.len(), 1, "the real folder lists the file written into it");
        assert_eq!(here.dirs.len(), 0);

        let gone = dir.path().join("verwijderd");
        workspace.open(&gone.to_string_lossy());
        assert_eq!(workspace.cwd(), Some(gone.to_string_lossy().as_ref()));
        let there = workspace.tree_at(gone.as_path());
        assert!(there.files.is_empty() && there.dirs.is_empty(), "a folder that does not exist has no tree");
    }

    #[test]
    fn build_output_and_dotfiles_are_not_offerings_to_the_tree() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join("target").join("artifact"), "x").unwrap();
        std::fs::write(dir.path().join("main.rs"), "x\n").unwrap();
        std::fs::write(dir.path().join(".hidden"), "x").unwrap();
        let mut workspace = Workspace::default();
        let listing = workspace.tree_at(dir.path());
        let folders: Vec<String> = listing.dirs.iter().map(|p| name_of(p.as_path())).collect();
        let files: Vec<String> = listing.files.iter().map(|p| name_of(p.as_path())).collect();
        assert_eq!(folders, vec!["src".to_string()], "source folders show, build output does not");
        assert_eq!(files, vec!["main.rs".to_string()], "the files of the level show, dot files do not");
    }

    #[test]
    fn een_selectie_zonder_map_verduistert_de_boom_met_zijn_reden() {
        let mut workspace = Workspace::default();
        workspace.show_message(NO_FOLDER);
        assert_eq!(workspace.cwd(), None, "een melding laat de map van een eerder project staan");
        assert_eq!(workspace.message(), Some(NO_FOLDER), "de boom zegt dat er geen map is");
    }

    #[test]
    fn een_selectie_vervangt_de_handmatig_gekozen_map() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut workspace = Workspace::default();
        workspace.open(&dir.path().to_string_lossy());
        workspace.show_message(NO_FOLDER);
        assert_eq!(workspace.cwd(), None, "het project zonder map vervangt de gebladerde map");
        assert_eq!(workspace.message(), Some(NO_FOLDER));
        workspace.open(&dir.path().to_string_lossy());
        assert_eq!(workspace.cwd(), Some(dir.path().to_string_lossy().as_ref()), "de nieuwe koppeling toont");
        assert_eq!(workspace.message(), None, "met een map is er niets meer te zeggen");
    }

    #[test]
    fn de_laatste_selectie_bepaalt_de_reden() {
        let mut workspace = Workspace::default();
        workspace.show_message(NO_READ);
        assert_eq!(workspace.message(), Some(NO_READ), "geen leestoegang is een eigen reden");
        workspace.show_message(NO_FOLDER);
        assert_eq!(workspace.message(), Some(NO_FOLDER), "de jongste selectie is wat er staat");
    }

    #[test]
    fn een_geopende_map_houdt_geen_reden_meer_over() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut workspace = Workspace::default();
        workspace.show_message(NO_FOLDER);
        workspace.open(&dir.path().to_string_lossy());
        assert_eq!(workspace.message(), None, "wie zelf een map kiest, hoeft geen uitleg meer");
        assert_eq!(workspace.cwd(), Some(dir.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn de_brief_onder_de_boom_gebruikt_de_woorden_uit_key_kt() {
        assert_eq!(PERMISSIONS_TITLE, "Folder permissions");
        assert_eq!(FOLDER_LABEL, "folder:");
        assert_eq!(FOLDER_NONE, "no folder linked");
        assert_eq!(UNASSIGNED, "Unassigned");
        assert_eq!(Texts::get(Key::InboxNode), UNASSIGNED);
    }

    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn private_data_home() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
        let guard = HOME_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::TempDir::new().expect("werpmap voor BAUDET_HOME");
        std::env::set_var("BAUDET_HOME", dir.path());
        (guard, dir)
    }

    #[test]
    fn zonder_selectie_is_er_geen_map_en_zijn_de_vinkjes_uit() {
        let (_guard, _home) = private_data_home();
        let model = SidebarModel::open_default();
        assert!(model.selected_project().is_none(), "een vers model claimt een selectie");
        assert!(model.permission_state().is_none(), "zonder selectie tekent de bron geen rechten-vak");
    }

    #[test]
    fn een_map_geeft_de_vinkjes_en_zijn_schijf_regel() {
        let (_guard, _home) = private_data_home();
        let dir = std::env::temp_dir();
        let path = dir.to_str().expect("temp-dir is geldig UTF-8");
        let mut model = SidebarModel::open_default();
        let project = model.add_project("woorden").expect("project aangemaakt");
        model.select_project(Some(&project.id));
        model.link_folder(&project.id, Some(path)).expect("map gekoppeld");
        let state = model.permission_state().expect("met selectie tekent de bron het vak");
        assert_eq!(state.folder.as_deref(), Some(path));
        assert!(state.checkbox_enabled, "met een map staan de vinkjes niet uit");
        assert!(state.disk_capability.is_some(), "met een map hoort de schijf-regel er te staan");
    }

    #[test]
    fn verborgen_stukken_mogen_mee_na_het_vinkje() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".github")).unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join(".env"), "x").unwrap();
        std::fs::write(dir.path().join("main.rs"), "x\n").unwrap();
        let mut workspace = Workspace::default();
        assert!(!workspace.show_hidden(), "de origineler start zonder vinkje (`JCheckBox` aan)");
        let stil = workspace.tree_at(dir.path());
        assert_eq!(stil.files.iter().map(|p| name_of(p)).collect::<Vec<_>>(), vec!["main.rs".to_string()]);
        workspace.set_show_hidden(true);
        let open = workspace.tree_at(dir.path());
        assert_eq!(
            open.files.iter().map(|p| name_of(p)).collect::<Vec<_>>(),
            vec![".env".to_string(), "main.rs".to_string()]
        );
        assert_eq!(
            open.dirs.iter().map(|p| name_of(p)).collect::<Vec<_>>(),
            vec![".github".to_string()],
            "`target` is geen verborgen naam, maar bouw-uitgang"
        );
    }
    #[test]
    fn folders_are_listed_before_files() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("aaa.txt"), "x\n").unwrap();
        std::fs::create_dir_all(dir.path().join("zzz")).unwrap();
        let mut workspace = Workspace::default();
        let listing = workspace.tree_at(dir.path());
        assert_eq!(listing.dirs.iter().map(|p| name_of(p.as_path())).collect::<Vec<_>>(), vec!["zzz".to_string()]);
        assert_eq!(listing.files.iter().map(|p| name_of(p.as_path())).collect::<Vec<_>>(), vec!["aaa.txt".to_string()]);
        let paint_order: Vec<String> =
            listing.dirs.iter().chain(listing.files.iter()).map(|p| name_of(p.as_path())).collect();
        assert_eq!(paint_order, vec!["zzz".to_string(), "aaa.txt".to_string()], "directories lead");
    }
}
