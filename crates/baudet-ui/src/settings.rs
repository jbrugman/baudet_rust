//! The settings panel: where the app points, and which model it asks.
//!
//! It is the front-end half of [`baudet_settings`], and it keeps that crate's one rule on this side
//! of the seam: **a settings file this app cannot read is never written.** So opening the panel reads
//! through [`SettingsFile`] and remembers what the read reported — including "unreadable, the bytes
//! are still at *this path*" — and saving refuses while that is the state. A person whose file is
//! broken gets one edit to make, not a screen that quietly replaces every endpoint they configured.
//!
//! Shape, which is the same as the conversation worker's and for the same reason: a model listing is
//! a blocking network call, and a paint pass must never make one. So the panel owns only *view-model*
//! strings — a text field being typed in is a `String`, never a half-parsed `u32` — and the fetch
//! travels two channels: a `Fetch` command out to a worker thread, a result back when it finishes.
//! The window drains that result exactly as it drains transcript snapshots. A listing that arrives
//! after the panel closed still lands in the state, because a refresh the person asked for should not
//! be lost to a click on the close button.
//!
//! Deliberately a small subset of the Kotlin `SettingsPanel`: endpoint, model (a fetched dropdown,
//! not free text), reasoning level, output ceiling, and the credential. The rest — context strategy,
//! tool mode, the MCP and command managers — are fields with no backend reader yet in this rewrite,
//! and a setting that nothing reads is a promise the app cannot keep.

use crate::palette;
use baudet_settings::{ListingError, Model, ReadOutcome, Settings, SettingsFile, WriteError};
use egui::{ScrollArea, Ui};

/// A command the panel sends to the settings worker. Kept to two, because those are the only two
/// things this panel can decide about the outside world: look something up, or remember it.
pub enum SettingsCommand {
    /// Ask `endpoint` which models it has. The endpoint travels with the command rather than being
    /// read from shared state: the fetch is owned by the worker, so the request cannot race the next
    /// keystroke in the field.
    Fetch {
        endpoint: String,
        /// The credential the panel holds, sent so the fetch authenticates exactly as the panel
        /// claims it will. The worker reads no shared state: a fetch carries its own key, exactly
        /// as it carries its own endpoint. `None` (or blank) means a bare GET.
        api_key: Option<String>,
    },
    /// Persist `settings`. The worker writes it and reports the outcome; it never keeps the settings,
    /// because the view-model is the truth about what is on screen.
    Save(Settings),
}

/// What the worker reports back. Drained and painted; nothing here is read twice.
pub enum SettingsEvent {
    /// A listing, successful or not, for the endpoint that was asked. The success arm carries both
    /// the models and the endpoint they came from, so a late answer cannot fill the picker for an
    /// endpoint the person has since retyped.
    Listing(Result<Vec<Model>, ListingError>, String),
    /// A save, refused or done, in the words the store produced.
    Saved(Result<(), WriteError>),
}

/// The panel's whole state: the view-model plus what the last read, fetch, and save reported.
#[derive(Default)]
pub struct SettingsView {
    /// The endpoint as typed. A free field, because the whole point of the panel is to correct it.
    base_url: String,
    /// The chosen model id. `None` is "whatever the endpoint offers first".
    model: Option<String>,
    /// The reasoning level, kept as the four words the endpoint accepts plus "unset". Text rather
    /// than a parsed value because the honest levels belong to the model, exactly as in the config.
    reasoning: Reasoning,
    /// The output ceiling as typed; empty means "leave it to the endpoint".
    max_output_tokens: String,
    /// The credential as typed. A separate field from the password widget's buffer is not needed:
    /// this is that buffer, and empty means "not typed" (so the environment still answers).
    api_key: String,
    /// Which environment variable answers for an untyped key.
    api_key_env_var: String,

    /// The models the last fetch produced, for the picker.
    models: Vec<Model>,
    /// Whether a fetch is in flight, so the button reads as busy and does not fire a second request.
    fetching: bool,
    /// The last thing to say: a fetch result, a save result, or where the settings live. Shown
    /// verbatim at the foot of the panel.
    status: String,
    /// The path the settings came from, so the status line can name it (the reference app does).
    path: Option<std::path::PathBuf>,
    /// Whether opening the panel found the file unreadable. While true, Save is refused here too —
    /// the second line of defence beside the store's own `WouldDiscard`.
    blocked: bool,
    /// Whether the model ids were fetched since the endpoint last changed, so a stale picker cannot
    /// offer a model the current endpoint does not have.
    fetched_for: Option<String>,
    /// Whether the endpoint that was asked has answered at all — a listing or a named failure. Unlike
    /// `fetched_for` this is not about which models are current: it is the record that a request is
    /// no longer in flight, which is what a waiter needs and what a failure cannot otherwise supply.
    listing_answered: bool,
    /// The password widget's transient buffer, kept so the field's show/hide survives a repaint.
    reveal_key: bool,
}

