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
use baudet_backend::protocol::Protocol;
use baudet_settings::config::ToolMode;
use baudet_settings::{ListingError, Model, ReadOutcome, Settings, SettingsFile, WriteError};
use egui::Ui;

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
    /// Ask `endpoint` which protocol it answers, on top of which models it has. Travels with its own
    /// endpoint and key for the same reason [`Fetch`] does: the worker reads no shared state.
    Probe { endpoint: String, api_key: Option<String> },
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
    /// What an endpoint turned out to be: the dialect its answer identified, plus the notes that say
    /// how that was decided. `None` means the probe did not identify one, which is a fact about the
    /// server rather than a failure of the request, and the notes are the evidence.
    Probed(Result<Option<Protocol>, String>, String),
    /// What a save just persisted, as the choices a session should stamp on its next request. Sent
    /// beside [`SettingsEvent::Saved`] rather than instead of it: whether the file took the bytes is
    /// the panel's business, and which model to ask next is the session's.
    Adopted { model: Option<String>, reasoning: Option<String>, max_output_tokens: Option<u32> },
    /// A save, refused or done, in the words the store produced.
    Saved(Result<(), WriteError>),
}

/// The panel's whole state: the view-model plus what the last read, fetch, and save reported.
///
/// The derived `Default` leaves every text field empty, which is right for all of them except one:
/// the iteration cap has no empty state, since the loop always runs under a cap. A panel that came
/// from nowhere would therefore refuse its own draft. That cannot happen on a real start — `new`
/// calls `load`, and `load` calls `adopt`, which fills the cap from the file or from the settings'
/// own default — so the empty is filled where it is actually observable: in the test fixture below.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SettingsView {
    /// The endpoint as typed. A free field, because the whole point of the panel is to correct it.
    base_url: String,
    /// The chosen model id. `None` is "whatever the endpoint offers first".
    model: Option<String>,
    /// The wire dialect, as the word the file carries (`responses`, `chat_completions`). Text for
    /// the same reason `reasoning` is text: a word this build does not know must survive a round trip
    /// rather than look like a corrupt file, and it is the transport's `Protocol::from_word` that
    /// decides whether a turn may be sent at all.
    protocol: Option<String>,
    /// Wanneer de assistent tools mag gebruiken, zoals het bestand het noemt. Nu alleen vastgelegd
    /// en doorgegeven: de keuzelijst die hierover beschikt komt pas als er een tool-lus onder staat
    /// (item E/F), want een keuzelijst die niets bediende is precies de fout die deze port al maakte.
    tool_mode: ToolMode,
    /// The reasoning level, kept as the four words the endpoint accepts plus "unset". Text rather
    /// than a parsed value because the honest levels belong to the model, exactly as in the config.
    reasoning: Reasoning,
    /// The output ceiling as typed; empty means "leave it to the endpoint".
    max_output_tokens: String,
    /// De deksel op de tool-lus, als tekst zoals het veld het toont. Poort van het
    /// `maxIterations`-veld uit `SettingsPanel.kt:264`.
    ///
    /// Een tekst en geen getal, omdat dit veld het antwoord van de persoon is en niet het besluit:
    /// "12" is nog geen limiet, pas `draft` beslist of het er een is. Een getal-veld zou een
    /// onleesbare tussentoestand verstoppen — de persoon zou niet meer kunnen typen wat hij bedoelt.
    max_tool_iterations: String,
    /// Het limiet zoals het bestand het draagt, naast het vakje. Alleen bij `Off` in gebruik:
    /// `SettingsPanel.kt:372` schakelt het vakje dan uit en laat het opgeslagen limiet staan, omdat
    /// de persoon toen alleen de lus uitzette en geen limiet corrigeerde. Zonder deze kopie zou
    /// "lus uit" het limiet van de persoon stilletjes op 8 terugzetten — een wijziging die hij niet
    /// maakte en die pas terugkomt zodra hij de lus weer aanzet.
    stored_max_tool_iterations: u32,
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
    /// The dialect the last scan found for this endpoint, and the endpoint it was found on. Kept
    /// beside `fetched_for` for the same reason: both are facts about the address that was asked, so
    /// retyping it retires them together.
    probed: Option<(String, Protocol)>,
    /// Whether a scan is in flight, so its button reads busy and cannot fire a second probe.
    probing: bool,
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
        // Dit veld is nooit blanco: het draagt altijd een getal, want de lus heeft altijd een
        // deksel. Een lege tekst zou een limiet betekenen dat niemand koos.
        self.max_tool_iterations = settings.max_tool_iterations.to_string();
        self.stored_max_tool_iterations = settings.max_tool_iterations;
        self.api_key = settings.api_key.clone().unwrap_or_default();
        self.api_key_env_var = settings.api_key_env_var.clone();
        self.protocol = settings.protocol_word().map(str::to_string);
        self.tool_mode = settings.tool_mode;
        self.fetched_for = None;
        self.listing_answered = false;
        // A stored dialect is a claim about an endpoint this panel has not asked yet, so it is shown
        // as what it is: chosen, not probed. `probed` stays empty until a scan says otherwise.
        self.probed = None;
        self.probing = false;
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
        // De regel van `SettingsPanel.kt:372-376`: bij `Off` is het vakje uitgeschakeld en blijft het
        // opgeslagen limiet staan, want geen enkele tool draait dan — de persoon heeft dan niets
        // gecorrigeerd, alleen de lus uit gezet. Een blanco of niet-getal veld bij een aanstaande
        // lus is wél een fout en mag niet stil op 8 terugvallen.
        let max_tool_iterations = match self.tool_mode {
            ToolMode::Off => self.stored_max_tool_iterations,
            _ => self
                .max_tool_iterations
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("max tool iterations is not a positive number: {}", self.max_tool_iterations.trim()))?,
        };
        Ok(Settings {
            max_tool_iterations,
            base_url: self.base_url.trim().to_string(),
            model: self.model.clone().filter(|m| !m.trim().is_empty()),
            reasoning: self.reasoning.as_str().map(str::to_string),
            max_output_tokens,
            api_key: Some(self.api_key.clone()).filter(|k| !k.trim().is_empty()),
            api_key_env_var: self.api_key_env_var.trim().to_string(),
            protocol: self.protocol.clone().map(|word| word.trim().to_string()).filter(|w| !w.is_empty()),
            tool_mode: self.tool_mode,
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
            SettingsEvent::Probed(result, endpoint) => {
                self.probing = false;
                match result {
                    Ok(Some(dialect)) => {
                        // The scan's answer replaces the field's word, because it is evidence rather
                        // than a preference: a server that answered a chat turn is a chat server.
                        self.protocol = Some(dialect.word().to_string());
                        self.probed = Some((endpoint, dialect));
                        self.status = format!("{}: {}", self.base_url.trim(), dialect.word());
                    }
                    // Nothing identified. The notes that say why are the whole value of this event,
                    // so they are what the status line carries; the chosen dialect is left alone,
                    // because a probe that learned nothing cannot unlearn what a person set.
                    Ok(None) => self.status = format!("no dialect identified: {endpoint}"),
                    Err(problem) => self.status = problem,
                }
            }
            // Nothing to remember here: the window forwards this one to the session and the panel's
            // own words come from the `Saved` that follows it. The arm exists so adding it to the
            // enum cannot silently drop through a wildcard.
            SettingsEvent::Adopted { .. } => {}
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
        self.probed = None;
    }

    /// The dialect to show beside the endpoint field: what a scan proved for this address, otherwise
    /// what the file claims, otherwise nothing.
    pub fn protocol_word(&self) -> Option<&str> {
        self.probed
            .as_ref()
            .filter(|(probed, _)| *probed == self.base_url.trim())
            .map(|(_, dialect)| dialect.word())
            .or_else(|| self.protocol.as_deref().map(str::trim).filter(|w| !w.is_empty()))
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

    /// The endpoint as typed, not as stored: the models tab must name the address it asked, which is
    /// the address in the box. Trimmed, because that is how every command and status line here spells it.
    pub fn endpoint(&self) -> String {
        self.base_url.trim().to_string()
    }

    /// The listing the model picker is offering. Shared rather than copied so the settings form and
    /// the models tab cannot show two different answers to "which models are on this server".
    pub fn models(&self) -> &[Model] {
        &self.models
    }

    /// The chosen model id, `None` meaning "whatever the endpoint offers first".
    pub fn chosen_model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Point the choice at `id`. Kept here rather than handed out as `&mut`, so the one field that
    /// decides which model a turn asks cannot be written from outside this file.
    pub fn choose_model(&mut self, id: &str) {
        self.model = Some(id.to_string());
    }

    /// The last thing the panel has to say, for the tab that shares the panel's state.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Whether a listing is in flight, so a second request cannot stack up behind the first.
    pub fn is_fetching(&self) -> bool {
        self.fetching
    }

    /// In words: what the picker holds for the endpoint in the box. Three states, one sentence each,
    /// and the empty list says which of the two kinds of empty it is — "never asked" and "asked, and
    /// this server has none" are different facts and a person needs the right one.
    ///
    /// `fetched_for` is the record that an answer arrived for *some* endpoint; `models` is the answer
    /// the picker still shows. A failed refresh keeps the old list (see `apply_event`), so the list
    /// alone cannot say whether it is current, and `fetched_for` alone cannot say whether it is empty.
    pub fn listing_words(&self) -> &'static str {
        if !self.models.is_empty() {
            ""
        } else if self.fetched_for.is_some() {
            "this endpoint lists none"
        } else {
            "not fetched yet"
        }
    }

    /// Use the model selected in the models tab: the same choice the picker's row would have stored,
    /// sent through the same Save the Save button sends.
    ///
    /// Deliberately not a second command and not a direct write to the file. Two writers to
    /// `~/.baudet/settings.json` would disagree the way two boolean owners of one panel did, and the
    /// store's own refusal of a file it cannot rewrite (`WouldDiscard`) only protects a save that
    /// goes through it. So a pick lands in the field here, and `draft()` — with its token-field check
    /// and the store's write — stays the single path to disk. The session hears about it through the
    /// `Adopted` report a save produces, which the window already forwards.
    pub fn request_use_selected(&mut self, tx: &std::sync::mpsc::Sender<SettingsCommand>) {
        if self.blocked {
            // The panel's own button refuses in this state; a second door must agree with it rather
            // than discover the store's refusal after a click that looked like a choice.
            self.status = "Your settings file is unreadable. Saving is disabled until it is fixed.".to_string();
            return;
        }
        match self.draft() {
            Ok(settings) => {
                self.status = "saving…".to_string();
                let _ = tx.send(SettingsCommand::Save(settings));
            }
            Err(problem) => self.status = problem,
        }
    }
}

