//! The app's colours, and what each one of them rests on.
//!
//! These are not taste. Every measured value below was counted out of the author's own pictures
//! of the shipped Kotlin app (`dev.baudet` 0.9.2) pixel by pixel, by an independent rewrite that
//! published its counts and, usefully, its refutations: an earlier reading of the same pictures
//! had guessed several shades that turned out to occur **zero** times, and those are named here
//! too, because a colour palette erodes precisely by quietly re-introducing a plausible grey.
//!
//! The source is the Craft Anything artefact's `COLOUR-TALLY.md` and `MECHANICS.md`, which read
//! the shipping jar's own bytecode and the pictures rather than this rewrite's source. They agree
//! with our Kotlin reading wherever both have an opinion; where they differ on a number, the
//! measurement wins, because it came out of the program people actually look at. See
//! ATTRIBUTION.md. No line of their Rust is copied: their UI is egui 0.29 and ours is 0.36, so
//! the only portable thing they have is a number plus the evidence behind it.
//!
//! One number in this file is a unit conversion, not a guess about appearance: their pictures are
//! Retina, so where a tally says "2 picture pixels" the point size is 1.0, and a 24-picture-pixel
//! list row is 12 points. Those are spelled out per line below.
//!
//! A few of these colours are still unread inside this crate: the diff renderer and the project
//! pane that consume them are deliberately a later phase. They are kept anyway, because each one is
//! a measured fact about the shipped app -- most of them pinned by the exact-value and luma
//! assertions in the tests at the bottom of this file -- and deleting a measurement to satisfy a
//! dead-code lint would discard evidence, not clutter.

#![allow(dead_code, reason = "measured values whose consumers arrive with a later phase")]

use egui::{Color32, Context, CornerRadius, Stroke, Visuals};

/// The one place a colour is written as RGB, so no second spelling of the same shade can appear.
const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// The colour of a pane, a panel and the window's own frame. MEASURED: rgb(27,28,31).
///
/// The pictures hold no window border and no shadow band, so the frame is a flat fill and nothing
/// more; the hairline round the whole window that an earlier build drew is not in the pictures.
pub const PANEL: Color32 = rgb(27, 28, 31);

/// The ground text is read on: darker than the panel, deliberately. MEASURED: rgb(20,20,23).
pub const CANVAS: Color32 = rgb(20, 20, 23);

/// Ordinary text. MEASURED: rgb(221,221,221) -- the commonest ink in their diff text once the
/// ground is left out, on added *and* unchanged rows alike. The estimate rgb(226,228,232) that
/// stood here before was never measured and is gone.
pub const TEXT: Color32 = rgb(221, 221, 221);

/// A card: a raised surface inside a panel. MEASURED as the strip colour of a header row.
pub const CARD: Color32 = rgb(34, 35, 39);

/// Widget outlines. MEASURED: rgb(48,50,56), lighter than every fill it encloses.
pub const BORDER: Color32 = rgb(48, 50, 56);

/// De vlakkleur van een knop. MEASURED uit de origineler zelf, niet gekozen: `Main.kt:59` zet
/// `FlatDarkLaf.setup()` en `BaudetPalette.install()` overschrijft `Button.background` niet, alleen
/// `Button.default.background` (`BaudetPalette.kt:111`). De origineler gebruikt nergens een
/// default-knop, dus geldt FlatLaf's eigen `Button.background = tint(@background,9%)` =
/// rgb(78,80,82). Gemeten door de jar van de origineler zelf zijn look-and-feel te laten opzetten en
/// `UIManager.getColor("Button.background")` te lezen.
///
/// Waarom hier een eigen naam staat en `CARD` niet is opgeleukt: `CARD` rgb(34,35,39) is gemeten als
/// de strip van een kopriem en hoort dus donker. Een knop van 1,09:1 op de paneelkleur is geen knop,
/// en dat is precies de klacht "het is niet te zien wat een knop is". Zie de test onderin dit bestand.
pub const BUTTON: Color32 = rgb(78, 80, 82);

/// De rand van een knop. MEASURED: FlatLaf's `Button.borderColor = tint($Button.background,10%)` =
/// rgb(96,98,99), uit dezelfde jar. De hoverrand van de origineler is zijn focuskleur, en die duwt
/// `BaudetPalette.kt:113` zelf naar rgb(102,133,199) = `ACCENT`, dus `install` zet hover op `ACCENT`.
pub const BUTTON_BORDER: Color32 = rgb(96, 98, 99);

