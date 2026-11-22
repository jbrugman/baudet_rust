//! De composer en de kop zeggen hetzelfde over dezelfde backend.
//!
//! Waarom dit binaire bestaat, is de toestand eronder. Tot de server de standaard werd, stond het
//! tekstveld in elke geleverde run aan -- er zat immers altijd een script achter -- en ging er toch
//! niets uit. De composer hangt daarom aan één veld, `server_beurt`, en de kop aan dezelfde. Twee
//! bronnen van waarheid zouden allebei kunnen zeggen wat er niet is, dus dit binaire speelt de keuze
//! van `run_from` na: dan moet elke toestand één antwoord dragen.
//!
//! De nazpeelde keuze (`with_chosen_backend`) is deze beurt uit de vensterfabricage gehaald omdat ze
//! erin stil defect was: `Some(reason) if startup.demo` wachtte op een melding die `--demo` nooit
//! meebrengt, dus werd de demo-tak nooit bereikt en toonde elk scriptvenster "scripted player: " met
//! een ledige reden alsof er iets mis was.
//!
//! Testisolatie: `chosen_backend` leest adres en dialect uit de map die `BAUDET_HOME` aanwijst, en
//! die variabele is proces-breed. `project_tree.rs` documenteert dezelfde val en zet daarom een
//! mutex rond zijn tests, en die mutex deelt dit binaire niet: binaire draaien in eigen processen.
//! Daarom één test hier, met een eigen slot en een werpmap die bij de drop opruimt.

use baudet_ui::{chosen_backend, status_balk, with_chosen_backend, Startup, TranscriptApp};
use std::sync::Mutex;
use tempfile::TempDir;

/// Het woord waarmee een script in de kop aankondigt dat het niet de server is.
const SCRIPTWOORD: &str = "scripted player";

/// Het slot rondom `BAUDET_HOME`, precies zoals `project_tree.rs` het zet: één proces, één
/// variabele, en twee tests die allebei een eigen map willen, concurreren om dezelfde.
static HOME_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn de_backend_keuze_zet_composer_en_kop_op_elkaar() {
    let _slot = HOME_LOCK.lock().unwrap();
    let thuis = TempDir::new().expect("een werpmap om de instellingen in te lezen");
    std::env::set_var("BAUDET_HOME", thuis.path());

    // 1. Ledig adres, geen vlag: er is geen server, dus mag het venster niets beloven. Dit is de
    //    toestand die de klacht opleverde, en hij mag alleen met een reden in de kop bestaan.
    let (backend, melding) = chosen_backend(&Startup::default());
    let terugval = with_chosen_backend(TranscriptApp::new(backend), false, melding);
    assert!(!terugval.composer_enabled(), "zonder server is er niets om naar te sturen");
    assert!(
        terugval.header_status().contains(SCRIPTWOORD),
        "en de kop moet zeggen wat er wel draait: {}",
        terugval.header_status()
    );

    // 2. `--demo`: dezelfde composer, ander woord. Wie om een script vraagt, krijgt geen
    //    terugval-verwijt, maar ook geen ledige balk: een script dat niets over zichzelf zegt,
    //    oogt als een wachtende server. De helper kent die grens op de vlag, niet op de melding.
    let (backend, melding) = chosen_backend(&Startup { demo: true, ..Startup::default() });
    assert!(melding.is_none(), "--demo is geen terugval en draagt dus niets uit `chosen_backend`");
    let demo = with_chosen_backend(TranscriptApp::new(backend), true, melding);
    assert!(!demo.composer_enabled(), "--demo heeft geen model om naar te sturen");
    assert!(
        demo.header_status().contains(SCRIPTWOORD),
        "en de kop noemt het script: {:?}",
        demo.header_status()
    );

    // 3. Een fabrikaatsvenster, dus vóórdat iemand een backend koos, stuurt al niets: de veilige
    //    kant van deze eigenschap, en de reden dat de vlag er één is en niet drie.
    let kaal = TranscriptApp::new(TranscriptApp::demo_backend());
    assert!(!kaal.composer_enabled(), "zonder gekozen backend kan er niets verstuurd worden");
}

#[test]
fn de_statusregel_is_in_elk_geval_een_woord_dat_iets_betekent() {
    // De helper die `run_from` bij het openen op stderr én in de kop zet, mag in geen enkel geval
    // een ledige balk opleveren: leeg is het beeld van een server die wachtend is, en zo werd een
    // scriptrun als defect gelezen. Drie gevallen, drie woorden.
    let script = status_balk(true, None, None);
    assert!(script.contains(SCRIPTWOORD), "--demo noemt het script: {script:?}");
    let terugval = status_balk(false, Some("scripted player: geen protocol"), None);
    assert!(terugval.contains(SCRIPTWOORD), "een terugval herhaalt zijn reden: {terugval:?}");
    let server = status_balk(false, None, Some("http://127.0.0.1:1234/v1".to_string()));
    assert!(server.contains("127.0.0.1:1234"), "een server noemt zijn endpoint: {server:?}");
    // Een ledige terugval-melding telt niet als reden: dan is er niets te verklaren.
    assert!(!status_balk(false, Some(""), Some("http://x/v1".to_string())).contains(SCRIPTWOORD));
    // En zonder endpoint-woord valt de server terug op een eerlijke zin, niet op leeg.
    assert!(!status_balk(false, None, None).is_empty(), "ook een blinde server krijgt een regel");
}
