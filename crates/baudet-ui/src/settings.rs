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
//! The panel carries what the Kotlin `SettingsPanel` carries, except one row: the MCP manager
//! (`SettingsPanel.kt:163-166`). `mcpServers` and `mcpAllowServers` have no client in this rewrite
//! yet, and a setting that nothing reads is a promise the app cannot keep — that row arrives with
//! the MCP-client item, not before it.

use crate::palette;
use baudet_backend::protocol::Protocol;
use baudet_i18n::{Key, Language, Texts};
use baudet_settings::config::{ContextStrategy, ToolMode};
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
    /// De inhoud van het vertrouwde-families-vak toepassen, zoals `SettingsPanel.kt:286-292` dat
    /// doet: de regels zijn hier al getrimd en van hun ledige regels ontdaan, en wat overgaat is de
    /// drager van de keuze. De **dragerlijst** gaat mee en niet de ruwe tekst uit het vak, want de
    /// regel die beslist of een gezin vertrouwd mag worden, kent alleen de tekenlaag — en een
    /// tweede implementatie van die regel in dit paneel is hoe een UI breder gaat vertrouwen dan
    /// de commandolaag eronder. Het tellen van afgevallen regels hoort daar daarom ook.
    ApplyTrusted(Vec<String>),
    /// Elke vertrouwde family wissen (`SettingsPanel.kt:294-297`).
    ClearTrusted,
    /// Elke altijd-toestaan-speld wissen, zodat elk commando opnieuw vraagt
    /// (`SettingsPanel.kt:299-302`).
    ClearAlways,
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
    /// Het vertrouwde-vak is toegepast: hoeveel regels er vielen, en of het bestand ze nam. De twee
    /// moeten allebei landen — `TrustedDropped` is een succes-melding, en wie niet weet dat de
    /// schrijfpoging weigerde, gelooft dat zijn lijst geldt.
    TrustedApplied {
        /// De lijsten zoals het bestand ze nu draagt: de families na teken-en-afval, en de pins.
        /// Ze reizen mee omdat het paneel anders meteen weer tekent wat er vóór de knop stond.
        trusted: Vec<String>,
        always: Vec<String>,
        dropped: usize,
        outcome: Result<(), WriteError>,
    },
}

