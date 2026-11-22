//! De tabstrip tekent twee knoppen, ook nadat er een op geklikt is.
//!
//! De klacht was "de tabs zien er niet uit als tab, maar als losse teksten". Een assert op een
//! stijlveld of op een response-rect had die klacht groen laten staan: de *platte woorden* waren
//! precies het gevolg van een style-gedrag dat pas na de klik toeslaat. `Button::selectable` zet
//! `frame_when_inactive: !selected` (`button.rs:78-83`) en `atom_ui` tekent het frame alleen bij
//! `state != Inactive || frame_when_inactive` (`button.rs:358-362`). Een muis die op een tab blijft
//! staan, laat die knop de hele pass als `Inactive` lezen, dus zonder die vlag tekende geen enkel
//! tabblad een vlak en oogde de strip als tekst. Daarom leest deze test de geschilderde vormen
//! idem `knop_zichtbaar.rs`, en wel na een klik.

use baudet_ui::{root_ui_id, Tab, TranscriptApp, TABSTRIP};
use eframe::egui;

const SCREEN: [f32; 2] = [1200.0, 800.0];

fn input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN[0], SCREEN[1]))),
        ..Default::default()
    }
}

/// Eén frame schilderen met een vooraf gevuld frame: vormen teruggeven en de output laten
/// varen (idem knop_zichtbaar.rs). Het frame moet werkelijk aan run_ui meegegeven worden; een
/// evenement dat alleen gebouwd en genegeerd wordt, ziet egui nooit.
fn paint_with(ctx: &egui::Context, app: &mut TranscriptApp, frame: egui::RawInput) -> Vec<egui::Shape> {
    let out = ctx.run_ui(frame, |ui| app.paint(ui));
    let shapes: Vec<egui::Shape> = out.shapes.clone().iter().map(|c| c.shape.clone()).collect();
    out.drop_without_applying_deltas();
    shapes
}

/// Passieve schilderbeurt zonder evenementen.
fn paint(ctx: &egui::Context, app: &mut TranscriptApp) -> Vec<egui::Shape> {
    paint_with(ctx, app, input())
}

/// Klik-helper:beweeg naar het middelpunt van de tabknop, druk in en laat los in twee passen.
fn click(ctx: &egui::Context, app: &mut TranscriptApp, target: Tab) {
    let center = {
        paint(ctx, app);
        ctx.read_response(root_ui_id(ctx).with((TABSTRIP, target.label())))
            .unwrap_or_else(|| panic!("tabknop {} tekent geen response", target.label()))
            .rect
            .center()
    };
    for pressed in [true, false] {
        let mut frame = input();
        frame.events.push(egui::Event::PointerMoved(center));
        frame.events.push(egui::Event::PointerButton {
            pos: center,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
        // Het frame bevat de beweging en de klik; alleen als het bij run_ui beland, ziet egui
        // de aanwijzer en schakelt de tab (gemeten: anders bleef muis None en tab Chat).
        paint_with(ctx, app, frame);
    }
}

/// Het kleinste gevulde afgeronde vlak dat de rect van een tabknop omvat.
fn tab_vlak(shapes: &[egui::Shape], rect: egui::Rect) -> Option<(egui::Rect, egui::Color32)> {
    shapes
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Rect(r) if r.corner_radius.nw > 0 && r.fill != egui::Color32::TRANSPARENT => {
                Some((r.rect, r.fill))
            }
            _ => None,
        })
        .filter(|(r, _)| r.contains_rect(rect))
        .min_by(|(a, _), (b, _)| a.area().partial_cmp(&b.area()).unwrap())
}

#[test]
fn beide_tabknoppen_schilderen_een_vlak_voordat_er_geklikt_is() {
    let ctx = egui::Context::default();
    baudet_ui::palette::install(&ctx);
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    let shapes = paint(&ctx, &mut app);
    for tab in Tab::ALL {
        let rect = ctx
            .read_response(root_ui_id(&ctx).with((TABSTRIP, tab.label())))
            .unwrap_or_else(|| panic!("tabknop {} tekent geen response", tab.label()))
            .rect;
        assert!(
            tab_vlak(&shapes, rect).is_some(),
            "tabknop {naam} op {rect:?} tekent geen vlak: de strip oogt als losse tekst",
            naam = tab.label()
        );
    }
}

#[test]
fn na_een_klik_blijven_alle_tabknoppen_een_vlak_schilderen() {
    // De regressie zélf: de muis blijft op de aangeklikte tab staan, die knop leest dan `Inactive`,
    // en zonder `frame_when_inactive` verliest juist díe knop zijn vlak.
    let ctx = egui::Context::default();
    baudet_ui::palette::install(&ctx);
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    let target = Tab::Settings;
    click(&ctx, &mut app, target);
    assert_eq!(app.tab(), target, "de klik haalde het tabblad niet — dan bewijst dit niets");
    let shapes = paint(&ctx, &mut app);
    for tab in Tab::ALL {
        let rect = ctx
            .read_response(root_ui_id(&ctx).with((TABSTRIP, tab.label())))
            .expect("tabknop laat geen response na")
            .rect;
        assert!(
            tab_vlak(&shapes, rect).is_some(),
            "na een klik op {doel:?} tekent {naam} geen vlak meer: de strip valt terug op platte tekst",
            doel = target,
            naam = tab.label()
        );
    }
}

#[test]
fn het_actieve_tabblad_is_te_onderscheiden_van_de_andere() {
    // Twee identieke vlakken zijn een strip zonder antwoord: "welke tab staat er" moet uit de
    // geschilderde vulling blijken, niet uit de kleur alleen (`visuals.selection`, gemeten).
    let ctx = egui::Context::default();
    baudet_ui::palette::install(&ctx);
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    let shapes = paint(&ctx, &mut app);
    let vullingen: Vec<egui::Color32> = Tab::ALL
        .iter()
        .map(|tab| {
            let rect = ctx
                .read_response(root_ui_id(&ctx).with((TABSTRIP, tab.label())))
                .expect("tabknop laat geen response na")
                .rect;
            tab_vlak(&shapes, rect).unwrap_or_else(|| panic!("tabknop {} tekent geen vlak", tab.label())).1
        })
        .collect();
    assert_ne!(
        vullingen[0],
        vullingen[1],
        "beide tabbladen vullen {vullingen0:?}: geen verschil te zien",
        vullingen0 = vullingen[0]
    );
}
