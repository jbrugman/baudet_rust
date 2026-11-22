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

/// De minste lucht die twee opeenvolgende widget-kaders van elkaar gescheiden houdt, in pt.
///
/// Gelijk aan [`ROW_GAP`], en dat is precies wat [`lucht_voor_kader`] in `settings.rs` moet opleveren:
/// de bron reserveert `ROW_GAP + 5` pt omdat een kader van een veld 5 pt buiten zijn reservering
/// reikt. Meten wat er tussen de **kaders** staat en niet wat er tussen de reserveringen staat, is de
/// enige vorm van deze test die de klacht kan horen: de klager ziet het kader en niet de reservering.
///
/// De uitzondering op deze maat is een `small_button` naast een veld — die telt niet als onderbuur,
/// want hij hangt aan het veld en niet onder het veld. Daarom filtreert
/// `geen_enkel_veld_vlakt_tegen_zijn_onderbuur` op paren die ook echt onder elkaar staan.
const KADER_VRIJ: f32 = ROW_GAP;

/// Het band waarin het instellingenformulier staat, op het metende scherm van 1200 pt breed.
///
/// Gemeten op deze breedte: het middenpaneel loopt van x = 217 tot x = 1020, het formulier zelf is
/// 360 pt en staat er gecentreerd in (velden en knopen beginnen op x = 389 of 397, en geen enkel
/// veld reikt voorbij x ≈ 710). De grenzen zijn dus geen smaak maar een meting van deze opbouw, en
/// `de_twee_genoemde_buren_ademen` valt luide zodra de opbouw verandert — dan gaat deze band mee.
///
/// Waarom een band en niet "x groter dan iets": de panelen van het venster staan links (t/m 216)
/// én rechts (vanaf 1021) van het formulier, en het scroll-vlak van het midden omvat het hele
/// formulier. Zo'n frame mag zijn inhoud omvatten; twee buren op dezelfde diepte mogen elkaar niet
/// raken. Zonder deze band "overlapt" het rechterscherm elke rij van het formulier.
const FORM_BREED: std::ops::Range<f32> = 300.0..800.0;

/// Het bovenste y dat tot het formulier behoort, op het metende scherm.
///
/// Gemeten: de tabstrip staat op y = 39..71 en de tabknopen vallen binnen `FORM_BREED` — een
/// filter op x alleen dus niet genoeg. Het eerste veld van het formulier begint op y = 108. Wat
/// boven deze grens ligt, is besturing en geen veld, en een buurmeter die beide door elkaar haalt,
/// meet de lay-out van de kop in plaats van die van het formulier.
const FORM_HOOG: f32 = 90.0;

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

/// Of een vlak bij het instellingenformulier hoort: binnen de breedte van de kolom, en onder de
/// tabstrip. Zie [`FORM_BREED`] en [`FORM_HOOG`] voor beide metingen.
fn van_de_formulierkolom(r: &egui::Rect) -> bool {
    FORM_BREED.contains(&r.min.x) && FORM_BREED.contains(&r.max.x) && r.min.y >= FORM_HOOG
}

