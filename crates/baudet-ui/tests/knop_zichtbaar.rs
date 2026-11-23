//! Een knop is een knop: elk afgerond vlak dat een knop tekent, moet op het paneel te zien zijn.
//!
//! De klacht was "het is niet duidelijk wat een knop is". De oorzaak zat niet in de kleurconstanten
//! van `palette` -- die waren al gemeten en goed -- maar in het veld dat de knoptekenaar werkelijk
//! haalt: `button_style` bouwt zijn frame uit `visuals.weak_bg_fill` (`widget_style.rs:159`), niet uit
//! `bg_fill`. `palette::install` zette alleen `bg_fill`, dus elke knop tekende egui's donkere
//! standaard `from_gray(60)` en op de donkere koppanelen zakte dat naar 1,01:1: aritmetisch geen
//! vlak. Deze test leest de *geschilderde* vormen, want precies dát is wat iemand ziet; een assert op
//! een stijlveld dat nergens getekend wordt, was eerder al groen terwijl de knoop onzichtbaar was.

use baudet_ui::{root_ui_id, TranscriptApp, PANEL_TOGGLES, SIDEBAR_ADD};
use eframe::egui;

const SCREEN: [f32; 2] = [1200.0, 800.0];

fn input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN[0], SCREEN[1]))),
        ..Default::default()
    }
}

/// Eén frame schilderen en zijn vormen teruggeven. Textuur-delta's gaan weg: deze test leest vormen,
/// en `FullOutput` mag niet dropped worden terwijl `shapes` nog geleend is.
fn paint_shapes(ctx: &egui::Context, app: &mut TranscriptApp) -> Vec<egui::Shape> {
    let mut out = ctx.run_ui(input(), |ui| app.paint(ui));
    let shapes: Vec<egui::Shape> = out.shapes.iter().map(|c| c.shape.clone()).collect();
    out.textures_delta.clear();
    shapes
}

/// WCAG 2.1 SC 1.4.11: niet-tekst Contrast-vereiste is 1,5:1.
fn contrast(a: egui::Color32, b: egui::Color32) -> f32 {
    fn linear(channel: u8) -> f32 {
        let u = f32::from(channel) / 255.0;
        if u <= 0.03928 {
            u / 12.92
        } else {
            ((u + 0.055) / 1.055).powf(2.4)
        }
    }
    fn luma(c: egui::Color32) -> f32 {
        0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
    }
    let (x, y) = (luma(a), luma(b));
    let (hi, lo) = (x.max(y), x.min(y));
    (hi + 0.05) / (lo + 0.05)
}

/// Het adres van elke knop die in de kop en de linkerprojectbalk op een eigen vierkant staat.
fn knop_adressen(ctx: &egui::Context) -> Vec<(&'static str, egui::Id)> {
    let root = root_ui_id(ctx);
    vec![
        ("Add project", root.with((SIDEBAR_ADD, "add"))),
        ("toggle flanker", root.with((PANEL_TOGGLES, "sidebar"))),
        ("toggle boom", root.with((PANEL_TOGGLES, "filetree"))),
    ]
}

