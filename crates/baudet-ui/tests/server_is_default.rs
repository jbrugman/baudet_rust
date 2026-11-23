
use baudet_ui::{chosen_backend, Startup};

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
    {
        let (_, notice) = chosen_backend(&startup(false));
        let reden = notice.unwrap_or_default();
        assert!(reden.contains(SCRIPTWOORD), "zonder server moet de kop zeggen dat het script antwoordt: {reden:?}");

        let (_, notice) = chosen_backend(&startup(true));
        assert!(notice.is_none(), "--demo is geen terugval en hoeft niets uit te leggen");

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
