
use baudet_ui::Startup;

fn parse(args: &[&str]) -> Result<Startup, String> {
    Startup::parse(args.iter().map(|arg| (*arg).to_string()))
}

#[test]
fn no_arguments_opens_a_window() {
    let startup = parse(&["baudet-ui"]).unwrap();
    assert_eq!(startup.screenshot, None);
}

#[test]
fn a_screenshot_path_is_honoured_in_both_spellings() {
    let equals = parse(&["b", "--screenshot=/tmp/a.png"]).unwrap().screenshot;
    assert_eq!(equals.as_deref(), Some(std::path::Path::new("/tmp/a.png")));
    let spaced = parse(&["b", "--screenshot", "/tmp/b.png"]).unwrap().screenshot;
    assert_eq!(spaced.as_deref(), Some(std::path::Path::new("/tmp/b.png")));
}

#[test]
fn the_panel_flags_are_off_unless_asked_and_set_when_asked() {
    let plain = parse(&["b"]).unwrap();
    assert!(!plain.settings && !plain.fetch_models);
    let open = parse(&["b", "--settings"]).unwrap();
    assert!(open.settings && !open.fetch_models);
    let fetched = parse(&["b", "--fetch-models"]).unwrap();
    assert!(!fetched.settings && fetched.fetch_models);
    let both = parse(&["b", "--settings", "--fetch-models"]).unwrap();
    assert!(both.settings && both.fetch_models);
}

#[test]
fn the_script_player_is_opt_in() {
    assert!(!parse(&["b"]).unwrap().demo, "zonder vlag praat het venster met de server");
    assert!(parse(&["b", "--demo"]).unwrap().demo, "--demo vraagt de scriptspeler");
}

#[test]
fn a_question_to_send_is_opt_in_and_needs_a_value() {
    assert_eq!(parse(&["b", "--demo"]).unwrap().send, None);
    assert_eq!(parse(&["b", "--send=why"]).unwrap().send.as_deref(), Some("why"));
    assert_eq!(parse(&["b", "--send", "why"]).unwrap().send.as_deref(), Some("why"));
    assert!(parse(&["b", "--send"]).is_err(), "a --send without a question is an error, not a window");
}

#[test]
fn a_screenshot_without_a_file_is_an_error_not_a_window() {
    assert!(parse(&["b", "--screenshot"]).is_err());
}

#[test]
fn a_workspace_folder_is_honoured_in_both_spellings() {
    let equals = parse(&["b", "--workspace=/tmp/bron"]).unwrap().workspace;
    assert_eq!(equals.as_deref(), Some(std::path::Path::new("/tmp/bron")));
    let spaced = parse(&["b", "--workspace", "/tmp/bron"]).unwrap().workspace;
    assert_eq!(spaced.as_deref(), Some(std::path::Path::new("/tmp/bron")));
}

#[test]
fn a_workspace_without_a_folder_is_an_error_not_a_window() {
    assert!(parse(&["b", "--workspace"]).is_err());
}

#[test]
fn the_data_folder_accepts_both_spellings_and_requires_a_path() {
    for args in [&["b", "--home", "data"][..], &["b", "--home=data"][..]] {
        assert_eq!(parse(args).unwrap().data_dir.unwrap(), std::path::Path::new("data"));
    }
    for args in [&["b", "--home"][..], &["b", "--home="][..], &["b", "--home", "--demo"][..]] {
        assert!(parse(args).is_err());
    }
}

#[test]
fn the_two_options_can_be_given_in_any_order() {
    let startup = parse(&["b", "--workspace=/tmp/bron", "--screenshot=/tmp/a.png"]).unwrap();
    assert_eq!(startup.workspace.as_deref(), Some(std::path::Path::new("/tmp/bron")));
    assert_eq!(startup.screenshot.as_deref(), Some(std::path::Path::new("/tmp/a.png")));
    let flipped = parse(&["b", "--screenshot=/tmp/a.png", "--workspace=/tmp/bron"]).unwrap();
    assert_eq!(flipped.workspace, startup.workspace);
    assert_eq!(flipped.screenshot, startup.screenshot);
}

#[test]
fn an_unknown_option_is_an_error() {
    assert!(parse(&["b", "--screenshotz=x.png"]).is_err(), "a typo must not silently open a window");
}

#[test]
fn a_discovery_sweep_is_opt_in() {
    assert!(!parse(&["b"]).unwrap().discover, "zonder vlag wordt er geen poort gevraagd");
    assert!(parse(&["b", "--discover"]).unwrap().discover, "--discover zet de sweep aan");
}
