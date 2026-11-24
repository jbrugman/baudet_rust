
pub const LEFT_WIDTH: f32 = 280.0;
pub const RIGHT_WIDTH: f32 = 320.0;

// Old configurations stored the offset of the right divider from the left edge.
pub const CENTER_WIDTH: f32 = 720.0;

pub const CHANGES_HEIGHT: f32 = 200.0;
pub const CHANGES_MIN: f32 = 96.0;

pub(crate) const CENTER_MIN: f32 = 220.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dividers {
    pub left: i32,
    pub right: i32,
    pub right_width: i32,
    pub files: i32,
    pub composer: i32,
    pub thinking: i32,
}

// Zero in existing settings means the panel has no saved width.
pub(crate) fn encode_width(width: f32) -> i32 {
    let width = width.round() as i32;
    if width > 0 { width } else { -1 }
}

pub(crate) fn decode_width(stored: i32, fallback: f32) -> f32 {
    if stored == -1 { 0.0 } else { resolve_divider(None, stored, fallback) }
}

pub fn resolve_divider(previous: Option<f32>, persisted: i32, fallback: f32) -> f32 {
    match previous {
        Some(seen) => seen,
        None if persisted > 0 => persisted as f32,
        None => fallback,
    }
}

pub fn worth_remembering(live: f32, extent: f32, stored: i32) -> i32 {
    if live <= 0.0 || live >= extent {
        stored
    } else {
        live as i32
    }
}

pub fn band_height(split_height: f32, divider: f32) -> f32 {
    (split_height - divider).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn een_gezien_stand_wint_van_het_bestand() {
        assert_eq!(resolve_divider(Some(311.0), 280, 240.0), 311.0);
    }

    #[test]
    fn een_ontbrekende_gezichtspunt_valt_op_het_bestand_terug() {
        assert_eq!(resolve_divider(None, 280, 240.0), 280.0);
    }

    #[test]
    fn een_nul_in_het_bestand_is_geen_breedte() {
        assert_eq!(resolve_divider(None, 0, 280.0), 280.0);
        assert_eq!(resolve_divider(None, -5, 280.0), 280.0, "een onleesbaar getal redt niets");
    }

    #[test]
    fn een_ingeklapte_balk_vervangt_het_onthield_niet() {
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
        assert_eq!(band_height(700.0, 500.0), 200.0);
        assert_eq!(band_height(900.0, 900.0 - CHANGES_HEIGHT), CHANGES_HEIGHT);
        assert_eq!(band_height(300.0, 480.0), 0.0, "een balk voorbij de vloer is geen diepte");
    }

    #[test]
    fn de_verticale_as_meten_tegen_zijn_hoogte() {
        assert_eq!(worth_remembering(480.0, 700.0, 200), 480);
        assert_eq!(worth_remembering(700.0, 700.0, 200), 200, "op de vloer is het een inklapping");
        assert_eq!(worth_remembering(900.0, 700.0, 200), 200);
    }
}
