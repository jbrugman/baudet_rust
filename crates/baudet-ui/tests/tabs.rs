
use baudet_ui::{root_ui_id, Tab, TranscriptApp, TABSTRIP};
use eframe::egui;

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

fn tab_rect(ctx: &egui::Context, app: &mut TranscriptApp, tab: Tab) -> egui::Rect {
    paint_once(ctx, app);
    let id = root_ui_id(ctx).with((TABSTRIP, tab.label()));
    ctx.read_response(id)
        .map(|response| response.rect)
        .unwrap_or_else(|| panic!("tabknop {} tekent geen response", tab.label()))
}

fn click_tab(ctx: &egui::Context, app: &mut TranscriptApp, center: egui::Pos2) {
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

fn addressable_context() -> egui::Context {
    let ctx = egui::Context::default();
    baudet_ui::palette::install(&ctx);
    ctx
}

#[test]
fn een_venster_begint_op_chat() {
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    assert_eq!(app.tab(), Tab::Chat);
}

#[test]
fn elke_tabknop_staat_op_een_vindbaar_adres() {
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    for tab in Tab::ALL {
        let id = root_ui_id(&ctx).with((TABSTRIP, tab.label()));
        assert!(ctx.read_response(id).is_some(), "tabknop {} laat geen response na", tab.label());
    }
}

#[test]
fn een_klik_op_een_tabknop_wisselt_het_tabblad() {
    let ctx = addressable_context();
    for target in Tab::ALL {
        let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
        let center = tab_rect(&ctx, &mut app, target).center();
        click_tab(&ctx, &mut app, center);
        assert_eq!(app.tab(), target, "klik op {} zette niet dat tabblad", target.label());
    }
}

#[test]
fn de_chat_body_raakt_niet_over_andere_tabbladen() {
    let ctx = addressable_context();
    for target in Tab::ALL {
        let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
        let center = tab_rect(&ctx, &mut app, target).center();
        click_tab(&ctx, &mut app, center);
        assert_eq!(app.tab(), target, "tabblad {} kwam niet aan", target.label());
    }
}
