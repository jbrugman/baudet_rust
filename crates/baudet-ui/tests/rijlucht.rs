
use baudet_ui::{palette, Tab, TranscriptApp, GROUP_GAP, ROW_GAP};
use eframe::egui;

const SCREEN: [f32; 2] = [1200.0, 1600.0];

const KADER_VRIJ: f32 = ROW_GAP;

const FORM_BREED: std::ops::Range<f32> = 300.0..1000.0;

const FORM_HOOG: f32 = 90.0;

fn input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN[0], SCREEN[1]))),
        ..Default::default()
    }
}

struct Geschilderd {
    vormen: Vec<egui::Shape>,
    midden: std::ops::RangeInclusive<f32>,
}

fn settings() -> Geschilderd {
    let ctx = egui::Context::default();
    palette::install(&ctx);
    let mut app = TranscriptApp::new(TranscriptApp::demo_backend()).with_tab(Tab::Settings);
    let out = ctx.run_ui(input(), |ui| app.paint(ui));
    let vormen: Vec<egui::Shape> = out.shapes.iter().map(|c| c.shape.clone()).collect();
    let panel = |id| egui::containers::PanelState::load(&ctx, egui::Id::new(id)).unwrap().outer_rect;
    let midden = panel("projects").right()..=panel("filetree").left();
    out.drop_without_applying_deltas();
    drop(ctx);
    Geschilderd { vormen, midden }
}

fn scheidingen(geschilderd: &Geschilderd) -> Vec<f32> {
    let mut ys: Vec<f32> = geschilderd
        .vormen
        .iter()
        .filter_map(|shape| match shape {
            egui::Shape::LineSegment { points, .. } => {
                let [p0, p1] = *points;
                ((p0.y - p1.y).abs() < 0.5
                    && p0.y >= FORM_HOOG
                    && geschilderd.midden.contains(&p0.x)
                    && geschilderd.midden.contains(&p1.x)
                    && (p1.x - p0.x).abs() > 180.0)
                    .then_some(p0.y)
            }
            _ => None,
        })
        .collect();
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ys.dedup();
    ys
}

fn van_de_formulierkolom(r: &egui::Rect) -> bool {
    FORM_BREED.contains(&r.min.x) && FORM_BREED.contains(&r.max.x) && r.min.y >= FORM_HOOG
}

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
    let geschilderd = settings();
    let ys = scheidingen(&geschilderd);
    assert!(
        ys.len() >= 6,
        "het instellingenformulier tekent maar {aantal} groepscheidingen: de body is niet geschilderd",
        aantal = ys.len()
    );
}

#[test]
fn twee_groepsbanden_plakken_niet_tegen_elkaar() {
    let geschilderd = settings();
    let ys = scheidingen(&geschilderd);
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
    let geschilderd = settings();
    let vlakken: Vec<egui::Rect> =
        vlakken(&geschilderd.vormen).into_iter().filter(|r| van_de_formulierkolom(r)).collect();
    assert!(vlakken.len() >= 6, "het formulier tekent maar {aantal} veldvlakken", aantal = vlakken.len());
    let mut buren = 0;
    for (i, boven) in vlakken.iter().enumerate() {
        for beneden in &vlakken[i + 1..] {
            if beneden.max.x <= boven.max.x && boven.min.x >= beneden.min.x && beneden.max.y <= boven.max.y {
                continue;
            }
            if beneden.max.x <= boven.min.x || beneden.min.x >= boven.max.x {
                continue;
            }
            if beneden.min.y < boven.max.y && boven.min.y < beneden.max.y {
                panic!("vlakken {boven:?} en {beneden:?} overlappen elkaar in y: dat is plakken");
            }
            if beneden.min.y < boven.max.y {
                continue;
            }
            let vrij = beneden.min.y - boven.max.y;
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

#[test]
fn de_twee_genoemde_buren_ademen() {
    let geschilderd = settings();
    let vlakken = vlakken(&geschilderd.vormen);
    let veldkolom: Vec<egui::Rect> = vlakken.iter().copied().filter(|r| van_de_formulierkolom(r)).collect();
    assert!(veldkolom.len() >= 6, "te weinig velden in de kolom om de klachten te vinden: {veldkolom:?}");
    let adres =
        *veldkolom.iter().find(|r| r.min.y < 150.0 && r.min.x < 700.0).expect("het adres-veld van het endpoint");
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
    assert!(
        ROW_GAP >= 8.0 && GROUP_GAP > ROW_GAP,
        "de marges van het formulier zijn {rij:.0}/{groep:.0} pt: de rijlucht staat onder de 8 pt \
         van de origineler",
        rij = ROW_GAP,
        groep = GROUP_GAP
    );
}