#[test]
fn elke_knop_schildert_voldoende_contrast_op_het_paneel() {
    let ctx = egui::Context::default();
    baudet_ui::palette::install(&ctx);
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    let shapes = paint_shapes(&ctx, &mut app);

    // Alle afgeronde vlakken: dat is het formaat waarop een egui-knoop tekent.
    let vlakken: Vec<(egui::Rect, egui::Color32)> = shapes
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Rect(r) if r.corner_radius.nw > 0 => Some((r.rect, r.fill)),
            _ => None,
        })
        .collect();
    assert!(!vlakken.is_empty(), "er wordt geen enkel afgerond vlak geschilderd: geen enkele knop meer");

    for (naam, id) in knop_adressen(&ctx) {
        let rect = ctx
            .read_response(id)
            .unwrap_or_else(|| panic!("knop {naam} tekent geen response, dus niemand kan hem vinden"))
            .rect;
        // Het vlak van de knop zelf. Meten was hier de enige weg: zoeken op "kleinste rechthoek rond
        // het middelpunt" pakte het 16 x 16 slag-rect van het toggle-icoon (transparante vulling) en
        // niet het knopvlak eromheen. Een knopvlak omvat de widgetrect entirely -- het icoon erin niet
        // -- en vult iets, dus dat is het kenmerk waarop gezocht wordt. `expansion` (4 pt) als
        // tolerantie bleek onbruikbaar: egui 0.36 verschuift de knoprect niet gelijkmatig
        // (`Add project` meet x 4-87.7 tegenover widget x 8-83.7), dus die grens vond niets.
        let vlak = vlakken
            .iter()
            .filter(|(r, fill)| *fill != egui::Color32::TRANSPARENT && r.contains_rect(rect))
            .min_by(|(a, _), (b, _)| a.area().partial_cmp(&b.area()).unwrap())
            .unwrap_or_else(|| panic!("knop {naam} op {rect:?} heeft geen gevuld vlak -- hij is onzichtbaar"));
        let ratio = contrast(vlak.1, baudet_ui::palette::PANEL);
        assert!(
            ratio >= 1.5,
            "knop {naam} vult {vulling:?} = {ratio:.2}:1 op het paneel; dat is geen knop",
            vulling = vlak.1
        );
    }
}

#[test]
fn het_palet_geeft_de_knoppen_het_veld_dat_de_tekenaar_haalt() {
    let ctx = egui::Context::default();
    baudet_ui::palette::install(&ctx);
    let widgets = &ctx.global_style().visuals.widgets;
    // `button_style` haalt zijn frame-vulling uit `weak_bg_fill` (`widget_style.rs:159`).
    assert_eq!(widgets.inactive.weak_bg_fill, baudet_ui::palette::BUTTON);
    assert_eq!(widgets.hovered.weak_bg_fill, baudet_ui::palette::BUTTON_HOVER);
    assert_eq!(widgets.active.weak_bg_fill, baudet_ui::palette::BUTTON_PRESSED);
}

#[test]
fn beide_chatknoppen_passen_onderin_een_smal_venster() {
    let ctx = egui::Context::default();
    baudet_ui::palette::install(&ctx);
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    let mut narrow = input();
    narrow.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(650.0, 360.0)));
    ctx.run_ui(narrow.clone(), |ui| app.paint(ui)).drop_without_applying_deltas();
    let out = ctx.run_ui(narrow, |ui| app.paint(ui));
    let composer = egui::containers::PanelState::load(&ctx, egui::Id::new("composer")).unwrap().outer_rect;
    for key in [baudet_i18n::Key::SendButton, baudet_i18n::Key::StopButton] {
        let label = baudet_i18n::Texts::get(key);
        assert!(
            out.shapes.iter().any(|shape| {
                let egui::Shape::Text(text) = &shape.shape else { return false };
                let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
                text.galley.text() == label && composer.contains_rect(rect) && shape.clip_rect.contains_rect(rect)
            }),
            "{label} is niet volledig zichtbaar in {composer:?}"
        );
    }
    out.drop_without_applying_deltas();
}

#[test]
fn de_koplijn_blijft_buiten_de_tekst_van_hidden_files() {
    let ctx = egui::Context::default();
    baudet_ui::palette::install(&ctx);
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend())
        .with_status_balk("http://localhost:1234/v1 • local endpoint".into());
    ctx.run_ui(input(), |ui| app.paint(ui)).drop_without_applying_deltas();
    let out = ctx.run_ui(input(), |ui| app.paint(ui));
    let label = baudet_i18n::Texts::get(baudet_i18n::Key::FileTreeHidden);
    let text_rect = out
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == label => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
            }
            _ => None,
        })
        .expect("Hidden files moet zichtbaar zijn");
    for shape in &out.shapes {
        if let egui::Shape::LineSegment { points: [a, b], .. } = shape.shape {
            if a.y == b.y {
                let line = egui::Rect::from_two_pos(a, b).intersect(shape.clip_rect);
                assert!(!line.intersects(text_rect), "scheidingslijn {line:?} kruist {label} op {text_rect:?}");
            }
        }
    }
    out.drop_without_applying_deltas();
}