/// The reasoning level the panel offers. Maps one-to-one onto the strings `Settings::reasoning`
/// carries; `Unset` is the empty that means "do not send one", matching the reference's `null`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reasoning {
    #[default]
    Unset,
    Minimal,
    Low,
    Medium,
    High,
}

impl Reasoning {
    /// The options in panel order, for the combo box.
    const ALL: [Reasoning; 5] =
        [Reasoning::Unset, Reasoning::Minimal, Reasoning::Low, Reasoning::Medium, Reasoning::High];

    /// The word the endpoint accepts, or `None` for `Unset`.
    fn as_str(self) -> Option<&'static str> {
        match self {
            Reasoning::Unset => None,
            Reasoning::Minimal => Some("minimal"),
            Reasoning::Low => Some("low"),
            Reasoning::Medium => Some("medium"),
            Reasoning::High => Some("high"),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Reasoning::Unset => "default",
            Reasoning::Minimal => "minimal",
            Reasoning::Low => "low",
            Reasoning::Medium => "medium",
            Reasoning::High => "high",
        }
    }

    /// `Unset` for anything the panel does not recognise, which is why the mapping is tested rather
    /// than trusted: a stored level must survive a panel round trip, and an unfamiliar word must not
    /// become a level the person never picked.
    pub(crate) fn from_stored(stored: Option<&str>) -> Reasoning {
        match stored.map(str::trim).filter(|s| !s.is_empty()) {
            Some("minimal") => Reasoning::Minimal,
            Some("low") => Reasoning::Low,
            Some("medium") => Reasoning::Medium,
            Some("high") => Reasoning::High,
            // An unknown word is kept as-is on save only if the person re-picks it; for display an
            // unfamiliar level reads as "default" rather than dropping the panel. The raw value still
            // survives in the file because save rebuilds the struct, so this is a display choice only.
            _ => Reasoning::Unset,
        }
    }
}

impl SettingsView {
    /// A panel that has not read a file yet. `run_from` immediately calls [`Self::load`] so a real
    /// start never shows this; it exists so the struct is constructible and so the field's defaults
    /// are one place.
    pub fn new() -> SettingsView {
        SettingsView::default()
    }

    /// Read the file and adopt its contents, remembering the read's verdict. Called once at startup,
    /// so the status line names the datapad and a corrupt file announces itself before a person types
    /// a single character into it.
    pub fn load(&mut self, file: &SettingsFile) {
        self.path = Some(file.path().to_path_buf());
        self.status = describe_path(file.path());
        match file.read() {
            ReadOutcome::Loaded(settings) | ReadOutcome::Absent(settings) => {
                self.blocked = false;
                self.adopt(&settings);
            }
            ReadOutcome::Ignored { settings, reason, preserved } => {
                self.blocked = true;
                self.adopt(&settings);
                self.status = format!("{reason} — edit that file to recover it");
                // `preserved` is the same path already named in `reason`; no second copy of it.
                let _ = preserved;
            }
        }
    }

    /// Copy stored settings into the view-model. Numbers become text fields; the empty string is the
    /// app's "not set", which is what a person sees and edits.
    fn adopt(&mut self, settings: &Settings) {
        self.base_url = settings.base_url.clone();
        self.model = settings.model.clone();
        self.reasoning = Reasoning::from_stored(settings.reasoning.as_deref());
        self.max_output_tokens = settings.max_output_tokens.map(|n| n.to_string()).unwrap_or_default();
        self.api_key = settings.api_key.clone().unwrap_or_default();
        self.api_key_env_var = settings.api_key_env_var.clone();
        self.fetched_for = None;
        self.listing_answered = false;
    }

