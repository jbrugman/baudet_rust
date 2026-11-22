//! De linkersidebar: wat de boom tekent en welke regels erachter staan.
//!
//! Twee lagen, twee soorten bewijs, en die worden hier niet door elkaar gehaald.
//!
//!  * **De pixels.** Eén adres dat van buiten te vinden moet zijn — de `Add project`-knop — plus de
//!    klik erop. Dat is dezelfde techniek als `tests/tabs.rs`: het response van een
//!    `UiBuilder::id(..)`-wikkel registreert op *exact* dat adres, dus `read_response` vindt de knop
//!    zonder de sidebar-lay-out na te bouwen, en een klik is een `PointerMoved` met daarna een press
//!    en een release op de gemeten middelpunt.
//!  * **De regels.** De cascade, de lege-invoer-regel en de maprechten hoeven dafür geen venster. Ze
//!    staan in `SidebarModel`, en een test die ze rechtstreeks aanroept, faalt op de regel en niet
//!    op een font-metriek.
//!
//! Testisolatie: `baudet_store::data_dir()` volgt `BAUDET_HOME` en `SidebarModel::open_default()`
//! opent precies daar. Een test die dat ongemoeid laat, schrijft zijn werpprojecten in de echte
//! data-map van de ontwikkelaar, dus elke aanroeper zet eerst een eigen `TempDir` neer. Omdat de
//! variabele proces-breed is, delen deze tests één slot.

use baudet_i18n::{Language, Texts};
use baudet_projects::{FolderPermission, Permissions};
use baudet_ui::sidebar::{Confirm, NameAsk, Selection, SidebarModel, SidebarOutcome};
use baudet_ui::{root_ui_id, TranscriptApp, ASK_OK, SIDEBAR_ADD};
use eframe::egui;
use std::sync::{Mutex, MutexGuard};
use tempfile::TempDir;

const SCREEN: [f32; 2] = [1200.0, 800.0];

/// Het slot rondom `BAUDET_HOME`. Eén proces, één variabele: twee tests die allebei een eigen map
/// willen, concurreren anders om dezelfde variabele en de een leest dan de catalogus van de ander.
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

/// Paint one frame and drop its texture delta, like `tests/tabs.rs` and `tests/folder_row.rs`: the
/// pass a test drives is the pass a person sees, and the delta wants handling before the drop.
fn paint_once(ctx: &egui::Context, app: &mut TranscriptApp) {
    ctx.run_ui(input(), |ui| app.paint(ui)).drop_without_applying_deltas();
}

/// Druk een aanwijzer-klik op een gemeten middelpunt: beweging, indruk, loslaat. Twee passen, want
/// een klik in egui vraagt een press- én een release-pass.
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

/// Zet `BAUDET_HOME` op een werpmap en geef die map terug, samen met het slot dat de variabele voor
/// deze test alleen houdt. `TempDir` ruimt zichzelf op bij de drop, dus het tuple houdt beide in
/// leven tot het einde van de test.
fn private_data_home() -> (MutexGuard<'static, ()>, TempDir) {
    let guard = HOME_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = TempDir::new().expect("werpmap voor BAUDET_HOME");
    // De omgeving is proces-breed; elke aanroeper houdt `HOME_LOCK` vast zolang hij er van afleest,
    // dus twee tests schrijven niet door elkaar.
    std::env::set_var("BAUDET_HOME", dir.path());
    (guard, dir)
}

/// Een model op een werpmap, zodat de regels hieronder niets in de echte data-map achterlaten.
fn model_in_tempdir() -> (MutexGuard<'static, ()>, TempDir, SidebarModel) {
    let (guard, dir) = private_data_home();
    let model = SidebarModel::open_default();
    (guard, dir, model)
}

/// Eén projectrij uit de catalogus, opgezocht op id — de lijst-lezing die het domeinmodel zelf ook
/// doet, hier zodat een assert niet van de volgorde van die lijst afhangt.
fn stored(model: &SidebarModel, project_id: &str) -> baudet_projects::Project {
    model
        .catalog()
        .list_projects()
        .into_iter()
        .find(|row| row.id == project_id)
        .expect("het project staat in de catalogus")
}

// ---------------------------------------------------------------------------------------------
// De pixels
// ---------------------------------------------------------------------------------------------

#[test]
fn de_add_knop_staat_op_een_vindbaar_adres() {
    // De linkersidebar tekent in élke pass en in élk tabblad — hij staat vóór het middenvlak, niet in
    // een van de drie bodies — dus dit adres moet antwoorden ongeacht wat er in het midden staat.
    let (_guard, _home) = private_data_home();
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    let id = root_ui_id(&ctx).with((SIDEBAR_ADD, "add"));
    assert!(ctx.read_response(id).is_some(), "de add-projectknop laat geen response na");
}

