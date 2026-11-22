//! De rechterstrook toont de boom van de geopende map, plus `Refresh` en het verborgen-vinkje.
//!
//! De map zelf wordt hier niet getypt of gekozen: de Kotlin `FileTreePanel` heeft geen
//! map-invoerveld — hij leest `project?.folderPath` (`:213`) en iemand koppelt een map via het
//! rechtermuismenu van de linkerprojectbalk (`ProjectSidebar.kt:225`). Eenmapveld rechtsboven was
//! een verzinsel en is verwijderd. Dit bestand bewaakt wat er wél is:
//!
//!  * `Refresh` tekent een response op zijn vaste adres (`TREE_REFRESH`) — de deur die de bron
//!    kent (`FileTreePanel.kt:126-131`).
//!  * `Workspace::open` verzet de cwd voor een map die op schijf bestaat: het enige pad dat de
//!    strook gebruikt om een map te tonen (via `--workspace` of een geopend project).
//!  * Het verborgen-vinkje start uit, zoals `FileTreePanel`.

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
    // Het enige pad waarmee de strook een map toont: `Workspace::open`. De map-invoer is weg (de
    // map komt uit de sidebar-link of `--workspace`), dus wat hier telt is dat open dé wrote is.
    let dir = std::env::temp_dir();
    let mut workspace = panels::Workspace::default();
    assert_eq!(workspace.cwd(), None, "verse workspace claimt al een map");
    workspace.open(dir.to_str().expect("temp-dir is geldig UTF-8"));
    assert_eq!(workspace.cwd(), Some(dir.to_str().unwrap()), "open zette de cwd niet");
}

#[test]
fn de_kop_van_de_boom_heeft_zijn_refresh_en_verborgen_vinkje() {
    // De klacht was dat deze twee knoppen ontbraken in de rechterstrook. Het adres van `Refresh`
    // staat vast (`TREE_REFRESH`), dus dit is geen pixel-gok maar een adres: tekent de knop,
    // dan bestaat er een response op dat adres, en klikbaar is hij zoals `Open` dat bewees.
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    let id = root_ui_id(&ctx).with((TREE_REFRESH, "refresh"));
    assert!(ctx.read_response(id).is_some(), "Refresh-knop tekent geen response op zijn adres");
    assert!(!app.show_hidden_tree(), "het vinkje start uit, als bij `FileTreePanel`");
}