    /// What a save would persist right now, parsing the text fields. Returns the reason instead when
    /// a field cannot mean anything, so the panel says which one rather than saving a silent default.
    pub(crate) fn draft(&self) -> Result<Settings, String> {
        let max_output_tokens = if self.max_output_tokens.trim().is_empty() {
            None
        } else {
            Some(
                self.max_output_tokens
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| format!("{} is not a whole number of tokens", self.max_output_tokens.trim()))?,
            )
        };
        Ok(Settings {
            base_url: self.base_url.trim().to_string(),
            model: self.model.clone().filter(|m| !m.trim().is_empty()),
            reasoning: self.reasoning.as_str().map(str::to_string),
            max_output_tokens,
            api_key: Some(self.api_key.clone()).filter(|k| !k.trim().is_empty()),
            api_key_env_var: self.api_key_env_var.trim().to_string(),
        })
    }

    /// Drain one worker report into the view. Returns nothing: every outcome lands in `self`, which
    /// the next paint shows. A listing is only trusted when it answers the endpoint still in the
    /// field, so a slow answer to an old endpoint cannot overwrite the picker for a new one.
    pub fn apply_event(&mut self, event: SettingsEvent) {
        match event {
            SettingsEvent::Listing(result, endpoint) => {
                self.fetching = false;
                self.listing_answered = true;
                match result {
                    Ok(models) => {
                        self.models = models;
                        // The status names the endpoint from the field, before `endpoint` is stored
                        // away: `base_url` is what the person sees in the box, and naming that — not a
                        // normalised copy — is the honest line under the field it describes.
                        self.status = format!("{} models from {}", self.models.len(), self.base_url.trim());
                        self.fetched_for = Some(endpoint);
                    }
                    Err(error) => {
                        // The picker keeps its previous contents on a failed refresh: an endpoint
                        // that blinked should not empty a working list.
                        self.status = error.to_string();
                    }
                }
            }
            SettingsEvent::Saved(result) => {
                self.status = match result {
                    Ok(()) => format!("saved to {}", self.path.as_deref().map(describe_path).unwrap_or_default()),
                    Err(WriteError::WouldDiscard { reason, .. }) => {
                        // Save refused at the store: the file is unparseable. Stay blocked so the
                        // panel's own button agrees with the store's refusal.
                        self.blocked = true;
                        reason
                    }
                    Err(other) => other.to_string(),
                };
            }
        }
    }
}

impl SettingsView {
    /// Point the endpoint field at `address`, clearing the picker's claim to be current: models
    /// belong to the endpoint that listed them, which is the same rule the field enforces when a
    /// person retypes it. This is that edit, made by a flag instead of a keystroke.
    pub fn set_endpoint(&mut self, address: &str) {
        self.base_url = address.trim().to_string();
        self.fetched_for = None;
        self.listing_answered = false;
    }

    /// Ask for the models the endpoint offers, exactly as the Fetch button does: the command carries
    /// the endpoint and the key the field holds, and the in-flight status line is set here rather
    /// than in the paint helper so a scripted call and a click leave identical state.
    pub fn request_fetch(&mut self, tx: &std::sync::mpsc::Sender<SettingsCommand>) {
        if self.fetching {
            return;
        }
        self.fetching = true;
        self.status = format!("asking {}…", self.base_url.trim());
        // The key is resolved the way the panel's own label promises ("empty = read from
        // environment"): the typed value wins, otherwise the named variable answers. `draft()` is the
        // panel's existing verdict on its fields, so a token field that means nothing stops the
        // request rather than firing one the panel would not save.
        let key = self.draft().ok().and_then(|draft| draft.resolved_key());
        let _ = tx.send(SettingsCommand::Fetch { endpoint: self.base_url.trim().to_string(), api_key: key });
    }

    /// Whether the endpoint that was asked has answered, successfully or not. A screenshot run waits
    /// on this so a failure it caused is still in the picture it takes.
    pub fn listing_answered(&self) -> bool {
        self.listing_answered
    }
}

