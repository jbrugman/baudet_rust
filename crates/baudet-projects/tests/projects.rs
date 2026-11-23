
use baudet_core::Msg;
use baudet_projects::{Catalog, FolderPermission, Permissions};
use std::fs;
use tempfile::TempDir;

#[test]
fn catalogus_leeft_een_herstart_en_bewaart_projectvolgorde() {
    let root = TempDir::new().unwrap();
    let catalog = Catalog::open(root.path()).unwrap();
    let active = catalog.create_project(" Actief ").unwrap();
    let archived = catalog.create_project("Gearchiveerd").unwrap();
    catalog.set_archived(&archived.id, true).unwrap();
    catalog.rename_project(&active.id, "Hernoemd").unwrap();
    catalog.set_folder(&active.id, Some("/tmp/project"), Permissions::encode([FolderPermission::Read])).unwrap();
    drop(catalog);

    let reopened = Catalog::open(root.path()).unwrap();
    let projects = reopened.list_projects();
    assert_eq!(projects.len(), 2);
    assert_eq!(projects[0].name, "Hernoemd");
    assert_eq!(projects[0].folder_path.as_deref(), Some("/tmp/project"));
    assert!(projects[0].is_granted(FolderPermission::Read));
    assert_eq!(projects[1].id, archived.id);
}

#[test]
fn projectverwijdering_cascade_naar_de_sessieindex() {
    let root = TempDir::new().unwrap();
    let catalog = Catalog::open(root.path()).unwrap();
    let project = catalog.create_project("Project").unwrap();
    catalog.upsert_session("s-one", None, Some(&project.id), 1).unwrap();
    catalog.upsert_session("s-inbox", None, None, 1).unwrap();
    catalog.delete_project(&project.id).unwrap();
    assert!(catalog.sessions_in(Some(&project.id)).is_empty());
    assert_eq!(catalog.sessions_in(None).len(), 1);
    assert!(catalog.project_of_session("s-one").is_none());
}

#[test]
fn sessieindex_houdt_een_bestaande_eigendom_en_titel_override() {
    let root = TempDir::new().unwrap();
    let catalog = Catalog::open(root.path()).unwrap();
    let project = catalog.create_project("Project").unwrap();
    catalog.upsert_session("s-chat", Some("gemma"), Some(&project.id), 0).unwrap();
    catalog.rename_session("s-chat", "  Bespreek de diff  ").unwrap();
    catalog.upsert_session("s-chat", None, None, 7).unwrap();

    let stored = fs::read(root.path().join("projects.json")).unwrap();
    let stored: serde_json::Value = serde_json::from_slice(&stored).unwrap();
    let session = stored["sessions"].as_array().unwrap().iter().find(|item| item["id"] == "s-chat").unwrap();
    assert_eq!(session["model"], "gemma");
    assert_eq!(session["project_id"], project.id);
    assert_eq!(session["last_seq"], 7);
    let sessions = catalog.sessions_in(Some(&project.id));
    assert_eq!(sessions[0].title, "Bespreek de diff");
}

#[test]
fn afgeleide_titel_neemt_eerste_gebruikersregel_en_cap_48() {
    let short = Catalog::derived_title(Some(&Msg::user("  \n  Eerste vraag  \n tweede regel  ")), "s-x");
    assert_eq!(short, "Eerste vraag");

    let first = Msg::assistant("verkeerde rol");
    assert_eq!(Catalog::derived_title(Some(&first), "s-x"), "s-x");

    let long = Msg::user("a".repeat(60));
    let capped = Catalog::derived_title(Some(&long), "s-x");
    assert_eq!(capped.chars().count(), 48);
    assert!(capped.ends_with('…'));
    assert!(capped.chars().take(47).eq(std::iter::repeat_n('a', 47)));
}

#[test]
fn een_fout_laat_het_vorige_complete_bestand_heel() {
    let root = TempDir::new().unwrap();
    let catalog = Catalog::open(root.path()).unwrap();
    let project = catalog.create_project("Blijft staan").unwrap();
    let before = fs::read(root.path().join("projects.json")).unwrap();

    let independent = Catalog::open(root.path()).unwrap();
    assert!(independent.rename_project("onbekend", "Mag niet").is_err());
    assert_eq!(fs::read(root.path().join("projects.json")).unwrap(), before);
    let _ = project;
}
