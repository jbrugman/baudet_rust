//! De poort en de builder achter de git-tool van het model.
//!
//! Poort van `git/GitCommand.kt`. Een assistent moet een repository kunnen zien, maar een ruwe git in de
//! handen van een model is een programma dat de geschiedenis kan herschrijven of zich via globale vlaggen op
//! een ándere repository kan richten. Daarom biedt dit een vaste set read-only subcommando's, elk met zijn
//! eigen veilige standaard-vlaggen, en weigert het alles anders: een onbekend werkwoord, of élk argument dat
//! met een dash begint — want dat zijn precies de globale opties (een andere werkboom, een config-override)
//! waarmee een call zijn map kan verlaten. Het enige dash-token dat wel mag, is het letterlijke `--`, de
//! standaard end-of-options-scheider om een pad eenduidig te maken.
//!
//! Wanneer schrijven aan staat, komt daar een klein setje werkwoorden bij die de repository veranderen
//! (add, commit, checkout, stash, push), elk begrensd aan de veilige vorm van zijn eigen handeling in plaats
//! van als ruwe git: een commit-bericht komt via zijn eigen argument zodat `-a` en `--amend` niet naar binnen
//! glippen, een checkout wisselt of maakt alleen een branch en gooit nooit een pad weg, een stash blijft bij
//! zijn niet-destructieve subcommando's, en een push kan niet geforceerd worden. Globale vlaggen worden zo of
//! zo geweigerd, dus een call verlaat nooit zijn repository, en een write die deze regels niet kunnen begrenzen
//! blijft een taak voor `run_command`. Alles is een argv-lijst die rechtstreeks naar een proces gaat, dus er is
//! nooit een shell bij. Proces- en UI-vrij, dus de regels zijn los te testen.

use std::path::Path;

/// De read-only model-werkwoorden, in de volgorde waarin ze geadverteerd worden.
const READ_ONLY: [&str; 9] =
    ["status", "diff", "diff_staged", "log", "show", "branch", "ls_files", "rev_parse", "blame"];

/// De werkwoorden die de repository veranderen; alleen geboden wanneer schrijven aan staat.
const WRITE: [&str; 5] = ["add", "commit", "checkout", "stash", "push"];

/// De stash-subcommando's die worden geboden; de destructieve (drop, clear) staan er bewust niet in.
const STASH_SUBS: [&str; 5] = ["push", "pop", "apply", "save", "list"];

/// Waar de git-tool in mag schrijven: niets, of het kleine setje hierboven. In de origineler een boolean
/// (`writesAllowed`) en hier een tipe, omdat er in Rust drie routes zijn die dat setje moeten delen — de
/// advertentie, de weigeringstekst en de uitvoering — en twee `bool`-argumenten naast elkaar (`writes_allowed`,
/// `write_granted`) is precies de verwarring die dit voorkomt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitWrites {
    /// Alleen de read-only werkwoorden; een write-woord wordt geweigerd met een eerlijke reden.
    Disabled,
    /// De vijf write-werkwoorden zijn geboden en hun veilige vorm geldt.
    Enabled,
}

/// Of een werkwoord de repository verandert, zodat de tool weet dat hij moet vragen.
pub fn is_write(command: &str) -> bool {
    WRITE.contains(&command)
}

/// De werkwoorden die het schema van de tool adverteert, zodat het model alleen ziet wat het kan aanroepen.
pub fn advertised(writes: GitWrites) -> Vec<&'static str> {
    match writes {
        GitWrites::Disabled => READ_ONLY.to_vec(),
        GitWrites::Enabled => READ_ONLY.into_iter().chain(WRITE).collect(),
    }
}