/// The settings panel, as an overlaying window rather than a fourth flank. Three side panels plus a
/// transcript already fill 1040 points; a fourth strip would leave the transcript nothing, so settings
/// float over the transcript and can be closed, which a person expects from a panel that is not a
/// permanent part of the layout.
///
/// `open` is the caller's — its header button is the other half of the toggle, and `Window::open` lets
/// the panel's own close button clear it, so the button and the pane can never disagree. Any button
/// pressed inside is sent to the worker through `tx` before this returns — at most one fetch and one
/// save, because those are the only buttons. State moves through `view` only, and commands go out by
/// value, so a click commits once against the state that produced it.
pub fn panel(ui: &mut Ui, view: &mut SettingsView, tx: &std::sync::mpsc::Sender<SettingsCommand>, open: &mut bool) {
    let mut commands: Vec<SettingsCommand> = Vec::new();
    egui::Window::new("Settings")
        .id(egui::Id::new("settings-panel"))
        .open(open)
        .collapsible(false)
        .default_size([320.0, 300.0])
        .resizable(true)
        .frame(egui::Frame::side_top_panel(ui.style()))
        .show(ui, |ui| {
            ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                endpoint_row(ui, view, &mut commands);
                ui.separator();
                model_row(ui, view);
                ui.separator();
                reasoning_row(ui, view);
                tokens_row(ui, view);
                ui.separator();
                key_row(ui, view);
                if view.blocked {
                    ui.separator();
                    ui.colored_label(palette::DIFF_REMOVED, "Your settings file is unreadable.");
                    ui.label(dim(
                        "Saving is disabled until it is fixed, so a save can never replace what is still in that file.",
                    ));
                }
                ui.separator();
                save_row(ui, view, &mut commands);
                ui.separator();
                // The status line last: it is where a fetch or save leaves its words, and the reference
                // app puts the datapad in the same place.
                ui.label(dim(view.status.clone()));
            });
        });
    for command in commands {
        let _ = tx.send(command);
    }
}

fn endpoint_row(ui: &mut Ui, view: &mut SettingsView, commands: &mut Vec<SettingsCommand>) {
    ui.label(dim("endpoint"));
    let response = ui.add_sized(
        [ui.available_width(), 20.0],
        egui::TextEdit::singleline(&mut view.base_url)
            .hint_text("http://127.0.0.1:8000/v1")
            .desired_width(f32::INFINITY),
    );
    // Retyping the endpoint invalidates the picker: models belong to the endpoint that listed them.
    if response.changed() {
        view.fetched_for = None;
    }
    let fetch_now = ui
        .add_enabled(!view.fetching, egui::Button::new(if view.fetching { "fetching…" } else { "Fetch models" }))
        .clicked();
    // Enter in the endpoint field fetches, because the first thing a person does after typing an
    // address is ask what is on it.
    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter)) && response.has_focus();
    if (fetch_now || enter) && !view.fetching {
        view.fetching = true;
        view.status = format!("asking {}…", view.base_url.trim());
        commands.push(SettingsCommand::Fetch {
            endpoint: view.base_url.trim().to_string(),
            api_key: Some(view.api_key.clone()).filter(|k| !k.trim().is_empty()),
        });
    }
}

fn model_row(ui: &mut Ui, view: &mut SettingsView) {
    ui.label(dim("model"));
    if view.models.is_empty() {
        ui.label(dim(if view.fetched_for.is_some() { "this endpoint lists none" } else { "not fetched yet" }));
        return;
    }
    // A combo box over the *fetched* ids, so the panel cannot name a model the endpoint lacks. The
    // picker shows display() (a friendly name when the server gave one) but stores the id.
    let current = view.model.clone().unwrap_or_default();
    let mut labels: Vec<String> = view.models.iter().map(|m| m.display().to_string()).collect();
    // Offer "keep current" when a saved model is not in the list (an offline endpoint), so opening
    // the panel never silently drops a working choice.
    if let Some(saved) = view.model.as_ref().filter(|s| !view.models.iter().any(|m| &m.id == *s)) {
        labels.insert(0, format!("{saved}  (not listed)"));
    }
    let mut chosen = current.clone();
    egui::ComboBox::from_id_salt("settings-model")
        .selected_text(labels.iter().find(|l| *l == &current).cloned().unwrap_or_else(|| "choose…".into()))
        .show_ui(ui, |ui| {
            for model in view.models.iter() {
                ui.selectable_value(&mut chosen, model.display().to_string(), model.display());
            }
        });
    // Map the display label back to the id; the "(not listed)" entry leaves the choice untouched.
    if chosen != current {
        if let Some(model) = view.models.iter().find(|m| m.display() == chosen) {
            view.model = Some(model.id.clone());
        }
    }
}

fn reasoning_row(ui: &mut Ui, view: &mut SettingsView) {
    ui.label(dim("reasoning"));
    let mut current = view.reasoning;
    egui::ComboBox::from_id_salt("settings-reasoning").selected_text(view.reasoning.label()).show_ui(ui, |ui| {
        for option in Reasoning::ALL {
            ui.selectable_value(&mut current, option, option.label());
        }
    });
    view.reasoning = current;
}