/// The panel's whole state: the view-model plus what the last read, fetch, and save reported.
///
/// The derived `Default` leaves every text field empty, which is right for all of them except one:
/// the iteration cap has no empty state, since the loop always runs under a cap. A panel that came
/// from nowhere would therefore refuse its own draft. That cannot happen on a real start — `new`
/// calls `load`, and `load` calls `adopt`, which fills the cap from the file or from the settings'
/// own default — so the empty is filled where it is actually observable: in the test fixture below.
#[derive(Debug, Clone, PartialEq, Default)]
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
    /// Het contextbeleid zoals het bestand het draagt, plus de drie verdeelstanden.
    ///
    /// Dit is de voorraad van alles waar deze panel géén widget voor heeft. Het beleid, het budget en
    /// de beurtteller hebben die wél (de drie velden hieronder), dus `draft` leest hen en niet deze
    /// kopie; wat hier blijft, is het geleerde venster en de verdeelstanden. De dragende regel is de
    /// `max_tool_iterations`-regel hierboven: een veld zonder widget mag bij een opslag niet op de
    /// standaard terugvallen — en het geleerde venster is precies het getal dat bij een geweigerde
    /// beurt werd betaald.
    stored_context: Settings,
    /// Het contextbeleid, bewerkbaar via de kiezer hieronder. `stored_context` bewaart de rest van
    /// het beleid (het geleerde venster, de verdeelstanden) die géén widget heeft; deze drie velden
    /// wél, dus `draft` leest hen en niet de kopie. Type volgt de origineler: de kiezer draagt het
    /// beleid zelf, de twee getallen zijn tekstvakken zoals `max_output_tokens`.
    context_strategy: ContextStrategy,
    context_budget: String,
    context_keep_recent: String,
    /// De sampling-temperatuur als tekst, precies zoals `max_output_tokens` dat doet: het vakje is
    /// het antwoord van de persoon, `draft` beslist of het een getal is. Een `f64`-veld zou een
    /// halve invoer ("0.") verstoppen die de persoon nog wil corrigeren.
    temperature: String,
    /// De `top_p`-marge als tekst, blanco = "stuur hem niet" — de toestand die het label van de
    /// origineler benoemt, dus blanco is hier wél betekenisvol en geen lege `f64`.
    top_p: String,
    /// Het aantal tegelijk gedraaide beurten als tekst, met dezelfde text-vs-getal-reden als hierboven.
    max_parallel_turns: String,
    /// De zeven vinkjes van `SettingsPanel.kt:41-49,31,38`. Booleans hebben geen blanco-toestand,
    /// dus ze leven hier als `bool` en niet als tekst: een vinkje is aan of uit, er is geen derde
    /// toestand die een `String` zou moeten verstoppen.
    use_responses_api: bool,
    store_responses: bool,
    use_server_session: bool,
    auto_approve_safe_commands: bool,
    git_allow_writes: bool,
    transcript_wrap: bool,
    honesty_footer: bool,
    /// De eigen systeemprompt als tekst, zoals de `JTextArea(5, 34)` van de origineler
    /// (`SettingsPanel.kt:52`). Tekst en niet `Option<String>`: blanco is hier de toestand "geen
    /// eigen prompt" (`AppConfig.kt:210` `ifBlank`), en die toestand bestaat al in de snaar.
    system_prompt: String,
    /// De vertrouwde commandofamilies als tekst, één per regel — de `JTextArea(6, 34)` van
    /// `SettingsPanel.kt:58`. De tekst is de **bewerking**, de lijst in `Settings` is het resultaat:
    /// pas `apply` vertaalt regels naar gezins-tekens, dus een half getypte regel kan hier staan
    /// zonder dat hij iets vertrouwt.
    trusted_text: String,
    /// De всегда-allow-spelden, als tekst die niemand bewerkt (`SettingsPanel.kt:61` zet
    /// `isEditable = false`). Ze ontstaan uit "altijd" op een goedkeuringskaart en niet uit dit
    /// vak, dus een bewerkbaar vakje zou goedkortjes suggereren die het domein niet kent.
    auto_allow_text: String,
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
    /// De taal uit het bestand, tot iemand opslaat.
    ///
    /// Een eigen veld en niet `stored_context.language`, om dezelfde reden als de lusdeksel daar
    /// staat: `stored_context` is de kopie van alles waar deze panel géén widget voor heeft, en de
    /// taal heeft er wél een. terwijs zou `draft()` het gekozen woord moeten overlappen terwijl er
    /// een veld naast staat dat het zegt.
    language: Language,
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
        // Alleen het contextbeleid wordt overgenomen, en dan als hele kopie: de velden die de panel
        // zélf draagt, horen bij die widgets en niet bij deze voorraad.
        self.context_strategy = settings.context_strategy;
        self.context_budget = settings.context_budget_tokens.to_string();
        self.context_keep_recent = settings.context_keep_recent_messages.to_string();
        // De drie generatie-parameters worden tekst, zoals de andere getal-velden: blanco bij
        // `top_p` betekent "staat niet in het bestand", want de default is `None`.
        self.temperature = settings.temperature.to_string();
        self.top_p = settings.top_p.map(|p| p.to_string()).unwrap_or_default();
        self.max_parallel_turns = settings.max_parallel_turns.to_string();
        // De vinkjes zijn booleans en reizen als booleans. Let op `use_server_session`: dat veld
        // had al een widget-loos leven in `stored_context`; nu de checkbox bestaat, is deze kopie de
        // dragende — `draft` leest hem hier en niet uit de voorraad.
        self.use_responses_api = settings.use_responses_api;
        self.store_responses = settings.store_responses;
        self.use_server_session = settings.use_server_session;
        self.auto_approve_safe_commands = settings.auto_approve_safe_commands;
        self.git_allow_writes = settings.git_allow_writes;
        self.transcript_wrap = settings.transcript_wrap;
        self.honesty_footer = settings.honesty_footer;
        self.system_prompt = settings.system_prompt.clone();
        self.trusted_text = settings.trusted_commands.join("\n");
        self.auto_allow_text = settings.auto_allow_commands.join("\n");
        self.stored_context.use_server_session = settings.use_server_session;
        self.stored_context.learned_context_window = settings.learned_context_window;
        self.stored_context.learned_context_key = settings.learned_context_key.clone();
        // De verdeelstanden zijn geen instelling die de panel bedient, en juist daarom moeten ze
        // mee: bij een opslag van de panel — of bij het sluiten van het venster — mag een veld waar
        // niemand aan zat niet op de fabrieksstand terugvallen.
        self.stored_context.left_divider = settings.left_divider;
        self.stored_context.right_divider = settings.right_divider;
        self.stored_context.files_divider = settings.files_divider;
        // De kiezer laat het bestandwoord zien en niet een vertaalde naam: hetzelfde argument als
        // bij de systeemprompt daarboven (`SettingsPanel.kt:258-261` over de raw stored text). Wat
        // hier zou staan als "Nederlands", gaat terug als gebruiker-tekst en bevriest wat bij een
        // latere taalwijziging mee moet bewegen. De kiezer zelf toont `Language::native_name`.
        self.language = settings.language;
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
            _ => self.max_tool_iterations.trim().parse::<u32>().ok().filter(|n| *n > 0).ok_or_else(|| {
                format!("max tool iterations is not a positive number: {}", self.max_tool_iterations.trim())
            })?,
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
            context_budget_tokens: parse_context_budget(&self.context_budget)?,
            context_strategy: self.context_strategy,
            context_keep_recent_messages: parse_keep_recent(&self.context_keep_recent)?,
            temperature: parse_temperature(&self.temperature)?,
            top_p: parse_top_p(&self.top_p)?,
            max_parallel_turns: parse_parallel_turns(&self.max_parallel_turns)?,
            use_responses_api: self.use_responses_api,
            store_responses: self.store_responses,
            use_server_session: self.use_server_session,
            auto_approve_safe_commands: self.auto_approve_safe_commands,
            git_allow_writes: self.git_allow_writes,
            transcript_wrap: self.transcript_wrap,
            honesty_footer: self.honesty_footer,
            // De tekst gaat zoals hij is: `resolved_system_prompt` kent de ledig-regel en `draft`
            // mag een spa vooraan niet wegpoetsten, want die kan bedoeld zijn.
            system_prompt: self.system_prompt.clone(),
            // De twee vertrouwenslijsten komen uit het **bestand** en niet uit de tekstvakken
            // hierboven: `SettingsPanel.kt:286-292` past ze alleen toe op zijn eigen
            // "vertrouwde lijst"-knop, en een opslagknop die een half getypte regel meeneemt,
            // vertrouwt iets wat niemand afmaakte. Daarom deze kopie uit de voorraad.
            trusted_commands: self.stored_context.trusted_commands.clone(),
            auto_allow_commands: self.stored_context.auto_allow_commands.clone(),
            learned_context_window: self.stored_context.learned_context_window,
            learned_context_key: self.stored_context.learned_context_key.clone(),
            left_divider: self.stored_context.left_divider,
            right_divider: self.stored_context.right_divider,
            files_divider: self.stored_context.files_divider,
            language: self.language,
        })
    }

    /// De drie verdeelstanden zoals het bestand ze draagt. De lezing die het venster nodig heeft
    /// vóórdat er iets getekend is — de panelbreedte bij het openen — komt hier vandaan en niet uit
    /// een tweede `read` van dezelfde file: één lezing per vensterleven is ook hier de regel.
    pub fn dividers(&self) -> crate::layout::Dividers {
        crate::layout::Dividers {
            left: self.stored_context.left_divider,
            right: self.stored_context.right_divider,
            files: self.stored_context.files_divider,
        }
    }

    /// Wat er bij het sluiten in diezelfde velden mag staan, met de live maten erin.
    pub fn set_dividers(&mut self, dividers: &crate::layout::Dividers) {
        self.stored_context.left_divider = dividers.left;
        self.stored_context.right_divider = dividers.right;
        self.stored_context.files_divider = dividers.files;
    }

    /// De taal die het gelezen bestand draagt, voor de vensterbouwer die vóór de eerste pass
    /// moet weten in welke taal hij labels gaat zetten. Zelfde pattern als `dividers()`: de
    /// lezing is eenmaal gedaan en de app leest haar uit de panel in plaats van het bestand nog
    /// een keer open te doen.
    pub fn language(&self) -> Language {
        self.language
    }

    /// Het bestand waarnaar een opslag gaat, of `None` wanneer nog niets werd gelezen.
    pub fn file_path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    /// Een opslag van alleen de venster-maten, via dezelfde deur als een gewone opslag: dezelfde
    /// worker, dus dezelfde weigering wanneer het bestand onleesbaar is.
    pub fn geometry_draft(&self, dividers: &crate::layout::Dividers) -> Result<Settings, String> {
        let mut settings = self.draft()?;
        settings.left_divider = dividers.left;
        settings.right_divider = dividers.right;
        settings.files_divider = dividers.files;
        Ok(settings)
    }

    /// Drain one worker report into the view. Returns nothing: every outcome lands in `self`, which
    /// the next paint shows. A listing is only trusted when it answers the endpoint still in the
    /// field, so a slow answer to an old endpoint cannot overwrite the picker for a new one.
    pub fn apply_event(&mut self, event: SettingsEvent) {
        match event {
            // De drie vertrouwensrapporten delen één slot: het paneel toont daarna wat er werkelijk
            // staat, want de knoppen schrijven zélf en hun tekstvak is een bewerking, niet de waarheid.
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
            // De knop schreef zélf, dus wat er nu in de vakken staat, komt uit het antwoord en niet
            // uit wat iemand typte. Bij een weigering blijven de vakken zoals ze waren: de persoon
            // wil zijn regels terugzien, niet een lijst die hij nooit goedkeurde. `editie`-velden
            // en voorraad gaan samen — één waarheid, twee plekken.
            SettingsEvent::TrustedApplied { trusted, always, dropped, outcome } => {
                if let Ok(()) = outcome {
                    self.stored_context.trusted_commands = trusted.clone();
                    self.stored_context.auto_allow_commands = always.clone();
                    self.trusted_text = trusted.join("\n");
                    self.auto_allow_text = always.join("\n");
                }
                self.status = match outcome {
                    Ok(()) => Texts::fmt(Key::TrustedDropped, &[&dropped]),
                    Err(WriteError::WouldDiscard { reason, .. }) => {
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

    /// Leg de gekozen model-id vast, zoals `ModelsPanel.kt:79-84` dat doet: dezelfde opslag die het
    /// ingestformulier gebruikt, niet een tweede pad naar dat bestand.
    ///
    /// De keuze zelf is al gezet toen een rij werd aangeklikt (`model`), dus dit is alleen de
    /// overdracht. Die gaat via `draft()` en `SettingsCommand::Save`, wat twee regels gratis meegeeft
    /// die een eigen schrijver zou vergeten: een onleesbaar instandbestand blijft onaangeroerd
    /// (`blocked`), en de sessie krijgt haar `Adopted` — dus het model van de volgende beurt is
    /// hetzelfde als dat op het scherm. Zonder deze weg kon het blad wel een keuze tonen die het
    /// bestand nooit haalde.
    ///
    /// Geen keuze is een melding en geen fout: de knop naast een lege lijst heeft niets te bewaren.
    pub fn pick_model(&mut self, tx: &std::sync::mpsc::Sender<SettingsCommand>) {
        let Some(chosen) = self.model.clone().filter(|m| !m.trim().is_empty()) else {
            self.status = "pick a model first".to_string();
            return;
        };
        match self.draft() {
            Ok(mut settings) => {
                settings.model = Some(chosen);
                self.status = "saving…".to_string();
                let _ = tx.send(SettingsCommand::Save(settings));
            }
            Err(problem) => self.status = problem,
        }
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

    /// De statusregel van de panel doorgeven aan een buitenstaander — het venster dat bij het
    /// sluiten niet kan opslaan, draagt die zin hierheen in plaats van het veld te raken. De
    /// panel blijft de eigenaar van de tekst, zoals hij die ook na elke worker-melding zet.
    pub(crate) fn report(&mut self, problem: String) {
        self.status = problem;
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
/// De breedte van de etiketkolom van het formulier.
///
/// `Form.kt:14-32` zet het etiket in kolom 0 en het veld in kolom 1 van een `GridBagLayout`, dus het
/// veld begint waar het langste etiket eindigt en al het overige gaat naar het veld (`weightx = 1.0`,
/// `fill = HORIZONTAL`). Dit formulier is smaller dan dat van de origineler — 360 pt tegenover diens
/// 34 tekens — dus de kolom is uit de eigen teksten opgemaakt: het langste etiket dat hier voorkomt is
/// het Nederlandse "system-prompt (leeg = ingebouwd)". Past een tekst toch niet in de kolom, dan valt
/// [`form_row`] terug op etiket-boven-veld in plaats van het etiket te kappen.
const LABEL_COLUMN: f32 = 168.0;

/// De lucht tussen twee formulierrijen, gemeten tussen hun **kaders**, in pt.
///
/// De origineler zet `Insets(4, gap, 4, gap)` op élke cel van zijn `GridBagLayout`
/// (`Form.kt:16`), dus twee rijen scheiden daar 4 + 4 = 8 pt. 8 is daarom de maat, en geen smaak:
/// minder is een verslechtering ten opzichte van wat werkt.
///
/// Wat deze waarde **niet** is, is het getal dat in `add_space` gaat. Een veld van 20 pt gereserveerd
/// tekent een kader van 28 pt — `FrameShape`(inner) 20 + 2×`button_padding`(4) + 2×`stroke`(1), en
/// 4 pt daarvan valt buiten de reservering. Gemeten met twee identieke velden onder elkaar en
/// `add_space(g)` ertussen: `g` = 0 → −5 pt, 6 → 1, 8 → 3, 12 → 7. De lucht tussen de kaders is dus
/// `g − 5`, en wie 8 pt tussen de kaders wil, moet 13 reserveren. Daarom gaat [`ROW_GAP`] hier door
/// [`lucht_voor_kader`] en staat dit document boven die functie, want deze 5 pt zijn precies de
/// "geen ruimte" uit de klachten over base-url/fetch en reasoning/top_p.
pub const ROW_GAP: f32 = 8.0;

/// de `add_space` die [`ROW_GAP`] pt lucht tussen twee kaders oplevert, in pt.
///
/// Het verschil is de kader-uitssteek, gemeten en niet verzonnen. Een tekstveld van 20 pt tekent 28
/// (vijf pt meer dan zijn reservering), een knop en een kiezer 26 (één pt minder), een veld met een
/// `add_enabled`-wikkel 27 (drie). De strakste paar in dit formulier is dus het veld-veld-paar, en
/// [`lucht_voor_kader`] rekent tegen die aan — een knop onder een veld krijgt er drie pt bij.
///
/// Waarom hier een functie en niet een hogere `ROW_GAP`: `add_space` is de enige ruimte tussen twee
/// `form_row`'s (`layout.rs:757` past `item_spacing.y` ná de widget toe), dus deze correctie hoort op
/// de plek die de lucht ingeest en niet in de maat zelf — `ROW_GAP` blijft de 8 pt van de origineler,
/// en de 5 gaan er wél in. Zonder deze functie staat élke rij van het formulier op 3 pt.
fn lucht_voor_kader(g: f32) -> f32 {
    if g <= 0.0 {
        0.0
    } else {
        g + 5.0
    }
}

/// De lucht rond een groepskop en tussen de knoppen onder een veld, in pt.
///
/// Groter dan [`ROW_GAP`], want een kop is geen rij: hij opent een band en moet van de rijen
/// eronder én erboven te onderscheiden zijn zonder kader of lijn. De origineler bereikt dat met een
/// `TitledBorder` (`SettingsPanel.kt:143,165`), en diens rand inneemt ook ruimtes in. Dit formulier
/// tekent geen kader, dus is de lucht het enige verschil tussen "kop van een band" en "nog een rij".
pub const GROUP_GAP: f32 = 16.0;

/// Eén formulierrij: etiket links, veld rechts, zoals `Form.kt:14-32` het doet.
///
/// De kolombreedte wordt hier bepaald en niet binnen `build`, want `build` roept
/// `add_sized([ui.available_width(), ..])` en mag dus alleen de veldkolom zien. Wat dit herstelt, is de
/// stapeling: het veld opeiste de volledige formularbreedte en het etiket verdween erbovenheen. Past
/// het etiket niet naast het veld, dan valt hij terug op etiket boven veld — maar de veldkolom blijft
/// waar hij is, op `LABEL_COLUMN`. Een lang etiket kost dan hoogte, kapt nooit zijn woorden af, en het
/// formulier zakt niet naar de linker rand: de kolom die bij de andere rijen gilt, geldt ook hier.
/// (Gemeten aan "max output tokens (0 = server)": dat etiket is breder dan de kolom en zijn veld
/// belandde vóór de velden erboven in plaats van onder ze.)
fn form_row(ui: &mut Ui, label: impl Into<String>, build: impl FnOnce(&mut Ui)) {
    form_row_with_gap(ui, ROW_GAP, label, build);
}

/// Als [`form_row`], met de lucht boven de rij alsparameter.
///
/// De lucht hoort vóórdat hem iets wordt aangeboden: `advance_after_rects` past `item_spacing.y` pas
/// ná een widget toe (`layout.rs:757`), dus een `add_space` ná de rij telt niet voor de rij eronder.
/// Of er lucht boven mag, weet de aanroeper en niet deze functie: de eerste rij van een groep hangt
/// aan een `Separator` en krijgt er dus geen, en die weet alleen `body`. Daarom een parameter en geen
/// blik op de beschikbare hoogte — het formulier staat in een `ScrollArea` met `auto_shrink` uit
/// (`app.rs`), dus die hoogte is er altijd en zegt niets over wat er boven deze rij stond.
fn form_row_with_gap(ui: &mut Ui, boven: f32, label: impl Into<String>, build: impl FnOnce(&mut Ui)) {
    let text = dim(label);
    let taken = ui.available_size_before_wrap();
    if boven > 0.0 {
        ui.add_space(lucht_voor_kader(boven));
    }
    // Meten in plaats van gokken op een tekenbreedte. `into_galley` is de weg die de label zelf ook
    // gaat: hij lost de letter op zoals de stijl die voorschrijft (`widget_text.rs:742`) en een
    // oneindige breedte breekt niets, dus `size()` is de echte breedte van het etiket. Zo is de maat
    // die meet ook de maat die tekent.
    let inkling = text.clone().into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body);
    if inkling.size().x + ROW_GAP <= LABEL_COLUMN {
        ui.horizontal(|ui| {
            ui.add_sized([LABEL_COLUMN, 20.0], egui::Label::new(text).selectable(false));
            build(ui);
        });
        // `ui.horizontal` krompt de rij tot zijn inhoud; deze zet houdt het formulier op zijn breedte,
        // zodat elke rij eindigt waar de vorige eindigde en het schuifvlak niet per rij verspringt.
        ui.set_min_width(taken.x);
    } else {
        // Eén rij met een smal dak en daaronder de gebruikelijke veldkolom. `ui.with` tekent de
        // kaders op dezelfde plek en in dezelfde volgorde, dus het etiket blijft boven het veld.
        ui.horizontal(|ui| {
            // `with_layout` en niet `with`: deze egui-build kent alleen de eerste (`ui.rs:2470`).
            ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                ui.set_width(LABEL_COLUMN - ROW_GAP);
                ui.label(text);
            });
            ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                ui.set_width((taken.x - LABEL_COLUMN).max(0.0));
                build(ui);
            });
        });
        ui.set_min_width(taken.x);
    }
}

/// commands go out by value, so a click commits once against the state that produced it.
pub fn body(ui: &mut Ui, view: &mut SettingsView, tx: &std::sync::mpsc::Sender<SettingsCommand>) {
    let mut commands: Vec<SettingsCommand> = Vec::new();
    // 360 pt: het plafond dat het venster gaf, niet de breedte van het middenvlak. Het formulier is
    // één kolom, en elke veldbreedte hieronder wordt uit deze breedte opgemaakt.
    ui.set_width(ui.available_width().min(360.0));
    // Taal bovenaan, zoals de rijenlijst van `SettingsPanel.kt:107` — daar staat
    // `LanguageLabel to language` als eerste rij, vóór het endpoint.
    // Eerste rij van het formulier: er hangt niets boven, dus geen lucht.
    language_row(ui, view, 0.0);
    group_break(ui);
    // Eerste rij van zijn groep, dus ook hier geen lucht boven de etiketrij.
    endpoint_row(ui, view, &mut commands, 0.0);
    group_break(ui);
    // Groep, dus geen lucht.
    model_row(ui, view, 0.0);
    group_break(ui);
    // De sampling-rijen staan vóór de redeneer-kiezer, zoals de volgorde van `SettingsPanel.kt:117-119`
    // (temperature en topP tussen de beurtteller en reasoning). De eerste draagt geen lucht boven
    // zich, want hij hangt aan de scheiding erboven.
    sampling_rows(ui, view);
    // De redeneer-kiezer hangt niet aan een band: hij volgt op `top_p`, dus hij draagt zijn
    // rijlucht. Met 0.0 hier plakten zijn 26 pt hoge Combo Box en het vak erboven aan elkaar
    // (gemeten: 309 < 313, dus 4 pt overlapping). De rijen eronder houden dezelfde maat.
    reasoning_row(ui, view, ROW_GAP);
    tokens_row(ui, view);
    iterations_row(ui, view);
    parallel_row(ui, view);
    group_break(ui);
    // De zeven vinkjes, als eigen groep direct na de parallelstand — dezelfde plek als in de
    // volgorde van `SettingsPanel.kt:121-128`.
    toggle_rows(ui, view);
    // De systeemprompt sluit de band af, als laatste rij vóór de contextgroepen — de plek die
    // `SettingsPanel.kt:129-130` hem geeft (na de vinkjes, vóór de knoppen).
    system_prompt_row(ui, view);
    group_break(ui);
    // Het commandevertrouwen als eigen band, de `commandPanel` van `SettingsPanel.kt:141-159`.
    command_rows(ui, view, &mut commands);
    group_break(ui);
    context_rows(ui, view);
    group_break(ui);
    key_row(ui, view, 0.0);
    if view.blocked {
        group_break(ui);
        ui.colored_label(palette::DIFF_REMOVED, "Your settings file is unreadable.");
        ui.label(dim("Saving is disabled until it is fixed, so a save can never replace what is still in that file."));
    }
    group_break(ui);
    save_row(ui, view, &mut commands);
    // Eén status, onderaan: het is de plek waar een listing of een save zijn woorden achterlaat, en
    // de referentie-app zet het datapad daar ook. Leeg is geen regel: dan staat er niets, ook geen
    // lege lijn met een scheiding erboven.
    if !view.status.is_empty() {
        group_break(ui);
        ui.label(dim(&view.status));
    }
    for command in commands {
        let _ = tx.send(command);
    }
}

/// De taalkeuze. Bovenstaande labels staan in deze taal; deze rij is waar iemand hem wisselt.
///
/// Een kiezer over `Language::ALL` en niet over vier losse woorden, zodat een taal die hier wordt
/// toegevoegd automatisch door het venster en door het bestand gaat. De items dragen
/// `native_name` — "Deutsch" blijft "Deutsch", want een kiezer die de taalnamen zélf vertaalt, is
/// onleesbaar voor iemand die juist die taal zoekt (`SettingsPanel.kt:40` doet eveneens).
///
/// Er gebeurt niets bij de klik. De toepassing hangt aan de opslag, want de rij is hier het enige
/// wat de taal wijzigt en de origineler herbouwt het venster alleen op diens eigen
/// opslagknop (`Main.kt:74-82`, en `TipSave`: "The language switch is applied here too.").
/// De kiezer toont dus wat er op slot staat en niet wat er nu op screen staat; dat verschil is
/// precies wat het verschil tussen "kies" en "pas toe" zichtbaar maakt.
fn language_row(ui: &mut Ui, view: &mut SettingsView, boven: f32) {
    form_row_with_gap(ui, boven, Texts::get(Key::LanguageLabel), |ui| {
        let mut current = view.language;
        let picker = egui::ComboBox::from_id_salt("settings-language")
            .selected_text(view.language.native_name())
            .show_ui(ui, |ui| {
                for option in Language::ALL {
                    ui.selectable_value(&mut current, option, option.native_name());
                }
            })
            .response;
        picker.on_hover_text(Texts::get(Key::TipLanguage));
        view.language = current;
    });
}

fn endpoint_row(ui: &mut Ui, view: &mut SettingsView, commands: &mut Vec<SettingsCommand>, boven: f32) {
    // Het adres is één rij met zijn etiket; de knoppen eronder zijn geen veld maar een bediening,
    // en die houden hun eigen regel — `SettingsPanel.kt` kent die knoppen niet eens, ze zijn hier
    // het antwoord op "welke server spreekt dit".
    // Of het adres de focus draagt, is nodig buiten de rij: Enter hieronder mag alleen gelden voor
    // dit veld, anders zou een Enter in welk veld dan ook een listing afvuren. Daarom staat de lezing
    // hier en niet in de sluiting, want `form_row` geeft die waar niet terug.
    let mut focused = false;
    // De breedte die het formulier hier bezet, gelezen vóördat de knoppen eronder hem kunnen verlagen:
    // `ui.horizontal` krompt een rij tot zijn inhoud, en zonder deze zet springt het schuifvlak per rij.
    let rij_breed = ui.available_size_before_wrap().x;
    form_row_with_gap(ui, boven, Texts::get(Key::BaseUrlLabel), |ui| {
        let response = ui
            .add_sized(
                [ui.available_width(), 20.0],
                egui::TextEdit::singleline(&mut view.base_url)
                    // Het zout is het adres van tests/tabs.rs: een test telt dit veld mee als bewijs dat het
                    // formulier getekend is. Waarom het adres hier aan de body-`Ui` hangt en niet vlak op de
                    // wortel staat — en wat de test daarvoor moet samenstellen — staat bij `FIELD_ENDPOINT`.
                    .id_salt(FIELD_ENDPOINT)
                    .hint_text("http://127.0.0.1:8000/v1")
                    .desired_width(f32::INFINITY),
            )
            .on_hover_text(Texts::get(Key::TipBaseUrl));
        // Retyping the endpoint invalidates the picker: models belong to the endpoint that listed them.
        if response.changed() {
            view.fetched_for = None;
        }
        focused = response.has_focus();
    });
    // De knoppen zijn geen veld maar een bediening, en daarom zijn ze geen etiket-rij: ze staan op de
    // veldkolom, direct onder het adres, met een leeg etiketslot erlinks. Daarvoor stond de "Fetch
    // (GET /models)" plat tegen de linker rand, los van de kolom van het veld waarop hij hoort.
    //
    // De lucht boven de eerste knop is dezelfde ROW_GAP als tussen twee rijen, en niet het
    // `item_spacing.y` van het venster: deze twee rijen staan buiten [`form_row`] en erven 6 pt,
    // en een knop van 26 pt op 6 pt onder een veld van 28 pt **overlapt** dat veld (gemeten: de Fetch
    // `Shape::Rect` begint 4 pt vóórdat het adresveld eindigt). `boven` is hier de ondergrens, want
    // 0.0 betekent elders "deze rij hangt aan een band" en mag hier niets kwijtraken.
    ui.add_space(lucht_voor_kader(boven.max(ROW_GAP)));
    let fetch_now = ui
        .horizontal(|ui| {
            ui.add_sized([LABEL_COLUMN, 20.0], egui::Label::new("").selectable(false));
            ui.add_enabled(
                !view.fetching,
                egui::Button::new(if view.fetching { Texts::get(Key::Fetching) } else { Texts::get(Key::FetchModels) }),
            )
            .clicked()
        })
        .inner;
    // Tussen de twee bedieningen gaat dezelfde lucht in als boven ze. Zonder deze regel resteert
    // daar het `item_spacing.y` van het venster, en gemeten was dat een overlap: Fetch tekende op
    // 140..166 en Scan op 163..189 — 3 pt in elkaar. Geen berekening van mij maar wat een knop
    // doet: `item_spacing.y` is 6 pt en het kader van een knop steekt 3 pt boven zijn tekst uit,
    // dus 6 − 3 = 3 pt. Met deze aanroep erbij, en door [`lucht_voor_kader`]: de volle rijlucht.
    ui.add_space(lucht_voor_kader(ROW_GAP));
    ui.horizontal(|ui| {
        ui.add_sized([LABEL_COLUMN, 20.0], egui::Label::new("").selectable(false));
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
    ui.set_min_width(rij_breed);
    // Enter in the endpoint field fetches, because the first thing a person does after typing an
    // address is ask what is on it.
    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter)) && focused;
    if (fetch_now || enter) && !view.fetching {
        view.fetching = true;
        view.status = format!("asking {}…", view.base_url.trim());
        commands.push(SettingsCommand::Fetch {
            endpoint: view.base_url.trim().to_string(),
            api_key: Some(view.api_key.clone()).filter(|k| !k.trim().is_empty()),
        });
    }
}

fn model_row(ui: &mut Ui, view: &mut SettingsView, boven: f32) {
    // Het woord van de rij is de vraag, het veld erachter is het antwoord; een los woord boven een
    // keuzelijst was het uiterlijk van een koptekst, niet van een veld (`Form.kt:14-32`).
    if view.models.is_empty() {
        // De rij blijft staan met wat er ontbreekt, want een verdwenen veld laat iemand zoeken naar
        // een instelling die er wél is. De vroege `return` staat hier en niet in de sluiting, want
        // die sluiting hoort bij `form_row` en niet bij deze functie.
        form_row_with_gap(ui, boven, Texts::get(Key::ModelLabel), |ui| {
            ui.label(dim(if view.fetched_for.is_some() { "this endpoint lists none" } else { "not fetched yet" }));
        });
        return;
    }
    form_row(ui, Texts::get(Key::ModelLabel), |ui| {
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
        egui::ComboBox::from_id_salt("settings-model")
            .selected_text(selected)
            .show_ui(ui, |ui| {
                for (model, label) in view.models.iter().zip(row_labels.iter()) {
                    ui.selectable_value(&mut chosen, model.id.clone(), label.as_str());
                }
            })
            .response
            .on_hover_text(Texts::get(Key::TipModel));
        // Only a listed id is stored; "choose…" and the "(not listed)" entry leave the choice untouched.
        if chosen != current && view.models.iter().any(|m| m.id == chosen) {
            view.model = Some(chosen);
        }
    });
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

fn reasoning_row(ui: &mut Ui, view: &mut SettingsView, boven: f32) {
    form_row_with_gap(ui, boven, Texts::get(Key::ReasoningEffortLabel), |ui| {
        let mut current = view.reasoning;
        let reasoning = egui::ComboBox::from_id_salt("settings-reasoning")
            .selected_text(view.reasoning.label())
            .show_ui(ui, |ui| {
                for option in Reasoning::ALL {
                    ui.selectable_value(&mut current, option, option.label());
                }
            })
            .response;
        reasoning.on_hover_text(Texts::get(Key::TipReasoning));
        view.reasoning = current;
    });
}

/// De band met de zeven vinkjes van `SettingsPanel.kt:133-139`, in die volgorde.
///
/// De origineler zet ze als losse `JCheckBox`-rijen met een ledig etiket ("\" to useResponses");
/// een vinkje draagt zijn eigen tekst, dus hier geen `form_row` met eenetiket-kolom maar een
/// `checkbox` met het label-woord van de taaltabel. De eerste hangt aan de scheiding erboven en
/// krijgt geen lucht, de rest de normale rijlucht.
fn toggle_rows(ui: &mut Ui, view: &mut SettingsView) {
    let toggles: [(bool, Key); 7] = [
        (view.use_responses_api, Key::UseResponsesApi),
        (view.store_responses, Key::StoreResponses),
        (view.use_server_session, Key::SendPreviousResponseId),
        (view.auto_approve_safe_commands, Key::AutoApproveSafe),
        (view.git_allow_writes, Key::GitAllowWrites),
        (view.transcript_wrap, Key::TranscriptWrap),
        (view.honesty_footer, Key::HonestyFooter),
    ];
    // Eerst de maten lezen, dan schrijven: de indeks van het tuple wijst naar welk veld de klik
    // mag zetten, zodat de sluiting niet op elke tak een losse `view.<veld> = checkbox` hoeft te
    // herhalen (zeven keer hetzelfde patroon is precies hoe zo'n lijst uit synchronie raakt).
    for (index, (current, key)) in toggles.into_iter().enumerate() {
        if index > 0 {
            ui.add_space(ROW_GAP);
        }
        let mut value = current;
        let response = ui.checkbox(&mut value, Texts::get(key));
        let tip = match key {
            Key::StoreResponses => Key::TipStoreResponses,
            Key::AutoApproveSafe => Key::TipAutoApproveSafe,
            Key::GitAllowWrites => Key::TipGitAllowWrites,
            Key::TranscriptWrap => Key::TipTranscriptWrap,
            Key::HonestyFooter => Key::TipHonestyFooter,
            _ => key,
        };
        response.on_hover_text(Texts::get(tip));
        match index {
            0 => view.use_responses_api = value,
            1 => view.store_responses = value,
            2 => view.use_server_session = value,
            3 => view.auto_approve_safe_commands = value,
            4 => view.git_allow_writes = value,
            5 => view.transcript_wrap = value,
            _ => view.honesty_footer = value,
        }
    }
}

/// De band "commandevertrouwen": twee meerregelige vakken en de drie knoppen eronder.
///
/// De titel is geen `form_row`: de origineler groepeert met een `TitledBorder`
/// (`SettingsPanel.kt:142`), en een kader met een kop is precies wat hier past — de twee vakken
/// horen bij elkaar en niet bij de rijen erboven of eronder. De breedte van de labelkolom wordt
/// gemeten (`into_galley`) en niet geraamd: de Nederlandse labels zijn langer dan de Engelse, en
/// een vaste raming zou in één taal de knoppen en in een andere de vakken laten verspringen.
///
/// Het rechtervak is `ui.add_enabled(false, ..)`: de spelden ontstaan uit "altijd" op een
/// goedkeuringskaart (`AppState.kt:385` in de origineler) en niet uit dit vak, dus tonen mag,
/// typen niet. De wiskunde van `apply` doet deze rij **niet**: die kent de tekenregels niet en
/// mag ze niet gaan dupliceren; wat hier gebeurt, is regels splitsen en ze overdragen.
fn command_rows(ui: &mut Ui, view: &mut SettingsView, commands: &mut Vec<SettingsCommand>) {
    let title = Texts::get(Key::CommandManagerTitle);
    ui.add_space(GROUP_GAP);
    ui.label(egui::RichText::new(title).weak());
    ui.add_space(ROW_GAP);
    form_row_with_gap(ui, 0.0, Texts::get(Key::TrustedPrefixesLabel), |ui| {
        ui.add_sized(
            [ui.available_width(), 0.0],
            egui::TextEdit::multiline(&mut view.trusted_text)
                .hint_text("mvn\ngit commit")
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        )
        .on_hover_text(Texts::get(Key::TrustedPrefixesHint));
    });
    form_row_with_gap(ui, ROW_GAP, Texts::get(Key::AlwaysAllowLabel), |ui| {
        ui.add_enabled(
            false,
            egui::TextEdit::multiline(&mut view.auto_allow_text).desired_rows(3).desired_width(f32::INFINITY),
        )
        .on_hover_text(Texts::get(Key::AlwaysAllowHint));
    });
    ui.add_space(lucht_voor_kader(ROW_GAP));
    ui.horizontal(|ui| {
        let apply =
            ui.button(Texts::get(Key::ApplyTrustedButton)).on_hover_text(Texts::get(Key::TipApplyTrusted)).clicked();
        let clear_trusted = ui
            .small_button(Texts::get(Key::ClearTrustedButton))
            .on_hover_text(Texts::get(Key::TipClearTrusted))
            .clicked();
        let clear_always = ui
            .small_button(Texts::get(Key::ClearAlwaysButton))
            .on_hover_text(Texts::get(Key::TipClearAlways))
            .clicked();
        // Eerst de knoppen lezen, dan pas de regels snijden: een klik die niets opstuurt omdat het
        // vak leeg is, is een klik die de lijst ledig maakt, en dat is een beslissing.
        if apply {
            commands.push(SettingsCommand::ApplyTrusted(split_trusted_lines(&view.trusted_text)));
        }
        if clear_trusted {
            commands.push(SettingsCommand::ClearTrusted);
        }
        if clear_always {
            commands.push(SettingsCommand::ClearAlways);
        }
    });
}

/// De regels uit het vertrouwde-vak, gesplitst zoals `SettingsPanel.kt:287` ze splitst.
///
/// Elke regel wordt getrimd en een ledige valt stil af: die is een lege regel en geen besluit. Hij
/// telt ook **niet** als afgevallen regel — `TrustedDropped` zegt "verwijderd omdat ze geen veilige
/// familie noemen", en dat is over een ledige regel onwaar. Wat **niet** hier gebeurt, is beslissen
/// of een gezin veilig is: die afdwinging kent alleen de tekenlaag (`command_signature` in
/// `baudet-tool`), en die leeft niet in deze crate. Wat hieruit komt, is daarom gesplitst maar nog
/// niet gecontroleerd, en zonder telling: het tellen van afgevallen regels hoort bij diezelfde
/// tekenlaag, want alleen zij weet of een regel een gezin noemt.
fn split_trusted_lines(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_string).collect()
}

/// De systeemprompt als meerregelig vak, de `JTextArea(5, 34)` van `SettingsPanel.kt:52`.
///
/// `multiline` en niet `singleline`, want dit is de enige plek in dit formulier waar iemand
/// regels schrijft: de ingebouwde prompt is er eentje van twee zinnen, en een eenregelig vak
/// zou hem horizontaal moeten laten schuiven — precies wat `SettingsPanel.kt:129` met zijn
/// `scrollingField` wilde vermijden. De hoogte is vijf regels, de hoogte van die text area, en
/// niet zo hoog dat het formulier eronder wegloopt: de tab eromheen is al een `ScrollArea`.
///
/// Blanco is hier een geldig antwoord en geen fout — het is "geen eigen prompt" — dus er staat
/// geen controle op `draft`. Het label zegt het met zijn eigen woorden ("leeg = ingebouwd"), en
/// de hint herhaalt de ingebouwde tekst niet: die is lang, en een hint die de hele prompt toont,
/// is een vak dat nooit leeg lijkt.
fn system_prompt_row(ui: &mut Ui, view: &mut SettingsView) {
    form_row(ui, Texts::get(Key::SystemPromptLabel), |ui| {
        ui.add_sized(
            [ui.available_width(), 0.0],
            egui::TextEdit::multiline(&mut view.system_prompt)
                .hint_text("blank = built-in prompt")
                .desired_rows(5)
                .desired_width(f32::INFINITY),
        )
        .on_hover_text(Texts::get(Key::TipSystemPrompt));
    });
}

/// De drie sampling-rijen: temperatuur en `top_p`, in de volgorde van `SettingsPanel.kt:117-118`
/// (na de beurtteller, vóór de redeneer-kiezer). Beide zijn tekstvakken in dezelfde vorm als
/// `max_output_tokens`, want blanco is hier betekenisvol: bij `top_p` is blanco "niet sturen".
fn sampling_rows(ui: &mut Ui, view: &mut SettingsView) {
    form_row(ui, Texts::get(Key::TemperatureLabel), |ui| {
        ui.add_sized(
            [ui.available_width(), 20.0],
            egui::TextEdit::singleline(&mut view.temperature).hint_text("e.g. 0.7").desired_width(f32::INFINITY),
        )
        .on_hover_text(Texts::get(Key::TipTemperature));
    });
    form_row(ui, Texts::get(Key::TopPLabel), |ui| {
        ui.add_sized(
            [ui.available_width(), 20.0],
            egui::TextEdit::singleline(&mut view.top_p).hint_text("blank = do not send").desired_width(f32::INFINITY),
        )
        .on_hover_text(Texts::get(Key::TipTopP));
    });
}

/// De parallelstand, na de lus-deksel, zoals `SettingsPanel.kt:121`. Bewerkbaar vak in dezelfde vorm
/// als de andere getallen; blanco valt bij `draft` op de default van 1.
fn parallel_row(ui: &mut Ui, view: &mut SettingsView) {
    form_row(ui, Texts::get(Key::MaxParallelTurnsLabel), |ui| {
        ui.add_sized(
            [ui.available_width(), 20.0],
            egui::TextEdit::singleline(&mut view.max_parallel_turns).hint_text("1").desired_width(f32::INFINITY),
        )
        .on_hover_text(Texts::get(Key::TipMaxParallelTurns));
    });
}

fn tokens_row(ui: &mut Ui, view: &mut SettingsView) {
    form_row(ui, Texts::get(Key::MaxTokensLabel), |ui| {
        ui.add_sized(
            [ui.available_width(), 20.0],
            egui::TextEdit::singleline(&mut view.max_output_tokens).hint_text("e.g. 4096").desired_width(f32::INFINITY),
        )
        .on_hover_text(Texts::get(Key::TipMaxTokens));
    });
}

/// De deksel op de tool-lus, naast de keuzelijst die er later boven komt.
///
/// Het vakje schakelt uit bij `Off`, zoals `SettingsPanel.kt:235` — dan draait er geen enkele tool, en
/// een bewerkbaar vakje zou een limiet beloven dat niets aangaat. `draft` laat bij `Off` het
/// opgeslagen limiet staan, dus wat hier niet te bereiken is, wordt ook niet stilletjes gewijzigd.
fn iterations_row(ui: &mut Ui, view: &mut SettingsView) {
    form_row(ui, Texts::get(Key::MaxToolIterationsLabel), |ui| {
        // `add_enabled` zoals `save_row` dat voor een onleesbaar bestand doet; de breedte volgt uit het
        // formulier, dus het vakje hoeft er niet apart een te krijgen.
        ui.add_enabled(
            view.tool_mode != ToolMode::Off,
            egui::TextEdit::singleline(&mut view.max_tool_iterations)
                .hint_text("e.g. 8")
                .desired_width(ui.available_width()),
        )
        .on_hover_text(Texts::get(Key::TipMaxIterations));
    });
}

/// Het contextbeleid: wat er gebeurt als een gesprek het model ontgroeit.
///
/// Drie rijen uit de volgorde van `SettingsPanel.kt:113-116`: eerst de kiezer (`ContextStrategyLabel`,
/// strategy-combo), dan het budget (`ContextBudgetLabel`), en tenslotte de beurtteller
/// (`KeepRecentLabel`). De twee getallen delen de `max_output_tokens`-vorm: een blanco vak is "auto"
/// voor het budget en de vloer-waarde voor de teller, zoals de origineler die vloer in het panel
/// afdwingt (`ContextLimits`-lezing: 1) en niet in het bestand.
fn context_rows(ui: &mut Ui, view: &mut SettingsView) {
    form_row(ui, Texts::get(Key::ContextStrategyLabel), |ui| {
        let mut chosen = view.context_strategy;
        egui::ComboBox::from_id_salt("settings-context-strategy")
            .selected_text(strategy_label(view.context_strategy))
            .show_ui(ui, |ui| {
                for option in ContextStrategy::ALL {
                    ui.selectable_value(&mut chosen, option, strategy_label(option));
                }
            })
            .response
            .on_hover_text(Texts::get(Key::TipContextStrategy));
        view.context_strategy = chosen;
    });
    form_row(ui, Texts::get(Key::ContextBudgetLabel), |ui| {
        ui.add_sized(
            [ui.available_width(), 20.0],
            egui::TextEdit::singleline(&mut view.context_budget).hint_text("0 = auto").desired_width(f32::INFINITY),
        )
        .on_hover_text(Texts::get(Key::TipContextBudget));
    });
    form_row(ui, Texts::get(Key::KeepRecentLabel), |ui| {
        ui.add_enabled(
            view.context_strategy.trims_by_turn_count(),
            egui::TextEdit::singleline(&mut view.context_keep_recent)
                .hint_text("e.g. 6")
                .desired_width(ui.available_width()),
        )
        .on_hover_text(Texts::get(Key::TipKeepRecent));
    });
}

/// De kiezer-woorden voor het beleid, zoals `SettingsPanel.kt:33-35` ze toont.
fn strategy_label(strategy: ContextStrategy) -> &'static str {
    match strategy {
        ContextStrategy::RecentMessages => Texts::get(Key::StrategyRecent),
        ContextStrategy::RollingWindow => Texts::get(Key::StrategyRolling),
        ContextStrategy::StopAtLimit => Texts::get(Key::StrategyStop),
    }
}

/// Het budget: blanco of "0" is auto (0), een getal is het plafond, alles anders is een fout.
fn parse_context_budget(text: &str) -> Result<u32, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }
    trimmed.parse::<u32>().map_err(|_| format!("context budget is not a token count: {trimmed}"))
}

