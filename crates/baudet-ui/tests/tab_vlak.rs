
use baudet_ui::{root_ui_id, tab_size, Tab, TranscriptApp, TABSTRIP, TAB_GAP};
use eframe::egui;

const SCREEN: [f32; 2] = [1200.0, 800.0];

fn input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN[0], SCREEN[1]))),
        ..Default::default()
    }
}

fn paint_with(ctx: &egui::Context, app: &mut TranscriptApp, frame: egui::RawInput) -> Vec<egui::Shape> {
    let out = ctx.run_ui(frame, |ui| app.paint(ui));
    let shapes: Vec<egui::Shape> = out.shapes.clone().iter().map(|c| c.shape.clone()).collect();
    out.drop_without_applying_deltas();
    shapes
}

fn paint(ctx: &egui::Context, app: &mut TranscriptApp) -> Vec<egui::Shape> {
    paint_with(ctx, app, input())
}

fn tab_maten(ctx: &egui::Context, bak: &mut Option<egui::Vec2>) -> Vec<egui::Rect> {
    let mut recten = Vec::new();
    let output = ctx.run_ui(input(), |ui| {
        *bak = Some(tab_size(ui));
        recten = Tab::ALL
            .iter()
            .map(|tab| {
                ctx.read_response(root_ui_id(ctx).with((TABSTRIP, tab.label())))
                    .unwrap_or_else(|| panic!("tabknop {} tekent geen response", tab.label()))
                    .rect
            })
            .collect();
    });
    output.drop_without_applying_deltas();
    recten
}

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
        paint_with(ctx, app, frame);
    }
}

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

#[test]
fn de_tabknoppen_plakken_niet_tegen_elkaar() {
    let ctx = egui::Context::default();
    baudet_ui::palette::install(&ctx);
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    let vormen = paint(&ctx, &mut app);
    let mut bak = None;
    let recten = tab_maten(&ctx, &mut bak);
    let bak = bak.expect("de strip meet geen tab-maat");
    for rect in &recten {
        assert!(
            (rect.width() - bak.x).abs() < 0.5 && (rect.height() - bak.y).abs() < 0.5,
            "tabknop {rect:?} vult zijn bak van {bak:?} niet: de strip deelt bredere sloten uit dan hij tekent",
        );
    }
    for paar in recten.windows(2) {
        let (vorige, volgende) = (paar[0], paar[1]);
        assert!(
            volgende.min.x - vorige.max.x >= TAB_GAP - 0.5,
            "tabknoppen {vorige:?} en {volgende:?} laten {vrij:.1} pt lucht, minder dan de {luchtmaat:.0} pt van TAB_GAP",
            vrij = volgende.min.x - vorige.max.x,
            luchtmaat = TAB_GAP
        );
        assert!(
            (volgende.min.y - vorige.min.y).abs() < 0.5 && (volgende.max.y - vorige.max.y).abs() < 0.5,
            "de knoppen {vorige:?} en {volgende:?} hangen niet op één lijn: de strip breekt",
        );
        assert!(
            tab_vlak(&vormen, vorige).is_some(),
            "tabknop op {vorige:?} bevat geen gevuld vlak: de strip oogt als losse tekst",
        );
    }
}
