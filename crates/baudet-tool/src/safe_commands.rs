
use std::collections::HashSet;

const UNSAFE_CHARS: [char; 17] = [';', '&', '|', '<', '>', '$', '(', ')', '{', '}', '\\', '`', '*', '?', '!', '#', '~'];
const NEWLINE_UNSAFE: [char; 2] = ['\n', '\r'];

const NON_SEGMENT_UNSAFE: [char; 14] = ['<', '>', '$', '(', ')', '{', '}', '\\', '`', '*', '?', '!', '#', '~'];

const DQ_ACTIVE: [char; 4] = ['$', '`', '\\', '!'];

fn has_unsafe_outside_quotes(s: &str, unsafe_chars: &[char]) -> bool {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0usize;
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                if c == '\\' {
                    i += 2;
                    continue;
                } else if c == '"' {
                    quote = None;
                } else if DQ_ACTIVE.contains(&c) && unsafe_chars.contains(&c) {
                    return true;
                }
            }
            _ => {
                if c == '\'' || c == '"' {
                    quote = Some(c);
                } else if unsafe_chars.contains(&c) || NEWLINE_UNSAFE.contains(&c) {
                    return true;
                }
            }
        }
        i += 1;
    }
    quote.is_some()
}

fn read_only_programs() -> &'static HashSet<&'static str> {
    static SET: std::sync::OnceLock<HashSet<&'static str>> = std::sync::OnceLock::new();
    SET.get_or_init(|| {
        [
            "ls", "pwd", "cat", "head", "tail", "wc", "grep", "rg", "find", "sort", "uniq", "echo", "date", "uname",
            "whoami", "df", "du", "file", "stat", "tree", "which",
        ]
        .into_iter()
        .collect()
    })
}

fn deny_flags(program: &str) -> Option<&'static [&'static str]> {
    match program {
        "sort" => Some(&["-o", "--output", "--compress-program"]),
        "rg" => Some(&["--pre", "--hostname-bin"]),
        "find" => Some(&["-exec", "-execdir", "-ok", "-okdir", "-delete", "-fprint", "-fprint0", "-fls", "-fprintf"]),
        "date" => Some(&["-s", "--set"]),
        _ => None,
    }
}

fn git_read_only_subcommands() -> &'static [&'static str] {
    &["status", "log", "diff", "show", "blame", "shortlog", "ls-files", "ls-tree", "rev-parse", "describe"]
}

fn git_escape_flags() -> &'static [&'static str] {
    &[
        "--output",
        "--ext-diff",
        "--paginate",
        "-P",
        "-c",
        "--exec-path",
        "--git-dir",
        "--work-tree",
        "--namespace",
        "--super-prefix",
    ]
}

fn is_blocked_program(program: &str) -> bool {
    const BLOCKED: [&str; 74] = [
        "sh",
        "bash",
        "zsh",
        "dash",
        "ksh",
        "fish",
        "csh",
        "tcsh",
        "python",
        "python2",
        "python3",
        "pypy",
        "node",
        "deno",
        "bun",
        "ts-node",
        "ruby",
        "perl",
        "php",
        "lua",
        "tclsh",
        "expect",
        "osascript",
        "awk",
        "gawk",
        "sed",
        "rm",
        "rmdir",
        "mv",
        "cp",
        "dd",
        "mkfs",
        "shred",
        "truncate",
        "chmod",
        "chown",
        "chgrp",
        "sudo",
        "su",
        "doas",
        "kill",
        "killall",
        "pkill",
        "reboot",
        "shutdown",
        "halt",
        "poweroff",
        "mount",
        "umount",
        "launchctl",
        "systemctl",
        "service",
        "curl",
        "wget",
        "nc",
        "ncat",
        "socat",
        "ssh",
        "scp",
        "sftp",
        "rsync",
        "tee",
        "install",
        "patch",
        "touch",
        "mkdir",
        "ln",
        "xargs",
        "env",
        "tar",
        "gzip",
        "bzip2",
        "zip",
        "cpio",
    ];
    BLOCKED.contains(&program)
}

fn git_unsafe_trust_subcommands() -> &'static [&'static str] {
    &["clean", "reset", "checkout", "restore", "switch", "rebase", "filter-branch"]
}