/// `None` wanneer `command` met `args` veilig te draaien is; anders een leesbare reden die ook als
/// model-gerichte feedback dient, want de tool geeft hem rechtstreeks terug. Read-only werkwoorden worden
/// altijd gecontroleerd; een write-woord wordt aan zijn eigen veilige vorm getoetst alleen wanneer `writes`
/// aan staat, en anders met een eerlijke reden geweigerd. `message` draagt het bericht van een commit uit de
/// argumenten weg, zodat hier geen commit-vlag geparseerd hoeft te worden.
pub fn check_allowed(command: &str, args: &[String], writes: GitWrites, message: Option<&str>) -> Option<String> {
    if command.trim().is_empty() {
        return Some("git: a command is required".to_owned());
    }
    if READ_ONLY.contains(&command) {
        return check_read_only(command, args);
    }
    if WRITE.contains(&command) {
        if writes == GitWrites::Disabled {
            return Some(format!(
                "git: '{command}' changes the repository. Git writes are disabled; enable them in Settings, or                  use the run_command tool instead."
            ));
        }
        return check_write(command, args, message);
    }
    Some(format!("git: '{command}' is not an offered command. Allowed: {}.", advertised(writes).join(", ")))
}

/// De argv om `command` tegen de repository in `root` te draaien, met de pager uit (er is geen terminal) en
/// pad-quoting uit (niet-ASCII-namen lezen dan raw terug). Ervan uitgaande dat [`check_allowed`] de call al
/// vrijgaf. Het bericht van een commit reist als zijn eigen `-m`-waarde.
pub fn argv(root: &Path, command: &str, args: &[String], message: Option<&str>) -> Vec<String> {
    let mut out = vec![
        "git".to_owned(),
        "-C".to_owned(),
        root.to_string_lossy().into_owned(),
        "-c".to_owned(),
        "core.quotePath=false".to_owned(),
        "--no-pager".to_owned(),
    ];
    out.extend(base(command, message));
    out.extend(args.iter().cloned());
    out
}

/// Een leesbare één-regelvorm van de run, getoond op de goedkeuringskaart die een write voor de mens opent,
/// en gebruikt als pin-teken waar een herhaling van dezelfde write tegen wordt vergeleken. De routerings-ruis
/// (de `-C root` en de twee `-c`/`--no-pager`-opties) valt weg, zodat de kaart leest als een git-commando dat
/// een persoon zou typen; een token met een spatie wordt gequote zodat het bericht leesbaar blijft.
pub fn summary(command: &str, args: &[String], message: Option<&str>) -> String {
    let tail: Vec<String> = base(command, message).into_iter().chain(args.iter().cloned()).collect();
    let joined = tail
        .iter()
        .map(|token| if token.contains(' ') { format!("\"{token}\"") } else { token.clone() })
        .collect::<Vec<_>>()
        .join(" ");
    format!("git {joined}")
}

/// De regels voor een read-only werkwoord: geen dash-optie die uit de map kanescape, en branch toont alleen.
fn check_read_only(command: &str, args: &[String]) -> Option<String> {
    if let Some(flag) = args.iter().find(|arg| arg.starts_with('-') && *arg != "--") {
        return Some(format!("git: the argument '{flag}' is not allowed; git global options cannot be passed."));
    }
    // branch wordt geboden om de branches te tonen, en dat is wat een kale `branch` doet. Elk overig argument
    // is een naam (dash-opties vielen hierboven af), en `git branch <naam>` zou een branch aanmaken — een
    // write die dit read-only pad nooit mag uitvoeren.
    if command == "branch" && args.iter().any(|arg| arg != "--") {
        return Some("git: branch lists only here; it cannot create, rename or delete a branch.".to_owned());
    }
    None
}

