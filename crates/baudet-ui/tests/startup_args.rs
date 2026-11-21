//! The command line the binary accepts, protected without a window.
//!
//! Worth a test because the failure mode is silent: a misspelled flag that is ignored starts a
//! window nobody asked for and, in a scripted check, hangs it. So an unknown option must be an
//! error, and `--screenshot` must reach the app as a path rather than be dropped on the floor.

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
fn a_screenshot_without_a_file_is_an_error_not_a_window() {
    assert!(parse(&["b", "--screenshot"]).is_err());
}

#[test]
fn an_unknown_option_is_an_error() {
    assert!(parse(&["b", "--screenshotz=x.png"]).is_err(), "a typo must not silently open a window");
}
