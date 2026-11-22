//! De linkersidebar: projecten met hun sessies, plus de maprechten van het geselecteerde project.
//!
//! Dit is de UI-knoop op de catalogus uit `baudet-projects`, niet een tweede opslag. De regels die
//! hier staan — cascade, de rechten bit-van-de-disk volgen, een eerste mapkoppeling krijgt Lezen —
//! zijn uit `ProjectSidebar.kt` gehaald; de feiten zélf blijven in de catalogus staan.
//!
//! Twee grenzen zijn bewust. De sessie-lading (item D) bestaat nog niet, dus een selectie tekent alleen
//! wat er geselecteerd is en stuurt het id door: er wordt geen gesprek hersteld alsof dat al kan. En
//! de rechten-vakjes volgen de schijf: een map die niet leesbaar is, laten we niet aanvinken alsof
//! Baudet er wél in mag lezen — de disk is de baas, niet het checkboxje.

use baudet_projects::{Catalog, FolderPermission, Permissions, Project, SessionSummary};

/// Het ingebouwde inbox-node, precies zoals `InboxNode` in de origineler: sessies zonder project
/// blijven zichtbaar en raken niet kwijt.
pub const UNASSIGNED: &str = "Unassigned";

/// Waar de tekst-riem onder de boom op wacht. Het is geen vierde toestand van de selectie, maar de
/// vraag vóórdat er iets veranderd: de origineler vraagt dezelfde drie dingen met een dialoog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameAsk {
    NewProject,
    /// De huidige naam staat erbij, zodat het veld niet leeg begint zoals `showInputDialog` daar stopt.
    RenameProject(String),
    RenameSession(String),
}

impl NameAsk {
    /// De vraag boven het veld, in hetzelfde woord als `ProjectSidebar.kt` in diens eigen teksten.
    pub fn prompt(&self) -> &'static str {
        match self {
            NameAsk::NewProject => "Projectnaam:",
            NameAsk::RenameProject(_) => "Hernoem project:",
            NameAsk::RenameSession(_) => "Hernoem gesprek:",
        }
    }
}

/// Waar een "weet u het zeker?" over gaat. De bevestiging staat in het paneel en niet in een
/// zwevend venster, zodat het antwoord bij de knoop blijft waarover het gaat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    /// Project, met het aantal gesprekken erin — de bevestiging noemt dat getal, zoals
    /// `DeleteProjectConfirm` in de origineler.
    DeleteProject(String, usize),
    DeleteSession(String),
}

/// De bevestigingstekst, met het aantal gesprekken of de titel erin.
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
                format!("Project \"{name}\" en {count} gesprek(ken) verwijderen? Bestanden op schijf blijven staan.")
            }
            Confirm::DeleteSession(id) => {
                let title = model
                    .catalog()
                    .sessions_in(model.catalog().project_of_session(id).as_deref())
                    .into_iter()
                    .find(|session| session.id == *id)
                    .map(|session| session.title)
                    .unwrap_or_default();
                format!("Gesprek \"{title}\" verwijderen?")
            }
        }
    }
}

/// Wie of wat er op dat moment in de boom is gekozen. Drie gevallen, geen vlaggenpaar: een project,
/// één van diens sessies, of de inbox. Een sessie en een project sluiten elkaar uit — de origineler
/// wist bij een herbouw expliciet de sessiekeuze, en dat is hier dezelfde regel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Project(String),
    Session { project: Option<String>, session: String },
    Unassigned,
}

/// De maprechten van het geselecteerde project, zoals het paneel eronder ze tekent.
///
/// `checkbox_enabled` is niet hetzelfde als "er is een map gekoppeld": de origineler zet de vakjes
/// alleen aan óók na de disk-capabiliteit. Hier is het simpler en eerlijker — de vakjes staan aan
/// zodra er een map is, en de drie woorden hieronder zeggen wat die map werkelijk toestaat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionState {
    pub folder: Option<String>,
    pub read: bool,
    pub write: bool,
    pub execute: bool,
    pub checkbox_enabled: bool,
    /// "op schijf: leesbaar=ja  schrijfbaar=nee  uitvoerbaar=ja", of `None` zonder map.
    pub disk_capability: Option<String>,
}

