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

use baudet_i18n::Language;
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

/// Wat er gebeurt als een gesprek het venster van het model voorbije groeit.
///
/// Poort van `config/ContextStrategy.kt`. De knijp-logica zelf staat al in `baudet-store::window`
/// en de rekensommen eromheen in `baudet-store::budget`; dit is de keuze van de persoon, dus de
/// instelling-variant van de enum staat hier — dezelfde plek als [`ToolMode`] koos, om dezelfde
/// reden: de enum is de instelling, niet het algoritme.
///
/// De Dependitie-richting is de reden dat dit twee enums zijn in plaats van één hergebruikte:
/// `baudet-settings` hangt af van `baudet-store`, nooit omgekeerd. [`Settings::context_limits`] is
/// de ene plek waar ze worden samengebracht, en hij is totaal — elke variant aan beide kanten, dus
/// een nieuw lid mist niet stil.
///
/// De declaratievolgorde drijft ook hier de index van de settings-combo (`SettingsPanel.kt:341`),
/// dus die volgorde blijft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContextStrategy {
    /// Stuur alleen de laatste beurten: de nieuwste plus `context_keep_recent_messages` ervoor. Dit
    /// houdt een lokaal model bij het onderwerp, omdat het nooit een oud begin ziet met een ontbrekend
    /// midden — de vorm waardoor het terugdraaide naar een onderwerp dat het gesprek al achter zich
    /// had liet.
    #[default]
    RecentMessages,
    /// Vul het venster zo vol als het model dragen kan en rol het oudste van de voorkant af.
    RollingWindow,
    /// Knip niets: weiger een te grote beurt vóórdat hij gaat, zodat de persoon zelf compakte of een
    /// nieuw gesprek begint. De enige strategie waarbij een server-side keten de context mag dragen.
    StopAtLimit,
}

impl ContextStrategy {
    /// De kiezer-items in de claimvolgorde van de origineler (RecentMessages|RollingWindow|StopAtLimit),
    /// zodat de combopositie dezelfde is als in de Kotlin-panel.
    pub const ALL: [ContextStrategy; 3] =
        [ContextStrategy::RecentMessages, ContextStrategy::RollingWindow, ContextStrategy::StopAtLimit];

    /// De index in de keuzelijst, volgens de claimvolgorde van de origineler.
    pub fn index(self) -> usize {
        match self {
            ContextStrategy::RecentMessages => 0,
            ContextStrategy::RollingWindow => 1,
            ContextStrategy::StopAtLimit => 2,
        }
    }