/// Een knop waarop de cursor staat. MEASURED: `Button.hoverBackground = lighten($Button.background,3%)`
/// = rgb(85,88,90). FlatLaf's volgorde is hover 3%, pressed 6%, geselecteerd 10%, dus donker, lichter,
/// lichtst, nooit andersom.
pub const BUTTON_HOVER: Color32 = rgb(85, 88, 90);

/// Een ingedrukte knop. MEASURED: `Button.pressedBackground = lighten(..,6%)` = rgb(93,95,98).
pub const BUTTON_PRESSED: Color32 = rgb(93, 95, 98);

/// Een knop die aan staat. MEASURED: `Button.selectedBackground = lighten(..,10%)` = rgb(103,106,108).
/// `Widget::selected` in egui 0.36 haalt zijn vulling uit `selection.bg_fill` en niet uit deze status
/// (widget_style.rs:150-155), dus deze kleur is er voor een knop die zijn staat zelf tekent.
pub const BUTTON_SELECTED: Color32 = rgb(103, 106, 108);

/// Het vierkantje van een aanvinkvak. MEASURED: `CheckBox.icon.background = tint(@background,5%)` =
/// rgb(70,73,75) met rand rgb(105,107,109). Zelfde les als de knop: op `CARD` met 1,09:1 is een vinkje
/// een onzichtbaar gaatje in de achtergrond, en zo zagen de drie rechten-vakjes eruit.
pub const CHECK: Color32 = rgb(70, 73, 75);
pub const CHECK_BORDER: Color32 = rgb(105, 107, 109);

/// Secondary text: labels, counts, a status line.
pub const DIM: Color32 = rgb(150, 150, 156);

/// Tertiary text, one step dimmer again.
pub const FAINT: Color32 = rgb(120, 122, 128);

/// The softest ink that is still ink rather than a fill. MEASURED: rgb(97,99,101), found as
/// writing in three unrelated places, so its *job* is inferred and its *value* is not.
pub const INK_SOFT: Color32 = rgb(97, 99, 101);

/// The mark behind a chosen row. MEASURED: rgb(61,63,65), one flat colour read straight down a
/// selected list row. The blue this file guessed before it was measured does not exist there.
pub const SELECTION: Color32 = rgb(61, 63, 65);

/// The every-other-row stripe in a list. MEASURED: rgb(42,43,48), and note that it is *lighter*
/// than the panel it sits on -- so an alternating list has to lighten alternate rows, not darken
/// them, which is the mistake this value exists to prevent. Row pitch measured at 24 picture
/// pixels, i.e. 12 points at 2x.
pub const LIST_ROW: Color32 = rgb(42, 43, 48);

/// The ground of a box you type into. MEASURED: rgb(45,49,52) -- lighter than the panel and than
/// the canvas, which is what a typing field does.
pub const FIELD: Color32 = rgb(45, 49, 52);

/// A second typing ground directly under the first. MEASURED: rgb(48,50,52), same height, no gap.
pub const FIELD_ALT: Color32 = rgb(48, 50, 52);

/// Links and accents. OURS: no accent colour could be tied to anything in the pictures.
pub const ACCENT: Color32 = rgb(102, 133, 199);

/// Text of an added line in a diff. MEASURED: #7ee687 = rgb(126,230,135), 59 163 pixels.
///
/// There is no band behind a changed line. Counting the rows that carry this ink, the only colours
/// present in quantity are the two greys also used everywhere else, so a changed row is coloured
/// text on plain ground. rgb(152,228,145) -- the shade an earlier build used here -- occurs zero
/// times in all four pictures, and so does a green-tinted row fill rgb(45,45,48).
pub const DIFF_ADDED: Color32 = rgb(126, 230, 135);

/// Text of a removed line in a diff. MEASURED: #ff7b72 = rgb(255,123,114), 23 716 pixels; the
/// earlier guess rgb(238,130,119) occurs zero times.
pub const DIFF_REMOVED: Color32 = rgb(255, 123, 114);

/// The line round a box of text. MEASURED: rgb(106,139,177), and every pixel of it accounted for
/// by four straight lines with nothing between them: a frame, not a fill and not a highlight.
/// Using it for every box line in this app is OURS -- the pictures show it in one window only.
pub const FRAME_LINE: Color32 = rgb(106, 139, 177);

