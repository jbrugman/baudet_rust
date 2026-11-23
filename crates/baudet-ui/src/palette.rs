
#![allow(dead_code, reason = "measured values whose consumers arrive with a later phase")]

use egui::{Color32, Context, CornerRadius, Stroke, Visuals};

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub const PANEL: Color32 = rgb(27, 28, 31);

pub const CANVAS: Color32 = rgb(20, 20, 23);

pub const TEXT: Color32 = rgb(221, 221, 221);

pub const CARD: Color32 = rgb(34, 35, 39);

pub const BORDER: Color32 = rgb(48, 50, 56);

pub const BUTTON: Color32 = rgb(78, 80, 82);

pub const BUTTON_BORDER: Color32 = rgb(96, 98, 99);

pub const BUTTON_HOVER: Color32 = rgb(85, 88, 90);

pub const BUTTON_PRESSED: Color32 = rgb(93, 95, 98);

pub const BUTTON_SELECTED: Color32 = rgb(103, 106, 108);

pub const CHECK: Color32 = rgb(70, 73, 75);
pub const CHECK_BORDER: Color32 = rgb(105, 107, 109);

pub const DIM: Color32 = rgb(150, 150, 156);

pub const FAINT: Color32 = rgb(120, 122, 128);

pub const INK_SOFT: Color32 = rgb(97, 99, 101);

pub const SELECTION: Color32 = rgb(61, 63, 65);

pub const LIST_ROW: Color32 = rgb(42, 43, 48);

pub const FIELD: Color32 = rgb(45, 49, 52);

pub const FIELD_ALT: Color32 = rgb(48, 50, 52);

pub const ACCENT: Color32 = rgb(102, 133, 199);

pub const DIFF_ADDED: Color32 = rgb(126, 230, 135);

pub const DIFF_REMOVED: Color32 = rgb(255, 123, 114);

pub const FRAME_LINE: Color32 = rgb(106, 139, 177);

pub const GIT_ADDED: Color32 = rgb(46, 160, 67);
pub const GIT_DELETED: Color32 = rgb(210, 70, 70);
pub const GIT_UNTRACKED: Color32 = rgb(130, 170, 220);

pub const EDGE_GRAB: f32 = 14.0;

pub fn install(ctx: &Context) {
    let mut v = Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
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
    v.widgets.inactive.weak_bg_fill = BUTTON;
    v.widgets.hovered.weak_bg_fill = BUTTON_HOVER;
    v.widgets.active.weak_bg_fill = BUTTON_PRESSED;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BUTTON_BORDER);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    ctx.set_visuals(v);

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

    ctx.global_style_mut(|style| style.interaction.resize_grab_radius_side = EDGE_GRAB);
}

pub fn diff_ink(kind: DiffLine) -> Color32 {
    match kind {
        DiffLine::Added => DIFF_ADDED,
        DiffLine::Removed => DIFF_REMOVED,
        DiffLine::Hunk => rgb(108, 158, 220),
        DiffLine::Header => rgb(196, 160, 92),
        DiffLine::Context => TEXT,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLine {
    Added,
    Removed,
    Hunk,
    Header,
    Context,
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn text_contrasts_with_every_ground_it_sits_on() {
        for ground in [PANEL, CANVAS, CARD, SELECTION, LIST_ROW, FIELD] {
            let delta = (luma(TEXT) - luma(ground)).abs();
            assert!(delta > 0.35, "text on this ground is unreadable: {delta:.2}");
        }
    }

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
        assert!(luma(BUTTON) < luma(BUTTON_HOVER));
        assert!(luma(BUTTON_HOVER) < luma(BUTTON_PRESSED));
        assert!(luma(BUTTON_PRESSED) < luma(BUTTON_SELECTED));
        assert!(contrast(TEXT, BUTTON) >= 4.5, "knoptekst is onleesbaar");
    }

    #[test]
    fn de_geinstalleerde_knopstatussen_zijn_de_gemeten() {
        let ctx = egui::Context::default();
        install(&ctx);
        let visuals = &ctx.global_style().visuals;
        assert_eq!(visuals.widgets.inactive.weak_bg_fill, BUTTON);
        assert_eq!(visuals.widgets.hovered.weak_bg_fill, BUTTON_HOVER);
        assert_eq!(visuals.widgets.active.weak_bg_fill, BUTTON_PRESSED);
        assert_eq!(visuals.widgets.inactive.bg_stroke.color, BUTTON_BORDER);
        assert_eq!(visuals.widgets.hovered.bg_stroke.color, ACCENT);
        assert_eq!(visuals.selection.bg_fill, SELECTION);
        assert_ne!(visuals.selection.bg_fill, BUTTON);
        assert!(contrast(SELECTION, PANEL) < contrast(BUTTON, PANEL));
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (x, y) = (linear_luma(a), linear_luma(b));
        let (hi, lo) = (x.max(y), x.min(y));
        (hi + 0.05) / (lo + 0.05)
    }

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
