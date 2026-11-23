
use baudet_i18n::{Language, Texts};
use baudet_ui::sidebar::{Selection, SidebarModel};
use baudet_ui::{root_ui_id, TranscriptApp, SIDEBAR_ADD};
use eframe::egui;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use tempfile::TempDir;

const SCREEN: [f32; 2] = [1200.0, 800.0];

const RIEL: &str = "riem";

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

fn app_on(folder: &Path, guard: MutexGuard<'static, ()>, pick_session: bool) -> TranscriptApp {
    let mut model = SidebarModel::open_default();
    model.link_folder("p-00000001", Some(&folder.to_string_lossy())).expect("de map linked aan het project");
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

    for _ in 0..3 {
        paint_once(&ctx, &mut app);
    }
    assert_eq!(app.workspace_folder(), Some(expected.as_str()), "de map bleef niet geopend");
}

#[test]
fn een_rechtermuisklik_op_een_riem_laat_de_gelinkte_map_staan() {
    let (guard, _home) = seeded_home();
    let repo = repo_with_a_change();
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

    let add_rect = ctx.read_response(root_ui_id(&ctx).with((SIDEBAR_ADD, "add"))).expect("de add-knop tekent").rect;
    let center = egui::pos2(add_rect.center().x, add_rect.max.y + 18.0);
    click_at(&ctx, &mut app, center, egui::PointerButton::Secondary);
    paint_once(&ctx, &mut app);

    assert_eq!(
        app.sidebar().selection(),
        Some(&Selection::Session { project: Some("p-00000001".to_string()), session: "s-00000001".to_string() }),
        "de rechtermuisklik verplaatste de selectie"
    );
    assert_eq!(app.workspace_folder().map(str::to_string), map_voor, "de geopende map werd gesloten");
    assert_eq!(app.workspace_message(), None, "er viel een 'no folder'-tekst in de plaats van de boom");
}