/// De widget-kaders van het formulier: een gevuld vlak **met een rand**, van boven naar beneden.
///
/// De rand is het onderscheid. Wat meetbaar een element is — het tekstvak, de knop, de combo —
/// tekent een gevuld vlak met een stroke van 1 pt: gemeten `fill #2D3134 stroke 1.0` voor een vak,
/// `fill #4E5052 stroke 1.0` voor een knop. Een gevuld vlak zónder rand is iets anders: het paneel
/// van een zijbalk (`#1B1C1F stroke 0.0`), de galley achter een etiket, het scroll-vlak van het
/// midden. Zo'n vlak omvat zijn inhoud en is geen buur; zonder deze scheiding "overlapt" het
/// middenscherm elke rij van het formulier.
fn vlakken(vormen: &[egui::Shape]) -> Vec<egui::Rect> {
    let mut recten: Vec<egui::Rect> = vormen
        .iter()
        .filter_map(|shape| match shape {
            egui::Shape::Rect(r)
                if r.fill != egui::Color32::TRANSPARENT && r.stroke.width > 0.0 && r.rect.height() > 6.0 =>
            {
                Some(r.rect)
            }
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
fn geen_enkel_veld_vlakt_tegen_zijn_onderbuur() {
    // De klacht ging over elementen, en een element is hier een vlak. Twee vragen, beide gemeten:
    // (1) bestaan er paren die elkaar in het geheel **raken of overlappen** — die zijn per definitie
    // plakken, en de vorige versie van deze test liet ze lopen, want hij eiste alleen `vrij > 0.0` en
    // telde paren mét lucht; (2) heeft elk paar dat elkaár in y niet snijdt minstens [`ROW_GAP`] vrij.
    // Paren op dezelfde rij (naast elkaar) zijn geen buren en vallen buiten beide tellingen.
    let geschilderd = settings();
    // Alleen de veldkolom. Een `vlakken()`-overal-meeting vangt ook de panelen van het venster: het
    // linker.projectvenster en het scroll-vlak van het midden omvatten het hele formulier, dus die
    // "overlappen" elke rij — wat geen plakken is, maar een kader. Het_frame-van-een-omhulsel mag
    // zijn inhoud omvatten; twee buren op dezelfde diepte mogen elkaar niet raken.
    let vlakken: Vec<egui::Rect> =
        vlakken(&geschilderd.vormen).into_iter().filter(|r| van_de_formulierkolom(r)).collect();
    assert!(vlakken.len() >= 6, "het formulier tekent maar {aantal} veldvlakken", aantal = vlakken.len());
    let mut buren = 0;
    for (i, boven) in vlakken.iter().enumerate() {
        for beneden in &vlakken[i + 1..] {
            if beneden.max.x <= boven.max.x && boven.min.x >= beneden.min.x && beneden.max.y <= boven.max.y {
                continue; // een vlak binnen een rij: besturing, geen onderbuur
            }
            if beneden.max.x <= boven.min.x || beneden.min.x >= boven.max.x {
                continue; // naast elkaar op één rij: geen buren
            }
            if beneden.min.y < boven.max.y && boven.min.y < beneden.max.y {
                panic!("vlakken {boven:?} en {beneden:?} overlappen elkaar in y: dat is plakken");
            }
            if beneden.min.y < boven.max.y {
                continue; // hij omvat de andere (een frame rond een rij): geen buurpaar
            }
            let vrij = beneden.min.y - boven.max.y;
            // Wat "plakken" is, is hier gemeten en niet berekend. Een `form_row` reserveert 20 pt
            // voor een veld en tekent er een kader van 28 pt omheen: 4 pt uitsteek boven en 4 pt
            // onder. `item_spacing.y` (6 pt) gaat vóór die uitsteek van het veld erboven af, dus
            // de lucht die een rij werkelijk overhoudt is `ROW_GAP - 4` en niet `ROW_GAP` —
            // kalibratie met twee identieke velden onder elkaar: 0 pt lucht -> −5 pt, 6 -> 1,
            // **8 -> 3**, 12 -> 7, 16 -> 11. Wie `ROW_GAP` met 4 optelt om "8 pt tussen de kaders"
            // te krijgen, wint dus 0 pt op de plaatsing van de rits en verliest hem op elke
            // groepsband. Wat hier wél doorbroken wordt, is een vergeten `add_space`: dan resteert
            // 6 pt, dus 1 pt — en bij de bedieningsrijen onder het adres, die buiten `form_row`
            // staan, was dat vóór de fix −4 pt: twee kaders in elkaar, precies de klacht.
            assert!(
                vrij >= KADER_VRIJ,
                "vlakken {boven:?} en {beneden:?} laten maar {vrij:.1} pt vrij: een rij met                  [`ROW_GAP`] van {rij:.0} pt houdt er {kader:.0} pt van over",
                rij = ROW_GAP,
                kader = KADER_VRIJ
            );
            buren += 1;
        }
    }
    assert!(buren >= 6, "slechts {buren} buurenparen gecontroleerd in {vlakken:?}: de meting is te dun");
}

/// De twee paren die de gebruiker met naam noemde, apart bevestigd.
///
/// De algemene test hierboven zegt dát er niets plakt; deze zegt dat de twee specifieke klachten —
/// "tussen base-url en de fetch-knop zit geen ruimte" en "tussen reasoning-effort en top_p ook niet" —
/// zijn gehoord, en niet per ongeluk groen werden. Beide paren plakten echt: 4 pt **overlapping**,
/// want de knoppen onder het adres erven het `item_spacing.y` van het venster (6 pt) en de
/// redeneer-kiezer droeg als "eerste rij van een groep" geen lucht terwijl hij op `top_p` volgt.
#[test]
fn de_twee_genoemde_buren_ademen() {
    let geschilderd = settings();
    let vlakken = vlakken(&geschilderd.vormen);
    // Het adres is het hoogste veld van de veldkolom (x begint op de kolom, en er is maar één veld
    // boven de eerste groepsband), en de Fetch-knop is het vlak dat direct onder zijn onderkant
    // begint. De twee sampling-vakken zijn de laagste twee vlakken vóór de redeneer-kiezer.
    let veldkolom: Vec<egui::Rect> = vlakken.iter().copied().filter(|r| van_de_formulierkolom(r)).collect();
    assert!(veldkolom.len() >= 6, "te weinig velden in de kolom om de klachten te vinden: {veldkolom:?}");
    let adres =
        *veldkolom.iter().find(|r| r.min.y < 150.0 && r.min.x < 420.0).expect("het adres-veld van het endpoint");
    let ernaast = veldkolom
        .iter()
        .filter(|r| r.min.y >= adres.max.y - 40.0 && r.min.y < adres.max.y + 80.0 && **r != adres)
        .min_by(|a, b| a.min.y.total_cmp(&b.min.y))
        .expect("geen bediening onder het adres-veld gevonden");
    let vrij = ernaast.min.y - adres.max.y;
    assert!(
        vrij >= KADER_VRIJ,
        "de knop onder het adres ({ernaast:?}) staat {vrij:.1} pt onder het adres zelf ({adres:?})"
    );

    // De redeneer-kiezer is het combo-vlak dat in de band met de sampling-vakken het laagst ligt.
    let kiezers: Vec<egui::Rect> =
        veldkolom.iter().copied().filter(|r| r.min.y > adres.max.y && r.min.y < 400.0).collect();
    assert!(kiezers.len() >= 3, "de sampling-band is te dun om de redeneer-kiezer te vinden: {kiezers:?}");
    let onderste = kiezers.iter().max_by(|a, b| a.min.y.total_cmp(&b.min.y)).copied().unwrap();
    let bovenbuur = kiezers
        .iter()
        .filter(|r| r.max.y <= onderste.min.y)
        .max_by(|a, b| a.max.y.total_cmp(&b.max.y))
        .copied()
        .expect("geen vak boven de redeneer-kiezer");
    let vrij = onderste.min.y - bovenbuur.max.y;
    assert!(
        vrij >= ROW_GAP,
        "de redeneer-kiezer ({onderste:?}) plakt tegen het vak erboven ({bovenbuur:?}): {vrij:.1} pt vrij"
    );
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
