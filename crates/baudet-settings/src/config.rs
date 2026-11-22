//! The choices a person makes, and the file that remembers them.
//!
//! Kotlin keeps these in `SettingsStore`, which reads a JSON file and falls back to defaults. The
//! fallback is where a small app quietly becomes hostile, so it is the rule this file is written
//! around: **a file that cannot be read is never rewritten.** A corrupt, half-written or
//! future-version file must cost the person one bad start, not every model and endpoint they ever
//! configured. The store layer already follows the same discipline at the line level
//! ([`crate`](crate) ignores what it cannot decode); this is the same instinct one level up.
//!
//! The format is JSON rather than TOML for one concrete reason: `serde_json` is already in the
//! graph and `toml` is not, and nothing here needs TOML's syntax. A settings file the person is
//! never promised to hand-edit does not earn a new dependency.

use serde::{Deserialize, Serialize};

/// When the assistant may use tools during a turn.
///
/// Poort van `config/ToolMode.kt`, including de volgorde van de claim: in de origineler drijft die
/// volgorde de index van de Settings-combo (`SettingsPanel.kt:263,371`), dus die blijft Off|Ask|Auto.
///
/// De drie betekenissen staan zo ook in de origineler (`AppState.kt:739,951`): `Off` haalt de lus
/// helemaal uit spel via `ToolGate.Disabled`, `Ask` legt elke schrijfwijziging apart ter
/// goedkeuring voor, en `Auto` schrijft direct. Een commando vraagt overigens altijd, ook bij
/// `Auto`, want een subproces valt niet binnen de mapgrens te vangen (`AppState.kt:958-965`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolMode {
    /// Geen enkele beurt bereikt de tool-lus.
    Off,
    /// Toon elke wijziging ter goedkeuring. De default van de origineler: schrijven in een
    /// projectmap gebeurt niet achter de rug van de persoon om bij een verse installatie
    /// (`AppConfig.kt:82-86`).
    #[default]
    Ask,
    /// Schrijf zonder vragen.
    Auto,
}

impl ToolMode {
    /// De index in de keuzelijst, volgens de claimvolgorde van de origineler. `word` is de
    /// settings-combo die zijn positie uit de declaratievolgorde haalt (`SettingsPanel.kt:263`).
    pub fn index(self) -> usize {
        match self {
            ToolMode::Off => 0,
            ToolMode::Ask => 1,
            ToolMode::Auto => 2,
        }
    }

    /// Het woord zoals de instelling er in staat, ook voor een keuzelijst die zijn eigen
    /// vertaalde teksten al kent (item H). De JSON-catalogus gebruikt exact deze schrijfwijze.
    pub fn word(self) -> &'static str {
        match self {
            ToolMode::Off => "off",
            ToolMode::Ask => "ask",
            ToolMode::Auto => "auto",
        }
    }

    /// De positie van een woord in de lijst, of `None` als het woord er niet in staat. Het woord
    /// komt van buiten (een bestand of een keuzelijst), dus onbekende tekst wordt hier geweigerd
    /// in plaats van stil op `Ask` gezet — een stil gerepareerde instelling is onleesbaar.
    pub fn from_word(word: &str) -> Option<ToolMode> {
        match word.trim().to_ascii_lowercase().as_str() {
            "off" => Some(ToolMode::Off),
            "ask" => Some(ToolMode::Ask),
            "auto" => Some(ToolMode::Auto),
            _ => None,
        }
    }
}

