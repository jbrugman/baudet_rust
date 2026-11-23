//! Wat een gesprek overleeft, en wat het er niet voor hoeft te betalen.
//!
//! De eis achter deze suite is niet "er staat een bestand" maar de twee prijzen eraan: een
//! herstart moet het gesprek teruggeven zoals het eruitzag, en een gesprek van honderd beurten
//! mag een venster niet honderd beurten laten laden om de laatste te tonen. Daarom meet deze
//! suite ook de grenzen: hoeveel er bij een opening wordt teruggelezen, en hoeveel er bij het
//! opslaan van één beurt wordt geschreven.
//!
//! Alles hier draait zonder server en zonder venster: een [`ScriptedBackend`] speelt de beurt,
//! een tijdelijke map doet de opslag. Dat is de scheiding die deze laag bewijst - de UI tekent
//! wat `AppState` draagt en draagt zelf niets.

use baudet_app::session::pump;
use baudet_app::AppState;
use baudet_backend::scripted::{Script, ScriptedBackend};
use baudet_backend::Backend;
use baudet_core::*;
use baudet_store::repository::page;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Een map die er nog niet is, met een naam die parallelle tests uit elkaar houdt.
fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("baudet-persist-{}-{}-{}", name, std::process::id(), line!()))
}

/// De events van één antwoord, in de vorm die de transport-laag zelf stuurt: openen ledig,
/// streamen, en sluiten met een ledig exemplaar - want `close_open_rows` (openai.rs) zendt de
/// tekst niet opnieuw, de reductor houdt hem vast.
fn answer(turn: &str, text: &str) -> Vec<ThreadEvent> {
    vec![
        ThreadEvent::ItemStarted {
            thread_id: "th_1".into(),
            turn_id: turn.into(),
            item: Item::AgentMessage(AgentMessageItem {
                item_id: format!("{turn}/msg"),
                text: String::new(),
                status: ItemStatus::InProgress,
            }),
        },
        ThreadEvent::ItemDelta {
            thread_id: "th_1".into(),
            turn_id: turn.into(),
            item_id: format!("{turn}/msg"),
            kind: ItemKind::AgentMessage,
            delta: text.into(),
        },
        ThreadEvent::ItemCompleted {
            thread_id: "th_1".into(),
            turn_id: turn.into(),
            item: Item::AgentMessage(AgentMessageItem {
                item_id: format!("{turn}/msg"),
                text: String::new(),
                status: ItemStatus::Completed,
            }),
        },
    ]
}

/// Een handvat op een gesprek dat op schijf schrijft, plus de speler die de beurten speelt.
fn persistent(root: &std::path::Path, scripts: Vec<Vec<ThreadEvent>>) -> (Arc<ScriptedBackend>, Arc<AppState>) {
    let scripts: Vec<Script> = scripts
        .into_iter()
        .map(|events| events.into_iter().fold(Script::new(), |script, event| script.event(event)))
        .collect();
    let backend = Arc::new(ScriptedBackend::new(scripts));
    let state = AppState::new(Arc::clone(&backend) as Arc<dyn Backend>, "systeem")
        .with_persistence(root, "model-van-hier")
        .expect("de map is te openen");
    (backend, Arc::new(state))
}

/// De beurt zoals een venster hem afhandelt, plus de opslag die erbij hoort.
fn run_turn(state: &Arc<AppState>, text: &str) {
    let sent = state.send(text).expect("de beurt moet opeisbaar zijn");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let handle = sent.take_reader().expect("één lezer per beurt");
    let folder = Arc::clone(state);
    let reader = std::thread::spawn(move || pump(&handle, |event| folder.apply(event), &tx));
    while rx.blocking_recv().is_some() {}
    reader.join().unwrap();
    state.finish();
    // De opslag is geen bijvangst van de beurt maar zijn afsluiting, dus de aanroeper - het
    // venster, in het echt - doet hem hier. Een fout mag het scherm niet stilzetten.
    if let Some(reason) = state.persist() {
        panic!("opslaan mislukte: {reason}");
    }
}