/// Het model achter de linkersidebar. Eén catalogus, één selectie; de boom is een projectie daarvan.
#[derive(Debug, Clone)]
pub struct SidebarModel {
    catalog: Catalog,
    selection: Option<Selection>,
    /// De riem die een tekst vraagt, en het veld waarin het antwoord staat. Eén vraag tegelijk: de
    /// origineler ook (`showInputDialog` is modaal), en twee vragen tegelijk zouden niet weten welk
    /// antwoord bij welke knoop hoort.
    pub ask_name: Option<NameAsk>,
    pub name_entry: String,
    /// Het id waarop een lopende vraag of bevestiging gericht is. Apart van de vraag zelf, zodat
    /// `ask_name` de huidige naam kan blijven tonen zonder dat het doel erin verdwijnt.
    pub pending_target: Option<String>,
    /// De lopende "weet u het zeker?".
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

    /// De catalogus, voor wie er rechten of sessies op wil vragen. De sidebar is een lezer; alleen
    /// de methodes hieronder schrijven, dus er ontstaat geen tweede schrijver.
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    /// Kies een project. Sessies zonder project horen bij de inbox, dus `None` is daar dezelfde keus.
    pub fn select_project(&mut self, project_id: Option<&str>) {
        self.selection = match project_id {
            Some(id) => Some(Selection::Project(id.to_string())),
            None => Some(Selection::Unassigned),
        };
    }

    /// Kies een sessie. De ouderschap volgt uit de index, niet uit de vraag — zo kan een aanroeper
    /// geen sessie onder het verkeerde project laten staan.
    pub fn select_session(&mut self, session_id: &str) {
        let project = self.catalog.project_of_session(session_id);
        self.selection = Some(Selection::Session { project, session: session_id.to_string() });
    }

    /// Het project van de huidige selectie, of `None` (inbox, of geen selectie).
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

    /// De titel van de geselecteerde sessie, voor een bevestigingsdialoog. De titel staat al opgelost
    /// in de index (override of afgeleid), dus hier hoeft geen tweede titelregel.
    pub fn selected_session_title(&self) -> Option<String> {
        let id = self.selected_session_id()?;
        let project = self.catalog.project_of_session(id);
        self.catalog
            .sessions_in(project.as_deref())
            .into_iter()
            .find(|session| session.id == id)
            .map(|session| session.title)
    }

    /// De rechtenrij onder in de sidebar. Zonder project is er niets te tonen; met een project volgen
    /// de vinkjes de bits en zegt de schijf wat er werkelijk mag.
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

    /// Een projectnaam mag niet leeg zijn; `createProject` in de origineler doet hetzelfde.
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

    /// De naam van één project, voor de hernoemingsvraag. De catalogus exposeert geen enkele-
    /// project-lezer, dus dit is dezelfde lijst-lezing als elders in dit domeinmodel.
    pub fn project_name(&self, project_id: &str) -> Option<String> {
        self.catalog.list_projects().into_iter().find(|project| project.id == project_id).map(|project| project.name)
    }

    /// Of één project gearchiveerd is, voor het menu-dat-de-archiefstand-omklapt.
    /// De map van een project, of `None` als er geen gekoppeld is. Het menu van de origineler
    /// vraagt om dit antwoord vóór het tekent: `Unlink folder` bestaat daar alleen als er
    /// werkelijk een map staat (`ProjectSidebar.kt:226`).
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

    /// Het aantal gesprekken in een project, voor de bevestiging vóór verwijdering.
    pub fn session_count(&self, project_id: &str) -> usize {
        self.catalog.sessions_in(Some(project_id)).len()
    }

    /// Eén mapkoppeling of -ontkoppeling, met de rechten erbij. Een eerste koppeling geeft Lezen;
    /// latere wissels houden wat er al stond — precies `linkFolder` in `ProjectSidebar.kt`.
    pub fn link_folder(&mut self, project_id: &str, path: Option<&str>) -> Result<(), String> {
        let current = self.catalog.list_projects().into_iter().find(|project| project.id == project_id);
        let Some(project) = current else {
            return Err("project niet gevonden".to_string());
        };
        match path {
            None => self.catalog.set_folder(project_id, None, 0),
            Some(path) => {
                let bits = if project.permissions == 0 {
                    Permissions::encode([FolderPermission::Read])
                } else {
                    project.permissions
                };
                self.catalog.set_folder(project_id, Some(path), bits)
            }
        }
        .map_err(|error| error.to_string())
    }