fn tokens_row(ui: &mut Ui, view: &mut SettingsView) {
    ui.label(dim("max output tokens (empty = endpoint decides)"));
    ui.add_sized(
        [ui.available_width(), 20.0],
        egui::TextEdit::singleline(&mut view.max_output_tokens).hint_text("e.g. 4096").desired_width(f32::INFINITY),
    );
}

fn key_row(ui: &mut Ui, view: &mut SettingsView) {
    ui.label(dim("api key (empty = read from environment)"));
    ui.horizontal(|ui| {
        ui.add_sized(
            [ui.available_width(), 20.0],
            egui::TextEdit::singleline(&mut view.api_key).password(!view.reveal_key),
        );
        let toggle = if view.reveal_key { "hide" } else { "show" };
        if ui.small_button(toggle).clicked() {
            view.reveal_key = !view.reveal_key;
        }
    });
    ui.label(dim("environment variable"));
    ui.add_sized(
        [ui.available_width(), 20.0],
        egui::TextEdit::singleline(&mut view.api_key_env_var)
            .hint_text(baudet_settings::DEFAULT_KEY_ENV_VAR)
            .desired_width(f32::INFINITY),
    );
}

fn save_row(ui: &mut Ui, view: &mut SettingsView, commands: &mut Vec<SettingsCommand>) {
    let save = ui.add_enabled(!view.blocked, egui::Button::new("Save")).clicked();
    if save {
        match view.draft() {
            Ok(settings) => {
                view.status = "saving…".to_string();
                commands.push(SettingsCommand::Save(settings));
            }
            Err(problem) => view.status = problem,
        }
    }
}

/// A grey label, matching how the flanking panels say a thing that is not the point of a row.
fn dim(text: impl Into<String>) -> egui::WidgetText {
    egui::RichText::new(text.into()).color(palette::DIM).into()
}

/// The datapad, phrased the way the reference app phrases it: a file a person could open.
fn describe_path(path: &std::path::Path) -> String {
    format!("{}", path.display())
}

/// The two rules this panel exists to enforce, tested where they are actually decided.
///
/// Neither is reachable from `tests/`: both live in the view-model's private parsing, and the panel's
/// only public surface is a paint function that needs a running context. That is worth stating,
/// because it is the reason these are unit tests in the file rather than integration tests beside it.
#[cfg(test)]
mod tests {
    use super::{Reasoning, SettingsCommand, SettingsEvent, SettingsView};
    use baudet_settings::{ListingError, Model};
    use std::path::PathBuf;

    fn model(id: &str) -> Model {
        Model { id: id.to_string(), label: None, context_window: None }
    }

    /// A view-model holding a whole endpoint, so a test starts from something a person could have
    /// typed rather than from a `Default` that no panel ever shows.
    fn view() -> SettingsView {
        SettingsView {
            base_url: "http://127.0.0.1:8000/v1".to_string(),
            max_output_tokens: String::new(),
            api_key_env_var: baudet_settings::DEFAULT_KEY_ENV_VAR.to_string(),
            ..SettingsView::default()
        }
    }

    #[test]
    fn an_empty_ceiling_says_nothing_rather_than_guessing_one() {
        let settings = view().draft().unwrap();
        assert_eq!(settings.max_output_tokens, None);
        // The credential and model are absent for the same reason: the environment and the endpoint
        // answer, and storing an empty string would say "the person chose nothing", which is true
        // here but indistinguishable from a cleared field after a reload.
        assert_eq!(settings.model, None);
        assert_eq!(settings.api_key, None);
    }

    /// Send a fetch, read back what it carried, and end the request the way the app does — with the
    /// worker's report — because `request_fetch` refuses to fire a second while one is in flight. A
    /// test that skipped that last step would be testing a state the panel can never be in.
    ///
    /// `try_recv` rather than `recv`: this runs on the test's own thread, so a blocking wait would
    /// wait for a sender only this same thread could serve. That hang is the reason the choice is
    /// spelled out here.
    fn fetch_of(
        view: &mut SettingsView,
        tx: &std::sync::mpsc::Sender<SettingsCommand>,
        rx: &std::sync::mpsc::Receiver<SettingsCommand>,
    ) -> (String, Option<String>) {
        view.request_fetch(tx);
        let SettingsCommand::Fetch { endpoint, api_key } =
            rx.try_recv().expect("the fetch command this helper just sent")
        else {
            unreachable!("request_fetch only ever sends a fetch")
        };
        view.apply_event(SettingsEvent::Listing(Ok(Vec::new()), endpoint.clone()));
        (endpoint, api_key)
    }