/// Een rij met **exact** deze tekst, niet één die die tekst als voorzetsel bevat.
///
/// Nodig omdat de paginarenaam "vraag 1" een voorzetsel is van "vraag 10" tot "vraag 19": een
/// `contains`-teller gaf dan elf treffers op een gesprek dat één "vraag 1" bevat, en dat meet niet
/// "staat deze rij er" maar "staat er iets dat erop lijkt". Exact tellen is wat de asserts bedoelen.
fn mentions_exact(state: &AppState, tekst: &str) -> usize {
    state.rows().iter().filter(|row| baudet_app::reducer::streamed_text(row) == tekst).count()
}

#[test]
fn een_beurt_overleeft_het_handvat() {
    let root = scratch("herstart");
    let (_, state) = persistent(&root, vec![answer("tu_1", "het eerste antwoord")]);
    run_turn(&state, "vraag een");
    assert!(state.persists(), "met een map is dit gesprek niet puur in het geheugen");

    // De herstart: een leeg handvat op dezelfde map, zonder backend-verleden.
    let id = state.session_id();
    let reopened = AppState::new(Arc::new(ScriptedBackend::new(vec![])) as Arc<dyn Backend>, "systeem")
        .on_session(&root, &id, "model-van-hier", None)
        .expect("hetzelfde gesprek, dezelfde map");

    let rows = reopened.rows();
    let bodies: Vec<String> = rows.iter().map(baudet_app::reducer::streamed_text).collect();
    assert_eq!(bodies, ["vraag een", "het eerste antwoord"]);
}

#[test]
fn een_herstart_leest_niet_het_hele_verleden() {
    // De paginaregel van de origineler (`AppState.buildSession` leest `INITIAL_ITEMS_PAGE`, niet
    // het bestand): een gesprek van veel beurten openen mag niet alles laden, en moet expliciet
    // zeggen dat er ouder is. Zonder deze grens zou een lang gesprek een venster openen dat
    // eerst zijn hele geschiedenis doorloopt - traag, en trager naarmate het langer duurt.
    let root = scratch("paging");
    let turns = 25;
    let scripts: Vec<Vec<ThreadEvent>> =
        (1..=turns).map(|index| answer(&format!("tu_{index}"), &format!("antwoord {index}"))).collect();
    let (_, state) = persistent(&root, scripts);
    for index in 1..=turns {
        run_turn(&state, &format!("vraag {index}"));
    }
    let total = (turns * 2) as u64;


    let id = state.session_id();
    let reopened = AppState::new(Arc::new(ScriptedBackend::new(vec![])) as Arc<dyn Backend>, "systeem")
        .on_session(&root, &id, "model-van-hier", None)
        .expect("hetzelfde gesprek, dezelfde map");

    assert_eq!(reopened.rows().len() as u64, page::INITIAL as u64, "openen leest de staart, niet het bestand");
    assert_eq!(baudet_app::reducer::streamed_text(reopened.rows().last().unwrap()), format!("antwoord {turns}"));
    let (has_more_older, cursor, _) = reopened.paging();
    assert!(has_more_older, "er staan {total} rijen op schijf, dit beeld houdt er {initial}", initial = page::INITIAL);
    assert!(cursor.is_some(), "een cursor hoort er te zijn zolang er ouder is");

    // De eis is niet "mag stoppen bij de eerste vraag" maar "de cursor boekt af tot index 0". Een
    // vraag kan met haar antwoord op een venstergrens in dezelfde pagina vallen, dus `mentions` zegt
    // pas iets ná de volgende voorzet; daarom doorlopen tot de opslag niets ouder meer houdt.
    let mut walked = 0usize;
    while let (true, Some(before), _) = reopened.paging() {
        let added = reopened.load_older(before).expect("een ouder venster");
        if added == 0 {
            break;
        }
        walked += added;
        if walked > total as usize {
            panic!("de cursor boekt niet af: {walked} rijen geladen van {total}");
        }
    }
    let (has_more_older, _, _) = reopened.paging();
    assert!(!has_more_older, "scrollen eindigt bij de allereerste rij, niet halverwege");
    assert_eq!(reopened.rows().len() as u64, total, "elke rij één keer en de volledige lading: {} van {total}", reopened.rows().len());
    let actual: Vec<_> = reopened.rows().iter().map(baudet_app::reducer::streamed_text).collect();
    let expected: Vec<_> =
        (1..=turns).flat_map(|index| [format!("vraag {index}"), format!("antwoord {index}")]).collect();
    assert_eq!(actual, expected, "terugscrollen behoudt de gespreksvolgorde");
}

