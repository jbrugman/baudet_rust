//! Het taalcontract van `i18n/Texts.kt`: Engels als default en als val, vier talen die dezelfde
//! sleutels dragen, en positionele vervanging.

use baudet_i18n::{Key, Language, Texts};

/// De taal die het proces draagt, is één waarde voor het hele proces — zie `Texts` zelf, waar de
/// reden staat waarom dat géén `thread_local` is. Criterium van deze tests is juist wat er na een
/// zet op screen staat, en dus mag er nooit een tweede test tegelijk aan die waarde draaien.
static TAAL_SLOT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Zet een taal voor de duur van de slot en herstelt Engels. De slot maakt dat exclusief, en het
/// herstel houdt het proces op zijn default: een test die hieruit komt, laat geen taal achter
/// waar een andere test toevallig baat bij heeft.
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
    // `word` en `parse` zijn elkaars inverse, want het bestand is wat er staat en een woord dat
    // zijn eigen taal niet terugleest, wist bij het opslaan de keuze van de persoon.
    for lang in [Language::En, Language::Nl, Language::Fr, Language::De] {
        assert_eq!(Language::parse(lang.word()), lang, "{:?} <-> {:?}", lang, lang.word());
    }
}

#[test]
fn de_actieve_taal_wisselt_zonder_herstart() {
    // Dit is het hele punt van item H: de persoon draait de taal en ziet hem meteen.
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
    // `Texts.fmt` vervangt alleen `{i}` waarvoor een argument is. Een sjabloon met {1} en één
    // argument houdt dus letterlijk `{1}`; dat is de afspraak van de origineler, en een `fmt` die
    // daar iets van maakt, verbergt een scheve tabel in plaats van te laten zien dat er een mist.
    in_language(Language::En, || {
        let out = Texts::fmt(Key::HttpFromBackend, &[&"500".to_owned()]);
        assert!(out.contains("{1}"), "{out}");
    });
}

#[test]
fn elke_sleutel_in_de_steekproef_heeft_in_alle_vier_talen_tekst() {
    // Een lege snaar tekent een kaal vakje. `Key` heeft geen opsomming (de origineler heeft die
    // lijst ook niet), dus deze steekproef raakt elke groep en bewijst daarmee dat geen van de
    // vier tabellen halverwege is afgebroken.
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
    // In `Key.kt` staat `"**Chat**\n\n- ..."`, en `\n` is daar een echte sprong, geen woord. De
    // tabel moet die sprong onveranderd dragen, want de hulptekst wordt op koppen gelezen.
    in_language(Language::En, || {
        let help = Texts::get(Key::HelpBodyChat);
        assert!(help.starts_with("**Chat**\n\n"), "{}", &help[..20.min(help.len())]);
        assert!(Texts::get(Key::AboutBody).contains('\n'), "AboutBody hoort meerregelig te zijn");
    });
}
