//! De maten die het venster onthoudt: de drie verdeelstanden tussen zijn panelen.
//!
//! De fabrieksmaten hieronder zijn geen eigen voorkeur: `LEFT_WIDTH` is de val van `Main.kt:164`,
//! `CENTER_WIDTH` die van `Main.kt:171` en `CHANGES_HEIGHT`/`CHANGES_MIN` die van
//! `FileTreePanel.kt:629` (`CHANGES_HEIGHT = 200`) en `MIN_FILES_VISIBLE`.
//!
//! De origineler kent ze als drie `Int`-velden in zijn één JSON-regel (`AppConfig.kt:152-158`) en
//! lost ze op met één formule (´WindowLayout.resolveDivider´, `WindowLayout.kt:36-37`) die bij het
//! sluiten wordt teruggevraagd (´clampedDivider´, `Main.kt:236-241`). Dit bestand poort die twee,
//! want het verschil is zichtbaar voor iemand die het venster inricht: zonder deze regel staat het
//! paneel na elke herstart weer op zijn fabrieksbreedte.

/// De breedte van de linkerbalk toen de venster-`JSplitPane` hem had, of 0 toen er niets was
/// opgeslagen (`AppConfig.kt:152`: ´var leftDivider: Int = 0´). Zoveel is er van de bestandsboom
/// (´rightDivider´, :153) en van de hoogte die de changes-lijst onder die boom krijgt
/// (´filesDivider´, :158).
/// De breedte waarmee de linkerbalk opent zonder dat er ooit een werd gemeten
/// (`Main.kt:164`: `resolveDivider(previous, state.config.leftDivider, 280)`).
pub const LEFT_WIDTH: f32 = 280.0;

/// De breedte waarmee de linkerflank en het middenvlak samen openden, dus de plek waar de
/// bestandsboom begint (`Main.kt:171`: `resolveDivider(previous, state.config.rightDivider, 720)`).
pub const CENTER_WIDTH: f32 = 720.0;

/// De hoogte waarmee de changes-lijst onder de bestandsboom opent (`FileTreePanel.kt:629`,
/// `CHANGES_HEIGHT = 200`), en de vloer die `MIN_FILES_VISIBLE` daaronder legt.
pub const CHANGES_HEIGHT: f32 = 200.0;
pub const CHANGES_MIN: f32 = 96.0;

/// De smalste strook die een zijkolom ooit krijgen mag: de vloer die `tests/folder_row.rs` per
/// constructie meet voor de maprij, en dus geen eigen voorkeur is maar de grens van die test.
pub const PANEL_MIN: f32 = 180.0;
pub(crate) const LEFT_PANEL_MIN: f32 = 190.0;
pub(crate) const CENTER_MIN: f32 = 220.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dividers {
    pub left: i32,
    pub right: i32,
    pub right_width: i32,
    pub files: i32,
}

/// De verdeelstand die een paneel bij het openen krijgt: wat deze sessie al liet zien, anders wat
/// het bestand onthield, anders de fabrieksmaat.
///
/// ´WindowLayout.resolveDivider´, regel voor regel: ´previous ?: persisted.takeIf { it > 0 } ?:
/// fallback´. De ´> 0´ is geen margin: 0 betekent "nooit opgeslagen" en geen "breedte nul", want
/// een ingeklapte balk mag niet de heropende standaard worden (´Main.kt:243-246´ zegt dat letterlijk).
pub fn resolve_divider(previous: Option<f32>, persisted: i32, fallback: f32) -> f32 {
    match previous {
        Some(seen) => seen,
        None if persisted > 0 => persisted as f32,
        None => fallback,
    }
}

/// Wat er de volgende keer in het bestand mag staan: de live-stand van een verdeel balk, tenzij die
/// staat ingeklapt of nog niet gelegd is — dan blijft wat er stond.
///
/// ´Main.kt:236-241´: ´if (location <= 0 || location >= extent) stored else location´. ´extent´ is
/// de as waar de balk overheen loopt: breedte voor de twee horizontale scheidingen, hoogte voor de
/// verticalen (´orientation == VERTICAL_SPLIT → split.height´). Zonder die as-verschuiving zou een
/// verticalen balk — die van de changes-lijst — op zijn breedte worden beoordeeld, en een venster
/// breder dan hoog zou zijn hoogte als "voorbij de rand" weggooien.
pub fn worth_remembering(live: f32, extent: f32, stored: i32) -> i32 {
    if live <= 0.0 || live >= extent {
        stored
    } else {
        live as i32
    }
}

