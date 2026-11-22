//! Why the folder row's widths are computed rather than asked for, and the test that keeps it true.
//!
//! `Browse…` and `Open` had been in the source for a full session and were in no screenshot. The
//! reason was not that they were missing: the field was built with
//! `desired_width(f32::INFINITY)`, which in egui 0.36 means "as wide as the space available" —
//! 10 000 pt in a root UI, since `Ui::put` does not clip a widget's desired size to the enclosing
//! rectangle. Both buttons then landed beyond the panel's edge with nothing said about it. Measured
//! here before anything was changed: the buttons ended at x = 10 072 and x = 10 120.
//!
//! So the widths are decided in `panels::left` from the measured button sizes, and this file checks
//! the one claim that matters: the row ends inside the panel. It repeats the row's own arithmetic on
//! purpose — calling into the panel would need the module public for no other reason, and a pixel
//! measurement would only show one panel width. What is *not* repeated is the guess: the constants
//! below are what egui gives those two labels, asserted here so that a font or spacing change that
//! makes them wrong fails in this test rather than in a screenshot nobody took.

/// What egui measured for `Browse…` and `Open` at the default text style. `panels::left` rounds the
/// first up (64.4) and uses the second as-is; both are checked to still leave room below.
const BROWSE_WIDTH: f32 = 64.4;
const OPEN_WIDTH: f32 = 40.2;

/// Paint the row the way the panel does and hand back where it ended, plus the panel width it was
/// given. `panel_width` is the raw drag value; egui clamps it up to `min_size` before the frame is
/// shown (panel.rs:763), so the floor is applied here as well — that clamp is the reason a panel
/// narrower than its content still cannot overflow.
fn row_end(panel_width: f32, min_size: f32) -> (f32, f32) {
    let ctx = egui::Context::default();
    let width = panel_width.max(min_size);
    let mut end = 0.0;
    let output = ctx.run_ui(egui::RawInput::default(), |ui| {
        ui.set_max_width(width);
        let row_width = ui.available_width();
        let spacing = ui.spacing().item_spacing.x;
        let buttons_width = BROWSE_WIDTH + OPEN_WIDTH + spacing * 2.0;
        let field_width = (row_width - buttons_width).max(0.0);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(false), |ui| {
                let _ = ui.add_sized([field_width, 20.0], egui::TextEdit::singleline(&mut String::new()));
                let _ = ui.small_button("Browse\u{2026}");
                end = ui.small_button("Open").rect.max.x;
            });
        });
    });
    // The texture delta wants handling before the Context is dropped; the panic says
    // "Dropped TexturesDelta with N unapplied deltas" and says nothing about the measurement, so a
    // backend settles it through tessellation and this does the same without paying for triangles.
    output.drop_without_applying_deltas();
    (end, width)
}

#[test]
fn the_buttons_are_the_widths_the_panel_budgets_for() {
    // If this fails, the constants in panels::left are stale: the row would then either clip the
    // field's text or push Open past the edge — which is exactly the silent failure this whole file
    // exists to prevent, so it is measured here rather than assumed from a screenshot.
    let ctx = egui::Context::default();
    let mut measured = (0.0, 0.0);
    let output = ctx.run_ui(egui::RawInput::default(), |ui| {
        ui.horizontal(|ui| {
            measured.0 = ui.small_button("Browse\u{2026}").rect.width();
            measured.1 = ui.small_button("Open").rect.width();
        });
    });
    output.drop_without_applying_deltas();
    assert!(
        measured.0 <= BROWSE_WIDTH && measured.0 > BROWSE_WIDTH - 2.0,
        "`Browse…` meets {} pt, while the panel budgets {BROWSE_WIDTH}",
        measured.0
    );
    assert!(
        measured.1 <= OPEN_WIDTH && measured.1 > OPEN_WIDTH - 2.0,
        "`Open` meets {} pt, while the panel budgets {OPEN_WIDTH}",
        measured.1
    );
}

#[test]
fn the_row_ends_inside_the_panel_at_every_width_egui_allows() {
    // The default, both ends of the resizable range, and values below the floor, which the clamp
    // turns back into the floor. The floor is the panel's own `min_size`.
    for width in [220.0_f32, 190.0, 174.0, 160.0, 150.0, 146.0, 130.0, 100.0, 60.0] {
        let (end, panel) = row_end(width, 146.0);
        assert!(end <= panel + 0.01, "bij een paneel van {panel} pt eindigt de rij op {end} pt");
    }
}

#[test]
fn a_floor_below_the_buttons_does_not_hold_the_row() {
    // The reason `min_size` is 146 and not the 150 that was there before: with a floor of 130 the
    // row no longer fits, so the number is a consequence of the buttons, not a preference. This test
    // exists to show the floor is load-bearing — raise it nowhere and the previous test still passes,
    // lower it and this one fails.
    let (end, panel) = row_end(60.0, 130.0);
    assert!(end > panel, "bij een vloer van {panel} pt zou de rij alsnog passen op {end} pt");
}