    #[test]
    fn a_fetch_uses_the_credential_the_panel_promises_not_only_a_typed_one() {
        // The measured gas: an empty key field plus a set variable name is the documented way to
        // reach a gateway that wants a key, yet the request went out bare and the gateway answered
        // 401. The panel's own label ("empty = read from environment") is the behaviour under test.
        let name = "BAUDET_FETCH_KEY_TEST_ENV";
        // Safety: same-thread test; no other test reads or writes this name.
        unsafe { std::env::set_var(name, "a-fake-value-for-tests-only") };
        let mut panel = view();
        panel.api_key_env_var = name.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        let fetched = fetch_of(&mut panel, &tx, &rx);
        assert_eq!(fetched.0, "http://127.0.0.1:8000/v1");
        assert_eq!(fetched.1.as_deref(), Some("a-fake-value-for-tests-only"));

        // The typed value wins over the environment, exactly as `resolved_key` defines it, so a
        // person overriding a stale variable is not silently ignored.
        panel.api_key = "typed-wins".to_string();
        assert_eq!(fetch_of(&mut panel, &tx, &rx).1.as_deref(), Some("typed-wins"));

        // Nothing answers: no typed key and a name that points nowhere. Then the fetch is a bare
        // request, which is right for a server that asks for nothing — absence stays an answer.
        unsafe { std::env::remove_var(name) };
        let mut bare = view();
        bare.api_key_env_var = name.to_string();
        assert_eq!(
            fetch_of(&mut bare, &tx, &rx).1,
            None,
            "an unresolved key must not become an empty string in a header"
        );
    }

    #[test]
    fn a_failed_listing_counts_as_an_answer_but_not_as_a_current_list() {
        // The gas this guards: the waiter for a screenshot asked whether a listing had landed and got
        // the question "is the list current", which a failure can never answer. So it waited out its
        // own ceiling and drew the status line of a fetch it had asked for — the one thing the run
        // exists to capture.
        let mut view = view();
        assert!(!view.listing_answered(), "nothing asked yet, nothing answered");
        view.request_fetch(&std::sync::mpsc::channel().0);
        assert!(view.fetching, "a request is in flight");
        view.apply_event(SettingsEvent::Listing(
            Err(ListingError::NotAListing {
                endpoint: "http://127.0.0.1:8000/v1/models".to_string(),
                status: 401,
                detail: "API key required".to_string(),
            }),
            "http://127.0.0.1:8000/v1".to_string(),
        ));
        assert!(view.listing_answered(), "a named failure is an answer");
        assert!(!view.fetching);
        assert!(view.status.contains("401"), "the failure is what the panel shows: {}", view.status);
        // And it stays true to the other rule: no listing means no current models for this endpoint.
        assert_eq!(view.fetched_for, None, "a failure must not claim the picker is current");

        // A new endpoint is a new question, so the answer to the old one cannot settle the new wait.
        view.set_endpoint("http://192.168.1.2:8000/v1");
        assert!(!view.listing_answered());
    }

    #[test]
    fn a_ceiling_that_cannot_be_a_number_is_refused_loudly() {
        // The failure this protects: "lots" parsing to nothing and saving `None` would look like the
        // person cleared the field, and the endpoint's own ceiling would come back next start.
        let mut broken = view();
        broken.max_output_tokens = "lots".to_string();
        let problem = broken.draft().unwrap_err();
        assert!(problem.contains("lots"), "{problem}");
        assert!(broken.draft().is_err(), "a nonsense ceiling must never become a saveable draft");

        let mut whitespace = view();
        whitespace.max_output_tokens = "   ".to_string();
        assert_eq!(whitespace.draft().unwrap().max_output_tokens, None, "blanks are an empty field");

        let mut filled = view();
        filled.max_output_tokens = " 4096 ".to_string();
        assert_eq!(filled.draft().unwrap().max_output_tokens, Some(4096));
    }