/// De hoogte die een band onder een vertikale balk overhoudt.
///
/// `WindowLayout.componentHeightForDivider`: de origineler bewaart deze hoogte en niet de positie
/// van de balk, want een hoogte hangt niet af van hoe hoog het venster op dat moment is. Vloer 0
/// zoals de bron (`coerceAtLeast(0)`): een balk voorbij de vloer is geen diepte.
pub fn band_height(split_height: f32, divider: f32) -> f32 {
    (split_height - divider).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn een_gezien_stand_wint_van_het_bestand() {
        // `previous ?: persisted`: een herbouwd venster draagt zijn live-stand over, zoals
        // `Main.kt:146-149` die van de vorige `FileTreePanel` overneemt.
        assert_eq!(resolve_divider(Some(311.0), 280, 240.0), 311.0);
    }

    #[test]
    fn een_ontbrekende_gezichtspunt_valt_op_het_bestand_terug() {
        assert_eq!(resolve_divider(None, 280, 240.0), 280.0);
    }

    #[test]
    fn een_nul_in_het_bestand_is_geen_breedte() {
        // `takeIf { it > 0 }`: 0 = nooit opgeslagen. Een fabrieksstart mag niet op nul breed
        // openen, en dat is precies wat een ruwe overname van de stored-waarde zou doen.
        assert_eq!(resolve_divider(None, 0, 280.0), 280.0);
        assert_eq!(resolve_divider(None, -5, 280.0), 280.0, "een onleesbaar getal redt niets");
    }

    #[test]
    fn een_ingeklapte_balk_vervangt_het_onthield_niet() {
        // De reden van de hele functie: "a collapse never becomes the reopened default".
        assert_eq!(worth_remembering(0.0, 1200.0, 280), 280);
        assert_eq!(worth_remembering(1200.0, 1200.0, 280), 280, "aan de rand is ook ingeklapt");
        assert_eq!(worth_remembering(-1.0, 1200.0, 280), 280);
    }

    #[test]
    fn een_gelegde_balk_is_wel_waard() {
        assert_eq!(worth_remembering(455.5, 1200.0, 280), 455);
    }

    #[test]
    fn de_band_hoogte_volgt_de_balk_en_niet_de_willekeur() {
        // Een balk op 500 in een kolom van 700 laat 200 onder zich: de fabrieksband van
        // `CHANGES_HEIGHT`. De positie vloeit uit de hoogte voort en niet omgekeerd, dus in een
        // kolom van 900 is dezelfde band van 200 een balk op 700.
        assert_eq!(band_height(700.0, 500.0), 200.0);
        assert_eq!(band_height(900.0, 900.0 - CHANGES_HEIGHT), CHANGES_HEIGHT);
        assert_eq!(band_height(300.0, 480.0), 0.0, "een balk voorbij de vloer is geen diepte");
    }

    #[test]
    fn de_verticale_as_meten_tegen_zijn_hoogte() {
        // Het verlengstuk van de opmerking in `Main.kt:236-241`: de changes-lijst loopt verticaal,
        // dus zijn remmend geval is de hoogte van de strook, niet de breedte. een venster van
        // 1200 x 700 met een balk op 480 is gelegd; gegenereerd op breedte zou 480 < 1200 ook
        // lukken, en dat geval is hier dezelfde — dus het onderscheid valt pas op zijn eigen as.
        assert_eq!(worth_remembering(480.0, 700.0, 200), 480);
        assert_eq!(worth_remembering(700.0, 700.0, 200), 200, "op de vloer is het een inklapping");
        // En het geval dat de verkeerde as verborgen hield: 900 op een hoogte van 700.
        assert_eq!(worth_remembering(900.0, 700.0, 200), 200);
    }
}
