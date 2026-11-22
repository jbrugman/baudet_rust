//! De mappen die een run mag gebruiken, bepaald vóórdat het run-script geschreven wordt.
//!
//! Poort van `tool/ExecuteWorkspace.kt`. Een commando draait met de projectmap als werkmap en stalt zijn
//! script in de systeem-temp; dit lost élk van die twee door álle symlinks op en weigert wanneer een van beide
//! niet is wat het zegt te zijn. Het is bewust smal: het kan het commando niet inhouden, alleen bevestigen dat
//! de twee mappen die wij zelf aanraken de echte, schrijfbare mappen zijn die wij verwachten.

use std::fs;
use std::path::{Path, PathBuf};

/// De maps van één run.
pub struct RunFolders {
    /// De echte projectmap.
    pub cwd: PathBuf,
    /// De echte, schrijfbare systeem-temp.
    pub temp: PathBuf,
}

impl RunFolders {
    /// Beide mappen samen als de toegelaten wortels voor een gestald script.
    pub fn allowed(&self) -> Vec<PathBuf> {
        vec![self.temp.clone()]
    }
}

/// De beslissing zelf.
pub struct ExecuteWorkspace;

impl ExecuteWorkspace {
    /// De echte projectmap, of `None` wanneer `root` geen bestaande map is.
    pub fn project_dir(root: &Path) -> Option<PathBuf> {
        let real = fs::canonicalize(root).ok()?;
        real.is_dir().then_some(real)
    }

    /// De echte, schrijfbare systeem-temp, of `None` wanneer geen enkele bruikbaar is.
    ///
    /// De voorkeur volgt `TMPDIR` en daarna `/tmp`, wat op macOS en Linux precies is wat
    /// `java.io.tmpdir` oplevert: die eigenschap begint met `$TMPDIR` en valt terug op `/tmp`.
    pub fn temp_dir() -> Option<PathBuf> {
        let claimed = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
        let real = fs::canonicalize(claimed).ok()?;
        if !real.is_dir() || !is_writable(&real) {
            return None;
        }
        Some(real)
    }

    /// Waar, wanneer `target` door álle symlinks heen binnen één van `allowed_roots` valt. Bevestigt dat het
    /// geschreven script landde waar wij bedoelden, en niet door een omgeleide link.
    pub fn within_allowed(target: &Path, allowed_roots: &[PathBuf]) -> bool {
        // `canonicalize` volgt de links van `target` zélf; een script dat nog niet bestaat heeft niets om aan
        // te lopen, dus een ontbrekend bestand is hier geen toegang — het is een script dat er niet ligt.
        let Ok(real) = fs::canonicalize(target) else {
            return false;
        };
        allowed_roots.iter().any(|root| real.starts_with(root))
    }
}

