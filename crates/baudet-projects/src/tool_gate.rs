
use crate::catalog::Catalog;
use crate::permissions::FolderPermission;
use crate::project::Project;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolGate {
    Disabled,

    NoProject,

    NoFolder,

    NoPermissions,

    Active,
}

impl ToolGate {
    pub const fn is_active(self) -> bool {
        matches!(self, ToolGate::Active)
    }

    pub const fn warns(self) -> bool {
        matches!(self, ToolGate::NoFolder | ToolGate::NoPermissions)
    }
}

// Check tool mode before project permissions. Disabled tools require no warning.
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
    if !FolderPermission::all().into_iter().any(|permission| project.is_granted(permission)) {
        return ToolGate::NoPermissions;
    }
    ToolGate::Active
}

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

    fn write_bits() -> i32 {
        Permissions::with(Permissions::encode([FolderPermission::Read]), FolderPermission::Write, true)
    }

    #[test]
    fn the_precedence_names_the_first_condition_that_holds() {
        assert_eq!(decide(false, Some(&project(Some("/tmp/x"), write_bits()))), ToolGate::Disabled);
        assert_eq!(decide(true, None), ToolGate::NoProject);
        for blank in [None, Some(""), Some("   ")] {
            assert_eq!(decide(true, Some(&project(blank, write_bits()))), ToolGate::NoFolder, "{blank:?}");
        }
        assert_eq!(decide(true, Some(&project(Some("/tmp/x"), 0))), ToolGate::NoPermissions);
        assert_eq!(
            decide(true, Some(&project(Some("/tmp/x"), Permissions::encode([FolderPermission::Read])))),
            ToolGate::Active
        );
        for extra in [FolderPermission::Write, FolderPermission::Execute] {
            let bits = Permissions::with(Permissions::encode([FolderPermission::Read]), extra, true);
            assert_eq!(decide(true, Some(&project(Some("/tmp/x"), bits))), ToolGate::Active, "{extra:?}");
        }
    }

    #[test]
    fn only_the_two_silent_surprises_are_worth_a_message() {
        assert!(ToolGate::NoFolder.warns());
        assert!(ToolGate::NoPermissions.warns());
        for quiet in [ToolGate::Disabled, ToolGate::NoProject, ToolGate::Active] {
            assert!(!quiet.warns(), "{quiet:?} staat voor een bewuste keuze");
        }
        assert!(ToolGate::Active.is_active());
        for inactive in [ToolGate::Disabled, ToolGate::NoProject, ToolGate::NoFolder, ToolGate::NoPermissions] {
            assert!(!inactive.is_active(), "{inactive:?}");
        }
    }

    #[test]
    fn a_session_without_a_project_gates_to_no_project() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path()).unwrap();
        catalog.upsert_session("inbox", None, None, 0).unwrap();
        assert_eq!(gate_for_session(&catalog, "inbox", true), ToolGate::NoProject);
        assert_eq!(gate_for_session(&catalog, "onbekend", true), ToolGate::NoProject);
    }

    #[test]
    fn a_session_follows_its_project_through_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path()).unwrap();
        let project = catalog.create_project("Baudet").unwrap();
        catalog.upsert_session("s1", None, Some(&project.id), 0).unwrap();

        assert_eq!(gate_for_session(&catalog, "s1", true), ToolGate::NoFolder);

        catalog.set_folder(&project.id, Some("/tmp/baudet"), 0).unwrap();
        assert_eq!(gate_for_session(&catalog, "s1", true), ToolGate::NoPermissions);
        catalog.set_folder(&project.id, Some("/tmp/baudet"), Permissions::encode([FolderPermission::Read])).unwrap();
        assert_eq!(gate_for_session(&catalog, "s1", true), ToolGate::Active);

        catalog.set_folder(&project.id, Some("/tmp/baudet"), write_bits()).unwrap();
        assert_eq!(gate_for_session(&catalog, "s1", true), ToolGate::Active);
        assert_eq!(gate_for_session(&catalog, "s1", false), ToolGate::Disabled);
    }

    #[test]
    fn a_blank_folder_from_the_catalog_is_no_folder() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path()).unwrap();
        let project = catalog.create_project("Baudet").unwrap();
        catalog.upsert_session("s1", None, Some(&project.id), 0).unwrap();
        catalog.set_folder(&project.id, Some("   "), write_bits()).unwrap();
        assert_eq!(catalog.project(&project.id).unwrap().folder_path, None);
        assert_eq!(gate_for_session(&catalog, "s1", true), ToolGate::NoFolder);
    }

    #[test]
    fn the_settings_default_leaves_the_loop_open_but_asking() {
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
