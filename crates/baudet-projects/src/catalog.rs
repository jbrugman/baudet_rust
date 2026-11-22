//! De JSON-catalogus achter de linkersidebar.
//!
//! De oorspronkelijke H2Store kent één database aan voor sessies en berichten. Deze catalogus is
//! alleen de index: welke sessie bij welk project hoort, hoe zij heet en wanneer zij laatst actief
//! was. De inhoud blijft in `baudet-store`. Daardoor hoeft de append-only rollout geen kolom of
//! rapportageprotocol te dragen, en blijft de catalogus klein genoeg om atomair te herschrijven.

use crate::ids::new_project_id;
use crate::project::Project;
use baudet_core::Msg;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// De catalogus is één bestand per data-map. Instanties delen hun staat via een slot, zodat twee
/// zijden van de app dezelfde index zien zonder een database te vereisen.
#[derive(Clone, Debug)]
pub struct Catalog {
    shared: Arc<Mutex<State>>,
}

#[derive(Debug)]
struct State {
    path: PathBuf,
    data: Data,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Data {
    #[serde(default)]
    version: u8,
    #[serde(default)]
    projects: Vec<Project>,
    #[serde(default)]
    sessions: Vec<SessionRecord>,
}

/// De metadata van een gesprek, zoals `sessions` in de oorspronkelijke tabel.
///
/// De titel is expliciet optioneel: een blanco of ontbrekende titel laat de afgeleide titel weer
/// terugkomen, precies zoals `renameSession` in H2Store NULL zet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub last_seq: i32,
    #[serde(default)]
    pub updated_at: i64,
}

