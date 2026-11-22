//! De server is de standaard, het script is de uitzondering.
//!
//! Dit is de eigenschap waar de klacht "we hebben nog niet eens een call naar het model vanuit de
//! chat" over ging, en die klacht was geen gevoel: `chosen_backend` gaf standaard `ScriptedBackend`
//! en pas op een `--live`-vlag ging er een aanvraag de wire op. De origineler kent die omkering
//! niet -- `AppState.kt:75` heeft de ene fabriek (`gatewayFactory = { ResponsesGateway(it) }`), en
//! het scripted backend komt daar alleen in de doc van de test-naad voor (`ModelGateway.kt:8`,
//! `TurnSource.kt:9`). De vlag is daarom omgedraaid, en dit binaire houdt die beslissing vast.
//!
//! Waarom dit één test is en niet drie, is de moeite van het uitleggen waard. `chosen_backend` leest
//! zijn adres en dialect uit het bestand dat `baudet_store::data_dir` aanwijst, en die plek hangt af
//! van de procesbrede variabele `BAUDET_HOME` -- er is geen parameter voor. cargo roept de tests in
//! één binaire parallel op, dus drie tests die elk een ander `BAUDET_HOME` zetten, write naar elkaars
//! bestand en beslissen over elkaars instellingen. Dit binaire heeft daarom één test, en die loopt
//! de drie toestanden in zijn eigen volgorde af. De overige twee binaire van deze crate (`rows.rs`,
//! `startup_args.rs`) raken die variabele niet en blijven gewoon parallel.
//!
//! Wat er waargenomen wordt, is het tweede deel van de return van `chosen_backend`: de regel die de
//! kop tekent. Dat is het aangewezen observatiepunt en geen omweg -- de `Backend`-trait draagt geen
//! `as_any`, en een trait om een herkenningsvraag voor één test uitgebreid is een middel dat duurder
//! is dan wat het bewijst. De drie uitkomsten zijn de drie regels die `run_from` verdeelt (vlag
//! `--demo` naar `with_demo`, anders `Some` naar `with_status`, anders `None` naar `with_server`),
//! dus wat hier afgetast wordt, is precies wat de kop van het venster toont. Dat de demo-arm niet op
//! een `Some` wacht, werd door deze test ontdekt: `chosen_backend` levert voor `--demo` geen regel,
//! dus liep `run_from` altijd naar `with_status` en toonde elk scriptvenster "scripted player: " met
//! een ledige reden alsof er iets mis was.

use baudet_ui::{chosen_backend, Startup};

/// Het woord waarmee een script in de kop aankondigt dat het niet de server is.
const SCRIPTWOORD: &str = "scripted player";

fn startup(demo: bool) -> Startup {
    Startup { demo, ..Startup::default() }
}

#[test]
fn een_venster_kiest_zonder_vlag_de_server_en_met_demo_het_script() {
    let thuis = std::env::temp_dir().join(format!("baudet-server-default-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&thuis);
    std::fs::create_dir_all(&thuis).expect("een lege data-map om tegen te testen");
    std::env::set_var("BAUDET_HOME", &thuis);
    // Ruimen aan het slot en niet in een pad: `thuis` draagt dit procesnummer, dus een restant na
    // een gegooides assert botst nooit met een volgende run -- die krijgt een eigen nummer.
    {
        // 1. Ledig bestand, geen vlag: er is geen adres en geen dialect, dus geen server. Dit is de
        //    enige tak die buiten `--demo` het script mag opleveren, en hij mag nooit zwijgen -- een
        //    stil script was precies de toestand die de klacht opleverde.
        let (_, notice) = chosen_backend(&startup(false));
        let reden = notice.unwrap_or_default();
        assert!(reden.contains(SCRIPTWOORD), "zonder server moet de kop zeggen dat het script antwoordt: {reden:?}");

        // 2. Ledig bestand, met `--demo`: hetzelfde antwoord, maar zonder verwijt. Wie om een script
        //    vraagt, krijgt geen terugvaltekst; `run_from` noemt zelf wat er draait.
        let (_, notice) = chosen_backend(&startup(true));
        assert!(notice.is_none(), "--demo is geen terugval en hoeft niets uit te leggen");

        // 3. Nu wél een adres en een dialect op schijf, en de vlag weg: dit is de tak die de app op
        //    een werkende machine echt neemt. De vervoerder wordt gebouwd zonder te scannen, dus er
        //    hoeft niets te draaien en er gaat geen aanvraag uit -- poort 59999 staat hier bewust
        //    buiten elk register van een server die iemand draait.
        let bestand = baudet_settings::SettingsFile::default_location();
        let mut settings = bestand.read().settings().clone();
        settings.base_url = "http://127.0.0.1:59999/v1".to_string();
        settings.model = Some("een-model".to_string());
        settings.protocol = Some("chat_completions".to_string());
        bestand.write(&settings).expect("een settings-bestand om tegen te lezen");
        let (backend, notice) = chosen_backend(&startup(false));
        assert!(
            !notice.unwrap_or_default().contains(SCRIPTWOORD),
            "met een dialect op schijf mag er geen script in de kop staan"
        );

        // Het sterkste bewijs dat deze tak haalt: de vraag wél stellen. Het script zou antwoorden
        // met wat het op voorhand in zich draagt; een vervoerder voor poort 59999 kan niets anders
        // doen dan falen, en wel met een netwerkfout. Dat onderscheidt de twee antwoorden zonder
        // dat er een server hoeft te staan en zonder dat er iets welslukt de wire op gaat.
        let request = baudet_backend::SendRequest {
            system: "test".to_string(),
            user_text: "toets of dit de vervoerder is".to_string(),
            history: Vec::new(),
            previous_response_id: None,
            tools: None,
            image: None,
            max_output_tokens: Some(16),
            model: Some("een-model".to_string()),
            reasoning: None,
        };
        // De stroom is het antwoord, niet de return van `send`: die geeft onmiddellijk een handle,
        // ook bij een vervoerder die meteen faalt (gemeten: `TurnStarted` op poort 59999). Een
        // script daarentegen kent zijn eigen antwoord al en eindigt met een voltooid item. Wat hier
        // dus staat, is de vraag of er iets anders dan succes uit deze beurt komt.
        let stroom = match backend.send(request) {
            Ok(mut handle) => {
                let mut tot_nu = Vec::new();
                while let Some(event) = handle.events().blocking_recv() {
                    let tekst = format!("{event:?}");
                    let einde = tekst.contains("TurnCompleted");
                    tot_nu.push(tekst);
                    if einde {
                        break;
                    }
                }
                tot_nu.concat()
            }
            Err(fout) => fout.to_string(),
        };
        assert!(
            !stroom.contains("script") && !stroom.contains("Script"),
            "met een dialect op schijf mag geen script antwoorden: {stroom}"
        );
        assert!(
            stroom.contains("59999")
                || stroom.to_lowercase().contains("connect")
                || stroom.contains("Failed")
                || stroom.contains("incomplete_reason: Some"),
            "deze beurt hoort een netwerkuitkomst te dragen, want er draait niets op die poort: {stroom}"
        );
    }
    let _ = std::fs::remove_dir_all(&thuis);
    std::env::remove_var("BAUDET_HOME");
}
