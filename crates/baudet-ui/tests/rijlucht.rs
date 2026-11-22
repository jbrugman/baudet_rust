//! De velden en groepsbanden van het instellingenformulier plakken niet tegen elkaar.
//!
//! De klacht luidde "er mag wat meer ruimte tussen de elementen, ze plakken nu tegen elkaar aan".
//! Voor de tabstrip bestond daar al een maat op (`tests/tab_vlak.rs`), maar niet voor het formulier in
//! het midden — en precies dát is waarom die klacht het kon overleven: er bestond niets dat hem groen
//! of rood kon maken. Een assert op een stijlveld had hem ook deze keer groen gelaten, want wat telt is
//! de **lucht tussen de geschilderde vormen**, en die ontstaat pas in een paint-pass uit het samenspel
//! van `item_spacing.y`, de `add_space` van `form_row_with_gap` en de `spacing` van de scheiding.
//!
//! Waarom dit bestand vormen leest en geen adressen, staat erboven bij `FIELD_ENDPOINT` in
//! `settings.rs`: de velden van dit formulier dragen een `id_salt`, en dat is een adres *relatief aan
//! zijn ouder* — geen van buiten reconstrueerbaar root-adres (opnieuw gemeten toen dit bestand dat
//! probeerde: `read_response` gaf voor élk zout `None`). Wat wél van buiten te zien is, is wat er staat
//! te liggen: de scheidingslijn (`painter.hline`, dus een `Shape::LineSegment`) en de vlakken van de
//! velden (`Shape::Rect`). Posities worden niet geassert op waarde maar op **vrijheid tussen buren**:
//! posities hangen af van de vensterhoogte, en vrijheid is precies wat de klacht noemt.
//!
//! Eén `paint` per test, met dezelfde `Context` die de vormen opleverde: `read_response` leest de
//! responses van de *vorige* pass op dat platform, dus een-context-per-paint laat geen enkel adres na.

use baudet_ui::{palette, Tab, TranscriptApp, GROUP_GAP, ROW_GAP};
use eframe::egui;

const SCREEN: [f32; 2] = [1200.0, 1600.0];

fn input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN[0], SCREEN[1]))),
        ..Default::default()
    }
}

/// Een venster op het instellingen-tabblad, geschilderd, met de vormen die het tekende. De context is
/// lokaal voor deze functie: `run_ui` leent hem, en de clonede `Shape`s overleven hem.
struct Geschilderd {
    vormen: Vec<egui::Shape>,
}

fn settings() -> Geschilderd {
    let ctx = egui::Context::default();
    palette::install(&ctx);
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend()).with_tab(Tab::Settings);
    let out = ctx.run_ui(input(), |ui| app.paint(ui));
    let vormen: Vec<egui::Shape> = out.shapes.iter().map(|c| c.shape.clone()).collect();
    out.drop_without_applying_deltas();
    drop(ctx);
    Geschilderd { vormen }
}

/// De horizontale groepscheidingen, van boven naar beneden, als y.
///
/// Een `Separator` roept `painter.hline` (`separator.rs:132`), en `hline` levert één
/// `Shape::LineSegment` van `rect.left()` naar `rect.right()` op `rect.center().y`. Zoek dus
/// horizontale segmenten breder dan de helft van het scherm: dat zijn de groepscheidingen, en geen
/// hairline van een veld — die horen bij een `Rect`-stroke.
fn scheidingen(vormen: &[egui::Shape]) -> Vec<f32> {
    let mut ys: Vec<f32> = vormen
        .iter()
        .filter_map(|shape| match shape {
            egui::Shape::LineSegment { points, .. } => {
                let [p0, p1] = *points;
                ((p0.y - p1.y).abs() < 0.5 && (p1.x - p0.x).abs() > SCREEN[0] * 0.35).then_some(p0.y)
            }
            _ => None,
        })
        .collect();
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ys.dedup();
    ys
}

/// Elk gevulde, afgeronde vlak dat een veld of knop is, gesorteerd op y.
fn vlakken(vormen: &[egui::Shape]) -> Vec<egui::Rect> {
    let mut recten: Vec<egui::Rect> = vormen
        .iter()
        .filter_map(|shape| match shape {
            egui::Shape::Rect(r) if r.fill != egui::Color32::TRANSPARENT && r.rect.height() > 6.0 => Some(r.rect),
            _ => None,
        })
        .collect();
    recten.sort_by(|a, b| a.min.y.partial_cmp(&b.min.y).unwrap());
    recten
}

