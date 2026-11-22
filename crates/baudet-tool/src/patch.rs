//! Eén exacte zoek-vervang in een bestand, en het vouwen van zodanige edits in bestands-tekst.
//!
//! Poort van `tool/Edit.kt`, `tool/PatchOutcome.kt` en `tool/PatchApplier.kt`. Beide zijn bewust puur — geen
//! bestandssysteem, geen draden, geen model — zodat een verkeerde patch een falende test is in plaats van een
//! slechte write. Exacte tekst gaat boven regelnummers, omdat een lokaal model regels slecht telt en bij elke
//! edit wegloopt van de werkelijkheid: of de match landt, of hij landt niet, wat triviaal te verifiëren is, en
//! de aanvrager krijgt een exacte reden wanneer hij niet landt.

/// Eén exacte zoek-vervang in een bestand. `old_text` moet letterlijk voorkomen; `replace_all` alleen wanneer
/// de aanvrager élke voorkoming bedoelt te veranderen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub old_text: String,
    pub new_text: String,
    pub replace_all: bool,
}

impl Edit {
    /// Een edit die één voorkoming vervangt, de vorm die de meeste aanvragen bedoelen.
    pub fn one(old_text: impl Into<String>, new_text: impl Into<String>) -> Edit {
        Edit { old_text: old_text.into(), new_text: new_text.into(), replace_all: false }
    }
}

/// De uitkomst van edits vouwen in de tekst van een bestand, als waarde gehouden zodat élke weg — een
/// weigering inbegrepen — testbaar is zonder de schijf aan te raken. Een weigering meldt nooit een half
/// ge-patcht bestand: of élke edit landde, of de aanvrager krijgt de ene precieze reden waarom het stopte. De
/// reden-tekst is model-gericht en blijft dus Engels: hij gaat rechtstreeks terug naar het model opdat het zich
/// kan verbeteren.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchOutcome {
    /// Élke edit toegepast; `content` is de nieuwe volledige tekst.
    Applied { content: String },
    /// Niets toegepast; `reason` noemt de enige edit die niet landde.
    Rejected { reason: String },
}

/// Het vouwen zelf.
pub struct PatchApplier;

impl PatchApplier {
    /// Past `edits` in volgorde toe op `content`, elk op het resultaat van het vorige, of weigert op de eerste
    /// edit die niet kan landen.
    pub fn apply(content: &str, edits: &[Edit]) -> PatchOutcome {
        if edits.is_empty() {
            return PatchOutcome::Rejected { reason: "no edits were given".into() };
        }
        let mut text = content.to_owned();
        for (index, edit) in edits.iter().enumerate() {
            // Eén op één met de origineler, die zijn telling op index 0 laat beginnen en in tekst nummer vanaf 1.
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
                // Alleen de éérste voorkoming, en `replace` met een count van een zou hier de hele string als
                // patroon moeten laten lopen; `replacen` is precies dat en staat buiten ASCII niet in de war.
                text.replacen(&edit.old_text, &edit.new_text, 1)
            };
        }
        PatchOutcome::Applied { content: text }
    }
}

/// Hoe vaak niet-overlappend `needle` in `haystack` voorkomt; `-1` voor een lege needle. Die -1 is geen
/// telling maar een signaal dat de aanroeper al vóór deze functie afvangt; de origineler kent hem en deze
/// poort behoudt hem, zodat een latere lezer niet denkt dat een lege needle hier een nul moet opleveren.
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
        // Het punt van vouwen: edit twee kan pas vinden wat edit één er zette.
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
        // De cap staat een meervoudige match alleen toe met replace_all, dus dit pad is er een van precies één
        // vindplaats: "doel" staat hier één keer, de rest van de tekst bevat de letters niet.
        let edits = [Edit::one("doel", "Vervangen")];
        assert_eq!(content_of(&PatchApplier::apply("kop doel staart\n", &edits)), "kop Vervangen staart\n");
    }

    #[test]
    fn a_needle_appearing_inside_other_words_still_counts_as_multiple_matches() {
        // "ab" staat drie keer als substring, en het zijn substring-tellingen, geen woord-tellingen. De cap
        // weigert derhalve, want het model bedoelde waarschijnlijk één van die drie nietblind.
        let outcome = PatchApplier::apply("ab ab2 ab3\n", &[Edit::one("ab", "Z")]);
        assert_eq!(outcome, rejected("edit 1: old_text matches 3 times; add surrounding context or set replace_all"));
    }

    #[test]
    fn occurrences_do_not_double_count_an_overlapping_needle() {
        // "aa" in "aaaa" landt op 0 en 2: niet-overlappend dus twee, niet drie.
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