    /// Het woord zoals de instelling er in staat.
    pub fn word(self) -> &'static str {
        match self {
            ContextStrategy::RecentMessages => "recent",
            ContextStrategy::RollingWindow => "rolling",
            ContextStrategy::StopAtLimit => "stop",
        }
    }

    /// De positie van een woord in de lijst, of `None` als het woord er niet in staat — een
    /// onbekend woord wordt geweigerd in plaats van stil op de default gezet.
    pub fn from_word(word: &str) -> Option<ContextStrategy> {
        match word.trim().to_ascii_lowercase().as_str() {
            "recent" => Some(ContextStrategy::RecentMessages),
            "rolling" => Some(ContextStrategy::RollingWindow),
            "stop" => Some(ContextStrategy::StopAtLimit),
            _ => None,
        }
    }

    /// Of een server-side keten onder deze strategie context mag dragen. Alleen `StopAtLimit`:
    /// knippen bewerkt wat we sturen en kan dus geen keten kleiner maken die de backend al houdt.
    /// (Zie `chainedPrev` in `AppState.kt:668`.)
    pub fn supports_server_session(self) -> bool {
        self == ContextStrategy::StopAtLimit
    }

    /// Of het venster bij beurten wordt begrensd in plaats van bij de budget van het model, dus of
    /// "houd N recente beurten" iets betekent.
    pub fn trims_by_turn_count(self) -> bool {
        self == ContextStrategy::RecentMessages
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
    /// De deksel op één gesprek, in tokens, waar een beurt tegen wordt afgetast.
    ///
    /// Poort van `AppConfig.kt:51` (`contextBudgetTokens`), default 0. Die 0 is een keuze en geen
    /// ontbrekende waarde: de app leidt het plafond dan af uit het venster van het model — knip op
    /// drie vierde daarvan, en weiger aan de rand ervan onder `StopAtLimit`. Een getyigd getal werkt
    /// als extra plafond erboven, nooit als vloer, dus een klein getal houdt een gesprek bewust goedkoop
    /// en een groot getal duwt een verzoek niet voorbij het model.
    pub context_budget_tokens: u32,
    /// Wat de app doet wanneer een gesprek die grens bereikt. Knippen is default, zoals in de
    /// origineler, zodat een lang gesprek gewoon doorgaat in plaats van geweigerd te worden.
    pub context_strategy: ContextStrategy,
    /// Hoeveel beurten vóór de huidige teruggaan bij `RecentMessages`. Het systeemvoorstel gaat altijd
    /// mee — het staat in de instructions en niet in dit venster — dus een kleine waarde verliest de
    /// staande instructies niet. Vloer 1, zoals de origineler die in het panel afdwingt; de teller
    /// wordt alleen gelezen door `RecentMessages` ([`ContextStrategy::trims_by_turn_count`]).
    pub context_keep_recent_messages: u32,
    /// Of `previous_response_id` mag mee. Uit voor backends die het veld alleen inslikken.
    ///
    /// Poort van `AppConfig.kt:76` (`useServerSession`), default aan. Inert onder een knippende
    /// strategie: daar speelt [`ContextStrategy::supports_server_session`] de rol.
    pub use_server_session: bool,
    /// Het contextvenster dat een weigering ons leerde, of 0 wanneer nooit geleerd.
    ///
    /// Dit is géén instelling: de persoon typt het nooit. Het bestaat omdat de app een beurt anders
    /// zou moeten af tasten tegen een grootte die niemand hem vertelde, en dat is precies hoe een
    /// gesprek naar een backend gaat die het alleen maar kan weigeren.
    pub learned_context_window: u32,
    /// De `base_url|model` waaronder het getal hierboven werd geleerd, zodat een model- of
    /// serverwissel geen stale getal achterlaat.
    pub learned_context_key: String,
    /// De breedte van de linkerbalk, zoals het venster hem bij het sluiten mat.
    ///
    /// Poort van `AppConfig.kt:152` (`var leftDivider: Int = 0`), en die 0 is de helft van de
    /// betekenis: hij zegt "nooit gemeten", niet "breedte nul". `layout::resolve_divider` laat een
    /// null dan op de fabrieksmaat van `Main.kt:164` vallen, en `worth_remembering` laat een
    /// ingeklapte balk het onthield heel laten — een inklapping mag nooit de heropende standaard
    /// worden.
    pub left_divider: i32,
    /// De breedte van het middenvlak plus de linkerbalk samen, dus de plek waar de bestandsboom
    /// begint (`AppConfig.kt:153`, `Main.kt:171`).
    pub right_divider: i32,
    /// De hoogte die de changes-lijst onder de bestandsboom krijgt (`AppConfig.kt:158`).
    ///
    /// De derde verdeelstand en dezelfde regel, met één verschil: hij loopt verticaal, dus zijn
    /// "voorbij de rand" wordt tegen de hoogte gemeten en niet tegen de breedte
    /// (`Main.kt:236-241`: `orientation == VERTICAL_SPLIT → split.height`).
    pub files_divider: i32,
    /// De taal van de interface.
    ///
    /// Poort van `AppConfig.kt:23-24` (`var language: String = "en"`), met één verschil: hier is het
    /// een `Language` in plaats van een frei woord. De opslag blijft kleinschrijfg (`en`, `nl`,
    /// `fr`, `de`) omdat de origineler dát in zijn bestand zet, en een bestand dat iets anders zegt,
    /// leest als Engels — precies de regel van `Texts.parse` (`Texts.kt:27-33`). Een latere build
    /// die een taal toevoegt, breekt dit bestand dus niet, en een ouder build dat dit veld niet
    /// kent, leest de rest gewoon.
    ///
    /// De herinnerde verdeelstanden staan hierboven en niet onder dit veld, en dat is geen volgorde
    /// uit gewoonte: `AppConfig.kt:151` zegt letterlijk dat de maten er zijn zodat "a rebuild, such
    /// as a language change, keeps the user layout". De taal wisselt het venster, de maten overleven
    /// die wisselt.
    ///
    /// Lezen gaat door `Language::parse` en niet door de afgeleide variantaam: die kent precies
    /// vier woorden en wijst alles af wat erbuiten staat, terwijl de origineler een vrij woord
    /// bewaart en het pas bij het lezen naar een taal vertaalt (`AppConfig.kt:24`, en
    /// `AppState.kt:195` -> `Texts.parse`). Zonder die poort breekt `"pools"` het hele bestand: de
    /// persoon is zijn endpoint, zijn model en zijn sleutel kwijt omdat één woord niet klopt.
    #[serde(deserialize_with = "parse_language")]
    pub language: Language,
}

