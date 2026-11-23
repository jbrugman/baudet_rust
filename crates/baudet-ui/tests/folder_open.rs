
use baudet_ui::{panels, root_ui_id, TranscriptApp, TREE_REFRESH};
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

fn addressable_context() -> egui::Context {
    let ctx = egui::Context::default();
    baudet_ui::palette::install(&ctx);
    ctx
}

#[test]
fn een_bestaande_map_verzet_de_workspace() {
    let dir = std::env::temp_dir();
    let mut workspace = panels::Workspace::default();
    assert_eq!(workspace.cwd(), None, "verse workspace claimt al een map");
    workspace.open(dir.to_str().expect("temp-dir is geldig UTF-8"));
    assert_eq!(workspace.cwd(), Some(dir.to_str().unwrap()), "open zette de cwd niet");
}

#[test]
fn de_kop_van_de_boom_heeft_zijn_refresh_en_verborgen_vinkje() {
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    let id = root_ui_id(&ctx).with((TREE_REFRESH, "refresh"));
    assert!(ctx.read_response(id).is_some(), "Refresh-knop tekent geen response op zijn adres");
    assert!(!app.show_hidden_tree(), "het vinkje start uit, als bij `FileTreePanel`");
}