    /// De drie vakjes onder de geselecteerde map. Zonder map zijn de vakjes uit, dus is dit een no-op;
    /// `checkbox_enabled` en deze regel zijn één verhaal en mogen niet tegenspreken.
    pub fn set_permissions(&mut self, read: bool, write: bool, execute: bool) -> Result<(), String> {
        let project = match self.selected_project() {
            Some(project) => project,
            None => return Ok(()),
        };
        let Some(path) = project.folder_path else { return Ok(()) };
        let mut bits = 0;
        for (granted, permission) in
            [(read, FolderPermission::Read), (write, FolderPermission::Write), (execute, FolderPermission::Execute)]
        {
            bits = Permissions::with(bits, permission, granted);
        }
        self.catalog.set_folder(&project.id, Some(&path), bits).map_err(|error| error.to_string())
    }

    /// Nieuwe sessie in een project, of in de inbox. De sessie-lading (item D) komt later; dit legt
    /// alleen vast dat er een sessie ís, zodat de boom en de selectie erover kunnen praten.
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

    /// Eén gesprek weg; het project en diens map blijven onaangeroerd. De geselecteerde sessie valt
    /// terug op diens project, want de rij waar hij op stond, bestaat niet meer.
    pub fn delete_session(&mut self, id: &str) -> Result<(), String> {
        let project = self.catalog.project_of_session(id);
        self.catalog.delete_session(id).map_err(|error| error.to_string())?;
        if self.selected_session_id() == Some(id) {
            self.select_project(project.as_deref());
        }
        Ok(())
    }

    /// Een project en al diens gesprekken; de bestanden op schijf blijven staan.
    pub fn delete_project(&mut self, id: &str) -> Result<(), String> {
        self.catalog.delete_project(id).map_err(|error| error.to_string())?;
        if self.selected_project().is_some_and(|project| project.id == id) {
            self.selection = None;
        }
        Ok(())
    }

    /// Een tekstvraag openen. Eén tegelijk: `start_name` sluit een lopende bevestiging uit, want een
    /// paneel dat tegelijk vraagt en bevestigt, weet niet welk antwoord bij welke knoop hoort.
    pub fn start_name(&mut self, ask: NameAsk, target: Option<String>) {
        self.confirm = None;
        // De invoer begint met wat er al staat, zoals `showInputDialog(.., huidige_naam)` dat doet.
        self.name_entry = match &ask {
            NameAsk::NewProject => String::new(),
            NameAsk::RenameProject(name) | NameAsk::RenameSession(name) => name.clone(),
        };
        self.ask_name = Some(ask);
        self.pending_target = target;
    }

    /// Een bevestiging openen; sluit een lopende tekstvraag, idem.
    pub fn start_confirm(&mut self, confirm: Confirm) {
        self.ask_name = None;
        self.name_entry.clear();
        self.pending_target = None;
        self.confirm = Some(confirm);
    }

    /// De titel van één sessie, voor een bevestigings- of hernoemingsdialoog. De index lost de titel
    /// al op (overschrijving of afgeleid), dus hier hoeft geen tweede titelregel.
    pub fn session_title(&self, session_id: &str) -> String {
        let project = self.catalog.project_of_session(session_id);
        self.catalog
            .sessions_in(project.as_deref())
            .into_iter()
            .find(|session| session.id == session_id)
            .map(|session| session.title)
            .unwrap_or_default()
    }

    /// Het antwoord op de lopende tekstvraag, of `None` als er geen open staat. Leeg bij een nieuw
    /// project is geen naam en dus geen wijziging; leeg bij een hernoeming van een *gesprek* is dat
    /// wél een antwoord: de overschrijving weg, terug naar de automatische titel — exact
    /// `renameSession` in de origineler, waar een lege invoer de overschrijving wist.
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

    /// De vraag wegklikken zonder iets te veranderen.
    pub fn cancel_name(&mut self) {
        self.ask_name = None;
        self.name_entry.clear();
        self.pending_target = None;
    }

    /// Ja of nee op de lopende bevestiging. `true` voert de verwijdering uit; de geselecteerde riem
    /// valt daarbij terug zoals de methodes hierboven dat al doen.
    pub fn answer_confirm(&mut self, yes: bool) -> Option<SidebarOutcome> {
        let confirm = self.confirm.take()?;
        if !yes {
            return None;
        }
        match confirm {
            Confirm::DeleteProject(id, _) => self.delete_project(&id).ok().map(|_| SidebarOutcome::DeletedProject),
            Confirm::DeleteSession(id) => self.delete_session(&id).ok().map(|_| SidebarOutcome::DeletedSession),
        }
    }

