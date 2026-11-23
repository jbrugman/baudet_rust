
use baudet_ui::{root_ui_id, TranscriptApp, PANEL_TOGGLES};
use eframe::egui;
use egui::containers::PanelState;
use egui::Id;

const SCREEN: [f32; 2] = [1200.0, 800.0];

fn input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN[0], SCREEN[1]))),
        ..Default::default()
    }
}

fn paint_once(ctx: &egui::Context, app: &mut TranscriptApp) {
    ctx.run_ui(input(), |ui| app.paint(ui)).drop_without_applying_deltas();
}

fn addressable_context() -> egui::Context {
    let ctx = egui::Context::default();
    baudet_ui::palette::install(&ctx);
    ctx
}

fn toggle_rect(ctx: &egui::Context, app: &mut TranscriptApp, salt: &str) -> egui::Rect {
    paint_once(ctx, app);
    let id = root_ui_id(ctx).with((PANEL_TOGGLES, salt));
    ctx.read_response(id).map(|response| response.rect).unwrap_or_else(|| panic!("toggle {salt} tekent geen response"))
}

fn click(ctx: &egui::Context, app: &mut TranscriptApp, center: egui::Pos2) {
    let mut press = input();
    press.events.push(egui::Event::PointerMoved(center));
    press.events.push(egui::Event::PointerButton {
        pos: center,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::default(),
    });
    ctx.run_ui(press, |ui| app.paint(ui)).drop_without_applying_deltas();

    let mut release = input();
    release.events.push(egui::Event::PointerMoved(center));
    release.events.push(egui::Event::PointerButton {
        pos: center,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::default(),
    });
    ctx.run_ui(release, |ui| app.paint(ui)).drop_without_applying_deltas();
}

fn remembered_width(ctx: &egui::Context, panel: &str) -> Option<f32> {
    ctx.data_mut(|d| d.get_persisted::<PanelState>(Id::new(panel))).map(|state| state.outer_rect.width())
}

#[test]
fn een_venster_begint_met_alle_vensters() {
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    assert!(app.sidebar_visible() && app.filetree_visible(), "start moet beide flanken tonen");
}

#[test]
fn beide_toggles_staan_op_een_vindbaar_adres() {
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    for salt in ["sidebar", "filetree"] {
        let id = root_ui_id(&ctx).with((PANEL_TOGGLES, salt));
        assert!(ctx.read_response(id).is_some(), "toggle {salt} laat geen response na");
    }
}

#[test]
fn de_west_toggle_verbergt_de_sidebar_en_houdt_zijn_breedte() {
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    let before = remembered_width(&ctx, "projects").expect("projects moet een breedte onthouden hebben");

    let center = toggle_rect(&ctx, &mut app, "sidebar").center();
    click(&ctx, &mut app, center);
    assert!(!app.sidebar_visible(), "west-toggle zette de sidebar niet uit");
    paint_once(&ctx, &mut app);

    let center = toggle_rect(&ctx, &mut app, "sidebar").center();
    click(&ctx, &mut app, center);
    assert!(app.sidebar_visible(), "west-toggle haalde de sidebar niet terug");
    paint_once(&ctx, &mut app);
    let after = remembered_width(&ctx, "projects").expect("projects moet na terugkeer nog Meetbaar zijn");
    assert_eq!(before, after, "de breedte van de sidebar ging over een verbergen verloren");
}

#[test]
fn de_oost_toggle_verbergt_de_bestandboom_en_houdt_zijn_breedte() {
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    let before = remembered_width(&ctx, "filetree").expect("filetree moet een breedte onthouden hebben");

    let center = toggle_rect(&ctx, &mut app, "filetree").center();
    click(&ctx, &mut app, center);
    assert!(!app.filetree_visible(), "oost-toggle zette de bestandboom niet uit");
    paint_once(&ctx, &mut app);

    let center = toggle_rect(&ctx, &mut app, "filetree").center();
    click(&ctx, &mut app, center);
    assert!(app.filetree_visible(), "oost-toggle haalde de bestandboom niet terug");
    paint_once(&ctx, &mut app);
    let after = remembered_width(&ctx, "filetree").expect("filetree moet na terugkeer nog Meetbaar zijn");
    assert_eq!(before, after, "de breedte van de bestandboom ging over een verbergen verloren");
}

#[test]
fn gesleepte_breedtes_blijven_staan_bij_andere_inhoud_en_venstermaat() {
    let ctx = addressable_context();
    let folder = tempfile::tempdir().unwrap();
    let mut sidebar = baudet_ui::SidebarModel::new(baudet_projects::Catalog::open(folder.path()).unwrap());
    let project = sidebar.add_project(&"Breed project ".repeat(40)).unwrap();
    sidebar.select_project(Some(&project.id));
    sidebar.link_folder(&project.id, folder.path().to_str()).unwrap();
    for number in 0..40 {
        let session = sidebar.add_session(Some(&project.id)).unwrap();
        sidebar.rename_session(&session.id, &format!("{number} {}", "brede sessienaam ".repeat(8))).unwrap();
        std::fs::write(folder.path().join(format!("{number}-{}.txt", "lang".repeat(32))), "").unwrap();
    }
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);

    for (id, right, delta) in [("projects", false, -65.0), ("filetree", true, 105.0)] {
        let panel = PanelState::load(&ctx, Id::new(id)).unwrap().outer_rect;
        let start = egui::pos2(if right { panel.left() } else { panel.right() }, panel.center().y);
        let end = start + egui::vec2(delta, 0.0);
        for (pos, button) in [(start, None), (start, Some(true)), (end, None), (end, Some(false))] {
            let mut frame = input();
            frame.events.push(egui::Event::PointerMoved(pos));
            if let Some(pressed) = button {
                frame.events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                });
            }
            ctx.run_ui(frame, |ui| app.paint(ui)).drop_without_applying_deltas();
        }
        assert_eq!(remembered_width(&ctx, id), Some(panel.width() - delta.abs()), "{id}");
    }

    let chosen = ["projects", "filetree"].map(|id| remembered_width(&ctx, id));
    app = app.with_workspace(folder.path()).with_sidebar_model(sidebar);
    for size in [[900.0, 360.0], [1400.0, 800.0], [900.0, 360.0]] {
        let mut frame = input();
        frame.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size[0], size[1])));
        for _ in 0..2 {
            ctx.run_ui(frame.clone(), |ui| app.paint(ui)).drop_without_applying_deltas();
        }
        let out = ctx.run_ui(frame, |ui| app.paint(ui));
        assert_eq!(["projects", "filetree"].map(|id| remembered_width(&ctx, id)), chosen);
        let changes = PanelState::load(&ctx, Id::new("filetree-changes")).unwrap().outer_rect;
        let tree = PanelState::load(&ctx, Id::new("filetree")).unwrap().outer_rect;
        assert!(changes.height() >= 96.0 && tree.contains_rect(changes));
        let footer = PanelState::load(&ctx, Id::new("project-details")).unwrap().outer_rect;
        for key in [baudet_i18n::Key::PermRead, baudet_i18n::Key::PermWrite, baudet_i18n::Key::PermExecute] {
            let label = baudet_i18n::Texts::get(key);
            assert!(
                out.shapes.iter().any(|shape| {
                    let egui::Shape::Text(text) = &shape.shape else { return false };
                    let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
                    text.galley.text() == label && footer.contains_rect(rect) && shape.clip_rect.contains_rect(rect)
                }),
                "{label} is niet volledig zichtbaar in {footer:?}"
            );
        }
        out.drop_without_applying_deltas();
    }
}