/// De veilige vorm van elk geboden write-woord. Alles wat breder is, wordt geweigerd in plaats van als ruwe
/// git gedraaid.
fn check_write(command: &str, args: &[String], message: Option<&str>) -> Option<String> {
    let flags: Vec<&String> = args.iter().filter(|arg| arg.starts_with('-') && *arg != "--").collect();
    let positional: Vec<&String> = args.iter().filter(|arg| !arg.starts_with('-')).collect();
    match command {
        // add stapt alleen de genoemde pads; een dash zou patch-mode of -A zijn, dus alleen pads.
        "add" => {
            if let Some(flag) = flags.first() {
                Some(flag_refusal(flag))
            } else if positional.is_empty() {
                Some("git: add needs at least one path to stage.".to_owned())
            } else {
                None
            }
        }
        // Het bericht is zijn eigen argument; een vlag in args zou -a of --amend zijn, nooit toegestaan.
        "commit" => {
            if message.is_none_or(|text| text.trim().is_empty()) {
                Some("git: commit needs a message in the 'message' argument.".to_owned())
            } else {
                flags.first().map(|flag| flag_refusal(flag))
            }
        }
        "checkout" => check_checkout(args, &flags, &positional),
        // Alleen de niet-destructieve subcommando's; drop en clear ontbreken bewust.
        "stash" => {
            if let Some(flag) = flags.first() {
                Some(flag_refusal(flag))
            } else if positional.len() > 1 {
                Some("git: stash takes a single subcommand.".to_owned())
            } else if positional.first().is_some_and(|sub| !STASH_SUBS.contains(&sub.as_str())) {
                Some(format!("git: stash '{}' is not allowed; only {} are.", positional[0], STASH_SUBS.join(", ")))
            } else {
                None
            }
        }
        // Geen enkele vlag, dus een force-push is onmogelijk; hoogstens volgen een remote en een branch.
        "push" => {
            if !flags.is_empty() {
                Some(
                    "git: push takes no flags here, so it cannot force; pass only an optional remote and branch."
                        .to_owned(),
                )
            } else if positional.len() > 2 {
                Some("git: push takes at most a remote and a branch.".to_owned())
            } else {
                None
            }
        }
        _ => Some(format!("git: '{command}' is not an offered write.")),
    }
}

/// checkout wisselt naar één bestaande branch, of maakt er een met `-b`/`-B`. `--` wordt geweigerd want
/// `git checkout -- <pad>` gooit werk weg, en dat is geen branch-handeling; meer dan één positioneel zou een
/// pad kunnen noemen of HEAD kunnen losmaken, dus er mag hoe dan ook precies één.
fn check_checkout(args: &[String], flags: &[&String], positional: &[&String]) -> Option<String> {
    if args.iter().any(|arg| arg == "--") {
        return Some(
            "git: checkout switches or creates a branch here; it cannot check out paths or discard changes.".to_owned(),
        );
    }
    match flags.len() {
        0 if positional.len() == 1 => None,
        0 => Some("git: checkout needs exactly one branch to switch to.".to_owned()),
        1 => {
            let flag = flags[0].as_str();
            if flag != "-b" && flag != "-B" {
                Some(flag_refusal(flag))
            } else if positional.len() == 1 {
                None
            } else {
                Some(format!("git: {flag} needs exactly one new branch name."))
            }
        }
        _ => Some("git: checkout takes at most one -b/-B flag.".to_owned()),
    }
}

fn flag_refusal(flag: &str) -> String {
    format!("git: the argument '{flag}' is not allowed for this command.")
}

