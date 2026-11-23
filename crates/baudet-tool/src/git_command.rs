
use std::path::Path;

// Keep the read-only command allowlist separate from write verbs.
const READ_ONLY: [&str; 9] =
    ["status", "diff", "diff_staged", "log", "show", "branch", "ls_files", "rev_parse", "blame"];

const WRITE: [&str; 5] = ["add", "commit", "checkout", "stash", "push"];

const STASH_SUBS: [&str; 5] = ["push", "pop", "apply", "save", "list"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitWrites {
    Disabled,
    Enabled,
}

pub fn is_write(command: &str) -> bool {
    WRITE.contains(&command)
}

pub fn advertised(writes: GitWrites) -> Vec<&'static str> {
    match writes {
        GitWrites::Disabled => READ_ONLY.to_vec(),
        GitWrites::Enabled => READ_ONLY.into_iter().chain(WRITE).collect(),
    }
}

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

pub fn summary(command: &str, args: &[String], message: Option<&str>) -> String {
    let tail: Vec<String> = base(command, message).into_iter().chain(args.iter().cloned()).collect();
    let joined = tail
        .iter()
        .map(|token| if token.contains(' ') { format!("\"{token}\"") } else { token.clone() })
        .collect::<Vec<_>>()
        .join(" ");
    format!("git {joined}")
}

fn check_read_only(command: &str, args: &[String]) -> Option<String> {
    if let Some(flag) = args.iter().find(|arg| arg.starts_with('-') && *arg != "--") {
        return Some(format!("git: the argument '{flag}' is not allowed; git global options cannot be passed."));
    }
    if command == "branch" && args.iter().any(|arg| arg != "--") {
        return Some("git: branch lists only here; it cannot create, rename or delete a branch.".to_owned());
    }
    None
}

fn check_write(command: &str, args: &[String], message: Option<&str>) -> Option<String> {
    let flags: Vec<&String> = args.iter().filter(|arg| arg.starts_with('-') && *arg != "--").collect();
    let positional: Vec<&String> = args.iter().filter(|arg| !arg.starts_with('-')).collect();
    match command {
        "add" => {
            if let Some(flag) = flags.first() {
                Some(flag_refusal(flag))
            } else if positional.is_empty() {
                Some("git: add needs at least one path to stage.".to_owned())
            } else {
                None
            }
        }
        "commit" => {
            if message.is_none_or(|text| text.trim().is_empty()) {
                Some("git: commit needs a message in the 'message' argument.".to_owned())
            } else {
                flags.first().map(|flag| flag_refusal(flag))
            }
        }
        "checkout" => check_checkout(args, &flags, &positional),
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

    fn list(items: &[&str]) -> Vec<String> {
        args(items)
    }

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
        for flag in ["-C", "/tmp/other", "--git-dir=/tmp/x", "--work-tree=/x", "-c", "core.pager=less"] {
            let why = waarom("status", &["-c", flag], GitWrites::Disabled, None);
            assert!(why.contains("global options cannot be passed"), "{flag}: {why}");
        }
        assert!(ok("status", &[], GitWrites::Disabled, None));
        assert!(waarom("log", &["--oneline"], GitWrites::Disabled, None).contains("global options"));
        assert!(ok("log", &["src/main.rs"], GitWrites::Disabled, None));
    }

    #[test]
    fn the_literal_double_dash_stays_allowed_as_a_separator() {
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
        assert_eq!(
            argv(root, "rev_parse", &list(&["HEAD"]), None),
            args(&["git", "-C", "/repo", "-c", "core.quotePath=false", "--no-pager", "rev-parse", "HEAD"])
        );
        let commit = argv(root, "commit", &[], Some("vast gezet"));
        let pos = commit.iter().position(|t| t == "-m").unwrap();
        assert_eq!(commit[pos + 1], "vast gezet");
        assert!(base("reset", None).is_empty());
    }

    #[test]
    fn the_summary_reads_as_a_command_a_person_would_type() {
        assert_eq!(summary("status", &[], None), "git status --short --branch");
        assert_eq!(summary("add", &list(&["src/a.rs"]), None), "git add -- src/a.rs");
        assert_eq!(summary("commit", &[], Some("vast gezet")), "git commit -m \"vast gezet\"");
        assert!(!summary("commit", &[], Some("x")).contains("-C"));
        assert!(!summary("commit", &[], Some("x")).contains("no-pager"));
    }
}
