//! De brug naar het systeemklembord, voor het ene menu dat de origineler in beide lijsten zet.
//!
//! `FileTreePanel.kt:533-539` is het hele verhaal: één menu met één item, en dat item schrijft
//! `displayPath(path)` op het klembord — het pad zoals het paneel het toont, niet zoals de schijf
//! het heet. Twee regels daarvan zijn geen bijzaak en hebben daarom een eigen functie en een eigen
//! test: `displayPath` relativizeert tegen de projectmap (`:540-544`, de map zelf wordt `"."`), en
//! het item is `isEnabled = path != null` — "it copies nothing rather than an empty string when
//! there is no path" (`:532`). Deze module kent geen egui, zodat die regels testeerbaar blijven
//! zonder venster, precies zoals `WindowLayout` in de origineler vrij van Swing is.

/// Het pad zoals het paneel het toont en zoals het op het klembord gaat.
///
/// `FileTreePanel.displayPath`: relativizeer tegen de projectmap, en laat de map zelf als `"."`
/// lezen. Zonder projectmap is er niets waartegen te relativeren, dan is de tekst wat het pad zelf
/// zegt (`path?.toString().orEmpty()` — blanco voor geen pad, en dat is precies wat `copyable`
/// hieronder tegenhoudt).
pub fn display_path(root: Option<&std::path::Path>, path: Option<&std::path::Path>) -> String {
    let Some(path) = path else {
        return String::new();
    };
    match root {
        Some(root) if path == root => ".".to_string(),
        // `root.relativize(path)` in de bron: lukt het inkorten niet, omdat het pad buiten de
        // map ligt, dan blijft de volledige naam staan — `strip_prefix` faalt op precies dezelfde
        // voorwaarde en is hier daarom de enkelste vorm.
        Some(root) => path
            .strip_prefix(root)
            .map(|rel| rel.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned()),
        None => path.to_string_lossy().into_owned(),
    }
}

/// Of het menu-item aandrukbaar is: `item.isEnabled = path != null`.
pub fn copyable(path: Option<&std::path::Path>) -> bool {
    path.is_some()
}

/// De tekst van het menu-item: `Texts[Key.CopyPath]`, wiens Engelse waarde in `Key.kt:390` staat.
/// Eenmaal hier, zodat de woorden uit één plek komen in plaats van uit het hoofd van een tekening.
pub const COPY_PATH: &str = "Copy path";

/// Het woord van `Key.CopyPath` in de taal van het venster, zodra i18n bestaat (TODO §H). Nu EN,
/// want `Texts` is EN-default (`Language.kt`), en de rest van de UI draagt die taal ook.
pub const COPY_PATH_NL: &str = "Kopieer pad";

/// Zet `text` op het systeemklembord.
///
/// De herkomst is één aanroep zonder terugwaarde (`Toolkit…setContents`, `:537`), dus dit is het
/// hele beleid: geen groottegrens, geen witlijst, geen melding. De `Result` blijft wél bestaan,
/// omdat "het klembordweigering bestaat niet" onbewezen is — wie hem wil afhandelen, kan; wie hem
/// negeert, doet dat net als de bron en niet per ongeluk.
pub fn copy(text: &str) -> Result<(), String> {
    arboard::Clipboard::new()
        .and_then(|mut board| board.set_text(text.to_string()))
        .map_err(|problem| format!("clipboard: {problem}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn een_projectmap_relatief_en_de_map_zelf_is_een_punt() {
        let root = Path::new("/projects/baudet");
        // De twee gevallen van `displayPath` én het geval ervóór: een pad buiten de map kan niet
        // relatief en blijft dus staan zoals het heet, net als `root.relativize` faalt-terugval.
        assert_eq!(display_path(Some(root), Some(root)), ".");
        assert_eq!(
            display_path(Some(root), Some(Path::new("/projects/baudet/src/main.rs"))),
            "src/main.rs"
        );
        assert_eq!(
            display_path(Some(root), Some(Path::new("/other/thing.txt"))),
            "/other/thing.txt"
        );
    }

    #[test]
    fn zonder_projectmap_is_het_pad_zijn_eigen_tekst() {
        assert_eq!(display_path(None, Some(Path::new("/tmp/x/y.rs"))), "/tmp/x/y.rs");
        // `path?.toString().orEmpty()`: geen pad is blanco tekst, en `copyable` houdt dat af.
        assert_eq!(display_path(None, None), "");
        assert!(!copyable(None), "wel blanco kopiëren is wat de bron weigert");
        assert!(copyable(Some(Path::new("/tmp"))));
    }

    #[test]
    fn het_woord_staat_in_key_dot_kt() {
        // `Key.kt:390`: CopyPath("Copy path", "Kopieer pad", "Copier le chemin", "Pfad kopieren").
        assert_eq!(COPY_PATH, "Copy path");
        assert_eq!(COPY_PATH_NL, "Kopieer pad");
    }
}