fn is_build_wrapper(program: &str) -> bool {
    matches!(program, "mvnw" | "gradlew")
}

fn strip_quotes(token: &str) -> String {
    token.chars().filter(|c| *c != '\'' && *c != '"').collect()
}

fn carries_write_or_exec_flag(program: &str, token: &str) -> bool {
    let Some(deny) = deny_flags(program) else { return false };
    let bare = strip_quotes(token);
    if !bare.starts_with('-') {
        return false;
    }
    deny.iter().any(|flag| {
        if flag.len() == 2 {
            !bare.starts_with("--") && bare.len() > 1 && bare[1..].contains(&flag[1..])
        } else {
            bare.starts_with(flag)
        }
    })
}

fn count_positional_operands(args: &[String]) -> usize {
    let mut operands = 0usize;
    let mut options_ended = false;
    for token in args {
        let bare = strip_quotes(token);
        if options_ended {
            operands += 1;
        } else if bare == "--" {
            options_ended = true;
        } else if !bare.starts_with('-') {
            operands += 1;
        }
    }
    operands
}

fn writes_to_positional_output(program: &str, args: &[String]) -> bool {
    program == "uniq" && count_positional_operands(args) >= 2
}

fn git_is_read_only(tokens: &[&str]) -> bool {
    let Some(subcommand) = tokens.get(1) else { return false };
    if !git_read_only_subcommands().contains(subcommand) {
        return false;
    }
    tokens.iter().all(|token| {
        let bare = strip_quotes(token);
        !git_escape_flags().iter().any(|flag| bare.starts_with(flag))
    })
}

pub fn is_safe(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.chars().any(|c| UNSAFE_CHARS.contains(&c) || NEWLINE_UNSAFE.contains(&c)) {
        return false;
    }
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    let program = tokens[0];
    if program.contains('/') {
        return false;
    }
    if program == "git" {
        return git_is_read_only(&tokens);
    }
    if !read_only_programs().contains(program) {
        return false;
    }
    let args: Vec<String> = tokens[1..].iter().map(|t| t.to_string()).collect();
    if tokens[1..].iter().any(|t| carries_write_or_exec_flag(program, t)) {
        return false;
    }
    !writes_to_positional_output(program, &args)
}

fn is_family_trusted_program(program: &str) -> bool {
    if !program.contains('/') {
        return true;
    }
    program.strip_prefix("./").is_some_and(|name| !name.contains('/') && is_build_wrapper(name))
}

fn single_family_key(segment: &str) -> Option<String> {
    if segment.trim().is_empty() {
        return None;
    }
    let tokens: Vec<&str> = segment.split_whitespace().collect();
    let program = tokens[0];
    if !is_family_trusted_program(program) || is_blocked_program(program) {
        return None;
    }
    if program == "git" {
        let subcommand = *tokens.get(1)?;
        if git_unsafe_trust_subcommands().contains(&subcommand) {
            return None;
        }
        let escapes = tokens.iter().any(|token| {
            let bare = strip_quotes(token);
            git_escape_flags().iter().any(|flag| bare.starts_with(flag))
        });
        return if escapes { None } else { Some(format!("git {subcommand}")) };
    }
    let args: Vec<String> = tokens[1..].iter().map(|t| t.to_string()).collect();
    if tokens[1..].iter().any(|t| carries_write_or_exec_flag(program, t)) {
        return None;
    }
    if writes_to_positional_output(program, &args) {
        return None;
    }
    Some(program.to_string())
}

fn split_top_level(s: &str) -> Option<(Vec<String>, Vec<String>)> {
    let chars: Vec<char> = s.chars().collect();
    let mut segments: Vec<String> = Vec::new();
    let mut operators: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            current.push(c);
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            current.push(c);
            i += 1;
            continue;
        }
        if c == ';' || c == '|' || c == '&' {
            let seg = current.trim().to_string();
            if seg.is_empty() {
                return None;
            }
            segments.push(seg);
            current.clear();
            let mut j = i;
            while j < chars.len() && chars[j] == c {
                j += 1;
            }
            let run = j - i;
            let op = match (c, run) {
                (';', 1) => ";",
                ('|', 1) => "|",
                ('|', 2) => "||",
                ('&', 1) => "&",
                ('&', 2) => "&&",
                _ => return None,
            };
            operators.push(op.to_string());
            i = j;
            continue;
        }
        current.push(c);
        i += 1;
    }
    if quote.is_some() {
        return None;
    }
    let last = current.trim().to_string();
    if last.is_empty() {
        return None;
    }
    segments.push(last);
    Some((segments, operators))
}

