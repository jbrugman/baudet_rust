//! De twee pane-toggles in de kop verbergen een flanker en halen hem op zijn breedte terug.
//!
//! Geen gok, maar een klik. Het adres van een toggle (`root_ui_id .. PANEL_TOGGLES`) staat vast, en
//! `Ui::put` tekent hem op een eigen vierkant op de rand van de kopstrook, dus de rect is niet af te
//! leiden uit ligging maar wél te lezen uit `read_response` (idem de tabstrip in tests/tabs.rs). Een
//! klik wordt op precies die rect gezet -- twee passen, want een klik in egui vraagt een pers- en een
//! release-pass.
//!
//! Het beweren is tweeërlei, en beide zijn nodig. De vlag (`sidebar_visible`) zegt dat de toggle
//! overschakelt; de `PanelState`-breedte zegt dat het paneel werkelijk van het scherm af is en later
//! op precies dezelfde breedte terugkomt. De breedte is de eigenschap van `sidebarToggle` uit
//! `Main.kt:247-269`: een verbergen mag nooit de breedte vergeten die iemand zelf afstelde.

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

/// Eén frame schilderen en zijn textuur-delta laten varen. De pass die een test stuurt en de pass die
/// iemand ziet, zijn dezelfde functie (`TranscriptApp::paint`).
fn paint_once(ctx: &egui::Context, app: &mut TranscriptApp) {
    ctx.run_ui(input(), |ui| app.paint(ui)).drop_without_applying_deltas();
}

fn addressable_context() -> egui::Context {
    let ctx = egui::Context::default();
    baudet_ui::palette::install(&ctx);
    ctx
}

/// De rect van een toggle, gemeten uit een passive paint. Paniek als de kop hem niet tekent: dan
/// bestaat de voorwaarde van deze test (vindbare knop) niet meer, en dat is een fout, geen skip.
fn toggle_rect(ctx: &egui::Context, app: &mut TranscriptApp, salt: &str) -> egui::Rect {
    paint_once(ctx, app);
    let id = root_ui_id(ctx).with((PANEL_TOGGLES, salt));
    ctx.read_response(id).map(|response| response.rect).unwrap_or_else(|| panic!("toggle {salt} tekent geen response"))
}

/// Een klik op dat middelpunt: beweging, indruk, loslaat. Twee passen, zoals een persoon het doet.
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

/// De buitenbreedte die een paneel zich herinnert, of `None` als er nog geen is onthouden.
/// `PanelState` is persistent en verdwijnt niet als een paneel ophoudt te tekenen -- juist dáárom
/// blijft de breedte over een verbergen heen bewaard.
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
    // Eerst een pass waarin het paneel staat: dáár onthoudt egui zijn default-breedte.
    paint_once(&ctx, &mut app);
    let before = remembered_width(&ctx, "projects").expect("projects moet een breedte onthouden hebben");

    let center = toggle_rect(&ctx, &mut app, "sidebar").center();
    click(&ctx, &mut app, center);
    assert!(!app.sidebar_visible(), "west-toggle zette de sidebar niet uit");
    paint_once(&ctx, &mut app);

    // Terug: dezelfde breedte, niet een verse default of een ingekrompen strook.
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