#[test]
fn opslaan_schrijft_per_beurt_allen_de_staart() {
    // Append-only is de hele prijs van dit ontwerp: een beurt mag niet het hele gesprek
    // herschrijven. Dit meet het aantal regels op schijf, niet het aantal bytes - een regel per
    // feit is de belofte, en een herschreven verleden zou er twee van maken.
    let root = scratch("tail");
    let (_, state) = persistent(&root, vec![answer("tu_1", "antwoord een"), answer("tu_2", "antwoord twee")]);
    run_turn(&state, "vraag een");
    let lines_een = count_lines(&root);
    run_turn(&state, "vraag twee");
    let lines_twee = count_lines(&root);

    assert_eq!(lines_een, 2, "één beurt laat precies vraag en antwoord achter: {lines_een}");
    let growth = lines_twee - lines_een;
    assert_eq!(growth, 2, "de tweede beurt hangt zijn eigen twee regels aan, niet het hele gesprek: {growth}");
    assert_eq!(mentions_exact(&state, "vraag een"), 1, "het verleden wordt niet opnieuw geschreven");
}

/// Het project van een gesprek dat via het handvat werd geopend, mag niet verdwijnen.
///
/// `on_session` zette `project_id` op `None` omdat de kop het veld niet kende. Na die
/// reparatie draagt `detail.jsonl` het project, dus een herstart geeft de koppeling terug
/// zonder dat de catalogus in de buurt hoeft te zijn.
#[test]
fn een_herstart_houdt_het_project_van_het_gesprek() {
    let root = scratch("project");
    let state = AppState::new(Arc::new(ScriptedBackend::new(vec![])) as Arc<dyn Backend>, "systeem")
        .on_session(&root, "s-project", "model-van-hier", Some("p-tuis"))
        .expect("een nieuw gesprek op schijf");
    // Pas een opslag maakt de kopregel: vóórdat er iets werd geschreven, was er geen kop.
    if let Some(reason) = state.persist() {
        panic!("opslaan mislukte: {reason}");
    }

    let reopened = AppState::new(Arc::new(ScriptedBackend::new(vec![])) as Arc<dyn Backend>, "systeem")
        .on_session(&root, "s-project", "model-van-hier", None)
        .expect("hetzelfde gesprek, dezelfde map");
    assert_eq!(reopened.project().as_deref(), Some("p-tuis"), "de kop draagt het project");
}

/// De twee bewegingen van een gesprekswissel: lezen mag overal, ruilen alleen als er niemand
/// schrijft. `open_store` raakt de staat van het handvat niet; `switch_to` is wat het wél doet.
#[test]
fn openen_ergens_raakt_de_huidige_staat_niet_aan() {
    let root = scratch("wissel");
    let (_, state) = persistent(&root, vec![answer("tu_1", "het eerste antwoord")]);
    run_turn(&state, "vraag een");
    let hier = state.session_id();

    // Dezelfde map, een ander gesprek: de lezing mag er niets van merken op het scherm.
    let (opened, session) = state
        .open_store(&root, "s-ander", "model-van-hier", Some("p-ander"))
        .expect("een tweede gesprek op dezelfde map");
    assert_eq!(state.session_id(), hier, "er is nog niets geruild");
    assert_eq!(state.rows().len(), 2, "het scherm draagt nog steeds dit gesprek");
    assert_eq!(session.project_id.as_deref(), Some("p-ander"), "het project komt mee uit de keuze");

    let project = state.switch_to(opened, session);
    assert_eq!(project.as_deref(), Some("p-ander"), "de ruil geeft het project terug");
    assert_eq!(state.session_id(), "s-ander", "dit handvat bedient nu dat gesprek");
    assert!(state.rows().is_empty(), "een leeg gesprek tekent een leeg beeld");
    assert!(state.persists(), "het handvat ging mee, anders schreef het volgende feit in het oude bestand");
}