/// Het subcommando plus zijn veilige standaard-vlaggen, of een lege lijst bij een onbekend werkwoord.
fn base(command: &str, message: Option<&str>) -> Vec<String> {
    let words: &[&str] = match command {
        "status" => &["status", "--short", "--branch"],
        "diff" => &["diff"],
        "diff_staged" => &["diff", "--cached"],
        "log" => &["log", "--oneline", "-25"],
        "show" => &["show"],
        "branch" => &["branch"],
        "ls_files" => &["ls-files"],
        "rev_parse" => &["rev-parse"],
        "blame" => &["blame"],
        "add" => &["add", "--"],
        "commit" => &["commit", "-m"],
        "checkout" => &["checkout"],
        "stash" => &["stash"],
        "push" => &["push"],
        _ => &[],
    };
    let mut out: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
    if command == "commit" && !words.is_empty() {
        // `message.orEmpty()` van de origineler: een afwezig bericht wordt een leeg token, want deze rij is
        // alleen bestemd voor een call die `check_allowed` al vrijgaf — en die eist een bericht.
        out.push(message.unwrap_or_default().to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::base;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    /// Een argumentlijst voor de twee functies die `&[String]` vragen.
    fn list(items: &[&str]) -> Vec<String> {
        args(items)
    }

    /// Het vrijgeven zelf: `None` betekent dat de call draait.
    fn ok(command: &str, extra: &[&str], writes: GitWrites, message: Option<&str>) -> bool {
        check_allowed(command, &args(extra), writes, message).is_none()
    }

    fn waarom(command: &str, extra: &[&str], writes: GitWrites, message: Option<&str>) -> String {
        check_allowed(command, &args(extra), writes, message).unwrap_or_default()
    }

    #[test]
    fn every_advertised_verb_is_read_only_until_writes_are_on() {
        assert_eq!(advertised(GitWrites::Disabled).len(), 9);
        assert_eq!(advertised(GitWrites::Disabled).last().copied(), Some("blame"));
        assert_eq!(advertised(GitWrites::Enabled).len(), 14);
        assert_eq!(advertised(GitWrites::Enabled).last().copied(), Some("push"));
        // De advertentie is precies wat `is_write` onderscheidt; twee lijsten die uit elkaar lopen, zou een
        // model een write laten zien zonder dat de tool ervoor vraagt.
        for verb in advertised(GitWrites::Enabled) {
            assert_eq!(is_write(verb), WRITE.contains(&verb), "{verb}");
        }
        assert!(is_write("commit"));
        assert!(!is_write("status"));
    }

    #[test]
    fn an_unknown_verb_is_refused_with_the_offered_list() {
        let tekst = waarom("reset", &[], GitWrites::Disabled, None);
        assert!(tekst.starts_with("git: 'reset' is not an offered command."), "{tekst}");
        assert!(tekst.contains("status, diff, diff_staged, log, show, branch, ls_files, rev_parse, blame"), "{tekst}");
        assert!(!tekst.contains("commit"), "uitgeschakeld betekent niet adverteren: {tekst}");
        assert!(waarom("reset", &[], GitWrites::Enabled, None).contains("commit"));
        // Een vlag is geen werkwoord: de set is sluitend, niet een voorstel.
        assert!(waarom("--help", &[], GitWrites::Enabled, None).contains("not an offered command"));
    }

    #[test]
    fn a_blank_verb_asks_for_one() {
        for command in ["", "   "] {
            assert_eq!(waarom(command, &[], GitWrites::Enabled, None), "git: a command is required");
        }
    }

    #[test]
    fn no_dash_option_reaches_a_read_only_verb() {
        // Eén vlag is hier al te veel: de read-only-route laat geen enkel dash-token toe behalve
        // het letterlijke `--`, en een `-c` is precies de deur naar een andere repository of config.
        for flag in ["-C", "/tmp/other", "--git-dir=/tmp/x", "--work-tree=/x", "-c", "core.pager=less"] {
            let why = waarom("status", &["-c", flag], GitWrites::Disabled, None);
            assert!(why.contains("global options cannot be passed"), "{flag}: {why}");
        }
        assert!(ok("status", &[], GitWrites::Disabled, None));
        // De poort kent geen per-subcommando-vlaggen en maakt geen uitzondering: `--oneline` is voor haar een
        // dash-token en valt dus af. De standaard-vlaggen zitten in `base`, waar het model ze niet hoeft te
        // vragen — `log` adverteert al `--oneline -25`. Een pad daarentegen mag gewoon.
        assert!(waarom("log", &["--oneline"], GitWrites::Disabled, None).contains("global options"));
        assert!(ok("log", &["src/main.rs"], GitWrites::Disabled, None));
    }

    #[test]
    fn the_literal_double_dash_stays_allowed_as_a_separator() {
        // Het enige dash-token dat mag, want het is geen optie maar de end-of-options-scheider.
        assert!(ok("diff", &["--", "src/main.rs"], GitWrites::Disabled, None));
        assert!(ok("status", &["--"], GitWrites::Disabled, None));
    }

    #[test]
    fn branch_lists_and_never_creates() {
        assert!(ok("branch", &[], GitWrites::Disabled, None));
        assert!(
            waarom("branch", &["--all"], GitWrites::Disabled, None).contains("global options"),
            "geen enkel dash-token buiten `--` komt door de read-only-route"
        );
        let why = waarom("branch", &["nieuw"], GitWrites::Disabled, None);
        assert_eq!(why, "git: branch lists only here; it cannot create, rename or delete a branch.");
    }

    #[test]
    fn a_write_verb_is_refused_with_an_honest_reason_when_writes_are_off() {
        for verb in WRITE {
            let why = waarom(verb, &[], GitWrites::Disabled, Some("bericht"));
            assert!(why.contains("changes the repository"), "{verb}: {why}");
            assert!(why.contains("enable them in Settings"), "{verb}: {why}");
            assert!(why.contains("run_command"), "{verb}: {why}");
        }
    }

    #[test]
    fn add_stages_only_named_paths() {
        assert!(ok("add", &["src/a.rs"], GitWrites::Enabled, None));
        assert!(ok("add", &["src/a.rs", "README.md"], GitWrites::Enabled, None));
        assert_eq!(waarom("add", &[], GitWrites::Enabled, None), "git: add needs at least one path to stage.");
        // `-A` en patch-mode zijn writes op wat niet genoemd is.
        assert!(waarom("add", &["-A"], GitWrites::Enabled, None).contains("not allowed for this command"));
    }

    #[test]
    fn commit_carries_its_message_as_its_own_argument() {
        assert!(ok("commit", &[], GitWrites::Enabled, Some("vast gezet")));
        for message in [None, Some(""), Some("   ")] {
            assert_eq!(
                waarom("commit", &[], GitWrites::Enabled, message),
                "git: commit needs a message in the 'message' argument."
            );
        }
        // Een vlag zou -a of --amend zijn: het bericht is het enige wat hier binnenkomt.
        assert!(waarom("commit", &["-a"], GitWrites::Enabled, Some("x")).contains("not allowed"));
        assert!(waarom("commit", &["--amend"], GitWrites::Enabled, Some("x")).contains("not allowed"));
    }

    #[test]
    fn checkout_switches_or_creates_a_branch_and_never_touches_a_path() {
        assert!(ok("checkout", &["main"], GitWrites::Enabled, None));
        assert!(ok("checkout", &["-b", "feature"], GitWrites::Enabled, None));
        assert!(ok("checkout", &["-B", "feature"], GitWrites::Enabled, None));
        assert_eq!(
            waarom("checkout", &[], GitWrites::Enabled, None),
            "git: checkout needs exactly one branch to switch to."
        );
        assert_eq!(
            waarom("checkout", &["main", "feature"], GitWrites::Enabled, None),
            "git: checkout needs exactly one branch to switch to."
        );
        assert_eq!(waarom("checkout", &["-b"], GitWrites::Enabled, None), "git: -b needs exactly one new branch name.");
        assert_eq!(
            waarom("checkout", &["-b", "a", "b"], GitWrites::Enabled, None),
            "git: -b needs exactly one new branch name."
        );
        assert!(waarom("checkout", &["-f", "main"], GitWrites::Enabled, None).contains("not allowed"));
        assert_eq!(
            waarom("checkout", &["-b", "-B"], GitWrites::Enabled, None),
            "git: checkout takes at most one -b/-B flag."
        );
        // `git checkout -- <pad>` gooit werk weg.
        assert_eq!(
            waarom("checkout", &["--", "src/a.rs"], GitWrites::Enabled, None),
            "git: checkout switches or creates a branch here; it cannot check out paths or discard changes."
        );
    }

    #[test]
    fn stash_stops_at_its_non_destructive_subcommands() {
        for sub in STASH_SUBS {
            assert!(ok("stash", &[sub], GitWrites::Enabled, None), "{sub}");
        }
        assert!(ok("stash", &[], GitWrites::Enabled, None), "een kale stash is een push");
        for verb in ["drop", "clear", "create"] {
            let why = waarom("stash", &[verb], GitWrites::Enabled, None);
            assert!(why.contains(&format!("stash '{verb}' is not allowed")), "{why}");
            assert!(why.contains("push, pop, apply, save, list"), "{why}");
        }
        assert_eq!(
            waarom("stash", &["push", "bericht"], GitWrites::Enabled, None),
            "git: stash takes a single subcommand."
        );
        assert!(waarom("stash", &["-u"], GitWrites::Enabled, None).contains("not allowed"));
    }

    #[test]
    fn push_cannot_be_forced() {
        assert!(ok("push", &[], GitWrites::Enabled, None));
        assert!(ok("push", &["origin"], GitWrites::Enabled, None));
        assert!(ok("push", &["origin", "main"], GitWrites::Enabled, None));
        for flag in ["-f", "--force", "--force-with-lease", "-u"] {
            let why = waarom("push", &[flag], GitWrites::Enabled, None);
            assert!(why.contains("cannot force"), "{flag}: {why}");
        }
        assert_eq!(
            waarom("push", &["origin", "main", "extra"], GitWrites::Enabled, None),
            "git: push takes at most a remote and a branch."
        );
    }

    #[test]
    fn the_argv_carries_its_routing_before_the_verb() {
        let root = Path::new("/repo");
        assert_eq!(
            argv(root, "status", &[], None),
            args(&["git", "-C", "/repo", "-c", "core.quotePath=false", "--no-pager", "status", "--short", "--branch"])
        );
        assert_eq!(argv(root, "diff_staged", &list(&["--", "a.rs"]), None).last().unwrap(), "a.rs");
        assert!(argv(root, "diff_staged", &[], None).contains(&"--cached".to_owned()));
        assert_eq!(
            argv(root, "ls_files", &[], None),
            args(&["git", "-C", "/repo", "-c", "core.quotePath=false", "--no-pager", "ls-files"])
        );
        // `rev_parse` wordt zijn echte git-naam, en de extra's komen achter de base.
        assert_eq!(
            argv(root, "rev_parse", &list(&["HEAD"]), None),
            args(&["git", "-C", "/repo", "-c", "core.quotePath=false", "--no-pager", "rev-parse", "HEAD"])
        );
        // Het bericht reist als zijn eigen -m-waarde, vóór de extra args.
        let commit = argv(root, "commit", &[], Some("vast gezet"));
        let pos = commit.iter().position(|t| t == "-m").unwrap();
        assert_eq!(commit[pos + 1], "vast gezet");
        // Een onbekend werkwoord geeft geen base: de routering alleen is geen git-run.
        assert!(base("reset", None).is_empty());
    }

    #[test]
    fn the_summary_reads_as_a_command_a_person_would_type() {
        assert_eq!(summary("status", &[], None), "git status --short --branch");
        assert_eq!(summary("add", &list(&["src/a.rs"]), None), "git add -- src/a.rs");
        // De routerings-ruis blijft weg en het bericht blijft als één token leesbaar.
        assert_eq!(summary("commit", &[], Some("vast gezet")), "git commit -m \"vast gezet\"");
        assert!(!summary("commit", &[], Some("x")).contains("-C"));
        assert!(!summary("commit", &[], Some("x")).contains("no-pager"));
    }
}