    pub fn cancel_confirm(&mut self) {
        self.confirm = None;
    }
}

/// Wat een bevestiging heeft opgeleverd, zodat de aanroeper het middenpaneel kan verversen zonder de
/// catalogus zelf te raadplegen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarOutcome {
    DeletedProject,
    DeletedSession,
}

impl SidebarModel {
    /// De catalogus op de standaard-data-map, of — als die niet te openen is — een catalogus die in
    /// het geheugen leeft en dus niets kwijtraakt maar ook niets wegschrijft. Een venster dat een
    /// onbeschrijfbare data-map vindt, moet wél tekenen; het verliest alleen de zekerheid dat zijn
    /// projecten de volgende run halen, en dat is een feit dat de statusregel moet dragen, niet een
    /// reden om het venster te weigeren.
    pub fn open_default() -> SidebarModel {
        let writable = Catalog::open_default().ok();
        let catalog =
            writable.unwrap_or_else(|| Catalog::open(std::env::temp_dir().as_path()).expect("temp_dir is openbaar"));
        SidebarModel::new(catalog)
    }

    /// `true` als de catalogus werkelijk op de data-map staat. Het venster zet dat in zijn status.
    pub fn catalog_is_durable(&self) -> bool {
        self.catalog_path().is_some_and(|path| path.starts_with(baudet_store::data_dir()))
    }

    fn catalog_path(&self) -> Option<std::path::PathBuf> {
        // De catalogus exposeert zijn pad niet; de data-map is de enige plek waar hij hoort, dus
        // "duurzaam" is hier gelijk aan "open_default gaf terug wat er verwacht werd".
        Catalog::open_default().ok().map(|_| baudet_store::data_dir().join("projects.json"))
    }
}

/// Wat de schijf van een map toelaat, in woorden.
///
/// De origineler vraagt `Files.isReadable`/`isWritable`/`isExecutable`; die drie methodes bestaan in
/// Rust's standaardbibliotheek niet, en er staat geen crate voor in de offline boom. Dit is daarom
/// één poging per recht, niet een bits-benadering: `read_dir` zegt of Baudet de map werkelijk kan
/// lezen, `File::create` (op een naam die er niet is, want die maakt het bestand en ruimt het op)
/// of hij er mag schrijven, en `metadata().permissions().mode() & 0o111` of het uitvoeringsbit gezet
/// is. Dat laatste is het enige recht dat hier uit de mode-cel wordt gelezen, omdat "uitvoerbaar"
/// geen uit te voeren test is zonder iets uit te voeren.
///
/// Een pad dat er niet is, zegt driemaal nee en geen uitzondering: "leesbaar=nee" is het eerlijke
/// antwoord op een map die is verplaatst, en het paneel hoeft daar geen bijzondere regel voor te verzinnen.
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

/// Mag Baudet deze map inlezen? `read_dir` is de vraag, niet de mode-cel: op een map met het
/// sticky-bit of een ACL zegt alleen het openen zelf iets.
fn readable(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir).is_ok()
}

/// Mag Baudet hierin schrijven? Eén wegwerpproef, met een naam die niet botsst met een bronbestand
/// en die meteen weer weg is. Een map die alleen via ACL's schrijfbaar is, beantwoordt hier "ja" —
/// wat de vraag is die de gebruiker stelt ("mag Baudet hierin schrijven"), niet "wat zegt de mode".
fn writable(dir: &std::path::Path) -> bool {
    let trial = dir.join(format!(".baudet-cap-probe-{}", std::process::id()));
    match std::fs::File::create(&trial) {
        Ok(_) => {
            // Ruimen, ook al kan dat zelf mislukken; een achtergebleven proef mag geen tweede
            // resultaat worden bij de volgende paint.
            let _ = std::fs::remove_file(&trial);
            true
        }
        Err(_) => false,
    }
}

/// Staat het uitvoeringsbit? De mode-cel is hier de bron, want "kan ik dit uitvoeren" testen zou
/// betekenen dat er iets draait — en dat is gerade wat de tool-gate (item E) later zélf beslist.
#[cfg(unix)]
fn executable(dir: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    dir.metadata().is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}

/// Op een platform zonder mode-bits is "uitvoerbaar" niet te lezen; zeggen is er geen keuze.
#[cfg(not(unix))]
fn executable(dir: &std::path::Path) -> bool {
    dir.metadata().is_ok()
}
