//! Een map die aan een project is linked, moet rechts de boom én de changes-band vullen.
//!
//! De klacht van de persoon was letterlijk: "ik had een folder gelinked, maar ik zie rechts geen
//! tree of changed files". Wat er miste, was niet de boom — die stond er al — maar de selectie die de
//! linkerbalk zelf wegwierp:
//!
//!  * `row_menu` riep `select_project`/`select_session` aan vóórdat `picked` werd bekeken, dus élke
//!    pass met een open menu zette de selectie op de aangeklikte riem;
//!  * de inbox-knoop selecteerde de inbox ook toen die leeg was.
//!
//! `apply_sidebar_intents` leest de map aan de hand van diezelfde selectie, dus de rechterstrook viel
//! terug op de 'no folder'-melding en verloor boom én changes in één beweging.
//!
//! Sector-tests, gemaakt op vaste id's (`p-00000001`, `s-00000001`) en één vaste riemtekst, dus de
//! boom van buitenaf is raakbaar zonder te raden. De rechtermuistest leest de selectie uit het model
//! en niet uit een menu-item-adres: de item-id van `push_id` is in deze egui-build geen voorspelbare
//! `(ROW_MENU, …)`-afgeleide (gemeten), en de beschermde eigenschap is model-staat, geen pixel.

use baudet_i18n::{Language, Texts};
use baudet_ui::sidebar::{Selection, SidebarModel};
use baudet_ui::{root_ui_id, TranscriptApp, SIDEBAR_ADD};
use eframe::egui;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use tempfile::TempDir;

const SCREEN: [f32; 2] = [1200.0, 800.0];

/// De riemtekst waar de muis op landt: één woord, damit de riem op één regel past in een balk van
/// 280 pt en zijn middelpunt dus binnen de riem blijft.
const RIEL: &str = "riem";

/// Het slot rond `BAUDET_HOME`, gedeeld door de tests die de catalogus aanraken.
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

/// Muis naar een punt en daar één klik (in- en uitdrukken) van knop `button`.
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

