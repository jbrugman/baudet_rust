
use crate::safe_commands;

pub fn pinned(command: &str) -> String {
    let canonical = safe_commands::fold_spaces(command);
    if safe_commands::is_safe(&canonical) {
        signature(&canonical)
    } else {
        canonical
    }
}

pub fn trusted_entry(command: &str) -> Option<String> {
    safe_commands::trusted_key(command)
}

fn signature(command: &str) -> String {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let program = tokens[0];
    let mut parts: Vec<String> = Vec::new();
    if program == "git" {
        parts.push(program.to_string());
        if let Some(sub) = tokens.get(1) {
            parts.push((*sub).to_string());
        }
        parts.extend(flags(&tokens[2..]));
    } else {
        parts.push(program.to_string());
        parts.extend(flags(&tokens[1..]));
    }
    parts.join(" ")
}

fn flags(tokens: &[&str]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for token in tokens {
        if token.starts_with('-') && !seen.iter().any(|seen| seen == token) {
            seen.push((*token).to_string());
        }
    }
    seen.sort();
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proved_read_signs_to_its_family_so_files_may_vary() {
        assert_eq!(pinned("grep pattern a.txt"), pinned("grep pattern b/c.txt"));
        assert_eq!(pinned("sort -n -k2 a.txt"), pinned("sort -k2 -n -k2 b.txt"));
    }

    #[test]
    fn a_git_read_signs_with_its_subcommand() {
        assert_eq!(pinned("git diff main --stat"), "git diff --stat");
        assert_eq!(pinned("git status"), "git status");
        assert_ne!(pinned("git diff main"), pinned("git log main"));
    }

    #[test]
    fn a_payload_program_pins_to_its_verbatim_text() {
        let read = pinned("sed -n 1,20p notes.txt");
        let write = pinned("sed -i s/a/b/ notes.txt");
        assert_eq!(read, "sed -n 1,20p notes.txt");
        assert_eq!(write, "sed -i s/a/b/ notes.txt");
        assert_ne!(read, write);
        assert_eq!(trusted_entry("sed -n 1,20p notes.txt"), None);
    }

    #[test]
    fn padding_never_makes_two_pins_of_one_chain() {
        assert_eq!(pinned("git diff    main  |  tail -5"), pinned("git diff main | tail -5"));
        assert_ne!(pinned("git diff main\ntail -5"), pinned("git diff main tail -5"));
    }

    #[test]
    fn a_trusted_entry_is_the_same_question_answered_once() {
        assert_eq!(trusted_entry("git diff main | tail -5").as_deref(), Some("git diff | tail"));
        assert_eq!(trusted_entry("rm -rf build"), None);
    }
}
