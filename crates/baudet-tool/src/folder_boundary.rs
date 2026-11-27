
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Inside(PathBuf),
    Refused(String),
}

impl Resolved {
    pub fn path(self) -> Option<PathBuf> {
        match self {
            Resolved::Inside(path) => Some(path),
            Resolved::Refused(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FolderBoundary;

impl FolderBoundary {
    // Containment must hold after resolving symlinks.
    pub fn real_within(root: &Path, resolved: &Path) -> Option<PathBuf> {
        let real_root = root.canonicalize().ok()?;
        let real = resolved.canonicalize().ok()?;
        if real.starts_with(&real_root) {
            Some(real)
        } else {
            None
        }
    }

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
        assert!(reason.contains("present.rs"), "de hint moet de map noemen: {reason}");
    }

    #[test]
    fn a_directory_is_not_a_file_for_this_gate() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        assert!(matches!(FolderBoundary::resolve_file_within(dir.path(), "sub"), Resolved::Refused(_)));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_file_outside_the_folder_is_refused() {
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

    #[cfg(unix)]
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

    #[cfg(unix)]
    #[test]
    fn a_symlink_staying_inside_the_folder_still_works() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("real")).unwrap();
        fs::write(dir.path().join("real/target.txt"), "x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real/target.txt"), dir.path().join("in.txt")).unwrap();
        assert!(matches!(FolderBoundary::resolve_file_within(dir.path(), "in.txt"), Resolved::Inside(_)));
    }

    #[cfg(unix)]
    #[test]
    fn real_within_needs_both_ends_to_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let inside = dir.path().join("a");
        fs::write(&inside, "x").unwrap();
        assert!(FolderBoundary::real_within(dir.path(), &inside).is_some());
        let broken = dir.path().join("broken");
        std::os::unix::fs::symlink(dir.path().join("niets"), &broken).unwrap();
        assert!(FolderBoundary::real_within(dir.path(), &broken).is_none());
        assert!(FolderBoundary::real_within(&dir.path().join("geen-map"), &inside).is_none());
    }
}