/// Een git-werkboom met één gewijzigd bestand, zodat de changes-band iets móet tonen.
fn repo_with_a_change() -> TempDir {
    let repo = TempDir::new().expect("werpmap voor de git-repo");
    std::fs::write(repo.path().join("a.txt"), "een\n").expect("bronbestand");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(repo.path())
            .output()
            .expect("git is beschikbaar in deze testomgeving");
        assert!(out.status.success(), "git {args:?} faalde: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["-c", "user.email=baudet@test", "-c", "user.name=baudet", "commit", "-qm", "begin"]);
    std::fs::write(repo.path().join("a.txt"), "een\n twee\n drie\n").expect("wijziging");
    repo
}

/// `BAUDET_HOME` op een werpmap met één project (`p-00000001`) met één sessie (`s-00000001`), zonder
/// map. Slot en werpmap blijven in leven tot het einde van de test.
fn seeded_home() -> (MutexGuard<'static, ()>, TempDir) {
    let guard = HOME_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = TempDir::new().expect("werpmap voor BAUDET_HOME");
    std::fs::write(
        home.path().join("projects.json"),
        format!(
            "{{\"version\":1,\"projects\":[{{\"id\":\"p-00000001\",\"name\":\"test\",\"archived\":false,\
             \"created_at\":1}}],\"sessions\":[{{\"id\":\"s-00000001\",\"project_id\":\"p-00000001\",\
             \"title\":\"{RIEL}\",\"last_seq\":1,\"updated_at\":2}}]}}"
        ),
    )
    .expect("kataloog op de werpmap");
    std::env::set_var("BAUDET_HOME", home.path());
    (guard, home)
}

/// Het venster op die catalogus, met `folder` linked aan het project: de staat direct nadat iemand in
/// het menu voor "Link folder…" koos. `pick_session` kiest de beginselectie: de sessie voor test 2
/// (de nagespeelde breuk), het project voor test 1.
fn app_on(folder: &Path, guard: MutexGuard<'static, ()>, pick_session: bool) -> TranscriptApp {
    let mut model = SidebarModel::open_default();
    model.link_folder("p-00000001", Some(&folder.to_string_lossy())).expect("de map linked aan het project");
    // `linkFolder()` selecteert het project ná het koppelen (`selectProjectNode`, `ProjectSidebar.kt:378`),
    // anders toont de rechterstrook 'no folder' bij een map die er net is. Een directe `link_folder` zet de
    // selectie níét, dus deze test zet haar zelf neer — dezelfde volgorde als het rechtermuismenu in `row_menu`.
    if pick_session {
        model.select_session("s-00000001");
    } else {
        model.select_project(Some("p-00000001"));
    }
    let app = TranscriptApp::new(TranscriptApp::demo_backend()).with_sidebar_model(model);
    drop(guard);
    app
}

#[test]
fn een_gelinkte_map_toont_rechts_de_boom_en_blijft_staan() {
    let (guard, _home) = seeded_home();
    let repo = repo_with_a_change();
    let mut app = app_on(repo.path(), guard, false);
    let ctx = addressable_context();
    let expected = repo.path().to_string_lossy().to_string();

    println!("selectie na link_folder, vóór de eerste pass: {:?}", app.sidebar().selection());
    paint_once(&ctx, &mut app);
    println!("selectie na de eerste pass: {:?}", app.sidebar().selection());
    assert_eq!(
        app.workspace_folder(),
        Some(expected.as_str()),
        "een project met een gelinkte map hoort die map rechts te openen"
    );
    assert_eq!(app.workspace_message(), None, "er staat een map, dus geen 'no folder'-uitleg");

    // En hij blijft staan over meerdere passen: de boom is geen een-frame-gratie.
    for _ in 0..3 {
        paint_once(&ctx, &mut app);
    }
    assert_eq!(app.workspace_folder(), Some(expected.as_str()), "de map bleef niet geopend");
}

#[test]
fn een_rechtermuisklik_op_een_riem_laat_de_gelinkte_map_staan() {
    // De gemeten breuk, nagespeeld met de muis. Vóór de fix zette precies die pass de selectie op de
    // aangeklikte riem en viel het rechterpane terug op 'no folder': boom en changes samen weg.
    let (guard, _home) = seeded_home();
    let repo = repo_with_a_change();
    // De echt gemeten stroom: een sessie staat gekozen en de map is gelinkt; de gebruiker rechtsklikt
    // dáárna de project-riem. De selectie begint dus als een sessie, niet als een project.
    let mut app = app_on(repo.path(), guard, true);
    let ctx = addressable_context();
    Texts::set_language(Language::En);

    println!("test 2: beginselectie = {:?}", app.sidebar().selection());
    paint_once(&ctx, &mut app);
    assert_eq!(
        app.sidebar().selection(),
        Some(&Selection::Session { project: Some("p-00000001".to_string()), session: "s-00000001".to_string() }),
        "de opstelling moet met een gekozen sessie beginnen, anders test deze de verkeerde stroom"
    );
    let map_voor = app.workspace_folder().map(str::to_string);
    assert_eq!(map_voor.as_deref(), Some(repo.path().to_string_lossy().as_ref()), "de map stond niet open");

    // Een rechtermuisklik op de project-kop — de rij ná de separators onder de `Add project`-knop —
    // opent het rijmenu. Dat mag nooit de selectie verzetten: het menu kiest een handeling, geen
    // project. Vóór de fix stond het `select_project`/`select_session` in `row_menu` vóór `picked`
    // werd gelezen, dus elke pass met een open menu gooide de geselecteerde sessie om, en `apply_
    // sidebar_intents` las de boom uit diezelfde selectie en viel terug op 'no folder': boom en
    // changes samen weg. Dit nagespeelde punt raakt de project-riem (gemeten: de project-kop ligt
    // op deze riemhoogte rond y 32..50 in deze venstermaat), dus de klik is geen lege muisbeweging.
    let add_rect = ctx.read_response(root_ui_id(&ctx).with((SIDEBAR_ADD, "add"))).expect("de add-knop tekent").rect;
    let center = egui::pos2(add_rect.center().x, add_rect.max.y + 18.0);
    click_at(&ctx, &mut app, center, egui::PointerButton::Secondary);
    // De popup tekent zijn inhoud één pass ná de klik; zonder deze pass is het menu nog leeg. De
    // selectie is model-staat, dus die lezen we direct — niet uit een menu-item-adres (de item-id
    // van `push_id` is in deze egui-build geen voorspelbare `(ROW_MENU, …)`-afgeleide, gemeten).
    paint_once(&ctx, &mut app);

    // De selectie is niet verzet: dát was de breuk.
    assert_eq!(
        app.sidebar().selection(),
        Some(&Selection::Session { project: Some("p-00000001".to_string()), session: "s-00000001".to_string() }),
        "de rechtermuisklik verplaatste de selectie"
    );
    assert_eq!(app.workspace_folder().map(str::to_string), map_voor, "de geopende map werd gesloten");
    assert_eq!(app.workspace_message(), None, "er viel een 'no folder'-tekst in de plaats van de boom");
}