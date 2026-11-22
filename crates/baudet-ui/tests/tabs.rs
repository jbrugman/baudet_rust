//! Het middenvlak kiest één tab, en de strip schakelt die — bewezen met een klik, niet met een gok.
//!
//! De vorige versie van dit bestand printte `MEET …: JA/nee` voor zeven *geraden* adressen en had
//! geen enkele assert; alle zeven zeiden `nee`, dus het bewees niets. Deze test doet maar twee dingen,
//! en beide zijn gemeten vóórdat ze hier stonden:
//!
//!  * **Een tabknop is van buiten te vinden.** De knoppen staan in een `UiBuilder::id(..)`-wikkel met
//!    click-sense op `root_ui_id(ctx).with((TABSTRIP, label))` (zie `TABSTRIP` in app.rs): een
//!    expliciet id registreert zijn response op *exact* dat adres, dus `read_response` vindt het
//!    zonder de paneel-lay-out na te bouwen. Een `.id_salt`-veld doet dat níét — dat adresseert
//!    relatief aan de ouder-`Ui`, en geen enkele root-keten gaf hetzelfde adres (gemeten). Daarom
//!    test dit bestand de strip en de models-body (beide expliciet-id) en reconstrueert het géén
//!    ketens voor de velden.
//!  * **Een klik schakelt de tab.** De rect van een knop komt uit `read_response`; een
//!    `PointerMoved` + `PointerButton{pressed}`/`{release}` op dat middelpunt doet `.clicked()`
//!    uitglijden, precies zoals `App::ui` hem voor een persoon doet. `app.tab()` is de lezing van het
//!    antwoord: hetzelfde veld dat de strip schrijft en de paint-pass leest.

use baudet_ui::{root_ui_id, Tab, TranscriptApp, TABSTRIP};
use eframe::egui;

const SCREEN: [f32; 2] = [1200.0, 800.0];

fn input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN[0], SCREEN[1]))),
        ..Default::default()
    }
}

/// Paint one frame and drop its texture delta. The pass a test drives and the pass a person sees are
/// the same function (`TranscriptApp::paint`), so what this asserts is what the window does.
fn paint_once(ctx: &egui::Context, app: &mut TranscriptApp) {
    ctx.run_ui(input(), |ui| app.paint(ui)).drop_without_applying_deltas();
}

/// De rect van de tabknop, gemeten uit een passive paint. Paniek als de strip hem niet tekent: dan is
/// de voorwaarde van deze test (vindbare knop) niet meer waar, en dat is een fout, geen skip.
fn tab_rect(ctx: &egui::Context, app: &mut TranscriptApp, tab: Tab) -> egui::Rect {
    paint_once(ctx, app);
    let id = root_ui_id(ctx).with((TABSTRIP, tab.label()));
    ctx.read_response(id)
        .map(|response| response.rect)
        .unwrap_or_else(|| panic!("tabknop {} tekent geen response", tab.label()))
}

/// Druk de tabknop met een echte aanwijzer-klik: beweging naar het middelpunt, indruk, loslaat.
/// Twee passen, omdat een klik in egui een press- en een release-pass nodig heeft.
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
    // Het eigen palet, niet het standaard-`Visuals`: de vormen waarmee deze maten tot stand kwamen,
    // zijn die van dit venster (idem `folder_row.rs`).
    baudet_ui::palette::install(&ctx);
    ctx
}

#[test]
fn een_venster_begint_op_chat() {
    // De startkeus is de chat: `with_tab` is de enige manier om van tevoren een ander tabblad te
    // vragen, en zonder die vraag is het antwoord chat — niet een vierde toestand.
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    assert_eq!(app.tab(), Tab::Chat);
}

#[test]
fn elke_tabknop_staat_op_een_vindbaar_adres() {
    // Alle drie de knoppen tekenen in élke pass — de strip verdwijnt niet mee met de body — dus elk
    // adres moet antwoorden, ongeacht welk tabblad open staat.
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
    // De kern van de vraag "staat settings in het midden, niet erboven": de strip is de toegang, en
    // één klik erop zet precies één tabblad op. Per bestemming een vers venster, want de klik moet
    // vanaf chat vertrekken — de strip staat in elk tabblad, maar een wissel is pas bewezen als de
    // bestemming vanuit een andere bestemming kwam.
    let ctx = addressable_context();
    for target in Tab::ALL {
        let mut app = TranscriptApp::new(TranscriptApp::demo_backend()); // begint op chat
        let center = tab_rect(&ctx, &mut app, target).center();
        click_tab(&ctx, &mut app, center);
        assert_eq!(app.tab(), target, "klik op {} zette niet dat tabblad", target.label());
    }
}
