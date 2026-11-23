
use baudet_ui::{chosen_backend, status_balk, with_chosen_backend, Startup, TranscriptApp};
use std::sync::Mutex;
use tempfile::TempDir;

const SCRIPTWOORD: &str = "scripted player";

static HOME_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn de_backend_keuze_zet_composer_en_kop_op_elkaar() {
    let _slot = HOME_LOCK.lock().unwrap();
    let thuis = TempDir::new().expect("een werpmap om de instellingen in te lezen");
    std::env::set_var("BAUDET_HOME", thuis.path());

    let (backend, melding) = chosen_backend(&Startup::default());
    let terugval = with_chosen_backend(TranscriptApp::new(backend), false, melding);
    assert!(!terugval.composer_enabled(), "zonder server is er niets om naar te sturen");
    assert!(
        terugval.header_status().contains(SCRIPTWOORD),
        "en de kop moet zeggen wat er wel draait: {}",
        terugval.header_status()
    );

    let (backend, melding) = chosen_backend(&Startup { demo: true, ..Startup::default() });
    assert!(melding.is_none(), "--demo is geen terugval en draagt dus niets uit `chosen_backend`");
    let demo = with_chosen_backend(TranscriptApp::new(backend), true, melding);
    assert!(!demo.composer_enabled(), "--demo heeft geen model om naar te sturen");
    assert!(
        demo.header_status().contains(SCRIPTWOORD),
        "en de kop noemt het script: {:?}",
        demo.header_status()
    );

    let kaal = TranscriptApp::new(TranscriptApp::demo_backend());
    assert!(!kaal.composer_enabled(), "zonder gekozen backend kan er niets verstuurd worden");
}

#[test]
fn de_statusregel_is_in_elk_geval_een_woord_dat_iets_betekent() {
    let script = status_balk(true, None, None);
    assert!(script.contains(SCRIPTWOORD), "--demo noemt het script: {script:?}");
    let terugval = status_balk(false, Some("scripted player: geen protocol"), None);
    assert!(terugval.contains(SCRIPTWOORD), "een terugval herhaalt zijn reden: {terugval:?}");
    let server = status_balk(false, None, Some("http://127.0.0.1:1234/v1".to_string()));
    assert!(server.contains("127.0.0.1:1234"), "een server noemt zijn endpoint: {server:?}");
    assert!(!status_balk(false, Some(""), Some("http://x/v1".to_string())).contains(SCRIPTWOORD));
    assert!(!status_balk(false, None, None).is_empty(), "ook een blinde server krijgt een regel");
}