/// De beurtteller: blanco behoudt het opgeslagen getal niet — de panel toont altijd een getal, dus
/// blanco of nul valt op de vloer van 1, zoals de origineler (`context_keep_recent_messages`-lezing).
/// Een niet-getal is een fout en mag niet stil terugvallen.
fn parse_keep_recent(text: &str) -> Result<u32, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(1);
    }
    trimmed.parse::<u32>().map(|n| n.max(1)).map_err(|_| format!("keep last turns is not a whole number: {trimmed}"))
}

/// De temperatuur: blanco of niet-getal is een fout, want unlike `top_p` gaat dit getal altijd mee
/// en bestaat er geen "stuur hem niet" voor. Het interval [0, 2] is de grens die de origineler en
/// elke OpenAI-compatibele server hanteren; erbuiten is het geen smakelijkheid maar een typefout.
fn parse_temperature(text: &str) -> Result<f64, String> {
    let trimmed = text.trim();
    // Blanco is hier een fout en geen "neem de default": unlike `top_p` is er geen "stuur hem niet"
    // voor de temperatuur, en deze panel valst nooit stil terug op een standaard (dezelfde regel als
    // `max_tool_iterations`). Het vakje toont daarom altijd een getal, en `draft` eist er een.
    let value = trimmed.parse::<f64>().map_err(|_| {
        format!("{} is not a number between 0 and 2", if trimmed.is_empty() { "temperature" } else { trimmed })
    })?;
    if (0.0..=2.0).contains(&value) {
        Ok(value)
    } else {
        Err(format!("temperature must be between 0 and 2, not {trimmed}"))
    }
}