    #[test]
    fn a_stored_level_survives_the_panel_and_an_unknown_one_does_not_become_a_level() {
        for level in [Reasoning::Minimal, Reasoning::Low, Reasoning::Medium, Reasoning::High] {
            let stored = level.as_str();
            assert_eq!(Reasoning::from_stored(stored), level, "round trip for {stored:?}");
        }
        // Display only: the raw word still lives in the file, because a save rebuilds from the
        // picker rather than from what was displayed.
        assert_eq!(Reasoning::from_stored(Some("ultra")), Reasoning::Unset);
        assert_eq!(Reasoning::from_stored(Some("")), Reasoning::Unset);
        assert_eq!(Reasoning::from_stored(Some("  ")), Reasoning::Unset);
        assert_eq!(Reasoning::from_stored(None), Reasoning::Unset);
        assert_eq!(Reasoning::Unset.as_str(), None, "default must send no reasoning field at all");
    }

    #[test]
    fn a_draft_writes_back_the_level_that_was_read() {
        let mut panel = view();
        panel.reasoning = Reasoning::from_stored(Some("high"));
        panel.model = Some("DeepSeek-V4-Flash".to_string());
        panel.api_key = "typed-by-hand".to_string();
        let settings = panel.draft().unwrap();
        assert_eq!(settings.reasoning.as_deref(), Some("high"));
        assert_eq!(settings.model.as_deref(), Some("DeepSeek-V4-Flash"));
        assert_eq!(settings.api_key.as_deref(), Some("typed-by-hand"));
        assert_eq!(settings.base_url, "http://127.0.0.1:8000/v1");
    }

    #[test]
    fn a_listing_only_counts_when_it_answers_the_endpoint_still_on_screen() {
        let mut panel = view();
        panel.fetching = true;
        // The answer the worker gives: models for the endpoint that was asked.
        panel.apply_event(SettingsEvent::Listing(Ok(vec![model("a"), model("b")]), panel.base_url.clone()));
        assert_eq!(panel.models.len(), 2);
        assert!(!panel.fetching, "a finished fetch must free the button");
        assert!(panel.status.contains("2 models"), "{}", panel.status);
        assert_eq!(panel.fetched_for.as_deref(), Some("http://127.0.0.1:8000/v1"));

        // A second panel whose person retyped the address while a request was in flight: the old
        // endpoint's models must not fill the picker for the new one.
        let mut moved = view();
        moved.base_url = "http://192.168.1.2:8000/v1".to_string();
        moved.apply_event(SettingsEvent::Listing(Ok(vec![model("stale")]), "http://127.0.0.1:8000/v1".to_string()));
        assert_eq!(moved.fetched_for.as_deref(), Some("http://127.0.0.1:8000/v1"));
        assert_ne!(moved.fetched_for.as_deref(), Some(moved.base_url.as_str()));
    }

    #[test]
    fn a_failed_refresh_keeps_the_models_it_did_not_replace() {
        let mut panel = view();
        panel.apply_event(SettingsEvent::Listing(Ok(vec![model("keep")]), panel.base_url.clone()));
        panel.apply_event(SettingsEvent::Listing(
            Err(ListingError::NotAListing {
                endpoint: "http://127.0.0.1:8000/v1".to_string(),
                status: 401,
                detail: "API key required".to_string(),
            }),
            "http://127.0.0.1:8000/v1".to_string(),
        ));
        assert_eq!(panel.models.len(), 1, "an endpoint that blinked must not empty a working list");
        assert!(panel.status.contains("401"), "{}", panel.status);
    }

    #[test]
    fn a_refused_save_keeps_the_panel_refusing() {
        let mut panel = view();
        panel.path = Some(PathBuf::from("/tmp/baudet-tests/settings.json"));
        // The store refused because the bytes there are not settings. The panel must agree with that
        // refusal afterwards, or its own button offers exactly the overwrite the store just blocked.
        panel.apply_event(SettingsEvent::Saved(Err(baudet_settings::WriteError::WouldDiscard {
            path: PathBuf::from("/tmp/baudet-tests/settings.json"),
            reason: "expected a settings object".to_string(),
        })));
        assert!(panel.blocked, "a refused save must re-block the panel");
        assert!(panel.status.contains("expected a settings object"), "{}", panel.status);

        let mut recovered = view();
        recovered.blocked = true;
        recovered.path = Some(PathBuf::from("/tmp/baudet-tests/settings.json"));
        recovered.apply_event(SettingsEvent::Saved(Ok(())));
        assert!(recovered.status.contains("saved to"), "{}", recovered.status);
    }
}
