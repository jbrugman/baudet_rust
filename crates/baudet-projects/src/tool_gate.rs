//! Waarom een beurt de tool-lus mag ingaan of niet, als één waarde in een vaste voorrang.
//!
//! Poort van `app/ToolGate.kt` en van de evaluatie in `AppState.kt:739-744`. De reden blijft een
//! enum en wordt niet een bool, precies zoals de origineler dat documenteert: de UI kan dan zeggen
//! wat de persoon moet repareren (een map koppelen, schrijfrecht geven) in plaats van een verzoek
//! om iets te wijzigen stil op het platte chatpad te laten vallen. `AppState.kt:767` noemt dat
//! `toolsActive()`, en dat is hier [`ToolGate::is_active`].
//!
//! De beslissing staat hier en niet in `baudet-ui`, omdat de vensterlay-out (item C) en de ToolLoop
//! (item E) allebei dezelfde vraag stellen. Wat hier bewust níét staat: de amber-tekst van
//! `ChatPanel.kt:437`. Die is item H (i18n), en deze crate heeft geen catalogus van teksten.

use crate::catalog::Catalog;
use crate::permissions::FolderPermission;
use crate::project::Project;

/// Wanneer een beurt deze applicatie werkelijk iets in een map mag laten doen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolGate {
    /// Tools staan uit in de instellingen, dus geen enkele beurt bereikt de lus.
    Disabled,

    /// Het gesprek hoort bij geen project, dus er is geen map in het spel.
    NoProject,

    /// Er is een project gekozen, maar het wijst naar geen map: er is niets te lezen of te winnen.
    NoFolder,

    /// De map is gekoppeld maar het schrijf- én het uitvoerrecht ontbreken, dus wijzigingen kunnen
    /// niet landen.
    NoWrite,

    /// Alles staat klaar: tools mogen over een beschrijfbare map gaan.
    Active,
}

impl ToolGate {
    /// Of een beurt de lus ingaat; exact waar deze poort [`ToolGate::Active`] is, zoals
    /// `AppState.kt:767`.
    pub const fn is_active(self) -> bool {
        matches!(self, ToolGate::Active)
    }

    /// Of het venster er iets over moet zeggen. De origineler toont amber alleen voor `NoFolder` en
    /// `NoWrite` (`ChatPanel.kt:437-442`): een chat zonder project of een uitge-zette tool-lus is
    /// een bewuste keuze en blijft stil.
    ///
    /// De naam is Engels: de UI-teksten zelf zijn item H en komen in vier talen.
    pub const fn warns(self) -> bool {
        matches!(self, ToolGate::NoFolder | ToolGate::NoWrite)
    }
}

/// De eerste voorwaarde die geldt, in precies dezelfde volgorde als `AppState.kt:739-744`.
///
/// De `tools_on`-vlag is hier een bool en niet de `ToolMode` van de settings-crate, omdat de
/// settings-crate deze crate niet mag kennen terwijl deze crate die wél zou moeten kennen voor één
/// vergelijking. `AppState.kt:739` vraagt daar ook alleen om `!= ToolMode.Off`, dus de aanroeper
/// zet de modus om; wat hier beslist, is de voorrang, niet de instelling.
pub fn decide(tools_on: bool, project: Option<&Project>) -> ToolGate {
    if !tools_on {
        return ToolGate::Disabled;
    }
    let Some(project) = project else {
        return ToolGate::NoProject;
    };
    if project.folder_path.as_deref().unwrap_or("").trim().is_empty() {
        return ToolGate::NoFolder;
    }
    if !project.is_granted(FolderPermission::Write) && !project.is_granted(FolderPermission::Execute) {
        return ToolGate::NoWrite;
    }
    ToolGate::Active
}