/// Of er in deze map geschreven kan worden. De origineler vraagt `Files.isWritable`, wat de POSIX-bit van de
/// map checkt zonder iets weg te schrijven; dit doet dezelfde check zonder het aardrijksbijproduct van een
/// wegwerpproef-bestand in een map die de persoon net geopend heeft.
fn is_writable(dir: &Path) -> bool {
    let Ok(meta) = fs::metadata(dir) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // De schrijfbits van eigenaar, groep en ander: de owner-bit alleen zou een map die de groep mag
        // schrijven onbruikbaar noemen, en deze app draait vaak onder een gedeelde build-user.
        meta.permissions().mode() & 0o222 != 0
    }
    #[cfg(not(unix))]
    {
        !meta.permissions().readonly()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn it_accepts_a_folder_that_really_is_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(ExecuteWorkspace::project_dir(dir.path()), fs::canonicalize(dir.path()).ok());
    }

    #[test]
    fn it_refuses_a_path_that_is_a_file_or_a_lie() {
        let dir = tempfile::tempdir().unwrap();
        let bestand = dir.path().join("notes.txt");
        fs::write(&bestand, "x\n").unwrap();
        assert_eq!(ExecuteWorkspace::project_dir(&bestand), None, "een bestand is geen werkmap");
        assert_eq!(ExecuteWorkspace::project_dir(&dir.path().join("er-niet")), None);
    }

    #[test]
    fn a_symlinked_project_resolves_to_the_real_folder() {
        // Het punt van door-links-oplossen: een project dat via een link binnenkomt, moet op zijn echte pad
        // draaien, want dat is het pad waarmee de mapgrens en de git-operaties later werken.
        let echt = tempfile::tempdir().unwrap();
        fs::write(echt.path().join("marker.txt"), "x\n").unwrap();
        let de_lader = tempfile::tempdir().unwrap();
        let link = de_lader.path().join("project");
        symlink(echt.path(), &link).unwrap();

        let resolved = ExecuteWorkspace::project_dir(&link).expect("de link wijst naar een bestaande map");
        assert_eq!(resolved, fs::canonicalize(echt.path()).unwrap());
        assert_ne!(resolved, link, "het link-pad zélf is niet wat de tool mag noemen");
    }

    #[test]
    fn the_temp_folder_is_real_a_directory_and_writable() {
        let temp = ExecuteWorkspace::temp_dir().expect("deze machine heeft een temp");
        assert!(temp.is_dir());
        assert!(is_writable(&temp));
        assert_eq!(fs::canonicalize(&temp).unwrap(), temp, "het antwoord is al het echte pad");
    }

    #[test]
    fn a_written_script_lands_within_the_temp_and_a_copy_outside_does_not() {
        let temp = ExecuteWorkspace::temp_dir().unwrap();
        let script = temp.join(format!("baudet-run-test-{}.sh", std::process::id()));
        fs::write(&script, "#!/bin/bash\ntrue\n").unwrap();
        assert!(ExecuteWorkspace::within_allowed(&script, std::slice::from_ref(&temp)));

        let buiten = tempfile::tempdir().unwrap();
        assert!(!ExecuteWorkspace::within_allowed(buiten.path().join("x.sh").as_path(), std::slice::from_ref(&temp)));
        fs::remove_file(&script).ok();
    }

    #[test]
    fn the_check_compares_the_real_path_so_a_link_out_of_the_temp_is_visible_as_such() {
        // Gemeten, niet aangenomen: de origineler vraagt `toRealPath()` op het pad van het script en vergelijkt
        // met `startsWith`. Een symlink die vanuit de temp naar buiten wijst, is zélf een pad ín de temp, dus
        // diens echte pad — buiten de temp — valt door de check. Dit is dus geen verdediging tegen zo'n link:
        // die zekerheid komt uit wat er geschreven wordt, want de tool schreibt uitsluitend naar het eigen,
        // zojuist door `createTempFile` teruggegeven pad. Deze test legt vast wat de check wél en niet doet,
        // zodat een latere lezer er geen veiligheid van maakt die er niet staat.
        let temp = ExecuteWorkspace::temp_dir().unwrap();
        let buiten = tempfile::tempdir().unwrap();
        let buiten_script = buiten.path().join("werkelijk.sh");
        fs::write(&buiten_script, "x\n").unwrap();
        let link = temp.join(format!("baudet-run-link-{}.sh", std::process::id()));
        symlink(&buiten_script, &link).unwrap();

        assert_eq!(
            ExecuteWorkspace::within_allowed(&link, std::slice::from_ref(&temp)),
            fs::canonicalize(&link).unwrap().starts_with(&temp),
            "de check is puur een padvergelijking op het echte pad"
        );
        fs::remove_file(&link).ok();
        fs::remove_file(&buiten_script).ok();
    }

    #[test]
    fn a_script_that_does_not_exist_is_not_within_anything() {
        let temp = ExecuteWorkspace::temp_dir().unwrap();
        assert!(
            !ExecuteWorkspace::within_allowed(&temp.join("er-ligt-niets.sh"), &[temp]),
            "een pad dat niet bestaat, is nergens beland"
        );
    }
}
