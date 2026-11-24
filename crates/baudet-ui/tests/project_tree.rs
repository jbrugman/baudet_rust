
use baudet_i18n::{Key, Language, Texts};
use baudet_projects::{FolderPermission, Permissions};
use baudet_ui::sidebar::{Confirm, NameAsk, Selection, SidebarModel, SidebarOutcome};
use baudet_ui::{root_ui_id, TranscriptApp, ASK_OK, CONFIRM_YES, SIDEBAR_ADD};
use eframe::egui;
use std::sync::{Mutex, MutexGuard};
use tempfile::TempDir;

const SCREEN: [f32; 2] = [1200.0, 800.0];

static HOME_LOCK: Mutex<()> = Mutex::new(());

fn input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN[0], SCREEN[1]))),
        ..Default::default()
    }
}

fn addressable_context() -> egui::Context {
    let ctx = egui::Context::default();
    baudet_ui::palette::install(&ctx);
    ctx
}

fn paint_once(ctx: &egui::Context, app: &mut TranscriptApp) {
    ctx.run_ui(input(), |ui| app.paint(ui)).drop_without_applying_deltas();
}

fn click_at(ctx: &egui::Context, app: &mut TranscriptApp, center: egui::Pos2, button: egui::PointerButton) {
    for pressed in [true, false] {
        let mut frame = input();
        frame.events.push(egui::Event::PointerMoved(center));
        frame.events.push(egui::Event::PointerButton {
            pos: center,
            button,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
        ctx.run_ui(frame, |ui| app.paint(ui)).drop_without_applying_deltas();
    }
}

fn label_at(ctx: &egui::Context, app: &mut TranscriptApp, label: &str) -> egui::Pos2 {
    let output = ctx.run_ui(input(), |ui| app.paint(ui));
    let pos = output.shapes.iter().find_map(|shape| match &shape.shape {
        egui::Shape::Text(text) if text.galley.text() == label => Some(text.pos + text.galley.size() / 2.0),
        _ => None,
    });
    output.drop_without_applying_deltas();
    pos.unwrap_or_else(|| panic!("missing label: {label}"))
}

fn private_data_home() -> (MutexGuard<'static, ()>, TempDir) {
    let guard = HOME_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = TempDir::new().expect("werpmap voor BAUDET_HOME");
    std::env::set_var("BAUDET_HOME", dir.path());
    (guard, dir)
}

fn model_in_tempdir() -> (MutexGuard<'static, ()>, TempDir, SidebarModel) {
    let (guard, dir) = private_data_home();
    let model = SidebarModel::open_default();
    (guard, dir, model)
}

fn stored(model: &SidebarModel, project_id: &str) -> baudet_projects::Project {
    model
        .catalog()
        .list_projects()
        .into_iter()
        .find(|row| row.id == project_id)
        .expect("het project staat in de catalogus")
}


#[test]
fn de_add_knop_staat_op_een_vindbaar_adres() {
    let (_guard, _home) = private_data_home();
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    let id = root_ui_id(&ctx).with((SIDEBAR_ADD, "add"));
    assert!(ctx.read_response(id).is_some(), "de add-projectknop laat geen response na");
}

#[test]
fn een_klik_op_add_vraagt_om_een_projectnaam() {
    let (_guard, _home) = private_data_home();
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    let center =
        ctx.read_response(root_ui_id(&ctx).with((SIDEBAR_ADD, "add"))).expect("de add-knop tekent").rect.center();
    click_at(&ctx, &mut app, center, egui::PointerButton::Primary);
    paint_once(&ctx, &mut app);
    assert_eq!(app.sidebar().ask_name.as_ref(), Some(&NameAsk::NewProject), "de klik zette geen naamvraag");
}

#[test]
fn alleen_een_linkerklik_vraagt_om_een_projectnaam() {
    let (_guard, _home) = private_data_home();
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    let center =
        ctx.read_response(root_ui_id(&ctx).with((SIDEBAR_ADD, "add"))).expect("de add-knop tekent").rect.center();

    click_at(&ctx, &mut app, center, egui::PointerButton::Secondary);
    paint_once(&ctx, &mut app);
    assert!(app.sidebar().ask_name.is_none(), "een secundaire klik zette de vraag neer");
    assert!(app.sidebar().confirm.is_none(), "een secundaire klik zette een bevestiging neer");

    click_at(&ctx, &mut app, center, egui::PointerButton::Primary);
    paint_once(&ctx, &mut app);
    assert_eq!(app.sidebar().ask_name.as_ref(), Some(&NameAsk::NewProject), "de linkerklik daarna zette de vraag niet");
}

#[test]
fn de_hele_add_project_stroom_eindigt_op_een_project() {
    let (_guard, _home) = private_data_home();
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    let center =
        ctx.read_response(root_ui_id(&ctx).with((SIDEBAR_ADD, "add"))).expect("de add-knop tekent").rect.center();
    click_at(&ctx, &mut app, center, egui::PointerButton::Primary);
    paint_once(&ctx, &mut app);
    let ok_center = ctx
        .read_response(root_ui_id(&ctx).with((ASK_OK, "ok")))
        .expect("de OK van de naamvraag tekent geen response")
        .rect
        .center();

    for letter in "Werp".chars() {
        let mut frame = input();
        frame.events.push(egui::Event::Text(letter.to_string()));
        ctx.run_ui(frame, |ui| app.paint(ui)).drop_without_applying_deltas();
    }
    click_at(&ctx, &mut app, ok_center, egui::PointerButton::Primary);
    paint_once(&ctx, &mut app);

    let names: Vec<String> = app.sidebar().catalog().list_projects().into_iter().map(|p| p.name).collect();
    assert_eq!(names, vec!["Werp".to_string()], "de stroom eindigde niet op het project");
    assert!(app.sidebar().ask_name.is_none(), "de vraag bleef na OK staan");
}


#[test]
fn een_lege_invoer_maakt_geen_project() {
    let (_guard, _home, mut model) = model_in_tempdir();
    model.start_name(NameAsk::NewProject, None);
    model.name_entry.clear();
    assert!(model.answer_name().is_none(), "een lege naam mag geen project opleveren");
    assert!(model.catalog().list_projects().is_empty(), "er stond ineens wél een project in de catalogus");
    assert!(model.ask_name.is_none(), "de vraag hoort na een poging gesloten te zijn");
}

#[test]
fn een_hernoeming_met_lege_invoer_haalt_de_titeloverschrijving_weg() {
    let (_guard, _home, mut model) = model_in_tempdir();
    let session = model.add_session(None).unwrap();
    model.rename_session(&session.id, "De overschrijving").unwrap();
    assert_eq!(model.session_title(&session.id), "De overschrijving");
    model.start_name(NameAsk::RenameSession("De overschrijving".to_string()), Some(session.id.clone()));
    model.name_entry.clear();
    model.answer_name();
    assert_ne!(model.session_title(&session.id), "De overschrijving", "de overschrijving stond na een blanco nog");
}

#[test]
fn een_hernoemd_project_houdt_zijn_map_en_rechten() {
    let (_guard, _home, mut model) = model_in_tempdir();
    let project = model.add_project("Oude naam").unwrap();
    model.link_folder(&project.id, Some("/tmp/baudet-sidebar-test")).unwrap();
    model.rename_project(&project.id, "Nieuwe naam").unwrap();

    let stored = stored(&model, &project.id);
    assert_eq!(stored.name, "Nieuwe naam");
    assert_eq!(stored.folder_path.as_deref(), Some("/tmp/baudet-sidebar-test"));
    assert!(stored.is_granted(FolderPermission::Read), "de hernoeming verloor het leesrecht");
}

#[test]
fn een_project_verdwijnt_met_zijn_sessies_en_de_inbox_blijft() {
    let (_guard, _home, mut model) = model_in_tempdir();
    let project = model.add_project("Te verwijderen").unwrap();
    model.add_session(Some(&project.id)).unwrap();
    model.add_session(None).unwrap();

    Texts::set_language(Language::Nl);
    model.start_confirm(Confirm::DeleteProject(project.id.clone(), model.session_count(&project.id)));
    assert_eq!(
        model.confirm.as_ref().map(|confirm| confirm.prompt(&model)),
        Some(
            "Project \"Te verwijderen\" en al zijn 1 gesprek(len) verwijderen? Elk bericht wordt definitief verwijderd. Bestanden in de gekoppelde map blijven ongemoeid. Dit kan niet ongedaan worden gemaakt."
                .to_string()
        )
    );
    assert_eq!(model.answer_confirm(true), Some(SidebarOutcome::DeletedProject));
    assert!(model.catalog().list_projects().is_empty());
    assert_eq!(model.catalog().sessions_in(None).len(), 1, "de inbox mocht niet meeverwijderd worden");
    assert_eq!(model.catalog().sessions_in(Some(&project.id)).len(), 0);
    Texts::set_language(Language::En);
    assert_eq!(model.selected_project(), None, "de selectie hangt aan een weg project");
}

#[test]
fn de_rechtenvakjes_volgen_de_bits_van_de_catalogus() {
    let (_guard, _home, mut model) = model_in_tempdir();
    let project = model.add_project("Rechten").unwrap();
    model.select_project(Some(&project.id));

    let before = model.permission_state().expect("een geselecteerd project heeft een rechtenrij");
    assert!(!before.checkbox_enabled, "zonder map mogen de vakjes niet aanraakbaar zijn");
    assert!(before.disk_capability.is_none(), "zonder map is er geen schijf-rij");
    model.set_permissions(true, true, true).unwrap();
    assert_eq!(stored(&model, &project.id).permissions, 0, "zonder map schreef de catalogus rechten bij");

    model.link_folder(&project.id, Some("/tmp")).unwrap();
    let after = model.permission_state().expect("met een map is er een rechtenrij");
    assert!(after.checkbox_enabled);
    assert!(after.read, "een eerste koppeling geeft Lezen");
    assert!(!after.write && !after.execute, "een eerste koppeling geeft méér dan Lezen");
    assert!(
        after.disk_capability.unwrap_or_default().starts_with("op schijf: leesbaar=ja"),
        "de schijf-rij gaat niet met de leesbaarheid in op woord"
    );

    model.set_permissions(true, true, false).unwrap();
    assert_eq!(
        stored(&model, &project.id).permissions,
        Permissions::encode([FolderPermission::Read, FolderPermission::Write])
    );
}

#[test]
fn een_wissel_van_map_houdt_de_rechten_die_er_stonden() {
    let (_guard, _home, mut model) = model_in_tempdir();
    let project = model.add_project("Wissel").unwrap();
    model.link_folder(&project.id, Some("/tmp")).unwrap();
    model.set_permissions(true, true, true).unwrap();
    model.link_folder(&project.id, Some("/var/tmp")).unwrap();
    let stored = stored(&model, &project.id);
    assert_eq!(
        stored.permissions,
        Permissions::encode([FolderPermission::Read, FolderPermission::Write, FolderPermission::Execute]),
        "de wissel poetste de rechten weg"
    );
    assert_eq!(stored.folder_path.as_deref(), Some("/var/tmp"), "de wissel zette het pad niet");
}

#[test]
fn toggling_project_and_inbox_keeps_the_current_conversation() {
    let (_guard, _data, mut model) = model_in_tempdir();
    let project = model.add_project("Toggle project").unwrap();
    let session = model.add_session(Some(&project.id)).unwrap();
    model.add_session(None).unwrap();
    model.select_session(&session.id);
    let catalog = model.catalog().clone();
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend()).with_sidebar_model(model);
    for title in ["Toggle project", Texts::get(Key::InboxNode)] {
        for _ in 0..2 {
            let row = label_at(&ctx, &mut app, title);
            click_at(&ctx, &mut app, row, egui::PointerButton::Primary);
        }
    }
    paint_once(&ctx, &mut app);
    assert_eq!(catalog.sessions_in(Some(&project.id)).len(), 1);
    assert_eq!(catalog.sessions_in(None).len(), 1);
    assert_eq!(app.sidebar().selected_session_id(), Some(session.id.as_str()));
}

#[test]
fn a_project_right_click_opens_its_menu_in_release_too() {
    let (_guard, _data, mut model) = model_in_tempdir();
    model.add_project("Menu project").unwrap();
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend()).with_sidebar_model(model);
    paint_once(&ctx, &mut app);
    let project = label_at(&ctx, &mut app, "Menu project");
    click_at(&ctx, &mut app, project, egui::PointerButton::Secondary);
    label_at(&ctx, &mut app, Texts::get(Key::LinkFolderButton));
}