/// Een rij zoals de sidebar hem tekent: de weergavetitel is hier al opgelost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub last_seq: i32,
    pub updated_at: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("catalogus {path}: {message}")]
    NotFound { path: String, message: String },
    #[error("kan catalogus niet lezen: {0}")]
    Io(#[from] std::io::Error),
    #[error("catalogus is niet leesbaar als JSON: {0}")]
    Json(#[from] serde_json::Error),
}

impl Catalog {
    /// Open of maak de catalogus in de standaard-data-map.
    pub fn open_default() -> Result<Catalog, CatalogError> {
        Self::open(&baudet_store::data_dir())
    }

    /// Open of maak `<directory>/projects.json`. Een ontbrekende map wordt niet gecreëerd totdat
    /// er werkelijk iets geschreven wordt; lezen van een eerste run is gewoon een lege catalogus.
    pub fn open(directory: &Path) -> Result<Catalog, CatalogError> {
        let path = directory.join("projects.json");
        let data = match fs::read(&path) {
            Ok(bytes) if bytes.iter().any(u8::is_ascii_whitespace) || !bytes.is_empty() => {
                serde_json::from_slice(&bytes)?
            }
            Ok(_) => Data::default(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Data::default(),
            Err(error) => return Err(error.into()),
        };
        Ok(Catalog { shared: Arc::new(Mutex::new(State { path, data })) })
    }

    /// Actieve projecten eerst, daarna gearchiveerde; binnen elke groep oudste eerst.
    pub fn list_projects(&self) -> Vec<Project> {
        let state = self.lock();
        let mut projects = state.data.projects.clone();
        projects.sort_by(|left, right| {
            (left.archived, left.created_at, &left.id).cmp(&(right.archived, right.created_at, &right.id))
        });
        projects
    }

    /// Maak een project, met de trimde naam en de Kotlin-creatie-tijd in milliseconden.
    pub fn create_project(&self, name: &str) -> Result<Project, CatalogError> {
        let project = Project::new(new_project_id(), name, now_millis());
        let mut state = self.lock();
        state.data.projects.push(project.clone());
        self.write(&mut state)?;
        Ok(project)
    }

    /// Hernoem een project. De identifier blijft de ankerwaarde van sessies en mappen.
    pub fn rename_project(&self, id: &str, name: &str) -> Result<(), CatalogError> {
        let mut state = self.lock();
        let project = Self::project_mut(&mut state.data, id)?;
        project.name = name.trim().to_owned();
        self.write(&mut state)
    }

    /// Zet de map en de maprechten together, zoals de SQL-update van de oorspronkelijke store.
    pub fn set_folder(&self, id: &str, path: Option<&str>, permissions: i32) -> Result<(), CatalogError> {
        let mut state = self.lock();
        let project = Self::project_mut(&mut state.data, id)?;
        project.folder_path = path.map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned);
        project.permissions = permissions;
        self.write(&mut state)
    }

    /// Archiveer of herstel een project zonder zijn sessies aan te raken.
    pub fn set_archived(&self, id: &str, archived: bool) -> Result<(), CatalogError> {
        let mut state = self.lock();
        Self::project_mut(&mut state.data, id)?.archived = archived;
        self.write(&mut state)
    }

    /// Verwijder een project en de index van al zijn sessies. De rolloutbestanden blijven hier
    /// onaangeraakt: deze methode corrupteert geen gespreksgeschiedenis om een index op te ruimen.
    pub fn delete_project(&self, id: &str) -> Result<(), CatalogError> {
        let mut state = self.lock();
        if Self::project_mut(&mut state.data, id).is_err() {
            return Err(CatalogError::NotFound {
                path: state.path.display().to_string(),
                message: format!("project {id} bestaat niet"),
            });
        }
        state.data.projects.retain(|project| project.id != id);
        state.data.sessions.retain(|session| session.project_id.as_deref() != Some(id));
        self.write(&mut state)
    }

    /// Voeg een sessie toe of werk haar index bij zonder een bestaande eigendom per ongeluk te
    /// wissen. Dit is de vervanger van H2's MERGE, niet van het gesprek zelf.
    pub fn upsert_session(
        &self,
        id: &str,
        model: Option<&str>,
        project_id: Option<&str>,
        last_seq: i32,
    ) -> Result<(), CatalogError> {
        let mut state = self.lock();
        let existing = state.data.sessions.iter().position(|session| session.id == id);
        if let Some(index) = existing {
            let session = &mut state.data.sessions[index];
            if let Some(project_id) = project_id {
                session.project_id = Some(project_id.to_owned());
            }
            if let Some(model) = model {
                session.model = Some(model.to_owned());
            }
            session.last_seq = last_seq;
        } else {
            state.data.sessions.push(SessionRecord {
                id: id.to_owned(),
                project_id: project_id.map(str::to_owned),
                title: None,
                model: model.map(str::to_owned),
                last_seq,
                updated_at: now_millis(),
            });
        }
        state.data.sessions.sort_by(|left, right| left.id.cmp(&right.id));
        self.write(&mut state)
    }

    /// Zet een expliciete sessietitel. Een blanco haalt de override weg, zoals NULL in H2.
    pub fn rename_session(&self, id: &str, title: &str) -> Result<(), CatalogError> {
        let mut state = self.lock();
        let session = Self::session_mut(&mut state.data, id)?;
        let title = title.trim();
        session.title = (!title.is_empty()).then(|| title.to_owned());
        session.updated_at = now_millis();
        self.write(&mut state)
    }

    /// Verwijder alleen de indexrij. De inhoud hoort bij `Rollout`, niet bij deze catalogus.
    pub fn delete_session(&self, id: &str) -> Result<(), CatalogError> {
        let mut state = self.lock();
        if Self::session_mut(&mut state.data, id).is_err() {
            return Err(CatalogError::NotFound {
                path: state.path.display().to_string(),
                message: format!("sessie {id} bestaat niet"),
            });
        }
        state.data.sessions.retain(|session| session.id != id);
        self.write(&mut state)
    }

    /// De sessies van een project, of van `None` (de inbox), meest recent eerst.
    pub fn sessions_in(&self, project_id: Option<&str>) -> Vec<SessionSummary> {
        let state = self.lock();
        let mut sessions = state
            .data
            .sessions
            .iter()
            .filter(|session| session.project_id.as_deref() == project_id)
            .cloned()
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| right.updated_at.cmp(&left.updated_at).then_with(|| right.id.cmp(&left.id)));
        sessions
            .into_iter()
            .map(|session| SessionSummary {
                title: session
                    .title
                    .clone()
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| session.id.clone()),
                id: session.id,
                last_seq: session.last_seq,
                updated_at: session.updated_at,
            })
            .collect()
    }

    /// Het project met deze id, of `None` als de catalogus het niet kent.
    ///
    /// De UI zocht dit vijf keer lineair in `list_projects()`, wat elk een volledige kopie van de
    /// lijst maakt. `currentProject` in `AppState.kt:724` is één opzoeking per beurt en die beurt
    /// loopt vóór élke toolcall, dus het hoort hier.
    ///
    /// Bewust géén `SessionRecord`-kopie: de catalogus is de index, niet de eigendom. Het gesprek
    /// woont in een rollout, dus een sessie die hier nog niet staat maar wél in een rollout is geen
    /// databug — het is een gesprek dat nog niet in de index is geland, en de oorspronkelijke
    /// `currentProject` heeft haar project in het geheugen en zal het dus nooit missen.
    pub fn project(&self, id: &str) -> Option<Project> {
        let state = self.lock();
        state.data.projects.iter().find(|project| project.id == id).cloned()
    }

    /// Het project waaraan een sessie is gekoppeld, of `None` voor de inbox en onbekende sessies.
    pub fn project_of_session(&self, session_id: &str) -> Option<String> {
        self.lock()
            .data
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.project_id.clone())
    }

    /// De afgeleide titel van een gesprek, exact als `H2Store.sessionTitle`: eerste niet-blanco
    /// regel uit de eerste gebruikersboodschap, maximaal 48 tekens, anders de sessie-id.
    pub fn derived_title(first_user_message: Option<&Msg>, fallback_id: &str) -> String {
        let line = first_user_message
            .filter(|message| message.role == "user")
            .and_then(|message| message.content.trim().lines().map(str::trim).find(|line| !line.is_empty()));
        match line {
            Some(line) if line.chars().count() <= 48 => line.to_owned(),
            Some(line) => format!("{}…", line.chars().take(47).collect::<String>()),
            None => fallback_id.to_owned(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn project_mut<'a>(data: &'a mut Data, id: &str) -> Result<&'a mut Project, CatalogError> {
        data.projects.iter_mut().find(|project| project.id == id).ok_or_else(|| CatalogError::NotFound {
            path: String::new(),
            message: format!("project {id} bestaat niet"),
        })
    }

    fn session_mut<'a>(data: &'a mut Data, id: &str) -> Result<&'a mut SessionRecord, CatalogError> {
        data.sessions
            .iter_mut()
            .find(|session| session.id == id)
            .ok_or_else(|| CatalogError::NotFound { path: String::new(), message: format!("sessie {id} bestaat niet") })
    }

    fn write(&self, state: &mut State) -> Result<(), CatalogError> {
        state.data.version = 1;
        let bytes = serde_json::to_vec_pretty(&state.data)?;
        write_atomic(&state.path, &bytes)?;
        Ok(())
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CatalogError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary =
        parent.join(format!(".{}.{}.tmp", path.file_name().unwrap_or_default().to_string_lossy(), now_millis()));
    let result = (|| -> Result<(), CatalogError> {
        let mut file: File = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        file.write_all(bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn now_millis() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_millis() as i64).unwrap_or_default()
}
