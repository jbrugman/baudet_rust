
use baudet_i18n::{Key, Texts};
use baudet_projects::{Catalog, FolderPermission, Permissions, Project, SessionSummary};

pub const UNASSIGNED: &str = "Unassigned";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameAsk {
    NewProject,
    RenameProject(String),
    RenameSession(String),
}

impl NameAsk {
    pub fn prompt(&self) -> &'static str {
        Texts::get(self.prompt_key())
    }

    pub fn prompt_key(&self) -> Key {
        match self {
            NameAsk::NewProject => Key::ProjectNamePrompt,
            NameAsk::RenameProject(_) => Key::RenameProjectPrompt,
            NameAsk::RenameSession(_) => Key::RenameSessionPrompt,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    DeleteProject(String, usize),
    DeleteSession(String),
}

impl Confirm {
    pub fn prompt(&self, model: &SidebarModel) -> String {
        match self {
            Confirm::DeleteProject(id, count) => {
                let name = model
                    .catalog()
                    .list_projects()
                    .into_iter()
                    .find(|row| row.id == *id)
                    .map(|row| row.name)
                    .unwrap_or_default();
                Texts::fmt(Key::DeleteProjectConfirm, &[&name, &count])
            }
            Confirm::DeleteSession(id) => {
                let title = model
                    .catalog()
                    .sessions_in(model.catalog().project_of_session(id).as_deref())
                    .into_iter()
                    .find(|session| session.id == *id)
                    .map(|session| session.title)
                    .unwrap_or_default();
                Texts::fmt(Key::DeleteConversationConfirm, &[&title])
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Project(String),
    Session { project: Option<String>, session: String },
    Unassigned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionState {
    pub folder: Option<String>,
    pub read: bool,
    pub write: bool,
    pub execute: bool,
    pub checkbox_enabled: bool,
    pub disk_capability: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SidebarModel {
    catalog: Catalog,
    selection: Option<Selection>,
    pub ask_name: Option<NameAsk>,
    pub name_entry: String,
    pub pending_target: Option<String>,
    pub confirm: Option<Confirm>,
}

impl SidebarModel {
    pub fn new(catalog: Catalog) -> SidebarModel {
        SidebarModel {
            catalog,
            selection: None,
            ask_name: None,
            name_entry: String::new(),
            pending_target: None,
            confirm: None,
        }
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    pub fn select_project(&mut self, project_id: Option<&str>) {
        self.selection = match project_id {
            Some(id) => Some(Selection::Project(id.to_string())),
            None => Some(Selection::Unassigned),
        };
    }

    pub fn select_session(&mut self, session_id: &str) {
        let project = self.catalog.project_of_session(session_id);
        self.selection = Some(Selection::Session { project, session: session_id.to_string() });
    }

    pub fn selected_project(&self) -> Option<Project> {
        let id = match self.selection.as_ref()? {
            Selection::Project(id) => id.clone(),
            Selection::Session { project, .. } => project.clone()?,
            Selection::Unassigned => return None,
        };
        self.catalog.list_projects().into_iter().find(|project| project.id == id)
    }

    pub fn selected_session_id(&self) -> Option<&str> {
        match self.selection.as_ref()? {
            Selection::Session { session, .. } => Some(session.as_str()),
            _ => None,
        }
    }

    pub fn selected_session_title(&self) -> Option<String> {
        let id = self.selected_session_id()?;
        let project = self.catalog.project_of_session(id);
        self.catalog
            .sessions_in(project.as_deref())
            .into_iter()
            .find(|session| session.id == id)
            .map(|session| session.title)
    }

    pub fn permission_state(&self) -> Option<PermissionState> {
        let project = self.selected_project()?;
        Some(PermissionState {
            disk_capability: project.folder_path.as_deref().map(disk_capability),
            folder: project.folder_path.clone(),
            read: project.is_granted(FolderPermission::Read),
            write: project.is_granted(FolderPermission::Write),
            execute: project.is_granted(FolderPermission::Execute),
            checkbox_enabled: project.folder_path.is_some(),
        })
    }

    pub fn add_project(&mut self, name: &str) -> Result<Project, String> {
        if name.trim().is_empty() {
            return Err("projectnaam mag niet leeg zijn".to_string());
        }
        let created = self.catalog.create_project(name).map_err(|error| error.to_string())?;
        self.selection = Some(Selection::Project(created.id.clone()));
        Ok(created)
    }

    pub fn rename_project(&mut self, id: &str, name: &str) -> Result<(), String> {
        if name.trim().is_empty() {
            return Err("projectnaam mag niet leeg zijn".to_string());
        }
        self.catalog.rename_project(id, name).map_err(|error| error.to_string())
    }

    pub fn project_name(&self, project_id: &str) -> Option<String> {
        self.catalog.list_projects().into_iter().find(|project| project.id == project_id).map(|project| project.name)
    }

    pub fn project_folder(&self, project_id: &str) -> Option<String> {
        self.catalog
            .list_projects()
            .into_iter()
            .find(|project| project.id == project_id)
            .and_then(|project| project.folder_path)
    }

    pub fn project_archived(&self, project_id: &str) -> Option<bool> {
        self.catalog
            .list_projects()
            .into_iter()
            .find(|project| project.id == project_id)
            .map(|project| project.archived)
    }

    pub fn set_archived(&mut self, id: &str, archived: bool) -> Result<(), String> {
        self.catalog.set_archived(id, archived).map_err(|error| error.to_string())
    }

    pub fn session_count(&self, project_id: &str) -> usize {
        self.catalog.sessions_in(Some(project_id)).len()
    }

    pub fn link_folder(&mut self, project_id: &str, path: Option<&str>) -> Result<(), String> {
        let current = self.catalog.list_projects().into_iter().find(|project| project.id == project_id);
        let Some(project) = current else {
            return Err("project niet gevonden".to_string());
        };
        match path {
            None => self.catalog.set_folder(project_id, None, 0),
            Some(path) => {
                let bits = if project.folder_path.as_deref() == Some(path.trim()) {
                    project.permissions
                } else {
                    Permissions::encode([FolderPermission::Read])
                };
                self.catalog.set_folder(project_id, Some(path), bits)
            }
        }
        .map_err(|error| error.to_string())
    }

    pub fn set_permissions(&mut self, read: bool, write: bool, execute: bool) -> Result<(), String> {
        let project = match self.selected_project() {
            Some(project) => project,
            None => return Ok(()),
        };
        let Some(path) = project.folder_path else { return Ok(()) };
        let bits = if execute {
            Permissions::encode([FolderPermission::Execute])
        } else if write {
            Permissions::encode([FolderPermission::Write])
        } else if read {
            Permissions::encode([FolderPermission::Read])
        } else {
            0
        };
        self.catalog.set_folder(&project.id, Some(&path), bits).map_err(|error| error.to_string())
    }

    pub fn add_session(&mut self, project_id: Option<&str>) -> Result<SessionSummary, String> {
        let id = baudet_projects::new_session_id();
        self.catalog.upsert_session(&id, None, project_id, 0).map_err(|error| error.to_string())?;
        self.select_session(&id);
        let project = self.catalog.project_of_session(&id);
        self.catalog
            .sessions_in(project.as_deref())
            .into_iter()
            .find(|session| session.id == id)
            .ok_or_else(|| "sessie niet gevonden".to_string())
    }

    pub fn rename_session(&mut self, id: &str, title: &str) -> Result<(), String> {
        self.catalog.rename_session(id, title).map_err(|error| error.to_string())
    }

    pub fn delete_project(&mut self, id: &str) -> Result<(), String> {
        self.catalog.delete_project(id).map_err(|error| error.to_string())?;
        if self.selected_project().is_some_and(|project| project.id == id) {
            self.selection = None;
        }
        Ok(())
    }

    pub fn start_name(&mut self, ask: NameAsk, target: Option<String>) {
        self.confirm = None;
        self.name_entry = match &ask {
            NameAsk::NewProject => String::new(),
            NameAsk::RenameProject(name) | NameAsk::RenameSession(name) => name.clone(),
        };
        self.ask_name = Some(ask);
        self.pending_target = target;
    }

    pub fn start_confirm(&mut self, confirm: Confirm) {
        self.ask_name = None;
        self.name_entry.clear();
        self.pending_target = None;
        self.confirm = Some(confirm);
    }

    pub fn session_title(&self, session_id: &str) -> String {
        let project = self.catalog.project_of_session(session_id);
        self.catalog
            .sessions_in(project.as_deref())
            .into_iter()
            .find(|session| session.id == session_id)
            .map(|session| session.title)
            .unwrap_or_default()
    }

    pub fn answer_name(&mut self) -> Option<String> {
        let ask = self.ask_name.take()?;
        let entry = std::mem::take(&mut self.name_entry);
        let target = self.pending_target.take();
        match ask {
            NameAsk::NewProject => self.add_project(&entry).ok().map(|project| project.id),
            NameAsk::RenameProject(_) => {
                if let Some(id) = &target {
                    let _ = self.rename_project(id, &entry);
                }
                target
            }
            NameAsk::RenameSession(_) => {
                if let Some(id) = &target {
                    let _ = self.rename_session(id, &entry);
                }
                target
            }
        }
    }

    pub fn cancel_name(&mut self) {
        self.ask_name = None;
        self.name_entry.clear();
        self.pending_target = None;
    }

    pub fn answer_confirm(&mut self, yes: bool) -> Option<SidebarOutcome> {
        let confirm = self.confirm.take()?;
        if !yes {
            return None;
        }
        match confirm {
            Confirm::DeleteProject(id, _) => self.delete_project(&id).ok().map(|_| SidebarOutcome::DeletedProject),
            Confirm::DeleteSession(id) => Some(SidebarOutcome::DeleteSession(id)),
        }
    }

    pub fn cancel_confirm(&mut self) {
        self.confirm = None;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarOutcome {
    DeletedProject,
    DeleteSession(String),
}

impl SidebarModel {
    pub fn open_default() -> SidebarModel {
        let writable = Catalog::open_default().ok();
        let catalog =
            writable.unwrap_or_else(|| Catalog::open(std::env::temp_dir().as_path()).expect("temp_dir is openbaar"));
        SidebarModel::new(catalog)
    }

    pub fn catalog_is_durable(&self) -> bool {
        self.catalog_path().is_some_and(|path| path.starts_with(baudet_store::data_dir()))
    }

    fn catalog_path(&self) -> Option<std::path::PathBuf> {
        Catalog::open_default().ok().map(|_| baudet_store::data_dir().join("projects.json"))
    }
}

fn disk_capability(path: &str) -> String {
    let probe = std::path::Path::new(path);
    let exists = probe.is_dir();
    let word = |name: &str, ok: bool| format!("{name}={}", if ok { "ja" } else { "nee" });
    format!(
        "op schijf: {}  {}  {}",
        word("leesbaar", exists && readable(probe)),
        word("schrijfbaar", exists && writable(probe)),
        word("uitvoerbaar", exists && executable(probe)),
    )
}

fn readable(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir).is_ok()
}

fn writable(dir: &std::path::Path) -> bool {
    let trial = dir.join(format!(".baudet-cap-probe-{}", std::process::id()));
    match std::fs::File::create(&trial) {
        Ok(_) => {
            let _ = std::fs::remove_file(&trial);
            true
        }
        Err(_) => false,
    }
}

#[cfg(unix)]
fn executable(dir: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    dir.metadata().is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable(dir: &std::path::Path) -> bool {
    dir.metadata().is_ok()
}
