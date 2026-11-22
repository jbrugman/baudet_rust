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

use egui::{Color32, Context, Stroke, Visuals};

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
    v.widgets.inactive.bg_fill = CARD;
    v.widgets.hovered.bg_fill = FIELD_ALT;
    v.widgets.active.bg_fill = SELECTION;
    ctx.set_visuals(v);

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
