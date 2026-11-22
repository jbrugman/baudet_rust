//! Een begrensde, verborgenvrije opsomming van een projectmap, als hint voor het model.
//!
//! Poort van `tool/FolderListing.kt`. Doel: een verkeerd pad moet een verbeterbare fout zijn, geen dood
//! eind. Daarom begrensd aan drie kanten — diepte, aantal en dot-mappen — zodat het noch de schijf lekkt
//! noch duur is om mee terug te sturen. Beide bestands-tool gebruiken dezelfde hint, dus ze noemen de map
//! op één manier.

use std::fs;
use std::path::Path;

/// Hoe diep de hint hooguit afdaalt.
const MAX_DEPTH: usize = 3;
/// Hoeveel namen de hint hooguit noemt.
const MAX_ENTRIES: usize = 50;

/// Lege string wanneer er niets te noemen is, anders een leidende spatie en de gesorteerde namen.
pub fn hint(root: &Path) -> String {
    let mut names: Vec<String> = Vec::new();
    walk(root, root, 1, &mut names);
    if names.is_empty() {
        return String::new();
    }
    names.sort();
    format!(" Files here: {}", names.join(", "))
}

/// Eén niveau. Een map die niet leesbaar is, telt als niets te melden — een hint mag nooit een fout zijn.
fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > MAX_DEPTH || out.len() >= MAX_ENTRIES {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut children: Vec<std::path::PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    children.sort();
    for child in children {
        let Some(name) = child.file_name().and_then(|n| n.to_str()) else { continue };
        if name.starts_with('.') {
            continue;
        }
        if child.is_dir() {
            walk(root, &child, depth + 1, out);
        } else if child.is_file() && out.len() < MAX_ENTRIES {
            if let Ok(relative) = root.normalize_relative(&child) {
                out.push(relative);
            }
        }
    }
}

/// Het map-relatieve pad als tekst. Java's `relativize` doet dit, en Rust' `Path::relativize` bestaat niet
/// in std, dus het is hier één hulpfunctie: de root afpellen en de rest als tekst geven.
trait RelativeTo {
    fn normalize_relative(&self, child: &Path) -> Result<String, ()>;
}

impl RelativeTo for Path {
    fn normalize_relative(&self, child: &Path) -> Result<String, ()> {
        let rest = child.strip_prefix(self).map_err(|_| ())?;
        rest.to_str().map(str::to_string).ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn an_empty_folder_hints_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(hint(dir.path()), "");
    }

    #[test]
    fn names_are_relative_and_sorted_behind_one_space() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("b.txt"), "x").unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/a.rs"), "x").unwrap();
        let text = hint(dir.path());
        assert_eq!(text, " Files here: b.txt, src/a.rs");
    }

    #[test]
    fn dot_entries_are_never_listed() {
        // `.git` zou de hint vullen met duizenden namen en `node_modules` doet hetzelfde: beide buiten spel.
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/config"), "x").unwrap();
        fs::write(dir.path().join(".env"), "x").unwrap();
        fs::write(dir.path().join("real.txt"), "x").unwrap();
        assert_eq!(hint(dir.path()), " Files here: real.txt");
    }

    #[test]
    fn the_walk_never_descends_past_the_depth_cap() {
        // De cap is relatief aan de root, dus het enige eerlijke bewijs is een ladder van mappen met een
        // kenbaar bestand op élk niveau: de eerste drie moeten er staan, de rest niet. (Een pad tellen
        // vanaf `/` zou hier misleiden: een tempdir van `tempfile` woont zelf al op niveau vier.)
        let dir = tempfile::tempdir().unwrap();
        for (level, name) in ["n1.txt", "n2.txt", "n3.txt", "n4.txt", "n5.txt"].iter().enumerate() {
            let mut path = dir.path().to_path_buf();
            for _ in 0..level {
                path = path.join("dieper");
            }
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join(name), "x").unwrap();
        }
        let text = hint(dir.path());
        for name in ["n1.txt", "n2.txt", "n3.txt"] {
            assert!(text.contains(name), "{name} staat binnen drie niveaus en moet genoemd worden: {text}");
        }
        for name in ["n4.txt", "n5.txt"] {
            assert!(!text.contains(name), "{name} staat onder de diepte-cap van drie: {text}");
        }
    }

    #[test]
    fn the_listing_is_capped_so_a_hint_stays_cheap() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..80 {
            fs::write(dir.path().join(format!("f{i:03}.txt")), "x").unwrap();
        }
        let text = hint(dir.path());
        let listed = text.strip_prefix(" Files here: ").unwrap().split(", ").count();
        assert_eq!(listed, MAX_ENTRIES, "een hint mag niet onbegrensd groeien");
    }
}