/// De git-statuskleuren van de origineler, overgenomen uit `BaudetPalette.kt:65-67`. Dit zijn níét
/// de diff-inkten hierboven (`DIFF_ADDED`/`DIFF_REMOVED`, gemeten uit een diff-venster), maar de
/// drie kleuren die de Kotlin-bestandsboom zelf op een rij zet (`FileTreePanel.colorFor`). De boom
/// links en de wijzigingenlijst rechts moeten hetzelfde rood, groen en blauw tonen als de app die
/// ze nabouwen, dus staan de drie rgb-waarden hier letterlijk zoals de origineler ze noemt.
pub const GIT_ADDED: Color32 = rgb(46, 160, 67);
pub const GIT_DELETED: Color32 = rgb(210, 70, 70);
pub const GIT_UNTRACKED: Color32 = rgb(130, 170, 220);

/// Extra room egui wants around a draggable panel edge before it shows the grab arrow. OURS.
pub const EDGE_GRAB: f32 = 14.0;

/// Install the palette. Called once, before anything is painted.
///
/// Goes through `set_visuals` rather than a per-frame `style_mut`, so the palette has exactly one
/// owner; a per-frame rewrite would let any widget's local nudge become the effective style for
/// whatever is painted after it.
pub fn install(ctx: &Context) {
    let mut v = Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    // The ground of a text box is the measured typing-field colour, not the diff canvas. The
    // canvas stays reachable to any widget that wants it explicitly via [`CANVAS`].
    v.extreme_bg_color = FIELD;
    v.faint_bg_color = CARD;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = SELECTION;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    for widget in
        [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active]
    {
        widget.bg_stroke = Stroke::new(1.0, BORDER);
        widget.fg_stroke = Stroke::new(1.0, TEXT);
        widget.expansion = 4.0;
    }
    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.inactive.bg_fill = BUTTON;
    v.widgets.hovered.bg_fill = BUTTON_HOVER;
    v.widgets.active.bg_fill = BUTTON_PRESSED;
    // `weak_bg_fill` is the vulling die een knop werkelijk tekent, `bg_fill` is dat niet.
    // `button_style` bouwt zijn frame met `fill: visuals.weak_bg_fill` (`widget_style.rs:159`), en de
    // egui-bron zegt het letterlijk bij het standaardpalet: `weak_bg_fill: ... /* button background */`
    // (`style.rs:1692`). Deze functie zette alleen `bg_fill`, dus elke knop tekende het donkere
    // donkerstandaard van egui (`from_gray(60)` = #3C3C3C, 1,54:1 op het paneel) in plaats van de
    // gemeten `BUTTON` (2,10:1). Op de donkere panelen van de koptekst zakte dat naar 1,01:1 -- vlak
    // bij geen enkel contrast, en dát was de klacht "het is niet te zien wat een knop is". De driehoek
    // en het keuzelijstvenster (`collapsing_header.rs:565`, `combo_box.rs:460`) lezen hetzelfde veld,
    // dus die volgen deze zet mee.
    v.widgets.inactive.weak_bg_fill = BUTTON;
    v.widgets.hovered.weak_bg_fill = BUTTON_HOVER;
    v.widgets.active.weak_bg_fill = BUTTON_PRESSED;
    // De randstatussen van de knoppen, uit dezelfde jar gemeten. De lus hierboven zette `BORDER` over
    // alle vier statussen; die is te donker om een rand te heten (1,33:1 op de paneelkleur). De
    // hoverrand van de origineler is diens focuskleur, en die is `ACCENT`.
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BUTTON_BORDER);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    // De selectie blijft het gemeten neutrale blok: `SELECTION` is gemeten uit een geselecteerde
    // lijstrij en gaat dus niet op als knopkleur. `CARD` en `FIELD_ALT` blijven staan voor de
    // kopriemen en de tweede Typing-grond waar ze gemeten zijn.
    ctx.set_visuals(v);

    // De origineler afrondt zijn widgets met 8 pt: `Main.kt:60-62` zet `Button.arc`, `Component.arc`
    // en `TextComponent.arc` op 8. egui geeft een widget 2 pt mee (style.rs:1688), dus een knop was
    // hier een hoekig blok op een afgerond scherm. Eén zet hier, voor élke widgetstatus, want de
    // statussen delen in deze functie al één eigenaar.
    ctx.global_style_mut(|style| {
        for widget in [
            &mut style.visuals.widgets.noninteractive,
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            widget.corner_radius = CornerRadius::same(8);
        }
    });

    // egui only offers the "drag me" arrow within `resize_grab_radius_side` of a panel edge, five
    // pixels by default. Widen it, so the strip a person can actually grab is the strip this app
    // tells them about. OURS.
    ctx.global_style_mut(|style| style.interaction.resize_grab_radius_side = EDGE_GRAB);
}