/// Everything the app remembers between starts.
///
/// Every field is `#[serde(default)]`-tolerant on read and skips nothing on write, so a newer
/// build adds a field by adding a default, and an older build reading this file keeps the fields
/// it knows. That is the same forward-compatibility rule the rollout records follow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The endpoint a turn is sent to. One base URL, because the seam takes one
    /// ([`crate::provider::Provider::from_base_url`]) and inventing a provider list before a second
    /// provider exists is the speculative abstraction this rewrite avoids.
    pub base_url: String,
    /// The model to send. `None` means "whatever the endpoint lists first", which is what a person
    /// who has never opened the settings panel actually wants: a first start should work.
    pub model: Option<String>,
    /// The reasoning level to ask for. Kept as text rather than an enum, because the honest values
    /// belong to the model — `off` through `max` is what the endpoint accepts, and a newer model
    /// with a new level must not look like a corrupt file.
    pub reasoning: Option<String>,
    /// The ceiling one turn may spend, carried to the backend on
    /// [`baudet_backend::SendRequest::max_output_tokens`]. `None` leaves the decision to the
    /// endpoint rather than inventing a budget here.
    pub max_output_tokens: Option<u32>,
    /// The credential as the person typed it. `None` means "not typed", which is not the same as
    /// "none": [`Settings::resolved_key`] still looks at the environment, and that is what lets a
    /// checkout work without putting a secret in a file at all.
    ///
    /// Kept in the file because the whole store layer is flat text in a folder the person can
    /// `cat`; a keychain is right for a shipped app and wrong for a tool this one is. The
    /// mitigation actually taken is the file's own mode — `0600`, set on the descriptor before the
    /// bytes exist. See [`crate::store`].
    pub api_key: Option<String>,
    /// Which environment variable answers for a key that was never typed. Named rather than fixed
    /// so a person with several local servers does not have to choose between their shell and the
    /// app.
    pub api_key_env_var: String,
    /// The wire dialect to send turns to: `responses` or `chat_completions`. Text rather than an
    /// enum for the same reason `reasoning` is: a newer server may answer a dialect this build has
    /// no name for, and that must read as an unknown word, not as a corrupt file.
    ///
    /// `None` means "not chosen", and the absence is load-bearing: a turn is only sent to a server
    /// whose dialect is known, so an unset dialect keeps the window on its scripted player instead
    /// of letting it guess a dialect against a server that may not speak it.
    pub protocol: Option<String>,
    /// Wanneer de assistent tools mag gebruiken. Standaard `Ask`, zoals in de origineler: de lus
    /// staat aan, maar elke schrijfwijziging vraagt eerst om goedkeuring.
    ///
    /// Het veld heet hier `tool_mode` en in de origineler `toolMode`, omdat de JSON hier de
    /// Rust-veljnaam woord-voor-woord herhaalt. De waarden blijven smallcase, zoals `protocol` dat ook is.
    pub tool_mode: ToolMode,
    /// Het aantal model-tussenpozen dat één beurt mag kosten: de deksel op een doorlopende lus.
    ///
    /// Poort van `config/AppConfig.kt:88`, default 8. De grens is de enige verdediging tegen een
    /// model dat om een call blijft vragen: [`baudet_tool::ToolLoop`] telt zijn call-stappen en
    /// kapt bij dit getal af. De vloer is 1 — `ToolLoop.kt:149` doet `coerceAtLeast(1)` en dezelfde
    /// vloer staat daar in `ToolLoop::with_extractor` — want een budget van nul zou een draai
    /// zonder énkel verzoek maken, en dat is nooit wat "begrens dit" betekent.
    ///
    /// De herkenning van de vloer staat hier en niet alleen in de lus: het veld mag geen waarde
    /// dragen die het bestand in leest als iets anders dan de app ernaar handelt.
    pub max_tool_iterations: u32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            // The endpoint the JVM reference defaults to, so a checkout of this rewrite starts
            // against the same local server the original does.
            base_url: "http://127.0.0.1:8000/v1".to_string(),
            model: None,
            reasoning: None,
            max_output_tokens: None,
            api_key: None,
            // The variable this machine's local server actually uses, which is why it is the default
            // rather than a generic `BAUDET_API_KEY`.
            api_key_env_var: DEFAULT_KEY_ENV_VAR.to_string(),
            // Nothing is known about the server behind a default endpoint, so nothing is claimed.
            protocol: None,
            // Ask by default, for the reason on the field: a fresh install must not write to a
            // project folder behind the person's back.
            tool_mode: ToolMode::default(),
            // De default van de origineler (`AppConfig.kt:88`), geen eigen voorkeur: een checkout
            // van deze herschrijving moet dezelfde lus-limiet hebben als het programma dat als
            // voorbeeld diende.
            max_tool_iterations: DEFAULT_MAX_TOOL_ITERATIONS,
        }
    }
}

/// The variable to fall back to when the field above names nothing. Kept in one place because the
/// reference app repeats the literal in two spots, and a second copy is how the two drift.
pub const DEFAULT_KEY_ENV_VAR: &str = "OMLX_API_KEY";

