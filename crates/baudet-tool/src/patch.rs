
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub old_text: String,
    pub new_text: String,
    pub replace_all: bool,
}

impl Edit {
    pub fn one(old_text: impl Into<String>, new_text: impl Into<String>) -> Edit {
        Edit { old_text: old_text.into(), new_text: new_text.into(), replace_all: false }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchOutcome {
    Applied { content: String },
    Rejected { reason: String },
}

pub struct PatchApplier;

impl PatchApplier {
    pub fn apply(content: &str, edits: &[Edit]) -> PatchOutcome {
        if edits.is_empty() {
            return PatchOutcome::Rejected { reason: "no edits were given".into() };
        }
        let mut text = content.to_owned();
        for (index, edit) in edits.iter().enumerate() {
            let at = index + 1;
            if edit.old_text.is_empty() {
                return PatchOutcome::Rejected { reason: format!("edit {at}: old_text must not be empty") };
            }
            let hits = occurrences(&text, &edit.old_text);
            if hits == 0 {
                return PatchOutcome::Rejected { reason: format!("edit {at}: old_text was not found") };
            }
            if hits > 1 && !edit.replace_all {
                return PatchOutcome::Rejected {
                    reason: format!(
                        "edit {at}: old_text matches {hits} times; add surrounding context or set replace_all"
                    ),
                };
            }
            text = if edit.replace_all {
                text.replace(&edit.old_text, &edit.new_text)
            } else {
                text.replacen(&edit.old_text, &edit.new_text, 1)
            };
        }
        PatchOutcome::Applied { content: text }
    }
}

fn occurrences(haystack: &str, needle: &str) -> i32 {
    if needle.is_empty() {
        return -1;
    }
    let mut count = 0i32;
    let mut from = 0usize;
    loop {
        match haystack[from..].find(needle) {
            None => return count,
            Some(at) => {
                count += 1;
                from += at + needle.len();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn applied(content: &str) -> PatchOutcome {
        PatchOutcome::Applied { content: content.into() }
    }

    fn rejected(reason: &str) -> PatchOutcome {
        PatchOutcome::Rejected { reason: reason.into() }
    }

    fn content_of(outcome: &PatchOutcome) -> &str {
        match outcome {
            PatchOutcome::Applied { content } => content,
            PatchOutcome::Rejected { reason } => panic!("verwacht toegepast, gekregen geweigerd: {reason}"),
        }
    }

    #[test]
    fn it_applies_one_edit_and_returns_the_whole_new_text() {
        let outcome = PatchApplier::apply("eerste regel\n", &[Edit::one("eerste", "tweede")]);
        assert_eq!(outcome, applied("tweede regel\n"));
    }

    #[test]
    fn it_folds_edits_in_order_each_on_the_result_of_the_previous() {
        let edits = [Edit::one("a", "b"), Edit::one("b", "c")];
        assert_eq!(content_of(&PatchApplier::apply("a\n", &edits)), "c\n");
    }

    #[test]
    fn it_refuses_when_there_are_no_edits() {
        assert_eq!(PatchApplier::apply("iets\n", &[]), rejected("no edits were given"));
    }

    #[test]
    fn it_refuses_an_empty_old_text_with_the_numbered_edit() {
        let edits = [Edit::one("iets", "goed"), Edit::one("", "leeg")];
        assert_eq!(PatchApplier::apply("iets\n", &edits), rejected("edit 2: old_text must not be empty"));
    }

    #[test]
    fn it_names_the_numbered_edit_that_was_not_found() {
        let edits = [Edit::one("iets", "goed"), Edit::one("ontbreekt", "goed")];
        assert_eq!(PatchApplier::apply("iets\n", &edits), rejected("edit 2: old_text was not found"));
    }

    #[test]
    fn it_refuses_an_ambiguous_match_and_offers_the_two_ways_out() {
        let outcome = PatchApplier::apply("x x x\n", &[Edit::one("x", "y")]);
        assert_eq!(outcome, rejected("edit 1: old_text matches 3 times; add surrounding context or set replace_all"));
    }

    #[test]
    fn replace_all_turns_the_refusal_into_a_change_of_every_occurrence() {
        let edit = Edit { old_text: "x".into(), new_text: "y".into(), replace_all: true };
        assert_eq!(content_of(&PatchApplier::apply("x x x\n", &[edit])), "y y y\n");
    }

    #[test]
    fn without_replace_all_only_the_first_occurrence_moves() {
        let edits = [Edit::one("doel", "Vervangen")];
        assert_eq!(content_of(&PatchApplier::apply("kop doel staart\n", &edits)), "kop Vervangen staart\n");
    }

    #[test]
    fn a_needle_appearing_inside_other_words_still_counts_as_multiple_matches() {
        let outcome = PatchApplier::apply("ab ab2 ab3\n", &[Edit::one("ab", "Z")]);
        assert_eq!(outcome, rejected("edit 1: old_text matches 3 times; add surrounding context or set replace_all"));
    }

    #[test]
    fn occurrences_do_not_double_count_an_overlapping_needle() {
        assert_eq!(occurrences("aaaa", "aa"), 2);
        assert_eq!(occurrences("aaa", "aa"), 1);
        assert_eq!(occurrences("niets", "aa"), 0);
        assert_eq!(occurrences("iets", ""), -1);
    }

    #[test]
    fn an_edit_may_insert_and_may_delete() {
        let insert = Edit::one("b\n", "b\nc\n");
        assert_eq!(content_of(&PatchApplier::apply("a\nb\n", &[insert])), "a\nb\nc\n");
        let delete = Edit::one("b\n", "");
        assert_eq!(content_of(&PatchApplier::apply("a\nb\n", &[delete])), "a\n");
    }

    #[test]
    fn utf8_survives_because_counting_is_in_bytes_but_replacing_keeps_characters() {
        let outcome = PatchApplier::apply("café\nnaïef\n", &[Edit::one("naïef", "droog")]);
        assert_eq!(content_of(&outcome), "café\ndroog\n");
    }
}