/// De leespoort voor de taal: een woord naar `Language`, via dezelfde regel als de origineler.
///
/// `deserialize_with` is hier de enige vorm die kan werken: het bestand bevat een vrij woord,
/// `Language` kent er vier, en wat daartussen staat moet een taal worden in plaats van een fout.
/// Alleen een woord wordt aangenomen - `null` of een getal is geen taal maar een kapot veld, en
/// dat verschil is precies wat `"pools"` van een getal scheidt.
fn parse_language<'de, D>(deserializer: D) -> Result<Language, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let stored = <String as serde::Deserialize>::deserialize(deserializer)?;
    Ok(Language::parse(&stored))
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
            // 0 laat de app afleiden, wat het enige eerlijke antwoord is zolang het model niet bekend is.
            context_budget_tokens: 0,
            context_strategy: ContextStrategy::default(),
            context_keep_recent_messages: DEFAULT_KEEP_RECENT_MESSAGES,
            use_server_session: true,
            learned_context_window: 0,
            learned_context_key: String::new(),
            // Drie nullen, geen maten: "er is nog nooit een venster gesloten". De maten zelf leven
            // bij de panelen en bij `layout::resolve_divider`, want de origineler bewaart een
            // verdeelstand en niet haar eigen fabriekswaarde (`AppConfig.kt:152-158`).
            left_divider: 0,
            right_divider: 0,
            files_divider: 0,
            // Engels, zoals `AppConfig.kt:24`. Niet "de taal van deze machine": de origineler kiest
            // bewust een vaste default zodat een scherm dat iemand deelt, er overal gelijk uitziet.
            language: Language::default(),
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

