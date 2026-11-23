
use baudet_i18n::{Key, Language, Texts};

static TAAL_SLOT: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn in_language<T>(lang: Language, run: impl FnOnce() -> T) -> T {
    let _guard = TAAL_SLOT.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    Texts::set_language(lang);
    let out = run();
    Texts::set_language(Language::En);
    out
}

#[test]
fn een_onbekend_taalfeld_leest_als_engels() {
    assert_eq!(Language::default(), Language::En);
    for woord in ["", "   ", "zz", "pools", "Nederlands"] {
        assert_eq!(Language::parse(woord), Language::En, "{woord} hoort naar en te vallen");
    }
}

#[test]
fn de_woorden_kennen_hun_taal_en_omgekeerd() {
    assert_eq!(Language::parse("nl"), Language::Nl);
    assert_eq!(Language::parse(" FR "), Language::Fr);
    assert_eq!(Language::parse("De"), Language::De);
    for lang in [Language::En, Language::Nl, Language::Fr, Language::De] {
        assert_eq!(Language::parse(lang.word()), lang, "{:?} <-> {:?}", lang, lang.word());
    }
}

#[test]
fn de_actieve_taal_wisselt_zonder_herstart() {
    in_language(Language::En, || {
        assert_eq!(Texts::get(Key::AddProjectButton), "Add project");
    });
    in_language(Language::Nl, || {
        assert_eq!(Texts::get(Key::AddProjectButton), "Project toevoegen");
    });
    in_language(Language::De, || {
        assert_eq!(Texts::get(Key::PermWrite), "Schreiben");
    });
}

#[test]
fn placeholders_zijn_positioneel() {
    in_language(Language::Nl, || {
        assert_eq!(Texts::fmt(Key::MessagesCount, &[&3u32]), "3 berichten");
        assert_eq!(Texts::fmt(Key::DiffTitle, &[&"baudet_rust".to_owned()]), "Git-wijzigingen voor baudet_rust");
    });
    in_language(Language::En, || {
        assert_eq!(Texts::fmt(Key::RunReviewPrompt, &[&"/tmp", &30u32]), "run this command in /tmp (timeout 30s)?");
    });
}

#[test]
fn een_onbenoemde_placeholder_blijft_staan() {
    in_language(Language::En, || {
        let out = Texts::fmt(Key::HttpFromBackend, &[&"500".to_owned()]);
        assert!(out.contains("{1}"), "{out}");
    });
}

#[test]
fn elke_sleutel_in_de_steekproef_heeft_in_alle_vier_talen_tekst() {
    let steekproef = [
        Key::SendButton,
        Key::TabSettings,
        Key::FolderPermissionsTitle,
        Key::FileTreeRefreshButton,
        Key::TipBaseUrl,
        Key::HelpBodyChat,
        Key::AboutBody,
    ];
    for lang in [Language::En, Language::Nl, Language::Fr, Language::De] {
        in_language(lang, || {
            for k in steekproef {
                assert!(!Texts::get(k).is_empty(), "{:?} {k:?} is leeg", lang);
            }
        });
    }
}

#[test]
fn een_hulptekst_bevhoudt_zijn_opmaak() {
    in_language(Language::En, || {
        let help = Texts::get(Key::HelpBodyChat);
        assert!(help.starts_with("**Chat**\n\n"), "{}", &help[..20.min(help.len())]);
        assert!(Texts::get(Key::AboutBody).contains('\n'), "AboutBody hoort meerregelig te zijn");
    });
}