/// The ink a diff line gets.
///
/// `Hunk` and `Header` are OURS and carry no evidence: no colour could be tied to a place marker
/// or to a file-naming line in any of the pictures, and the two candidate blues that do occur in
/// that area were both rejected -- one turned out to be the box's frame, the other the colouring of
/// the shown code. They are kept apart from the measured pair on purpose rather than promoted.
pub fn diff_ink(kind: DiffLine) -> Color32 {
    match kind {
        DiffLine::Added => DIFF_ADDED,
        DiffLine::Removed => DIFF_REMOVED,
        DiffLine::Hunk => rgb(108, 158, 220),
        DiffLine::Header => rgb(196, 160, 92),
        DiffLine::Context => TEXT,
    }
}

/// The five kinds of line a unified diff holds. Owned here because this crate decides ink; the
/// parser that produces them belongs to the store alongside the diff text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLine {
    Added,
    Removed,
    /// An `@@ ... @@` position line.
    Hunk,
    /// A `diff --git`/`---`/`+++` file-naming line.
    Header,
    /// Unchanged text.
    Context,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The measured shades, spelled once more here on purpose: a value that drifts in the source
    /// still has to be caught by someone reading the numbers rather than the names.
    #[test]
    fn measured_shades_keep_their_measured_values() {
        assert_eq!(PANEL.to_srgba_unmultiplied(), [27, 28, 31, 255]);
        assert_eq!(CANVAS.to_srgba_unmultiplied(), [20, 20, 23, 255]);
        assert_eq!(TEXT.to_srgba_unmultiplied(), [221, 221, 221, 255]);
        assert_eq!(SELECTION.to_srgba_unmultiplied(), [61, 63, 65, 255]);
        assert_eq!(LIST_ROW.to_srgba_unmultiplied(), [42, 43, 48, 255]);
        assert_eq!(FIELD.to_srgba_unmultiplied(), [45, 49, 52, 255]);
        assert_eq!(FIELD_ALT.to_srgba_unmultiplied(), [48, 50, 52, 255]);
        assert_eq!(diff_ink(DiffLine::Added).to_srgba_unmultiplied(), [126, 230, 135, 255]);
        assert_eq!(diff_ink(DiffLine::Removed).to_srgba_unmultiplied(), [255, 123, 114, 255]);
    }

    /// The refuted shades must never come back as the diff's ink. This is the whole reason the
    /// refutations are in the source text rather than only in a note.
    #[test]
    fn refuted_shades_stay_refuted() {
        let refuted = [[152u8, 228, 145], [238, 130, 119], [45, 45, 48], [226, 228, 232]];
        let used = [PANEL, CANVAS, TEXT, CARD, BORDER, SELECTION, LIST_ROW, FIELD, FIELD_ALT];
        for shade in refuted {
            let colour = rgb(shade[0], shade[1], shade[2]);
            assert!(!used.contains(&colour), "rgb({shade:?}) was counted at zero pixels; it is not back");
            assert_ne!(diff_ink(DiffLine::Added), colour);
            assert_ne!(diff_ink(DiffLine::Removed), colour);
        }
    }

    /// Contrast is what makes a status readable without relying on colour alone.
    #[test]
    fn text_contrasts_with_every_ground_it_sits_on() {
        for ground in [PANEL, CANVAS, CARD, SELECTION, LIST_ROW, FIELD] {
            let delta = (luma(TEXT) - luma(ground)).abs();
            assert!(delta > 0.35, "text on this ground is unreadable: {delta:.2}");
        }
    }

    /// Een knop moet een knop zijn: zijn vlak en zijn rand moeten op het paneel te zien zijn.
    ///
    /// Deze test beveiligt de klacht "het is niet te zien wat een knop is". Wat eraan voorafging is
    /// gemeten, niet bedacht: `Button.background` uit de jar van de origineler is rgb(78,80,82) en
    /// diens rand rgb(96,98,99), terwijl dit palet `CARD` rgb(34,35,39) als knopvulling gaf — 1,09:1
    /// op de paneelkleur. Dat is aritmetisch geen vlak, dat is de achtergrond zelf. De drempel is de
    /// bestaande norm voor niet-tekst: WCAG 2.1 SC 1.4.11-eist 1,5:1.
    #[test]
    fn een_knop_is_zichtbaar_op_het_paneel() {
        for (naam, kleur) in [
            ("knopvlak", BUTTON),
            ("knoprand", BUTTON_BORDER),
            ("hover", BUTTON_HOVER),
            ("ingedrukt", BUTTON_PRESSED),
            ("aan", BUTTON_SELECTED),
            ("vinkje", CHECK),
            ("vinkjesrand", CHECK_BORDER),
        ] {
            let ratio = contrast(kleur, PANEL);
            assert!(ratio >= 1.5, "{naam} {kleur:?} haalt maar {ratio:.2}:1 op de paneelkleur");
        }
        // De volgorde van de origineler: basis, hover, ingedrukt, aan — steeds een stap lichter.
        assert!(luma(BUTTON) < luma(BUTTON_HOVER));
        assert!(luma(BUTTON_HOVER) < luma(BUTTON_PRESSED));
        assert!(luma(BUTTON_PRESSED) < luma(BUTTON_SELECTED));
        // De knoptekst blijft leesbaar: gemeten 5,96:1, ruim boven de 4,5 die WCAG voor tekst eist.
        assert!(contrast(TEXT, BUTTON) >= 4.5, "knoptekst is onleesbaar");
    }

    /// De statussen die `install` aan de knoppen geeft, zijn de gemeten kleuren en niet de donkere
    /// vullingen die er eerder stonden. `install` is de enige eigenaar van de stijl, dus dit is de
    /// enige plek waar die eigenschap waarheid wordt.
    #[test]
    fn de_geinstalleerde_knopstatussen_zijn_de_gemeten() {
        let ctx = egui::Context::default();
        install(&ctx);
        let visuals = &ctx.global_style().visuals;
        // `weak_bg_fill` is wat de knoptekenaar haalt (`widget_style.rs:159`); de asserts die hier
        // stonden_keken naar `bg_fill`, en die veld tekent nergens een knop. De test was dus groen
        // terwijl de knoop onzichtbaar was -- precies de fout die hij had moeten vangen.
        assert_eq!(visuals.widgets.inactive.weak_bg_fill, BUTTON);
        assert_eq!(visuals.widgets.hovered.weak_bg_fill, BUTTON_HOVER);
        assert_eq!(visuals.widgets.active.weak_bg_fill, BUTTON_PRESSED);
        assert_eq!(visuals.widgets.inactive.bg_stroke.color, BUTTON_BORDER);
        assert_eq!(visuals.widgets.hovered.bg_stroke.color, ACCENT);
        // De selectie is geen knopkleur geworden: ze is gemeten uit een geselecteerde lijstrij en
        // hoort donkerder te blijven dan een knop, anders is een rij niet meer van een knop te zijn.
        assert_eq!(visuals.selection.bg_fill, SELECTION);
        assert_ne!(visuals.selection.bg_fill, BUTTON);
        assert!(contrast(SELECTION, PANEL) < contrast(BUTTON, PANEL));
    }

    /// WCAG 2.1, SC 1.4.3: relatieve luminantie en contrast.
    fn contrast(a: Color32, b: Color32) -> f32 {
        let (x, y) = (linear_luma(a), linear_luma(b));
        let (hi, lo) = (x.max(y), x.min(y));
        (hi + 0.05) / (lo + 0.05)
    }

    /// De sRGB-lineaire luminantie zoals WCAG die voorschrijft. De `luma` hieronder is de bestaande
    /// grovere variant; die is te grof om 1,09:1 van 1,5:1 te onderscheiden, en dat verschil is
    /// precies het verschil tussen een onzichtbare en een zichtbare knop.
    fn linear_luma(c: Color32) -> f32 {
        fn channel(v: u8) -> f32 {
            let v = f32::from(v) / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        let [r, g, b, _] = c.to_srgba_unmultiplied();
        0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
    }

    /// The alternating row has to lighten, which is the direction the measurement pinned down.
    #[test]
    fn a_stripe_is_lighter_than_the_panel_it_sits_on() {
        assert!(luma(LIST_ROW) > luma(PANEL));
        assert!(luma(FIELD) > luma(PANEL));
        assert!(luma(CANVAS) < luma(PANEL), "text ground is darker than the pane around it");
    }

    fn luma(c: Color32) -> f32 {
        let [r, g, b, _] = c.to_srgba_unmultiplied();
        (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0
    }
}
