//! The models tab: the listing as a list a person can choose from, rather than a combo box buried
//! in the settings form.
//!
//! It exists because that is where the reference app puts it. `Main.kt:130-134` builds a tabbed pane
//! with Chat, Modellen and Ingestellingen in the middle, and its `ModelsPanel.kt:19-49` is a bar with
//! two buttons above a monospaced list with a status line under it. The Rust settings form already
//! had the same information in a `ComboBox` — readable, but a row a person has to open before they
//! can compare anything, while a list shows every model at once. Both read the same `SettingsView`,
//! which is the reason this file is small.
//!
//! The choice it makes is not its own: a pick writes the field in [`SettingsView`] and then asks for
//! the same save the settings form's Save button asks for. There is exactly one path to the settings
//! file and exactly one `Adopted` report per save, so the next turn cannot ask a model that was never
//! written, and the models tab cannot win an argument with the form about which one is chosen.

use crate::palette;
use crate::settings::{row_for, SettingsCommand, SettingsView};
use egui::{Ui, UiBuilder};

/// Het adres van "Use this model".
///
/// Same reason as the settings form's field address: tests/tabs.rs asks whether this tab painted, and
/// an auto-id would answer a different question every time a row appears above it. Unlike that field
/// this is a button, so the address comes from the `UiBuilder::id(..)`-wikkel around it — see the call
/// site and `TABSTRIP`.
pub const SELECTED_ROW: &str = "models-use-selected";

/// Paint the tab in the caller's `Ui`, which is the middle panel's scroll area.
///
/// The width is the middle's, not the settings form's 360 pt: a model id is long and the point of a
/// list is to read several of them without opening anything, so this body takes the room it is given.
pub fn body(ui: &mut Ui, view: &mut SettingsView, tx: &std::sync::mpsc::Sender<SettingsCommand>) {
    let endpoint = view.endpoint();
    // Two buttons above the list, as the reference has them. Fetch is disabled while a request is in
    // flight rather than queueing a second one; Use is disabled while nothing is selected, so the
    // reference's "select one first" status line has no reason to exist here.
    let fetch = ui
        .add_enabled(
            !view.is_fetching(),
            egui::Button::new(if view.is_fetching() { "fetching…" } else { "Fetch models" }),
        )
        .clicked();
    let chosen = view.chosen_model().map(str::to_string);
    // A draft separate from the stored choice. The reference's bar has two buttons, and "Use this
    // model" only means something after a row has been clicked; writing the stored field on every
    // click would save a settings file on a click nobody asked to save.
    let mut draft: Option<String> = None;
    let list = view.models();
    // De gemarkeerde rij is het model dat de app nu gebruikt — dat is de vraag die dit tabblad
    // beantwoordt zonder een keuzelijst open te vouwen. Een klik hieronder markeert pas ná de klik die
    // hem vraagt, dus "Use this model" is in datzelfde frame nog gedeactiveerd: een markeer-staat die
    // de knop in hetzelfde frame actief maakt, zou van één klik twee handelingen maken.
    list_rows(ui, list, chosen.as_deref(), &mut draft);
    // De knop in een `UiBuilder::id(..)`-wikkel, niet met een eigen zout: `egui::Button` kent geen
    // `id_salt` (gemeten: die methode bestaat op deze build niet op dat type), en `push_id` geeft de
    // knop de auto-id van de kind-`Ui`, die van buiten niet te reconstrueren is. Zie `TABSTRIP` in
    // app.rs voor beide metingen.
    // Anker op de wortel-Ui + click-sense-wikkel, dezelfde regel als de tabknoppen: `Button` draagt
    // een eigen auto-adres dat van buiten niet te reconstrueren is, dus de klik leeft op de wikkel,
    // en die is via `root_ui_id` van buiten te vinden. De knop blijft de knop die iemand indrukt.
    let use_it = ui
        .scope_builder(
            UiBuilder::new()
                .id(crate::root_ui_id(ui.ctx()).with(SELECTED_ROW))
                .sense(egui::Sense::click()),
            |ui| {
                ui.add_enabled(draft.is_some() && !view.is_fetching(), egui::Button::new("Use this model"));
            },
        )
        .response
        .clicked();

    if fetch {
        view.request_fetch(tx);
    }
    if let Some(id) = draft.filter(|_| use_it) {
        view.choose_model(&id);
        view.request_use_selected(tx);
    }

    // The same three words the settings form uses for an empty picker, so "never asked" and "asked,
    // and this server has none" read the same in both places.
    let words = view.listing_words();
    if !words.is_empty() {
        ui.label(dim(words));
    }
    ui.separator();
    ui.label(dim(format!("{} · {}", view.status(), if endpoint.is_empty() { "no endpoint" } else { &endpoint })));
}

/// The list itself: one row per model, `chosen` marked, and the clicked row handed back in `draft`.
///
/// `ui.selectable_label` rather than the `SelectableLabel` widget built by hand: that is the helper
/// this crate already reaches for — `ComboBox::selectable_value` is a wrapper over it — and egui only
/// re-exports the widget type itself behind a feature this build does not enable. The *text* is
/// `row_for` while the *value* is the id, the same split the settings form's picker makes, and the
/// reason a window suffix can never reach the settings file.
fn list_rows(ui: &mut Ui, models: &[baudet_settings::Model], chosen: Option<&str>, draft: &mut Option<String>) {
    for model in models {
        // De gemarkeerde rij volgt `chosen` en niet een draft: de selectielabels dragen het adres van
        // de rij zelf, dus één rij is het bewijs van één keuze, en een klik schrijft alleen de draft.
        let row =
            ui.selectable_label(chosen == Some(model.id.as_str()), egui::RichText::new(row_for(model)).monospace());
        if row.clicked() {
            *draft = Some(model.id.clone());
        }
    }
}

/// A grey label, the same shade and reason as the settings form's: a word that is not the point of
/// the row it sits under.
fn dim(text: impl Into<String>) -> egui::WidgetText {
    egui::RichText::new(text.into()).color(palette::DIM).into()
}