/// De `top_p`-marge: blanco is `None` ("stuur hem niet", de toestand van het label), een getal in
/// [0, 1] is de marge, en alles erbuiten of niet-getals is een fout die niet stil mag terugvallen.
fn parse_top_p(text: &str) -> Result<Option<f64>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let value = trimmed.parse::<f64>().map_err(|_| format!("top_p is not a number between 0 and 1: {trimmed}"))?;
    if (0.0..=1.0).contains(&value) {
        Ok(Some(value))
    } else {
        Err(format!("top_p must be between 0 and 1, not {trimmed}"))
    }
}

/// De parallelstand: blanco valt op de default van 1, en een getal ≥ 1 is de stand. De vloer is de
/// regel van `ErrMaxParallelTurns` ("1 or more"): nul beurten tegelijk is geen "serieel" maar "niets".
fn parse_parallel_turns(text: &str) -> Result<u32, String> {
    let trimmed = text.trim();
    // Blanco is een fout, geen stil-terugval-op-de-default: dezelfde regel als bij de lus-deksel,
    // want het vakje toont altijd een getal. De vloer is `ErrMaxParallelTurns` ("1 or more"): nul
    // beurten tegelijk is geen "serieel" maar "niets".
    let value = trimmed.parse::<u32>().map_err(|_| Texts::get(Key::ErrMaxParallelTurns).to_string())?;
    if value >= 1 {
        Ok(value)
    } else {
        Err(Texts::get(Key::ErrMaxParallelTurns).to_string())
    }
}