/// Het stabiele zout van het endpointveld.
///
/// `.id_salt` houdt het adres vast *relatief aan zijn ouder-`Ui`*, zodat een auto-id niet meeschuift
/// zodra er boven in het formulier iets bij komt. In tegenstelling tot een tabknop (`TABSTRIP`) is dit
/// géén van buiten reconstrueerbaar root-adres: het formulier zit in de scroll-area van een tab, en
/// geen enkele root-keten gaf dit adres terug (gemeten). tests/tabs.rs bewijst het settings-tabblad
/// daarom met een klik op de strip (`app.tab()`), niet met dit adres; het zout blijft nuttig voor de
/// focusvolgorde en om een repaint het veld niet zijn toetsenbordfocus te laten verliezen.
pub const FIELD_ENDPOINT: &str = "settings-endpoint-field";

/// Het ingestformulier, getekend in de `Ui` van de aanroeper: het middenvlak van een tab, niet als
/// venster boven het transcript.
///
/// Het was een `egui::Window`, en dat verbergde de klacht "overheengepropt" in plaats van op te
/// lossen: de statusregel stond keurig in de plaatjes die er van getekend zijn, terwijl de status en
/// Save van het paneel er zelf buiten vielen en het formulier over de rijen heen lag. De
/// referentie-app kent geen ingest-venster — `Main.kt:130-134` zet Chat, Modellen en Ingestellingen
/// als tabs in het middenvlak, en de kop bevat daar alleen de pane-toggles. Deze functie tekent dus
/// in dezelfde `Ui` waarin de transcript-rijen getekend worden, en de vraag "hoe hoog mag dit
/// formulier" verdwijnt: het middenvlak rekt mee, en de `ScrollArea` van de tab vangt een te small
/// venster op.
///
/// De breedte blijft 360 pt — wat de velden in het venster ook hadden, dus geen enkel veld wordt
/// korter — met `available_width` als plafond. Het formulier is een stapel van één kolom; uitrekken
/// tot 1400 pt zou tekstvelden maken langer dan iemand kan overzien. Elke veldbreedte wordt hier uit
/// die beschikbare breedte opgemaakt, dus een smaller venster geeft smallere velden en nooit een
/// afgekapte rij.
///
/// Any button pressed inside is sent to the worker through `tx` before this returns — at most one
/// fetch and one save, because those are the only buttons. State moves through `view` only, and
/// commands go out by value, so a click commits once against the state that produced it.
pub fn body(ui: &mut Ui, view: &mut SettingsView, tx: &std::sync::mpsc::Sender<SettingsCommand>) {
    let mut commands: Vec<SettingsCommand> = Vec::new();
    // 360 pt: het plafond dat het venster gaf, niet de breedte van het middenvlak. Het formulier is
    // één kolom, en elke veldbreedte hieronder wordt uit deze breedte opgemaakt.
    ui.set_width(ui.available_width().min(360.0));
    endpoint_row(ui, view, &mut commands);
    ui.separator();
    model_row(ui, view);
    ui.separator();
    reasoning_row(ui, view);
    tokens_row(ui, view);
    iterations_row(ui, view);
    ui.separator();
    key_row(ui, view);
    if view.blocked {
        ui.separator();
        ui.colored_label(palette::DIFF_REMOVED, "Your settings file is unreadable.");
        ui.label(dim("Saving is disabled until it is fixed, so a save can never replace what is still in that file."));
    }
    ui.separator();
    save_row(ui, view, &mut commands);
    // Eén status, onderaan: het is de plek waar een listing of een save zijn woorden achterlaat, en
    // de referentie-app zet het datapad daar ook. Leeg is geen regel: dan staat er niets, ook geen
    // lege lijn met een scheiding erboven.
    if !view.status.is_empty() {
        ui.separator();
        ui.label(dim(&view.status));
    }
    for command in commands {
        let _ = tx.send(command);
    }
}