/// De poort voor één gesprek, zoals `AppState.kt:738`: het project van deze sessie opzoeken en dan
/// dezelfde voorrang.
///
/// Een onbekende sessie, een sessie zonder project en een sessie die nog niet in de index is
/// geland geven allemaal [`ToolGate::NoProject`], niet een foutmelding: de inbox is een normale
/// toestand, geen databug. Zie [`Catalog::project`] voor de grens van de index.
pub fn gate_for_session(catalog: &Catalog, session_id: &str, tools_on: bool) -> ToolGate {
    let project_id = catalog.project_of_session(session_id);
    let project = project_id.as_deref().and_then(|id| catalog.project(id));
    decide(tools_on, project.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::Permissions;
    use baudet_settings::config::ToolMode;

    /// Een project met map en gegeven rechtenbits; `None` is "geen map", zoals de SQL-kolom.
    fn project(folder: Option<&str>, permissions: i32) -> Project {
        Project {
            id: "p1".into(),
            name: "Project".into(),
            folder_path: folder.map(str::to_owned),
            permissions,
            archived: false,
            created_at: 0,
        }
    }

    /// Read plus Write, de toestand waarin een beurt werkelijk iets mag winnen.
    fn write_bits() -> i32 {
        Permissions::with(Permissions::encode([FolderPermission::Read]), FolderPermission::Write, true)
    }

    #[test]
    fn the_precedence_names_the_first_condition_that_holds() {
        // Uitgeschakelde tools winnen van alles, ook van een perfect project.
        assert_eq!(decide(false, Some(&project(Some("/tmp/x"), write_bits()))), ToolGate::Disabled);
        // Geen project, ook al zijn de tools aan.
        assert_eq!(decide(true, None), ToolGate::NoProject);
        // Een lege en een blanco map zijn dezelfde toestand, net als `isNullOrBlank()`.
        for blank in [None, Some(""), Some("   ")] {
            assert_eq!(decide(true, Some(&project(blank, write_bits()))), ToolGate::NoFolder, "{blank:?}");
        }
        // Map zonder rechten: noch Write noch Execute.
        assert_eq!(decide(true, Some(&project(Some("/tmp/x"), 0))), ToolGate::NoWrite);
        assert_eq!(
            decide(true, Some(&project(Some("/tmp/x"), Permissions::encode([FolderPermission::Read])))),
            ToolGate::NoWrite
        );
        // En met één van de twee rechten is de lus open: de origineler laat Execute ook mee tellen.
        for extra in [FolderPermission::Write, FolderPermission::Execute] {
            let bits = Permissions::with(Permissions::encode([FolderPermission::Read]), extra, true);
            assert_eq!(decide(true, Some(&project(Some("/tmp/x"), bits))), ToolGate::Active, "{extra:?}");
        }
    }

    #[test]
    fn only_the_two_silent_surprises_are_worth_a_message() {
        // De amber van `ChatPanel.kt:437-442`, en alleen die.
        assert!(ToolGate::NoFolder.warns());
        assert!(ToolGate::NoWrite.warns());
        for quiet in [ToolGate::Disabled, ToolGate::NoProject, ToolGate::Active] {
            assert!(!quiet.warns(), "{quiet:?} staat voor een bewuste keuze");
        }
        assert!(ToolGate::Active.is_active());
        for inactive in [ToolGate::Disabled, ToolGate::NoProject, ToolGate::NoFolder, ToolGate::NoWrite] {
            assert!(!inactive.is_active(), "{inactive:?}");
        }
    }

    #[test]
    fn a_session_without_a_project_gates_to_no_project() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path()).unwrap();
        catalog.upsert_session("inbox", None, None, 0).unwrap();
        assert_eq!(gate_for_session(&catalog, "inbox", true), ToolGate::NoProject);
        // Een sessie die hier helemaal niet staat is dezelfde toestand, geen foutmelding.
        assert_eq!(gate_for_session(&catalog, "onbekend", true), ToolGate::NoProject);
    }

    #[test]
    fn a_session_follows_its_project_through_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path()).unwrap();
        let project = catalog.create_project("Baudet").unwrap();
        catalog.upsert_session("s1", None, Some(&project.id), 0).unwrap();

        // Vers aangemaakt: geen map, dus de amber van `WarnNoFolder`.
        assert_eq!(gate_for_session(&catalog, "s1", true), ToolGate::NoFolder);

        catalog.set_folder(&project.id, Some("/tmp/baudet"), Permissions::encode([FolderPermission::Read])).unwrap();
        assert_eq!(gate_for_session(&catalog, "s1", true), ToolGate::NoWrite);

        catalog.set_folder(&project.id, Some("/tmp/baudet"), write_bits()).unwrap();
        assert_eq!(gate_for_session(&catalog, "s1", true), ToolGate::Active);
        // De instelling gaat voor: tools uit, en geen enkele beurt komt meer toe.
        assert_eq!(gate_for_session(&catalog, "s1", false), ToolGate::Disabled);
    }

    #[test]
    fn a_blank_folder_from_the_catalog_is_no_folder() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path()).unwrap();
        let project = catalog.create_project("Baudet").unwrap();
        catalog.upsert_session("s1", None, Some(&project.id), 0).unwrap();
        // `set_folder` trimt en wist een blanco pad zelf, dus de index kan geen "  " dragen.
        catalog.set_folder(&project.id, Some("   "), write_bits()).unwrap();
        assert_eq!(catalog.project(&project.id).unwrap().folder_path, None);
        assert_eq!(gate_for_session(&catalog, "s1", true), ToolGate::NoFolder);
    }

    #[test]
    fn the_settings_default_leaves_the_loop_open_but_asking() {
        // De instap-eigenschap die `AppState.kt:739` in het echt gebruikt: de default van de
        // instellingen is `Ask`, dus `!= Off`, dus een projectmap met Write is Active.
        assert_eq!(ToolMode::default(), ToolMode::Ask);
        assert_eq!(
            decide(ToolMode::default() != ToolMode::Off, Some(&project(Some("/tmp/x"), write_bits()))),
            ToolGate::Active
        );
        assert_eq!(
            decide(ToolMode::Off != ToolMode::Off, Some(&project(Some("/tmp/x"), write_bits()))),
            ToolGate::Disabled
        );
    }
}