/// De standaarddeksel op de tool-lus, uit `AppConfig.kt:88`. Een eigen naam omdat drie kanten —
/// de default hier, de instellingskaart die hem toont en de test die een oud bestand leest —
/// hetzelfde getal moeten dragen; een tweede letterlijke 8 is hoe ze uit elkaar gaan lopen.
pub const DEFAULT_MAX_TOOL_ITERATIONS: u32 = 8;

impl Settings {
    /// The credential to actually send: what was typed, otherwise what the named variable holds.
    ///
    /// Typed wins, which matters more than it looks: a person who types a key into a panel expects
    /// that key to be used, and an environment inherited from an editor's shell would otherwise
    /// silently answer with a different credential than the one on screen.
    ///
    /// A blank name falls back to [`DEFAULT_KEY_ENV_VAR`] rather than to "read no variable",
    /// because an empty field reads as "I did not fill this in", not as "opt out".
    pub fn resolved_key(&self) -> Option<String> {
        if let Some(typed) = self.api_key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
            return Some(typed.to_string());
        }
        let name = self.api_key_env_var.trim();
        let name = if name.is_empty() { DEFAULT_KEY_ENV_VAR } else { name };
        std::env::var(name).ok().filter(|value| !value.trim().is_empty())
    }

    /// Whether anything at all answers for a credential, without saying what it is.
    pub fn has_key(&self) -> bool {
        self.resolved_key().is_some()
    }

    /// The wire dialect the file names, in the words the transport uses, or `None` when the file
    /// names none. Untrusted words stay `None` for the reason stated on the field.
    pub fn protocol_word(&self) -> Option<&str> {
        self.protocol.as_deref().map(str::trim).filter(|word| !word.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_file_is_valid_and_names_the_local_endpoint() {
        let json = serde_json::to_string(&Settings::default()).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Settings::default());
        // The reference app's own default, so this checkout starts against the same server.
        assert_eq!(back.base_url, "http://127.0.0.1:8000/v1");
    }

    #[test]
    fn tool_mode_waits_for_approval_by_default_and_survives_the_file() {
        // De default van de origineler (`AppConfig.kt:86`), en de reden ervoor: een verse
        // installatie mag niet zelfstandig in een projectmap schrijven.
        assert_eq!(Settings::default().tool_mode, ToolMode::Ask);
        assert_eq!(ToolMode::default(), ToolMode::Ask);

        let json = serde_json::to_string(&Settings::default()).unwrap();
        assert!(json.contains("\"tool_mode\":\"ask\""), "het bestand noemt het veld zoals het heet: {json}");
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Settings::default());
    }

    #[test]
    fn an_older_file_without_tool_mode_still_reads_as_ask() {
        // Forward-compatibiliteit voor dit veld specifiek: een bestand van vóór de tool-lus moet
        // gewoon openen met de veilige default, en mag niet op `Off` vallen — dat zou de lus
        // stil uitzetten. Het bekende `an_unknown_field...`-bewijs dekt de regel, dit dekt het veld.
        let json = r#"{"base_url":"http://studio:8000/v1","model":"gemma"}"#;
        let read: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(read.tool_mode, ToolMode::Ask);
    }

    #[test]
    fn tool_mode_words_and_positions_match_the_reference_list() {
        // De claimvolgorde van `ToolMode.kt` drijft de combopositie in de origineler; die mag niet
        // stilletjes omwisselen als hier later een variant bijkomt.
        let modes = [ToolMode::Off, ToolMode::Ask, ToolMode::Auto];
        for (expected, mode) in modes.iter().copied().enumerate() {
            assert_eq!(mode.index(), expected);
            assert_eq!(ToolMode::from_word(mode.word()), Some(mode));
        }
        // Een woord van buiten is geen garantie: `ToolsOn` is niet hetzelfde woord als de
        // settings-lijst van de origineler en wordt geweigerd in plaats van gerepareerd.
        assert_eq!(ToolMode::from_word("ToolsOn"), None);
        assert_eq!(ToolMode::from_word(" ASK "), Some(ToolMode::Ask), "spaties en kas zijn geen betekenis");
    }

    #[test]
    fn the_iteration_cap_defaults_to_eight_and_survives_the_file() {
        // De deksel van de origineler (`AppConfig.kt:88`), en hij moet het bestand halen: een lus
        // die zijn limiet niet bewaart, begint elke start opnieuw met 8 terwijl de persoon er 3
        // koos.
        assert_eq!(Settings::default().max_tool_iterations, DEFAULT_MAX_TOOL_ITERATIONS);
        assert_eq!(DEFAULT_MAX_TOOL_ITERATIONS, 8, "de origineler begint op 8");

        let settings = Settings { max_tool_iterations: 3, ..Default::default() };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"max_tool_iterations\":3"), "het bestand noemt het veld zoals het heet: {json}");
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.max_tool_iterations, 3);
    }

    #[test]
    fn an_older_file_without_the_cap_still_reads_as_eight() {
        // Forward-compatibiliteit voor dit veld, zoals `tool_mode` dat heeft: een bestand van vóór
        // de tool-lus moet openen met de limiet van de origineler en mag niet op 0 of 1 vallen —
        // een lus die nul tussenpozen toestaat, doet nooit wat de persoon bedoelde.
        let json = r#"{"base_url":"http://studio:8000/v1","model":"gemma"}"#;
        let read: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(read.max_tool_iterations, DEFAULT_MAX_TOOL_ITERATIONS);
    }

    #[test]
    fn a_cap_of_zero_survives_the_file_and_is_the_loops_problem_to_floor() {
        // Twee eigenschappen, twee eigenaars, en de grens ertussen is geen versiersel: dit bestand
        // bewaart wat erin stond — 0 blijft 0 — en repareert niets stilletjes, zoals het een
        // onbekend `tool_mode`-woord ook niet repareert. De vloer van `ToolLoop.kt:149`
        // (`coerceAtLeast(1)`) hoeft hier niet bewezen te worden, want de lus doet die zelf al en
        // meet dat in zijn eigen crate; een tweede bewijsplaats zou verbergen als die vloer ooit
        // verdwijnt. Wat wél van hier komt: een waarde van 0 wordt doorgegeven, niet verzwegen.
        let settings = Settings { max_tool_iterations: 0, ..Default::default() };
        let json = serde_json::to_string(&settings).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.max_tool_iterations, 0, "een 0 uit de hand blijft een 0, tot bij de lus");
    }

    #[test]
    fn a_typed_key_wins_and_a_blank_one_falls_back_to_the_named_variable() {
        // Process-global, so this touches one variable and puts it back on every path — the same
        // discipline `baudet-store` documents for `BAUDET_HOME`.
        let name = "BAUDET_SETTINGS_TEST_KEY";
        std::env::set_var(name, "from-the-environment");
        let from_env = Settings { api_key: None, api_key_env_var: name.into(), ..Default::default() };
        assert_eq!(from_env.resolved_key().as_deref(), Some("from-the-environment"));
        assert!(from_env.has_key());

        let typed = Settings { api_key: Some("typed-wins".into()), api_key_env_var: name.into(), ..Default::default() };
        assert_eq!(typed.resolved_key().as_deref(), Some("typed-wins"), "the key on screen must be the key sent");

        // A blank name is "not filled in", so the default name still answers. Proved by setting
        // that name rather than by assuming it is unset: `OMLX_API_KEY` genuinely exists in the
        // shells this runs in, and an assertion of `None` there was the test being wrong, not the
        // code.
        std::env::set_var(DEFAULT_KEY_ENV_VAR, "from-the-default-name");
        let blank_name = Settings { api_key: None, api_key_env_var: "   ".into(), ..Default::default() };
        assert_eq!(blank_name.api_key_env_var.trim(), "");
        assert_eq!(
            blank_name.resolved_key().as_deref(),
            Some("from-the-default-name"),
            "a blank name means I did not fill this in, not do not read any variable"
        );
        std::env::remove_var(DEFAULT_KEY_ENV_VAR);

        // Whitespace-only is not a credential.
        std::env::set_var(name, "   ");
        assert_eq!(from_env.resolved_key(), None);
        assert!(!from_env.has_key());
        std::env::remove_var(name);
    }

    #[test]
    fn an_unknown_field_survives_and_a_missing_one_default() {
        // The forward-compatibility rule in one assertion: next week's field must not make this
        // build refuse last week's file, and this build's new field must not make it lose the old.
        let json = r#"{"base_url":"http://studio:8000/v1","a_field_from_next_week":42}"#;
        let read: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(read.base_url, "http://studio:8000/v1");
        assert_eq!(read.model, None);
        assert_eq!(read, Settings { base_url: "http://studio:8000/v1".into(), ..Default::default() });
    }
}