fn key_row(ui: &mut Ui, view: &mut SettingsView, boven: f32) {
    form_row_with_gap(ui, boven, Texts::get(Key::ApiKeyLabel), |ui| {
        ui.horizontal(|ui| {
            ui.add_sized(
                [ui.available_width(), 20.0],
                egui::TextEdit::singleline(&mut view.api_key).password(!view.reveal_key),
            )
            .on_hover_text(Texts::get(Key::TipApiKey));
            let toggle = if view.reveal_key { "hide" } else { "show" };
            if ui.small_button(toggle).clicked() {
                view.reveal_key = !view.reveal_key;
            }
        });
    });
    form_row(ui, Texts::get(Key::OrEnvVar), |ui| {
        ui.add_sized(
            [ui.available_width(), 20.0],
            egui::TextEdit::singleline(&mut view.api_key_env_var)
                .hint_text(baudet_settings::DEFAULT_KEY_ENV_VAR)
                .desired_width(f32::INFINITY),
        )
        .on_hover_text(Texts::get(Key::TipEnvVar));
    });
}

fn save_row(ui: &mut Ui, view: &mut SettingsView, commands: &mut Vec<SettingsCommand>) {
    // De knop staat los van het veld erboven: er sits al een `Separator` boven deze band, en zonder
    // eigen marge plakte de knop tegen zijn eigen scheiding.
    ui.add_space(lucht_voor_kader(ROW_GAP));
    let save = ui
        .add_enabled(!view.blocked, egui::Button::new(Texts::get(Key::SaveButton)))
        .on_hover_text(Texts::get(Key::TipSave))
        .clicked();
    if save {
        match view.draft() {
            Ok(settings) => {
                // De taal gaat mee met de opslag en niet met de klik op de kiezer:
                // `Main.kt:74-82` herbouwt de panelen alleen wanneer het persistentie-weerstand
                // verschilt van de taal op screen, en `TipSave` zegt het met woorden. Hier staat
                // daarom dezelfde zet als bij de sessie (`AppState.kt:228`).
                Texts::set_language(settings.language);
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

/// De groepscheiding van dit formulier: egui's eigen lijn, met lucht eromheen.
///
/// `ui.separator()` is exact deze widget met `spacing` op de stijlstandaard van 6 pt
/// (`SeparatorStyle::separator_style`: `spacing: 6.0`, stroke = `noninteractive().bg_stroke`), en de
/// lijn wordt in het midden van die ruimte getekend (`separator.rs:130-140`). Zes pt op een letter van
/// 12 pt is de helft van een regel: de lijn raakte de tekst erboven en eronder, en dat is het
/// "plakt"-beeld van de klacht. Dit is daarom dezelfde `Separator` met `spacing(GROUP_GAP)`, en geen
/// handgetekende `hline` — de lijn, de kleur en de breedte blijven die van het venster.
fn group_break(ui: &mut Ui) {
    ui.add(egui::Separator::default().spacing(GROUP_GAP));
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
    use super::{split_trusted_lines, Language, Reasoning, SettingsCommand, SettingsEvent, SettingsView, ToolMode};
    use baudet_settings::{ListingError, Model};
    use std::path::PathBuf;

    fn model(id: &str) -> Model {
        Model { id: id.to_string(), label: None, context_window: None }
    }

    /// Het contextbeleid overleeft het rondje view → bestand → view, en de twee getallen doen hun
    /// vloerwerk: een blanco budget is auto (0), blanco of 0 voor de teller is de vloer 1 (de
    /// origineler dwingt die in het panel af), en een ander woord voor het beleid is een item uit de
    /// kiezer en niet een woord uit het bestand.
    #[test]
    fn contextbeleid_rondje_en_vloeren() {
        use baudet_settings::config::ContextStrategy;
        let mut view = view();
        view.context_strategy = ContextStrategy::RollingWindow;
        view.context_budget = "48000".to_string();
        view.context_keep_recent = String::new();
        let drafted = view.draft().expect("een geldig contextbeleid mag niet worden geweigerd");
        assert_eq!(drafted.context_strategy, ContextStrategy::RollingWindow);
        assert_eq!(drafted.context_budget_tokens, 48000);
        assert_eq!(drafted.context_keep_recent_messages, 1, "blanco valt op de vloer van 1");

        // Een 0 in de teller blijft de vloer, en het budget mag expliciet 0 zijn.
        view.context_budget = "0".to_string();
        view.context_keep_recent = "0".to_string();
        let drafted = view.draft().expect("nul mag geen fout zijn");
        assert_eq!(drafted.context_budget_tokens, 0);
        assert_eq!(drafted.context_keep_recent_messages, 1);

        // Een niet-getal is een fout en valt niet stil terug.
        view.context_budget = "veel".to_string();
        assert!(view.draft().is_err(), "een woord als budget moet geweigerd worden");
    }

    /// De drie generatie-rijen doen hun werk in `draft`: temperatuur is een getal in [0, 2] en blanco
    /// is een fout (geen stil-terugval, zoals de lus-deksel); `top_p` is blanco-bewarend (blanco =
    /// `None` = "stuur hem niet", niet 1,0); de parallelstand heeft de vloer van 1 en witgeeft een
    /// witregel niet op de default. Dit staat hier en niet in `baudet-settings`: deze grenzen leven in
    /// de tekst-naar-waarde-vertaling van de panel, niet in het bestand.
    #[test]
    fn sampling_en_parallel_rondje_en_vloeren() {
        // Gelddrige invoer reist naar de gereserveerde waarde.
        let mut view = view();
        view.temperature = "0.3".to_string();
        view.top_p = "0.9".to_string();
        view.max_parallel_turns = "3".to_string();
        let drafted = view.draft().expect("een geldige sampling mag niet worden geweigerd");
        assert_eq!(drafted.temperature, 0.3);
        assert_eq!(drafted.top_p, Some(0.9));
        assert_eq!(drafted.max_parallel_turns, 3);

        // Blanco `top_p` is `None` en niet 1,0 of 0: dat is de betekenis van het label.
        view.top_p = String::new();
        let drafted = view.draft().expect("blanco top_p is een keuze en geen fout");
        assert_eq!(drafted.top_p, None, "blanco betekent niet sturen");

        // Blanco temperatuur en blanco parallelstand zijn fouten: geen stil-terugval-op-de-default.
        view.temperature = String::new();
        assert!(view.draft().is_err(), "blanco temperatuur mag niet stilletjes op 0,7 terugvallen");
        view.temperature = "0.7".to_string();
        view.max_parallel_turns = String::new();
        assert!(view.draft().is_err(), "blanco parallelstand mag niet stilletjes op 1 terugvallen");
        view.max_parallel_turns = "1".to_string();

        // Buiten het interval of niet-getals is een fout.
        view.temperature = "3".to_string();
        assert!(view.draft().is_err(), "temperatuur boven 2 is geen smakelijkheid maar een typefout");
        view.temperature = "0.7".to_string();
        view.top_p = "1.5".to_string();
        assert!(view.draft().is_err(), "top_p boven 1 moet geweigerd worden");
        view.top_p = String::new();
        view.max_parallel_turns = "0".to_string();
        assert!(view.draft().is_err(), "nul beurten tegelijk is geen 'serieel' maar 'niets'");
    }

    /// De zeven vinkjes reizen door `draft` als booleans, elk naar zijn eigen veld. De dragende
    /// eigenschap is dat `use_server_session` — dat eerder widget-loos in `stored_context` leefde —
    /// nu door de checkbox wordt gezet en niet meer vanuit de voorraad.
    #[test]
    fn de_zeven_vinkjes_reizen_naar_hun_eigen_veld() {
        let mut view = view();
        // Zet er drie aan/uit die de default tegengaan, laat de rest zoals hij is.
        view.use_responses_api = false;
        view.git_allow_writes = true;
        view.use_server_session = false;
        let drafted = view.draft().expect("vinkjes veranderen mogen `draft` niet breken");
        assert!(!drafted.use_responses_api);
        assert!(drafted.git_allow_writes, "git-schrijven aan komt als aan terug");
        assert!(!drafted.use_server_session, "de checkbox wint van de oude voorraad");
        // De defaults die hier niet zijn aangeraakt, blijven hun default.
        assert!(drafted.auto_approve_safe_commands);
        assert!(drafted.transcript_wrap);
        assert!(drafted.honesty_footer);
    }

    /// De systeemprompt reist als tekst, ongecontroleerd, en blanco blijft blanco. De eigenschap die
    /// hier telt, is dat `draft` de ledige snaar niet vult met de ingebouwde prompt: het bestand
    /// bewaart de **afwijking**, en `resolved_system_prompt` levert de rest ('t is de enige manier
    /// waarop een latere verandering van die ingebouwde tekst nog ergens doorwerkt).

    /// De twee vertrouwens-knoppen sturen de lijst en geen lege tekst: een ledig vak betekent een
    /// ledige lijst, en `draft` mag de bestaande lijst niet meenemen van wat er in het vak stond.
    /// Dat is de scheiding die `SettingsPanel.kt:286-292` tekent tussen bewerking en besluit.
    #[test]
    fn een_lijst_knop_neemt_het_besluit_en_nooit_de_halve_tekst() {
        let mut view = view();
        // Het vak bevat iets dat nog niet is toegepast; de voorraad draagt de vorige lijst.
        view.trusted_text = "mvn
git com"
            .into();
        let drafted = view.draft().expect("een half getypt gezin mag het formulier niet breken");
        assert_eq!(drafted.trusted_commands, view.stored_context.trusted_commands, "opslaan vertrouwt niets nieuw");

        let lines = split_trusted_lines("mvn\n\n  git commit  \n");
        assert_eq!(lines, vec!["mvn".to_string(), "git commit".to_string()], "elke regel getrimd");
    }

    #[test]
    fn een_ledige_systeemprompt_blijft_ledig_wat_de_ingebouwde_prompt_ook_wil() {
        let mut view = view();
        assert!(view.system_prompt.trim().is_empty(), "de fixture start zonder eigen prompt");
        assert!(view.draft().expect("blanco mag geen fout zijn").system_prompt.is_empty());

        view.system_prompt = "Focus on Swift.\nTweede regel.".into();
        assert_eq!(view.draft().unwrap().system_prompt, "Focus on Swift.\nTweede regel.");
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
            // Zelfde reden als `max_tool_iterations` hierboven: deze twee getallen kennen geen
            // blanco-toestand en `draft` valst er niet stil op terug. `top_p` mag wél blanco, want
            // daar is blanco de betekenis "stuur hem niet".
            temperature: baudet_settings::config::DEFAULT_TEMPERATURE.to_string(),
            max_parallel_turns: baudet_settings::DEFAULT_MAX_PARALLEL_TURNS.to_string(),
            // De afgeleide `Default` van de view zou de vinkjes op `false` zetten, maar een paneel
            // uit `load` krijgt ze uit het bestand of uit de domein-default — en die zijn niet
            // allemaal `false` (auto-approve, wrappen en honesty staan aan; git-schrijven uit). Deze
            // fixture boots het bestand na, dus ze spiegelen de domein-defaults, net zoals
            // `max_tool_iterations` hierboven dat doet. `git_allow_writes` en `use_responses_api`
            // hoeven niet: hun default is al `false`/`true` zoals de domein ze wil, en de fixture
            // test de overdracht, niet de default.
            use_responses_api: true,
            store_responses: true,
            use_server_session: true,
            auto_approve_safe_commands: true,
            git_allow_writes: false,
            transcript_wrap: true,
            honesty_footer: true,
            // `system_prompt` hoeft hier niet: ledig is de toestand die een paneel uit het bestand
            // ook heeft wanneer iemand nooit iets typte, en de vinkjes hierboven moesten wél omdat
            // hun domein-default niet allemaal `false` is.
            ..SettingsView::default()
        }
    }

    /// De taal overleeft het rondje bestand → view → opslag.
    ///
    /// Dit is de drager van de hele i18n-draad: als `draft()` het woord van de kiezer niet
    /// meedraagt, wordt elke opslag — en elk sluiten van het venster dat alleen de maten onthoudt —
    /// een terugval naar Engels die niemand koos.
    #[test]
    fn de_gekozen_taal_reist_mee_naar_het_bestand() {
        let mut panel = view();
        assert_eq!(panel.language, Language::En, "een paneel zonder bestand begins bij Engels");
        panel.language = Language::Nl;
        assert_eq!(panel.draft().unwrap().language, Language::Nl);
        // En terug: `adopt` is de kant waar het veld binnenkomt, en `geometry_draft` is de opslag
        // die niemand voor een taalwijziging maakt — die mag eenmaal gekozen taal niet wissen.
        panel.adopt(&panel.draft().unwrap());
        assert_eq!(panel.language, Language::Nl);
        let dividers = crate::layout::Dividers { left: 1, right: 2, files: 3 };
        assert_eq!(panel.geometry_draft(&dividers).unwrap().language, Language::Nl);
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
            assert!(
                problem.contains("not a positive number"),
                "witruimte en onzin moeten benoemd worden, niet gerepareerd: {problem}"
            );
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
