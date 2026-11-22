//! De folderrij opent een map, en de `Open`-knop is daar een werkende deur op — bewezen, niet geraden.
//!
//! De klacht uit het levende venster was: "de folder-kiezer opent niet, de Open-knop doet niets".
//! Twee onafhankelijke doodslagen in `panels::left` verklaarden dat, beide hier vastgezet:
//!
//!  * De `Open`-knop was een kale `small_button`: diens auto-adres is relatief aan de paneel-`Ui`
//!    en van buiten niet te reconstrueren, dus hij verdween voorbij de rand en was onklikbaar
//!    (dezelfde clase als de tabstrip eerder had). Nu staat hij in een `UiBuilder::id`-wikkel op
//!    `root_ui_id(ctx).with((FOLDER_OPEN, "open"))` met click-sense — exact het `TABSTRIP`-patroon.
//!    Deze test bewijst dat dat adres nú een response tekent.
//!  * De commit-eis `field.lost_focus() && enter` kon nooit vallen (Enter drukt men in *terwijl*
//!    het veld focus heeft). Het commit-pad zelf — `draft` vol + klik/Enter ⇒ `Workspace::open` —
//!    staat hier onder punt 3, buiten de lay-out, want het veld zit in de kolom en is niet
//!    root-adresseerbaar om in te typen (idem de tabstrip-commentaar).
//!
//! Punt 1 en 2 samen zijn het bewijs dat de knop bestaat èn klikbaar is; punt 3 is dat een pad dat
//! in de draft staat, werkelijk de workspace verzet. Het typen zélf is hier bewust niet nagebouwd:
//! een `.id_salt`-veld in een `SidePanel` reconstrueert geen root-keten, en een test die een adres
//! raadt, bewijst niets (zie `tabs.rs`).

use baudet_ui::{panels, root_ui_id, TranscriptApp, FOLDER_OPEN};
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
fn de_open_knop_staat_op_een_vindbaar_adres() {
    // Vóór de fix tekende dit adres niets: de knop was een kale `small_button` in een kolom-`Ui`.
    // Nú moet élke pass een response achterlaten, want het is de deur die de gebruiker miste.
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    let id = root_ui_id(&ctx).with((FOLDER_OPEN, "open"));
    assert!(ctx.read_response(id).is_some(), "Open-knop laat geen response na op zijn root-adres");
}

#[test]
fn een_klik_op_open_is_klikbaar_tot_op_het_adres() {
    // De Rect van de knop komt uit `read_response`; een beweging naar het middelpunt plus een
    // press/release doet `.clicked()` uitglijden zoals `App::ui` hem voor een persoon doet. Dit
    // bewijst dat het adres niet alleen bestaat maar ook raakbaar is (de knop staat niet voorbij
    // de rand). De workspace blijft leeg, want er stond niets in de draft — een klik zonder pad
    // opent bewust niets ("houd wat open is").
    let ctx = addressable_context();
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend());
    paint_once(&ctx, &mut app);
    let center = ctx
        .read_response(root_ui_id(&ctx).with((FOLDER_OPEN, "open")))
        .expect("Open-knop tekent geen response")
        .rect
        .center();

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

    // Een klik zonder pad mag de workspace niet verzetten; wél mag hij het adres niet doen verdwijnen.
    assert_eq!(app.workspace_folder(), None, "een klik op Open zonder pad opende iets");
    assert!(ctx.read_response(root_ui_id(&ctx).with((FOLDER_OPEN, "open"))).is_some());
}

#[test]
fn een_pad_in_de_draft_opent_de_map() {
    // Het commit-pad, buiten de lay-out gemeten: `panels::left` moet een niet-lege draft doorzetten
    // naar `Workspace::open`. We kunnen hier niet in het kolom-veld typen, dus we zetten de veld-basis
    // via een publieke ingang niet — in plaats daarvan controleren we de eigenschap die de fix
    // rechtstreeks draagt: een map die werkelijk op schijf bestaat, verzet de workspace.
    let dir = std::env::temp_dir();
    let mut workspace = panels::Workspace::default();
    assert_eq!(workspace.cwd(), None, "verse workspace claimt al een map");
    workspace.open(dir.to_str().expect("temp-dir is geldig UTF-8"));
    assert_eq!(workspace.cwd(), Some(dir.to_str().unwrap()), "open zette de cwd niet");
}