/// Het gesprek waarnaar werd geruild, is het gesprek dat opslaat.
///
/// Dit is de fout die de `Clone`-route zou maken: twee handvaten op hetzelfde bestand. Na
/// `switch_to` mag een beurt in het oude spoor geen regel achterlaten.
#[test]
fn na_de_wissel_schrijft_de_beurt_in_het_nieuwe_spoor() {
    let root = scratch("spoor");
    let (_backend, state) = persistent(&root, vec![answer("tu_1", "hier"), answer("tu_2", "ginder")]);
    run_turn(&state, "vraag hier");
    let hier = state.session_id();

    let (opened, session) = state.open_store(&root, "s-ginder", "model-van-hier", None).expect("open");
    state.switch_to(opened, session);
    run_turn(&state, "vraag ginder");

    assert_eq!(count_lines_in(&root, "s-ginder"), 2, "de beurt hangt aan het gesprek dat openstaat");
    assert_eq!(count_lines_in(&root, &hier), 2, "het vorige gesprek houdt zijn eigen twee regels");
}

/// Een herstart nummert dóór waar het geladen gesprek ophield.
///
/// De beurtnamer is proces-breed en start bij 0, maar de id's van een geladen gesprek zijn al
/// namen (`tu_1/user`, `tu_1/msg`). Zónder dat de teller bij het laden wordt bijgetrokken, gaf een
/// herstart die een sessie met `tu_1` opende de volgende vraag wéér `tu_1` en panikte de
/// "twee keer opgeslaan"-assertie van de reductor op de worker-thread -- wat daar bleef hangen
/// als "blijft wachten, niets meer te sturen". De fix zaadt de teller tot boven het hoogste
/// `tu_<n>` in de geladen items; deze test legt de beide kanten vast: de eerste beurt na de
/// herstart overleeft, en de regel die hij opent draagt een beurtnummer dat nog niet bestond.
///
/// De scripted backend nummert zelf door (die met een verse speler telt weer vanaf `tu_1`), dus
/// het antwoord dat hij voor de tweede beurt publiceert, hardcodeert `tu_2` -- de id die de
/// gesprekseigenaar ná de fix kiest -- precies zoals de `answer`-fixture hierboven dat ook doet
/// ("een script dat een andere id kiest dan de eigenaar, opent een rij die bij een andere beurt
/// hoort").
#[test]
fn een_herstart_nummert_door_waar_het_gesprek_ophield() {
    let root = scratch("dornummer");
    let (_, state) = persistent(&root, vec![answer("tu_1", "het eerste antwoord")]);
    run_turn(&state, "vraag een");
    let id = state.session_id();

    // De herstart: een fris handvat (teller 0) op dezelfde map, die `tu_1` al draagt.
    let script = answer("tu_2", "het tweede antwoord")
        .into_iter()
        .fold(Script::new(), |s, event| s.event(event));
    let reopened_backend = ScriptedBackend::new(vec![script]);
    let reopened = Arc::new(
        AppState::new(Arc::new(reopened_backend) as Arc<dyn Backend>, "systeem")
            .on_session(&root, &id, "model-van-hier", None)
            .expect("hetzelfde gesprek, dezelfde map"),
    );
    assert_eq!(reopened.rows().len(), 2, "de herstart draagt vraag en antwoord");

    // De tweede beurt mag de eerste niet overnemen: zonder fix paniekte hij hier met
    // "tu_1/user state twee keer opgeslaan".
    run_turn(&reopened, "vraag twee");

    let rows = reopened.rows();
    let bodies: Vec<String> = rows.iter().map(baudet_app::reducer::streamed_text).collect();
    assert_eq!(bodies.len(), 4, "vraag een + antwoord een + vraag twee + antwoord twee: {bodies:?}");
    assert!(bodies.iter().any(|body| body == "vraag twee"), "de nieuwe vraag staat er: {bodies:?}");
    assert!(bodies.iter().any(|body| body == "het tweede antwoord"), "het antwoord van de tweede beurt staat er: {bodies:?}");
    assert_eq!(mentions_exact(&reopened, "vraag een"), 1, "de oude vraag blijft één keer: {bodies:?}");
}