#[test]
fn deleting_a_conversation_removes_its_files_and_keeps_the_project() {
    use baudet_store::{Rollout, SessionMeta};
    let (_guard, data, mut model) = model_in_tempdir();
    let linked = TempDir::new().unwrap();
    let source = linked.path().join("keep.txt");
    std::fs::write(&source, "project content").unwrap();
    let project = model.add_project("Blijft").unwrap();
    model.link_folder(&project.id, linked.path().to_str()).unwrap();
    let session = model.add_session(Some(&project.id)).unwrap();
    model.rename_session(&session.id, "Delete me").unwrap();
    let sibling = model.add_session(Some(&project.id)).unwrap();
    model.select_session(&session.id);
    let mut rollout = Rollout::create(data.path(), &SessionMeta::new(&session.id)).unwrap();
    rollout.append_msg(&baudet_core::Msg::user("conversation message")).unwrap();
    let removed = rollout.path().to_owned();
    drop(rollout);
    let kept = Rollout::create(data.path(), &SessionMeta::new(&sibling.id)).unwrap().path().to_owned();

    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend()).with_sidebar_model(model);
    paint_once(&ctx, &mut app);
    let row = label_at(&ctx, &mut app, "Delete me");
    click_at(&ctx, &mut app, row, egui::PointerButton::Secondary);
    let delete = label_at(&ctx, &mut app, Texts::get(Key::DeleteConversationButton));
    click_at(&ctx, &mut app, delete, egui::PointerButton::Primary);
    paint_once(&ctx, &mut app);
    let confirm = ctx.read_response(root_ui_id(&ctx).with((CONFIRM_YES, "yes"))).unwrap().rect.center();
    click_at(&ctx, &mut app, confirm, egui::PointerButton::Primary);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !app.header_status().contains("conversation deleted") && std::time::Instant::now() < deadline {
        paint_once(&ctx, &mut app);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(app.header_status(), "conversation deleted");
    assert!(!removed.exists());
    assert!(kept.is_dir());
    assert_eq!(std::fs::read_to_string(source).unwrap(), "project content");
    assert_eq!(stored(app.sidebar(), &project.id).folder_path.as_deref(), linked.path().to_str());
    let reopened = baudet_projects::Catalog::open(data.path()).unwrap();
    let remaining = reopened.sessions_in(Some(&project.id));
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, sibling.id);
    assert_eq!(app.sidebar().selection(), Some(&Selection::Project(project.id)));
}

#[test]
fn de_inbox_is_een_keus_en_geen_restprobleem() {
    let (_guard, _home, mut model) = model_in_tempdir();
    model.select_project(None);
    assert_eq!(model.selection(), Some(&Selection::Unassigned));
    assert!(model.selected_project().is_none(), "de inbox is geen project");
    let inbox = model.add_session(None).unwrap();
    assert_eq!(model.catalog().project_of_session(&inbox.id), None);
}

#[test]
fn een_venster_opent_zijn_sidebar_op_de_data_map() {
    let (_guard, _home) = private_data_home();
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    assert!(app.sidebar().catalog_is_durable(), "de sidebar denkt dat hij niet duurzaam kan schrijven");
    assert!(app.selected_project().is_none());
    assert!(app.selected_session().is_none());
}
