//! De enige plek die beslist of een pad waar een tool aan wil komen echt binnen de projectmap blijft.
//!
//! Poort van `tool/FolderBoundary.kt`. De lexicale `starts_with` die een aanroeper eerst doet, vouwt alleen
//! het letterlijke pad; hij lost geen symlinks op. Een link die binen de map woont maar ernaar buiten wijst,
//! zou daardoor doorkomen en daarna door die link gelezen of geschreven worden. Deze module lost het echte
//! doel op, volgt élke symlink, en geeft het alleen terug wanneer het onder de echte root blijft.
//!
//! Hij eist dat het pad bestaat — beide bestands-tools garanderen dat al vóór ze hierbellen.

use std::path::{Path, PathBuf};

/// Het resultaat van [`FolderBoundary::resolve_file_within`]: het echte bestand, of de reden van weigering
/// zoals het model die hoort te lezen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// Een regulair bestand dat binnen de map blijft, als opgelost pad.
    Inside(PathBuf),
    /// De weigering, in dezelfde model-gerichte formulering voor beide bestands-tool.
    Refused(String),
}

impl Resolved {
    /// Het pad wanneer het binnen bleef. Handig voor een tool die alleen deInside-kant doorschuift.
    pub fn path(self) -> Option<PathBuf> {
        match self {
            Resolved::Inside(path) => Some(path),
            Resolved::Refused(_) => None,
        }
    }
}

/// De mapgrens zelf. Een struct in plaats van de `object` van Kotlin, maar met dezelfde staatsloosheid: al
/// zijn input is het root- en het kandidaat-pad.
#[derive(Debug, Clone, Copy)]
pub struct FolderBoundary;

impl FolderBoundary {
    /// Het echte, symlink-opgeloste pad wanneer het binnen `root` blijft, anders `None`.
    ///
    /// Beide kanten moeten oplossen; faalt een van beide — de root bestaat niet, of de kandidaat is een
    /// kapotte link — dan is het antwoord onthouden, niet "binnen".
    pub fn real_within(root: &Path, resolved: &Path) -> Option<PathBuf> {
        let real_root = root.canonicalize().ok()?;
        let real = resolved.canonicalize().ok()?;
        if real.starts_with(&real_root) {
            Some(real)
        } else {
            None
        }
    }

    /// Lost `rel` op binnen `root` en bevestigt dat het een regulair bestand is dat binnen de map blijft
    /// zodra links worden gevolgd. Gemeenschappelijk voor de read- en de edit-tool, zodat een verkeerd pad
    /// bij allebei op dezelfde manier wordt geweigerd, met dezelfde formulering.
    pub fn resolve_file_within(root: &Path, rel: &str) -> Resolved {
        let resolved = root.join(rel).normalize();
        if !resolved.starts_with(root) {
            return Resolved::Refused(format!("{rel} resolves outside the project folder"));
        }
        if !resolved.is_file() {
            return Resolved::Refused(format!(
                "{rel} does not exist inside the folder.{}",
                crate::folder_listing::hint(root)
            ));
        }
        if Self::real_within(root, &resolved).is_none() {
            return Resolved::Refused(format!("{rel} resolves outside the folder once links are followed"));
        }
        Resolved::Inside(resolved)
    }
}

/// Een pad zonder `..`- en `.`-segmenten, puur tekstueel — de `normalize()` van de JDK. Componenten na een
/// `..` op de top blijven staan, want er is niets om tegen in te korten; de `starts_with`-controle eroverheen
/// wijst ze dan af, wat de conservatieve richting is.
trait Normalize {
    fn normalize(&self) -> PathBuf;
}