/// Regelentelling in het spoor van één sessie-id, ongeacht welk dagbucket hem draagt.
fn count_lines_in(root: &std::path::Path, session_id: &str) -> usize {
    let mut total = 0usize;
    for day in std::fs::read_dir(root).expect("de data-map") {
        let dir = day.expect("een bucket").path().join(session_id);
        let Ok(items) = std::fs::read_to_string(dir.join("items.jsonl")) else { continue };
        total += items.lines().filter(|line| !line.trim().is_empty()).count();
    }
    total
}

#[test]
fn de_handvat_telling_is_de_schrijftelling() {
    // `disk_items` is wat de statusregel met het beeld vergelijkt ("12 of 40 regels"), dus zijn
    // enige eis is dat hij zegt wat er werkelijk staat: niet het gesprek, niet het model-boek,
    // de regel. `count_lines` telt die regels onafhankelijk, met een lezing van het bestand zelf,
    // dus deze test vergelijkt de handvat-telling met de grondwaarheid in plaats van met nog een
    // telling uit dezelfde bron.
    let root = scratch("telling");
    let (_, state) = persistent(&root, vec![answer("tu_1", "het eerste antwoord"), answer("tu_2", "het tweede antwoord")]);
    assert_eq!(state.disk_items(), Some(0), "een gesprek dat nog niets zei, staat met nul regels op schijf");
    run_turn(&state, "vraag een");
    let na_een = count_lines(&root);
    assert_eq!(state.disk_items(), Some(na_een), "na één beurt: handvat {:#?} tegen schijf {na_een}", state.disk_items());
    assert!(na_een > 0, "er moet iets op schijf staan, anders meet deze test niets");
    run_turn(&state, "vraag twee");
    let na_twee = count_lines(&root);
    assert_eq!(state.disk_items(), Some(na_twee), "na twee beurten: handvat {:?} tegen schijf {na_twee}", state.disk_items());
    assert!(na_twee > na_een, "de tweede beurt moet meer regels hebben achtergelaten");

    // Een handvat zonder opslag heeft geen schrijftelling, en liegt dus ook niet over "opgeslagen".
    let (_, leads) = ledig_handvat();
    assert_eq!(leads.disk_items(), None, "zonder handvat is er niets te tellen");
}

/// Een handvat zonder data-map: het venster dat tekent zonder te schrijven.
fn ledig_handvat() -> (Arc<ScriptedBackend>, Arc<AppState>) {
    let backend = Arc::new(ScriptedBackend::new(vec![]));
    let state = AppState::new(Arc::clone(&backend) as Arc<dyn Backend>, "systeem");
    (backend, Arc::new(state))
}

/// Regelentelling over het item-spoor van één sessie: `items.jsonl`.
///
/// Het spoor is de rijenlijst, precies zoals `Repository::persist` en `persistTo` in de origineler
/// alleen `items` aanhangen. Het secondaire model-boek (`history.jsonl`) en de kop (`detail.jsonl`,
/// door `write_detail` per beurt herschreven) dragen geen beurten en tellen hier niet mee. Een
/// sessie staat twee niveaus diep (`root/<dag>/<id>/`, `rollout::session_dir`), dus deze telling
/// daalt die niveaus af en telt dan één bestand; één niveau zou niets vinden en de staart-meting
/// groen maken op nul, wat juist de meting is waar het om gaat.
fn count_lines(root: &std::path::Path) -> usize {
    const FILES: [&str; 1] = ["items.jsonl"];
    let mut total = 0usize;
    let dag = match std::fs::read_dir(root) {
        Ok(iter) => iter,
        Err(_) => return 0,
    };
    for bucket in dag.flatten() {
        let bucket = bucket.path();
        if !bucket.is_dir() {
            continue;
        }
        let sessies = match std::fs::read_dir(&bucket) {
            Ok(iter) => iter,
            Err(_) => continue,
        };
        for sessie in sessies.flatten() {
            let dir = sessie.path();
            if !dir.is_dir() {
                continue;
            }
            for name in FILES {
                if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
                    total += text.lines().filter(|line| !line.trim().is_empty()).count();
                }
            }
        }
    }
    total
}