/// Het aantal beurten dat `RecentMessages` teruggestuurt, uit `AppConfig.kt:64`. Een eigen naam
/// omdat de default hier, de settingskaart en de test die een oud bestand leest hetzelfde getal
/// moeten dragen.
pub const DEFAULT_KEEP_RECENT_MESSAGES: u32 = 15;

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

    /// The model this file names, or the empty string when it names none. An unnamed model has no
    /// window to look up, which is the same answer as an unknown one and needs no third state.
    pub fn model_name(&self) -> &str {
        self.model.as_deref().unwrap_or("")
    }

    /// The measurements the ceilings are computed from, in the shape `baudet-store` reasons over.
    ///
    /// This is the one place the two vocabularies meet, and it is a pure read: nothing here decides
    /// anything, it only hands the numbers over. `max_output_tokens` of `None` — the endpoint decides
    /// — becomes "reserve nothing", because reserving an invented number would shrink a window nobody
    /// asked to shrink.
    pub fn context_limits(&self) -> baudet_store::ContextLimits {
        baudet_store::ContextLimits {
            strategy: match self.context_strategy {
                ContextStrategy::RecentMessages => baudet_store::ContextStrategy::RecentMessages,
                ContextStrategy::RollingWindow => baudet_store::ContextStrategy::RollingWindow,
                ContextStrategy::StopAtLimit => baudet_store::ContextStrategy::StopAtLimit,
            },
            budget_tokens: self.context_budget_tokens,
            max_output_tokens: self.max_output_tokens.unwrap_or(0),
            learned_context_window: self.learned_context_window,
            learned_context_key: self.learned_context_key.clone(),
            keep_recent_messages: usize::try_from(self.context_keep_recent_messages.max(1)).unwrap_or(usize::MAX),
            model: self.model_name().to_string(),
            base_url: self.base_url.clone(),
        }
    }

    /// Leer het venster dat een weigering noemde, wanneer het er een noemde.
    ///
    /// `AppState.kt:681`. Direct persisten is de helft van de betekenis: het getal is precies zoveel
    /// waard na een herstart als in de volgende beurt, en verliezen betekent opnieuw op dezelfde manier
    /// falen. Dit verhoogt nooit wat de persoon zette — een typed budget verlaagt alleen
    /// ([`baudet_store::ContextLimits::capped_by_budget`]).
    pub fn learn_context_window(&mut self, claimed: Option<u32>, base_url: &str) {
        let Some(claimed) = claimed.filter(|claimed| *claimed > 0) else { return };
        let key = baudet_store::window_key(base_url, self.model_name());
        if self.learned_context_window == claimed && self.learned_context_key == key {
            return;
        }
        self.learned_context_window = claimed;
        self.learned_context_key = key;
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
    fn de_taal_is_engels_en_overleeft_het_bestand() {
        // De default van de origineler (`AppConfig.kt:24`), en het woord zoals hij het op schijf
        // zet: kleinschrijft, niet `En` en niet een object. De schrijfwijze is het contract met het
        // bestand van de JVM-app, dus een afgeleide serde-naam die `Nl` of `nl` maar één letter
        // verschilt, maakt van een gedeeld profiel twee half lege bestanden.
        assert_eq!(Settings::default().language, Language::En);
        let json = serde_json::to_string(&Settings::default()).unwrap();
        assert!(json.contains("\"language\":\"en\""), "het bestand noemt het veld zoals het heet: {json}");

        let mut settings = Settings::default();
        settings.language = Language::Nl;
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"language\":\"nl\""), "{json}");
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.language, Language::Nl);
        assert_eq!(back, settings);
    }

    #[test]
    fn een_older_bestand_zonder_taal_leest_als_engels() {
        // Hetzelfde bewijs als bij `tool_mode`: een bestand van vóór dit veld moet gewoon openen.
        // Hier valt niets verkeerd te kiezen — Engels is de enige uitkomst — maar wat wel kan
        // misgaan, is dat het hele bestand geweigerd wordt omdat er een veld ontbreekt. Dat is de
        // eigendom van `#[serde(default)]` op de struct, en dit is het moment waarop die wordt
        // aangesproken voor een taal die er toe doet.
        let json = r#"{"base_url":"http://studio:8000/v1","model":"gemma","tool_mode":"ask"}"#;
        let read: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(read.language, Language::En);
        assert_eq!(read.model.as_deref(), Some("gemma"));
    }

    #[test]
    fn een_onbekende_taal_in_het_bestand_valt_terug_naar_engels() {
        // De regel van `Texts.parse` (`Texts.kt:27-33`), hier op de plek waar het bestand naar
        // `Language` wordt vertaald. Met de hand aangepast of half afgeschreven profiel tekst mag
        // het venster niet breken; wel mag het wél leesbaar blijven, dus de rest van het bestand
        // blijft staan.
        let json = r#"{"base_url":"http://studio:8000/v1","model":"gemma","language":"pools"}"#;
        let read: Settings = serde_json::from_str(json).expect("een onbekend taalwoord mag geen kapot bestand maken");
        assert_eq!(read.language, Language::En);
        assert_eq!(read.model.as_deref(), Some("gemma"), "de rest van het profiel blijft staan");
        // Ruimte en hoofdletter maken geen verschil, want de origineler trimt en kleint vóór de
        // keus (`Texts.kt:27`). Vanaf de schijf geldt dezelfde regel.
        let spaced: Settings =
            serde_json::from_str(r#"{"language":" NL "}"#).expect("hetzelfde woord met randen is hetzelfde woord");
        assert_eq!(spaced.language, Language::Nl);
        // Een veld dat géén woord is, is wél kapot: dat is geen taalkeuze van iemand, dat is een
        // bestand dat niets meer voorstelt, en `store.rs` moet het als schade kunnen melden.
        let wrong_kind: Result<Settings, _> = serde_json::from_str(r#"{"language":7}"#);
        assert!(wrong_kind.is_err(), "een getal is geen taal");
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
    fn the_context_defaults_are_the_reference_ones_and_survive_the_file() {
        // De defaults van `AppConfig.kt:51-76`: knippende strategie, 15 beurten terug, niets
        // getygd, niets geleerd. Een eigen bewijs omdat deze vijf velden samen één beleid vormen —
        // wie ze afzonderlijk test, mist dat `RecentMessages` en 15 samen horen.
        let settings = Settings::default();
        assert_eq!(settings.context_strategy, ContextStrategy::RecentMessages);
        assert_eq!(settings.context_keep_recent_messages, DEFAULT_KEEP_RECENT_MESSAGES);
        assert_eq!(settings.context_budget_tokens, 0, "0 betekent: de app leidt het af");
        assert!(settings.use_server_session);
        assert_eq!(settings.learned_context_window, 0, "een venster wordt geleerd, niet ingetypt");
        assert!(settings.learned_context_key.is_empty());

        let written = Settings {
            context_budget_tokens: 8_000,
            context_strategy: ContextStrategy::StopAtLimit,
            context_keep_recent_messages: 3,
            use_server_session: false,
            learned_context_window: 12_288,
            learned_context_key: "http://a:8000/v1|m".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&written).unwrap();
        assert!(json.contains("\"context_strategy\":\"stopatlimit\""), "het bestand noemt het veld: {json}");
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, written);
    }

    #[test]
    fn de_verdeelstanden_zijn_eerst_niets_en_daarna_hun_getal() {
        // De drie nullen van `AppConfig.kt:152-158` zijn een toestand en geen maat. Een test die
        // alleen `Default::default()` naspeelt, bewijst dat een constructor nul typt; deze bewijst
        // wat de app ermee doet: een vers bestand laat `resolve_divider` op de fabrieksmaat vallen,
        // en een gemeten stand overleeft het bestand.
        let fresh = Settings::default();
        assert_eq!((fresh.left_divider, fresh.right_divider, fresh.files_divider), (0, 0, 0));
        assert_eq!(
            baudet_ui_divider_is_not_invented(fresh.left_divider, 280.0),
            280.0,
            "een null in het bestand is geen breedte"
        );

        let measured = Settings { left_divider: 311, right_divider: 940, files_divider: 168, ..Default::default() };
        let json = serde_json::to_string(&measured).unwrap();
        assert!(json.contains("\"left_divider\":311"), "het bestand noemt de stands: {json}");
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, measured);
    }

    /// Het fragment van `WindowLayout.resolveDivider` dat hier thuishoort: zonder vorige waarneming
    /// is een null "nooit gemeten". De hele functie staat in `baudet-ui::layout`; dit is de enige
    /// regel die de opslag zelf aangaat, en daarom geen kopie van die module.
    fn baudet_ui_divider_is_not_invented(persisted: i32, fallback: f32) -> f32 {
        if persisted > 0 {
            persisted as f32
        } else {
            fallback
        }
    }

    #[test]
    fn an_older_file_without_the_context_fields_still_opens() {
        // Forward-compatibiliteit voor deze velden: een bestand van vóór het contextbeleid moet
        // openen met knippen als strategie en niet op `StopAtLimit` vallen — dat zou elk lang
        // gesprek weigeren dat eerst gewoon doorging.
        let json = r#"{"base_url":"http://studio:8000/v1","model":"gemma-3-4b-it"}"#;
        let read: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(read.context_strategy, ContextStrategy::RecentMessages);
        assert_eq!(read.context_keep_recent_messages, DEFAULT_KEEP_RECENT_MESSAGES);
        assert_eq!(read.learned_context_window, 0);
        // De drie verdeelstanden kwamen later dan dit bestand en moeten afwezig kunnen zijn; een
        // afwezig stand telt als "nooit gemeten", precies zoals een null in een vers bestand.
        assert_eq!((read.left_divider, read.right_divider, read.files_divider), (0, 0, 0));
    }

    #[test]
    fn context_strategy_words_and_positions_match_the_reference_list() {
        let strategies =
            [ContextStrategy::RecentMessages, ContextStrategy::RollingWindow, ContextStrategy::StopAtLimit];
        for (expected, strategy) in strategies.iter().copied().enumerate() {
            assert_eq!(strategy.index(), expected);
            assert_eq!(ContextStrategy::from_word(strategy.word()), Some(strategy));
        }
        assert_eq!(ContextStrategy::from_word("lastFew"), None);
        // Alleen `StopAtLimit` laat een serverketen toe, en alleen `RecentMessages` leest de
        // beurtteller; die twee regels beslissen wat het panel grijs zet.
        assert!(ContextStrategy::StopAtLimit.supports_server_session());
        assert!(!ContextStrategy::RollingWindow.supports_server_session());
        assert!(ContextStrategy::RecentMessages.trims_by_turn_count());
        assert!(!ContextStrategy::RollingWindow.trims_by_turn_count());
    }

    #[test]
    fn the_settings_hand_the_store_one_reading_of_every_number() {
        // De brug naar `baudet-store` is de plek waar de twee talen mekaar raken; hij mag niets
        // beslissen behalve wat `None` betekent.
        let settings = Settings {
            base_url: "http://a:8000/v1".into(),
            model: Some("gemma-3-4b-it".into()),
            max_output_tokens: Some(4_000),
            context_budget_tokens: 8_000,
            context_strategy: ContextStrategy::RollingWindow,
            context_keep_recent_messages: 6,
            learned_context_window: 16_384,
            learned_context_key: "http://a:8000/v1|gemma-3-4b-it".into(),
            ..Default::default()
        };
        let limits = settings.context_limits();
        assert_eq!(limits.strategy, baudet_store::ContextStrategy::RollingWindow);
        assert_eq!(limits.budget_tokens, 8_000);
        assert_eq!(limits.max_output_tokens, 4_000);
        assert_eq!(limits.keep_recent_messages, 6);
        assert_eq!(limits.model, "gemma-3-4b-it");
        assert_eq!(limits.base_url, "http://a:8000/v1");
        // 16 384 is geleerd onder deze sleutel, dus het wint van de tabel: het plafond is
        // min(8 000, 16 384 - 4 096) = 8 000.
        assert_eq!(limits.hard_ceiling(), 8_000);

        // `max_output_tokens: None` wordt "reserveer niets", niet "reserveer een getal". De brug
        // mag er geen getal van maken, want "niets reserveren" is een keuze en "een getal" een ander.
        let bare = Settings { model: Some("gemma-3-4b-it".into()), ..Default::default() };
        assert_eq!(bare.context_limits().max_output_tokens, 0);

        // Een bekend venster geeft altijd een plafond, ook zonder ingetypt budget: de herkomst is
        // het venster zelf, minus de marge en minus de vaste overhead. "Geen budget" betekent bij
        // een bekend model dus niet "uit", alleen "het app-besluit".
        let limits = bare.context_limits();
        // De getallen eronder staan als som, niet als letterlijke 28 672 / 26 672, omdat de regel die
        // eronder hoort juist is: het venster minus de marge, en voor de lus nog minus de overhead.
        let window = baudet_core::known_context_window("gemma-3-4b-it").unwrap();
        assert_eq!(window, 32_768);
        assert_eq!(limits.hard_ceiling(), window - baudet_store::WINDOW_MARGIN_TOKENS);
        assert_eq!(
            limits.context_ceiling(),
            u64::from(window)
                - u64::from(baudet_store::WINDOW_MARGIN_TOKENS)
                - u64::from(baudet_store::INPUT_OVERHEAD_TOKENS)
        );

        // Uit is uit op een model dat niemand kent: zonder venster én zonder budget wordt er niets
        // verzonnen, en dat is het enige geval waarin het plafond 0 is.
        let blind = Settings { model: Some("helemaal-niets-bekends".into()), ..Default::default() };
        assert_eq!(blind.context_limits().hard_ceiling(), 0);
        assert_eq!(blind.context_limits().context_ceiling(), 0, "geen venster en geen budget, dus uit");
    }

    #[test]
    fn a_zero_turn_count_is_read_as_one_and_not_as_send_nothing() {
        // De vloer van de origineler (`SettingsPanel.kt` wijst 0 af) leeft hier bij het lezen: het
        // bestand zelf repareert niet — dat zou een waarde verzwijgen die erin stond — maar de lus
        // mag nooit met een lege context gaan staan.
        let settings = Settings { context_keep_recent_messages: 0, ..Default::default() };
        assert_eq!(settings.context_limits().keep_recent_messages, 1);
    }

    #[test]
    fn a_learned_window_lands_under_the_identity_and_never_rises_again() {
        let mut settings =
            Settings { base_url: "http://a:8000/v1".into(), model: Some("gemma-3-4b-it".into()), ..Default::default() };
        // Geen getal is niets te leren: een weigering zonder cijfer laat het beleid heel.
        settings.learn_context_window(None, "http://a:8000/v1");
        assert_eq!(settings.learned_context_window, 0);
        settings.learn_context_window(Some(0), "http://a:8000/v1");
        assert_eq!(settings.learned_context_window, 0);

        settings.learn_context_window(Some(12_288), "http://a:8000/v1");
        assert_eq!(settings.learned_context_window, 12_288);
        assert_eq!(settings.learned_context_key, "http://a:8000/v1|gemma-3-4b-it");

        // Idem onder dezelfde sleutel is geen wijziging en schrijft het bestand niet nieuw.
        let before = settings.clone();
        settings.learn_context_window(Some(12_288), "http://a:8000/v1");
        assert_eq!(settings, before);

        // Een aantal onder een andere sleutel is hier niet geleerd: de sleutel van het gesprek
        // volgt de base url van de instellingen, dus het geleerde getal Verdwijnt eronder en valt
        // het beleid terug op de tabel. Daarvoor zijn de twee apart gezet.
        settings.learn_context_window(Some(4_096), "http://b:8000/v1");
        assert_eq!(settings.learned_context_key, "http://b:8000/v1|gemma-3-4b-it");
        assert_eq!(
            settings.context_limits().context_window(),
            Some(32_768),
            "een getal dat op een andere server leerde, geld hier niet"
        );
        // Op die andere server gaat het getal wél terug wonnen.
        settings.base_url = "http://b:8000/v1".into();
        assert_eq!(settings.context_limits().context_window(), Some(4_096));
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