#[test]
fn een_klik_op_add_vraagt_om_een_projectnaam() {
    // De knop laat zelf nog geen project ontstaan: hij zet de vraag-om-naam-riem. Dat is het enige
    // zichtbare gevolg van die klik, en het is precies wat er onzichtbaar zou zijn als de klik in de
    // lay-out verloren liep — dezelfde reden dat de tabstrip met een klik bewezen werd.
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
    // Een secundaire klik is geen handeling. De add-knop heeft geen contextmenu en reageert via
    // `.clicked()`, dat primary-only is — dus de vraag-om-naam-riem mag van een rechtermuisklik niet
    // opstaan. Dit is de goedkope kant van dezelfde munt die het dure gedrag afdekt: `row_menu` roept
    // de handeling alleen af bij `secondary_clicked`, en die scheiding staat hier bewezen op de
    // enige place in het paneel die van buiten te adresseren is.
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
    // De klacht was "add project werkt niet". De klik alleen bewijst daar te weinig van: die zet
    // slechts de vraag. Dit loopt de hele weg — klik, typ, OK — en assert het resultaat: een project
    // in de catalogus.
    //
    // Twee dingen kosten deze test zijn bestaansrecht, en beide zijn gemeten in plaats van aangenomen:
    //  * de vraag-riem moet bóven de scrollbalk staan. Toen hij onder aan het paneel zat, viel zijn
    //    OK in een 800 pt venster op y 840-859; egui kapt `interact_rect` daar af op het scherm, dus
    //    de knop bestond, tekende, en kon niet geklikt worden.
    //  * `read_response` leest het *vorige* pass, dus een riem die deze pass verscheen, is pas een
    //    pass later te vinden. Daarom `paint_once` vóór het adres.
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

    // Typ alsof het veld de foyer heeft — dat krijgt het mee bij het openen van de vraag.
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

// ---------------------------------------------------------------------------------------------
// De regels, rechtstreeks op het domeinmodel
// ---------------------------------------------------------------------------------------------

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
    // Exact `renameSession` in de origineler: een blanco is daar `NULL`, dus de titel valt terug op
    // wat de index afleidt. Dit is géén "negeer de lege invoer" — dat zou de overschrijving laten staan.
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
    // De identifier is de ankerwaarde van map en rechten; de naam mag wisselen zonder dat er iets van
    // die ankerwaarde verdwijnt, anders verliest een hernoeming meer dan hij belooft.
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

    // De bevestiging is riemtekst en riemtekst draagt de taal van de instelling: het venster zet
    // die bij het openen uit het bestand (`AppState.kt:195`), dus dit doet hetzelfde, en haalt hem
    // na de slot weer weg — `Texts` is één waarde voor het hele proces en deze tests delen het.
    Texts::set_language(Language::Nl);
    model.start_confirm(Confirm::DeleteProject(project.id.clone(), model.session_count(&project.id)));
    // De bevestiging noemt het aantal gesprekken, zoals `DeleteProjectConfirm` in de origineler:
    // iemand die bevestigt, moet zien wát er weggaat.
    assert_eq!(
        model.confirm.as_ref().map(|confirm| confirm.prompt(&model)),
        Some(
            // Het NL-woord van `Key.DeleteProjectConfirm` (`Key.kt:336`), letterlijk: de bevestiging
            // noemt de naam én het aantal gesprekken, en beloofde eerder minder dan hij wist.
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
    // Het paneel tekent geen eigen waarheid: wat er aangevinkt staat, komt uit de bits, en de
    // schijf-rij zegt wat er werkelijk mag. Zonder map zijn de vakjes uit — dat is één verhaal met
    // de no-op hieronder.
    let (_guard, _home, mut model) = model_in_tempdir();
    let project = model.add_project("Rechten").unwrap();
    model.select_project(Some(&project.id));

    let before = model.permission_state().expect("een geselecteerd project heeft een rechtenrij");
    assert!(!before.checkbox_enabled, "zonder map mogen de vakjes niet aanraakbaar zijn");
    assert!(before.disk_capability.is_none(), "zonder map is er geen schijf-rij");
    // De no-op-belofte: zonder map mag er niets bijgeschreven worden, ook niet als het paneel vraagt.
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
    // `linkFolder` in de origineler geeft Lezen bij de *eerste* koppeling en daarna alleen het pad.
    // Een latere wissel die de rechten herschrijft, ontneemt wat iemand aanvinkte.
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
fn een_sessie_verwijderen_lat_het_project_en_zijn_map_staan() {
    let (_guard, _home, mut model) = model_in_tempdir();
    let project = model.add_project("Blijft").unwrap();
    model.link_folder(&project.id, Some("/tmp")).unwrap();
    let session = model.add_session(Some(&project.id)).unwrap();
    model.start_confirm(Confirm::DeleteSession(session.id.clone()));
    assert_eq!(model.answer_confirm(true), Some(SidebarOutcome::DeletedSession));
    assert_eq!(model.catalog().list_projects().len(), 1);
    assert!(stored(&model, &project.id).folder_path.is_some(), "het gesprek nam de map van het project mee");
    assert!(model.catalog().sessions_in(Some(&project.id)).is_empty());
    // De rij waar de selectie op stond, bestaat niet meer; de selectie valt terug op het project.
    assert_eq!(model.selection(), Some(&Selection::Project(project.id.clone())));
}

#[test]
fn de_inbox_is_een_keus_en_geen_restprobleem() {
    // `None` bij `select_project` is de inbox, niet "niets". Een sessie die daar ontstaat, heeft geen
    // ouderschap — dezelfde betekenis als `InboxNode` in de origineler.
    let (_guard, _home, mut model) = model_in_tempdir();
    model.select_project(None);
    assert_eq!(model.selection(), Some(&Selection::Unassigned));
    assert!(model.selected_project().is_none(), "de inbox is geen project");
    let inbox = model.add_session(None).unwrap();
    assert_eq!(model.catalog().project_of_session(&inbox.id), None);
}

#[test]
fn een_venster_opent_zijn_sidebar_op_de_data_map() {
    // `open_default` zwijgt niet over de normale data-map, alleen over een onbeschrijfbare. Deze test
    // bewijst de duurzame tak én dat het venster er iets mee doet: de sidebar staat vóór het
    // middenvlak en tekent dus ook als er nog geen project bestaat.
    let (_guard, _home) = private_data_home();
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    assert!(app.sidebar().catalog_is_durable(), "de sidebar denkt dat hij niet duurzaam kan schrijven");
    assert!(app.selected_project().is_none());
    assert!(app.selected_session().is_none());
}