impl Normalize for PathBuf {
    fn normalize(&self) -> PathBuf {
        let mut parts: Vec<std::path::Component<'_>> = Vec::new();
        for part in self.components() {
            match part {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    if matches!(parts.last(), Some(std::path::Component::Normal(_))) {
                        parts.pop();
                    } else {
                        parts.push(part);
                    }
                }
                other => parts.push(other),
            }
        }
        parts.iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn a_plain_file_inside_the_folder_passes() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("notes.txt"), "x").unwrap();
        let outcome = FolderBoundary::resolve_file_within(dir.path(), "notes.txt");
        let Resolved::Inside(path) = &outcome else {
            panic!("een gewoon bestand binnen de map moet doorkomen, kreeg {outcome:?}");
        };
        assert!(path.ends_with("notes.txt"));
    }

    #[test]
    fn a_dot_dot_escape_is_refused_before_any_read() {
        let dir = tempfile::tempdir().unwrap();
        // De parent van de tijdelijke map bevat werelike bestanden; de lexicale check moet ze afweren.
        let outcome = FolderBoundary::resolve_file_within(dir.path(), "../outside.txt");
        assert!(matches!(outcome, Resolved::Refused(reason) if reason.contains("outside the project folder")));
    }

    #[test]
    fn a_missing_file_is_a_refusal_that_names_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("present.rs"), "x").unwrap();
        let Resolved::Refused(reason) = FolderBoundary::resolve_file_within(dir.path(), "geen.txt") else {
            panic!("een ontbrekend bestand is een weigering");
        };
        // De hint maakt een verkeerd pad een verbeterbare fout in plaats van een dood eind.
        assert!(reason.contains("present.rs"), "de hint moet de map noemen: {reason}");
    }

    #[test]
    fn a_directory_is_not_a_file_for_this_gate() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        assert!(matches!(FolderBoundary::resolve_file_within(dir.path(), "sub"), Resolved::Refused(_)));
    }

    #[test]
    fn a_symlink_to_a_file_outside_the_folder_is_refused() {
        // Dit is de echte greep: de link zelf woont binen de map en doorstaat dus elke lexicale check.
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        fs::write(&secret, "geheim").unwrap();
        std::os::unix::fs::symlink(&secret, dir.path().join("link.txt")).unwrap();

        let outcome = FolderBoundary::resolve_file_within(dir.path(), "link.txt");
        assert!(
            matches!(&outcome, Resolved::Refused(reason) if reason.contains("once links are followed")),
            "een link naar buiten mag nooit door: {outcome:?}"
        );
    }

    #[test]
    fn a_symlink_to_a_directory_outside_is_refused_too() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("top.txt"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("ext")).unwrap();

        let outcome = FolderBoundary::resolve_file_within(dir.path(), "ext/top.txt");
        assert!(
            matches!(&outcome, Resolved::Refused(reason) if reason.contains("once links are followed")),
            "een map-link naar buiten is dezelfde hazard: {outcome:?}"
        );
    }

    #[test]
    fn a_symlink_staying_inside_the_folder_still_works() {
        // Omgekeerde kant, anders bewijst de symlink-test alleen dat alles geweigerd wordt.
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("real")).unwrap();
        fs::write(dir.path().join("real/target.txt"), "x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real/target.txt"), dir.path().join("in.txt")).unwrap();
        assert!(matches!(FolderBoundary::resolve_file_within(dir.path(), "in.txt"), Resolved::Inside(_)));
    }

    #[test]
    fn real_within_needs_both_ends_to_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let inside = dir.path().join("a");
        fs::write(&inside, "x").unwrap();
        assert!(FolderBoundary::real_within(dir.path(), &inside).is_some());
        // Een kapotte link lost niet op en is dus niet "binnen".
        let broken = dir.path().join("broken");
        std::os::unix::fs::symlink(dir.path().join("niets"), &broken).unwrap();
        assert!(FolderBoundary::real_within(dir.path(), &broken).is_none());
        // Een onbestaande root is hetzelfde antwoord.
        assert!(FolderBoundary::real_within(&dir.path().join("geen-map"), &inside).is_none());
    }
}