fn endpoint_row(ui: &mut Ui, view: &mut SettingsView, commands: &mut Vec<SettingsCommand>) {
    ui.label(dim("endpoint"));
    let response = ui.add_sized(
        [ui.available_width(), 20.0],
        egui::TextEdit::singleline(&mut view.base_url)
            // Het zout is het adres van tests/tabs.rs: een test telt dit veld mee als bewijs dat het
            // formulier getekend is. Waarom het adres hier aan de body-`Ui` hangt en niet vlak op de
            // wortel staat — en wat de test daarvoor moet samenstellen — staat bij `FIELD_ENDPOINT`.
            .id_salt(FIELD_ENDPOINT)
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
    ui.horizontal(|ui| {
        // The second question about an address: not which models are on it, but what it answers.
        // Deliberately its own button rather than part of Fetch, because a probe names a model, and
        // naming one a machine has to page in displaces the one it is keeping warm.
        let scan = ui
            .add_enabled(!view.probing, egui::Button::new(if view.probing { "scanning…" } else { "Scan endpoint" }))
            .clicked();
        if scan {
            view.probing = true;
            view.status = format!("scanning {}…", view.base_url.trim());
            commands.push(SettingsCommand::Probe {
                endpoint: view.base_url.trim().to_string(),
                api_key: Some(view.api_key.clone()).filter(|k| !k.trim().is_empty()),
            });
        }
        // What the app believes this server speaks. Proven wins over stored, which is what
        // `protocol_word` encodes; showing only the stored word would let a stale file outlive the
        // scan that disproved it.
        if let Some(word) = view.protocol_word() {
            ui.label(dim(format!("protocol: {word}")));
        }
    });
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
    // row is what a person reads and the id is what gets stored: `row_for` adds the window to the
    // name, and the choice below is carried as the id, so a suffix can never reach the settings file.
    let row_labels: Vec<String> = view.models.iter().map(row_for).collect();
    let current = view.model.clone().unwrap_or_default();
    let mut labels: Vec<String> = row_labels.clone();
    // Offer "keep current" when a saved model is not in the list (an offline endpoint), so opening
    // the panel never silently drops a working choice.
    if let Some(saved) = view.model.as_ref().filter(|s| !view.models.iter().any(|m| &m.id == *s)) {
        labels.insert(0, format!("{saved}  (not listed)"));
    }
    // The choice is carried as the id and only *drawn* as a row. Matching the stored id against the
    // label instead would find nothing once a row reads "Qwen … · 256k", and the picker would open
    // as "choose…" above a model that is still selected.
    let mut chosen = current.clone();
    // The "(not listed)" entry is the one label equal to a bare id, so it is found by this lookup
    // and every listed model by its row: the field shows exactly what its row shows.
    let selected = view
        .models
        .iter()
        .find(|m| m.id == current)
        .map(row_for)
        .or_else(|| labels.iter().find(|l| *l == &current).cloned())
        .unwrap_or_else(|| "choose…".to_string());
    egui::ComboBox::from_id_salt("settings-model").selected_text(selected).show_ui(ui, |ui| {
        for (model, label) in view.models.iter().zip(row_labels.iter()) {
            ui.selectable_value(&mut chosen, model.id.clone(), label.as_str());
        }
    });
    // Only a listed id is stored; "choose…" and the "(not listed)" entry leave the choice untouched.
    if chosen != current && view.models.iter().any(|m| m.id == chosen) {
        view.model = Some(chosen);
    }
}

/// A picker row: the name the server prefers, and its window when the server said one.
///
/// This is the consumer `Model::context_window` was waiting for. The gateway publishes every
/// model's ceiling (measured: Qwen 262144, DeepSeek 128000, GLM 1048576) and while there was no
/// consumer, not one of those numbers reached the screen.
///
/// A model that says nothing gets its bare name, so the picker does not grow an "unknown" column:
/// a server without a window field — measured: LM Studio, which returns no length field at all —
/// lists models just as well as one that has it.
pub(crate) fn row_for(model: &Model) -> String {
    match model.window_label() {
        Some(window) => format!("{} · {}", model.display(), window),
        None => model.display().to_string(),
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

/// De deksel op de tool-lus, naast de keuzelijst die er later boven komt.
///
/// Het vakje schakelt uit bij `Off`, zoals `SettingsPanel.kt:235` — dan draait er geen enkele tool, en
/// een bewerkbaar vakje zou een limiet beloven dat niets aangaat. `draft` laat bij `Off` het
/// opgeslagen limiet staan, dus wat hier niet te bereiken is, wordt ook niet stilletjes gewijzigd.
fn iterations_row(ui: &mut Ui, view: &mut SettingsView) {
    ui.label(dim("max tool iterations (turns the loop may spend)"));
    // `add_enabled` zoals `save_row` dat voor een onleesbaar bestand doet; de breedte volgt uit het
    // formulier, dus het vakje hoeft er niet apart een te krijgen.
    ui.add_enabled(
        view.tool_mode != ToolMode::Off,
        egui::TextEdit::singleline(&mut view.max_tool_iterations).hint_text("e.g. 8").desired_width(ui.available_width()),
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
    use super::{Reasoning, SettingsCommand, SettingsEvent, SettingsView, ToolMode};
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
            // Niet leeg, omdat dit veld geen "niet gekozen" kent: een blanco deksel wijst `draft`
            // af, en diep in een test over iets anders is dat een ruis die niets bewijst. Een paneel
            // uit `load` krijgt deze waarde vanzelf uit het bestand of uit de default.
            max_tool_iterations: baudet_settings::DEFAULT_MAX_TOOL_ITERATIONS.to_string(),
            stored_max_tool_iterations: baudet_settings::DEFAULT_MAX_TOOL_ITERATIONS,
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
    fn the_iteration_cap_survives_a_panel_round_trip() {
        // Het veld is geen sierraad: wat erin stond, moet door `draft` heen halen, want de lus
        // handelt ernaar. Een 12 die hier op 8 terugvalt, is een limiet die de persoon kwijt is
        // zonder dat er iets omviel.
        let mut panel = view();
        panel.max_tool_iterations = "12".into();
        assert_eq!(panel.draft().unwrap().max_tool_iterations, 12);
    }

    #[test]
    fn a_blank_or_nonsense_cap_is_refused_rather_than_repaired() {
        // `SettingsPanel.kt:373-375` faalt luid op een blanco of niet-getal veld, en repareert het
        // niet op de default. Een stil gerepareerd limiet is erger dan een weigering: de persoon
        // denkt dat hij 20 koos en draait op 8.
        for tekst in ["", "   ", "lots", "0", "-1", "2.5"] {
            let mut panel = view();
            panel.max_tool_iterations = tekst.into();
            let problem = panel.draft().unwrap_err();
            assert!(problem.contains("not a positive number"), "witruimte en onzin moeten benoemd worden, niet gerepareerd: {problem}");
        }
    }

    #[test]
    fn switching_tools_off_keeps_the_cap_the_person_chose() {
        // De andere helft van `SettingsPanel.kt:372`: bij `Off` is het vakje uitgeschakeld, dus wat
        // er staat is niet wat de persoon bewerkte — het opgeslagen limiet blijft staan. Anders zou
        // "lus uit" zijn limiet stilletjes op 8 terugzetten, en pas terugkomen als hij hem weer aanzet.
        let mut panel = view();
        panel.max_tool_iterations = "3".into();
        panel.stored_max_tool_iterations = 3;
        panel.max_tool_iterations.clear(); // het vakje is uitgeschakeld: de tekst is niet meer van hem
        panel.tool_mode = ToolMode::Off;
        assert_eq!(panel.draft().unwrap().max_tool_iterations, 3, "lus uit mag een limiet niet wissen");

        // En terug aan: het vakje is van de persoon, dus zijn tekst geldt — blanco wordt dan wél geweigerd.
        panel.tool_mode = ToolMode::Ask;
        assert!(panel.draft().is_err(), "terug aan hoort het blanco vakje te horen");
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

    /// Draw a row, store the choice, save it: the suffix belongs to the screen and must not
    /// survive into the file. What goes out is the id the row was built from, so a picker that
    /// reads "… · 256k" still writes the name the endpoint actually accepts.
    #[test]
    fn a_row_with_a_window_still_saves_the_bare_id() {
        let mut panel = view();
        panel.models =
            vec![Model { id: "Qwen3.8-Flash-Next-oQ4e-mtp".to_string(), label: None, context_window: Some(262144) }];
        panel.model = Some("Qwen3.8-Flash-Next-oQ4e-mtp".to_string());
        // The row a person reads, and the draft a save would write, from the same state.
        assert_eq!(super::row_for(&panel.models[0]), "Qwen3.8-Flash-Next-oQ4e-mtp · 256k");
        let settings = panel.draft().unwrap();
        // Equality with the id is the whole claim; the row's own suffix must be absent from it.
        assert_eq!(settings.model.as_deref(), Some("Qwen3.8-Flash-Next-oQ4e-mtp"));
        assert!(!settings.model.as_deref().unwrap().contains(" · "), "a stored model id carries no row suffix");
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

    /// The row a person reads. `Model::context_window` was parsed and read by nobody, so the
    /// gateway's published ceilings reached no screen; `row_for` is that consumer, and this test is
    /// the reason it cannot silently become unread again.
    #[test]
    fn a_fetched_window_shows_beside_the_name_it_belongs_to() {
        let windowed = |id: &str, tokens| Model { id: id.to_string(), label: None, context_window: tokens };
        // Measured on the gateway, in the gateway's own numbers.
        assert_eq!(
            super::row_for(&windowed("Qwen3.8-Flash-Next-oQ4e-mtp", Some(262144))),
            "Qwen3.8-Flash-Next-oQ4e-mtp · 256k"
        );
        // A server that publishes no window — measured: LM Studio — gets the bare name, so the
        // picker shows no empty column and invents no ceiling.
        assert_eq!(super::row_for(&windowed("google/gemma-4-26b-a4b", None)), "google/gemma-4-26b-a4b");
        // The friendly name wins over the id, as it does without a window: the suffix is added to
        // what display() already chose, not to the id.
        let named =
            Model { id: "qwen-turbo".to_string(), label: Some("Turbo".to_string()), context_window: Some(131072) };
        assert_eq!(super::row_for(&named), "Turbo · 128k");
    }
}