#[test]
fn het_formulier_schildert_zijn_groepscheidingen() {
    // Zonder deze zekerheid is de rest een lege overwinning: tekent het middenvlak geen formulier, dan
    // zijn er geen banden om lucht tussen te meten. Het venster start op chat (`tests/tabs.rs`), dus
    // zonder `with_tab` staat hier een transcript en zijn er nul tot twee lijnen — gemeten.
    let geschilderd = settings();
    let ys = scheidingen(&geschilderd.vormen);
    assert!(
        ys.len() >= 6,
        "het instellingenformulier tekent maar {aantal} groepscheidingen: de body is niet geschilderd",
        aantal = ys.len()
    );
}

#[test]
fn twee_groepsbanden_plakken_niet_tegen_elkaar() {
    // Twee scheidingen omvatten altijd minstens één rij. Tussen twee lijnen zit dan: de helft van de
    // eigen ruimte van elke lijn (een scheiding tekent in het midden van zijn `spacing`) plus de
    // lucht van wat ertussen staat. Vóór de meting was die eigen ruimte de stijlnorm van 6 pt
    // (`SeparatorStyle::separator_style`: `spacing: 6.0`), en de klacht was dat dat plakken was.
    // Deze ondergrens is daarom geen verzinsel: GROUP_GAP is de marge die `group_break` zet.
    let geschilderd = settings();
    let ys = scheidingen(&geschilderd.vormen);
    assert!(ys.len() >= 2, "te weinig banden om lucht tussen te meten: {ys:?}");
    for paar in ys.windows(2) {
        let vrij = paar[1] - paar[0];
        assert!(
            vrij >= GROUP_GAP,
            "twee groepscheidingen op {y0:.0} en {y1:.0} laten maar {vrij:.1} pt: minder dan één              groepsband van {band:.0} pt, dus de groepen lezen als één blok",
            y0 = paar[0],
            y1 = paar[1],
            band = GROUP_GAP
        );
    }
}

#[test]
fn een_veld_vlakt_niet_tegen_zijn_onderbuur() {
    // De klacht ging over elementen, en een element is hier een vlak. Twee vlakken die elkaar raken,
    // zijn één blok: vandaar deze assert op vrije ruimte tussen opeenvolgende buren in de
    // veldkolom. Gelijkbenige vlakken (dezelfde rij, naast elkaar) zijn geen buren en vallen buiten
    // deze meting — daarom alleen paren die elkáár in y niet overlappen.
    let geschilderd = settings();
    let vlakken = vlakken(&geschilderd.vormen);
    assert!(vlakken.len() >= 6, "het formulier tekent maar {aantal} vlakken", aantal = vlakken.len());
    let mut gecontroleerd = 0;
    for paar in vlakken.windows(2) {
        let (boven, beneden) = (paar[0], paar[1]);
        if beneden.min.y < boven.max.y {
            continue; // dezelfde rij of een wikkel om het bovenste vlak: geen opeenvolgende buren
        }
        let vrij = beneden.min.y - boven.max.y;
        assert!(
            vrij > 0.0,
            "vlakken {boven:?} en {beneden:?} raken elkaar: {vrij:.1} pt lucht tussen twee veldvlakken",
        );
        gecontroleerd += 1;
    }
    assert!(gecontroleerd > 0, "geen enkel paar opeenvolgende vlakken gevonden in {vlakken:?}");
}

#[test]
fn de_marges_zijn_de_gematen_waarden_en_geen_smaak() {
    // De origineler zet Insets(4, ..) aan lélke cel van lélke rij (`Form.kt:16`), wat aan twee
    // zijden van een cel 4 + 4 = 8 pt tussen twee rijen geeft. `ROW_GAP` is hier de enige
    // rijlucht, dus daaronder is een verslechtering die niemand vroeg — en dát was de klacht.
    // `GROUP_GAP` is groter dan `ROW_GAP`, want een groepsband is geen rij.
    assert!(
        ROW_GAP >= 8.0 && GROUP_GAP > ROW_GAP,
        "de marges van het formulier zijn {rij:.0}/{groep:.0} pt: de rijlucht staat onder de 8 pt \
         van de origineler",
        rij = ROW_GAP,
        groep = GROUP_GAP
    );
}