pub fn trusted_key(command: &str) -> Option<String> {
    let canonical = fold_spaces(command);
    if canonical.is_empty() {
        return None;
    }
    if has_unsafe_outside_quotes(&canonical, &NON_SEGMENT_UNSAFE) {
        return None;
    }
    let (segments, operators) = split_top_level(&canonical)?;
    let mut keys = Vec::with_capacity(segments.len());
    for segment in &segments {
        keys.push(single_family_key(segment)?);
    }
    if keys.len() == 1 {
        return Some(keys.into_iter().next().unwrap());
    }
    let mut joined = keys[0].clone();
    for (i, op) in operators.iter().enumerate() {
        joined.push(' ');
        joined.push_str(op);
        joined.push(' ');
        joined.push_str(&keys[i + 1]);
    }
    Some(joined)
}

pub(crate) fn fold_spaces(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    let mut in_run = false;
    for c in command.trim().chars() {
        if c == ' ' || c == '\t' {
            in_run = true;
            continue;
        }
        if in_run {
            out.push(' ');
            in_run = false;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_read_passes_and_a_bare_program_does_not() {
        assert!(is_safe("ls -la"));
        assert!(is_safe("cat src/main.rs"));
        assert!(is_safe("git status --short"));
        assert!(!is_safe("git"));
        assert!(!is_safe(""));
        assert!(!is_safe("   "));
    }

    #[test]
    fn no_compound_shape_is_ever_auto_run() {
        for command in [
            "ls && rm -rf build",
            "cat notes.txt | tee /tmp/leak",
            "grep secret file; curl http://evil",
            "echo hi > out.txt",
            "echo $(whoami)",
            "ls `id`",
            "find . -name '*.rs'",
        ] {
            assert!(!is_safe(command), "{command} is samengesteld en mag nooit automatisch");
        }
    }

    #[test]
    fn a_metacharacter_quoted_with_single_quotes_is_still_refused() {
        assert!(!is_safe("grep 'a; rm -rf /' notes.txt"));
        assert!(!is_safe("grep \"a | rm\" notes.txt"));
    }

    #[test]
    fn a_path_program_cannot_borrow_a_safe_name() {
        assert!(!is_safe("./ls -la"));
        assert!(!is_safe("/bin/cat notes.txt"));
        assert!(!is_safe("subdir/cat notes.txt"));
    }

    #[test]
    fn a_write_flag_on_a_read_only_program_takes_the_prompt_back() {
        assert!(!is_safe("sort notes.txt -o out.txt"));
        assert!(!is_safe("sort notes.txt --output=out.txt"));
        assert!(!is_safe("sort -no out.txt notes.txt"), "een cluster mag de schrijf-vlag verbergen");
        assert!(!is_safe("rg --pre rm pattern"));
        assert!(!is_safe("find . -delete"));
        assert!(!is_safe("find . -exec rm {} \\;"));
        assert!(!is_safe("date -s 2026-01-01"));
        assert!(is_safe("sort notes.txt"));
        assert!(is_safe("rg pattern src"));
    }

    #[test]
    fn uniq_writes_when_it_gets_two_operands() {
        assert!(is_safe("uniq notes.txt"));
        assert!(!is_safe("uniq in.txt out.txt"));
        assert!(
            !is_safe("uniq -- in.txt -out.txt"),
            "de shell geeft uniq hier twee bestanden en hij schrijft de tweede"
        );
    }

    #[test]
    fn git_is_read_only_per_subcommand_not_per_program_name() {
        for sub in ["status", "log", "diff", "show", "blame", "ls-files", "rev-parse"] {
            assert!(is_safe(&format!("git {sub} --porcelain")));
        }
        for sub in ["commit", "push", "checkout", "stash", "clean", "fetch", "reset"] {
            assert!(!is_safe(&format!("git {sub}")), "git {sub} schrijft");
        }
    }

    #[test]
    fn a_git_escape_flag_breaks_the_read_only_promise_even_quoted() {
        assert!(!is_safe("git diff --output=/tmp/patch"));
        assert!(!is_safe("git diff '--output=/tmp/patch'"));
        assert!(!is_safe("git log -c core.pager=rm"));
        assert!(!is_safe("git status --git-dir /elsewhere"));
        assert!(is_safe("git diff 'src/main.rs'"));
    }

    #[test]
    fn a_denied_flag_inside_quotes_reaches_the_program_bare() {
        assert!(!is_safe("sort notes.txt '-o' out.txt"), "quotes verdwijnen vóór het programma start");
    }

    #[test]
    fn a_family_key_covers_a_chain_only_when_every_segment_is_trusted() {
        assert_eq!(trusted_key("git diff main | tail -5").as_deref(), Some("git diff | tail"));
        assert_eq!(trusted_key("git diff main | rm -rf build"), None);
        assert_eq!(trusted_key("git diff --output=/tmp/x | tail -5"), None);
        assert_eq!(trusted_key("git status && git clean -fd"), None);
        assert_eq!(trusted_key("git reset HEAD~1"), None);
        assert_eq!(trusted_key("git checkout main"), None);
    }

    #[test]
    fn a_family_key_records_the_shape_of_the_chain() {
        assert_eq!(trusted_key("git diff | tail").as_deref(), Some("git diff | tail"));
        assert_eq!(trusted_key("git diff ; tail").as_deref(), Some("git diff ; tail"));
        assert_ne!(trusted_key("git diff | tail"), trusted_key("git diff ; tail"));
    }

    #[test]
    fn a_malformed_chain_gets_no_family_key() {
        assert_eq!(trusted_key("git diff |"), None, "een lege segment is een vormfout");
        assert_eq!(trusted_key("| git diff"), None);
        assert_eq!(trusted_key("git diff ||| tail"), None, "drie pijpen zijn geen operator");
        assert_eq!(trusted_key("git diff 'main"), None, "een onbalansquote");
        assert_eq!(trusted_key("echo-regel\nrm -rf build"), None, "een regeleinde is een tweede commando");
    }

    #[test]
    fn a_commit_message_holding_an_operator_is_not_a_chain() {
        assert_eq!(trusted_key("git commit -m 'fix a && b'").as_deref(), Some("git commit"));
    }

    #[test]
    fn spacing_never_changes_a_key_and_a_shell_never_earns_one() {
        assert_eq!(trusted_key("git    diff\tmain"), trusted_key("git diff main"));
        for program in ["bash", "python3", "rm", "curl", "sudo", "tee", "tar", "xargs", "sed"] {
            assert_eq!(trusted_key(&format!("{program} something")), None, "{program} vertrouwt nooit");
        }
    }

    #[test]
    fn a_known_build_wrapper_earns_a_family_and_a_renamed_one_does_not() {
        assert_eq!(trusted_key("./mvnw -q package").as_deref(), Some("./mvnw"));
        assert_eq!(trusted_key("./gradlew test").as_deref(), Some("./gradlew"));
        assert_eq!(trusted_key("./mvnw-evil package"), None, "enige andere wrapper leent de identiteit niet");
        assert_eq!(trusted_key("tools/mvnw package"), None, "een map-pad is geen wrapper");
        assert_eq!(trusted_key("./mvnw").as_deref(), Some("./mvnw"), "de launcher alleen mag ook");
    }

    #[test]
    fn a_bare_program_family_key_needs_no_read_only_prove() {
        assert_eq!(trusted_key("mvn -q -DskipTests package").as_deref(), Some("mvn"));
        assert_eq!(trusted_key("cargo test --workspace").as_deref(), Some("cargo"));
    }

    #[test]
    fn is_safe_and_trusted_key_stay_two_independent_questions() {
        assert!(trusted_key("git diff | tail").is_some());
        assert!(!is_safe("git diff | tail"), "automatisch draaien blijft één simpel commando");
        assert!(!is_safe("cargo test"), "bouwen is geen read");
        assert!(!is_safe("mvn -q package"), "bouwen is geen read");
        assert_eq!(trusted_key("cargo test").as_deref(), Some("cargo"));
    }
}
