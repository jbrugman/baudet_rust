//! Wat één enkele "altijd"-beslissing pinnt, en waarover hij dus zwijgt.
//!
//! Poort van `tool/CommandSignature.kt`. Deze module verleent **geen** vertrouwen: het read-only-ambacht
//! leeft volledig in [`crate::safe_commands`], en een herbruikbaar gezins-teken wordt alleen gegeven aan een
//! commando dat die belofte al houdt. Elk ander commando — een keten, een omleiding of een programma dat de
//! poort niet bewezen read-only kan heten — pint op zijn volledige getrimde tekst. De verleende gunst is dus
//! precies het commando waar de mens ja op zei.
//!
//! Dat sluit het gat waarbij één goedkeuring van een read van een programma later een write van hetzelfde
//! programma dekte: `sed` houdt zijn schrijfkracht in zijn positionele script, dus het is nooit bewezen
//! read-only, en zowel zijn read als zijn write signeren nu op hun eigen letterlijke tekst. De prijs is
//! bewust: een payload-programma moet per variatie opnieuw goedgekeurd worden.

use crate::safe_commands;

/// De waarde om voor `command` in de altijd-allow-lijst op te slaan en tegen te vergelijken bij een latere
/// run: een gezins-teken wanneer [`crate::safe_commands`] bewijst dat het read-only is, anders zijn hele
/// getrimde tekst. Opslaan en controleren moeten deze ene functie gebruiken, zodat beide kanten nooit
/// uit elkaar lopen.
pub fn pinned(command: &str) -> String {
    // Alleen runs van spaties en tabs vouwen. Een regeleinde blijft zoals het is: door een shell gescheiden
    // beteken het twee commando's, dus een commando dat daarin verschilt is een tweede commando en moet die
    // betekenis houden — nooit gevouwen tot één onschuldig ogende regel waar een gezin voor in zou staan.
    let canonical = safe_commands::fold_spaces(command);
    if safe_commands::is_safe(&canonical) {
        signature(&canonical)
    } else {
        canonical
    }
}

/// Het gezins-teken van `command`, als hij er een heeft. `None` betekent: geen veilig gezin, dus het commando
/// houdt zijn letterlijke pin uit [`pinned`]. Zie [`crate::safe_commands::trusted_key`] voor de regels.
pub fn trusted_entry(command: &str) -> Option<String> {
    safe_commands::trusted_key(command)
}

/// Het herbruikbare teken van een commando dat de poort al read-only bewees: het programma, zijn subcommando
/// wanneer het programma git is, en zijn vlaggen gesorteerd, zodat quoting en argumentvolgorde het teken niet
/// veranderen. Een bewezen read-only programma draagt geen positioneel script dat schrijven kan, dus één pin
/// dekt daarna dezelfde read over andere bestanden; positionele bestandsargumenten vallen daarvoor af.
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

/// De vlaggen, ontdubbeld en gesorteerd.
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
        // Dezelfde read over andere bestanden mag één pin dragen; dat is het hele idee.
        assert_eq!(pinned("grep pattern a.txt"), pinned("grep pattern b/c.txt"));
        // Volgorde en herhaling van vlaggen veranderen het teken niet.
        assert_eq!(pinned("sort -n -k2 a.txt"), pinned("sort -k2 -n -k2 b.txt"));
    }

    #[test]
    fn a_git_read_signs_with_its_subcommand() {
        assert_eq!(pinned("git diff main --stat"), "git diff --stat");
        assert_eq!(pinned("git status"), "git status");
        // Twee verschillende inspecties zijn twee tekens: één pin op diff dekt log niet.
        assert_ne!(pinned("git diff main"), pinned("git log main"));
    }

    #[test]
    fn a_payload_program_pins_to_its_verbatim_text() {
        // `sed` is niet bewezen read-only, dus zijn read en zijn signeren allebei op eigen tekst. Eén
        // beslissing reikt damit niet van de ene naar de andere.
        let read = pinned("sed -n 1,20p notes.txt");
        let write = pinned("sed -i s/a/b/ notes.txt");
        assert_eq!(read, "sed -n 1,20p notes.txt");
        assert_eq!(write, "sed -i s/a/b/ notes.txt");
        assert_ne!(read, write);
        assert_eq!(trusted_entry("sed -n 1,20p notes.txt"), None);
    }

    #[test]
    fn padding_never_makes_two_pins_of_one_chain() {
        // Een keten vertrouwd op letterlijke tekst mag niet opnieuw vragen omdat het model spatievingerde.
        assert_eq!(pinned("git diff    main  |  tail -5"), pinned("git diff main | tail -5"));
        // …maar een regeleinde is wél een tweede commando en blijft dus een ander teken.
        assert_ne!(pinned("git diff main\ntail -5"), pinned("git diff main tail -5"));
    }

    #[test]
    fn a_trusted_entry_is_the_same_question_answered_once() {
        // Opslaan en controleren delen één definitie: wat hier Some is, kan in de vertrouwde lijst matchen.
        assert_eq!(trusted_entry("git diff main | tail -5").as_deref(), Some("git diff | tail"));
        assert_eq!(trusted_entry("rm -rf build"), None);
    }
}
