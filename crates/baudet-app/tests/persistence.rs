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
        .on_session(&root, &id, "model-van-hier")
        .expect("hetzelfde gesprek, dezelfde map");

    let rows = reopened.rows();
    let bodies: Vec<String> = rows.iter().map(baudet_app::reducer::streamed_text).collect();
    assert!(bodies.iter().any(|body| body == "vraag een"), "de vraag staat er weer: {bodies:?}");
    assert!(bodies.iter().any(|body| body == "het eerste antwoord"), "het antwoord staat er weer: {bodies:?}");
    assert_eq!(bodies.len(), 2, "twee regels en geen derde: {bodies:?}");
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
        .on_session(&root, &id, "model-van-hier")
        .expect("hetzelfde gesprek, dezelfde map");

    assert_eq!(reopened.rows().len() as u64, page::INITIAL as u64, "openen leest de staart, niet het bestand");
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
    assert_eq!(mentions_exact(&reopened, "vraag 1"), 1, "één rij per vraag, ook na scrollen");
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
