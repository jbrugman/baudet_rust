
use baudet_i18n::Language;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolMode {
    Off,
    #[default]
    Ask,
    Auto,
}

impl ToolMode {
    pub fn index(self) -> usize {
        match self {
            ToolMode::Off => 0,
            ToolMode::Ask => 1,
            ToolMode::Auto => 2,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            ToolMode::Off => "off",
            ToolMode::Ask => "ask",
            ToolMode::Auto => "auto",
        }
    }

    pub fn from_word(word: &str) -> Option<ToolMode> {
        match word.trim().to_ascii_lowercase().as_str() {
            "off" => Some(ToolMode::Off),
            "ask" => Some(ToolMode::Ask),
            "auto" => Some(ToolMode::Auto),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContextStrategy {
    #[default]
    RecentMessages,
    RollingWindow,
    StopAtLimit,
}

impl ContextStrategy {
    pub const ALL: [ContextStrategy; 3] =
        [ContextStrategy::RecentMessages, ContextStrategy::RollingWindow, ContextStrategy::StopAtLimit];

    pub fn index(self) -> usize {
        match self {
            ContextStrategy::RecentMessages => 0,
            ContextStrategy::RollingWindow => 1,
            ContextStrategy::StopAtLimit => 2,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            ContextStrategy::RecentMessages => "recent",
            ContextStrategy::RollingWindow => "rolling",
            ContextStrategy::StopAtLimit => "stop",
        }
    }

    pub fn from_word(word: &str) -> Option<ContextStrategy> {
        match word.trim().to_ascii_lowercase().as_str() {
            "recent" => Some(ContextStrategy::RecentMessages),
            "rolling" => Some(ContextStrategy::RollingWindow),
            "stop" => Some(ContextStrategy::StopAtLimit),
            _ => None,
        }
    }

    pub fn supports_server_session(self) -> bool {
        self == ContextStrategy::StopAtLimit
    }

    pub fn trims_by_turn_count(self) -> bool {
        self == ContextStrategy::RecentMessages
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
// Missing fields use defaults so settings from earlier builds still load.
pub struct Settings {
    pub base_url: String,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub max_output_tokens: Option<u32>,
    pub temperature: f64,
    pub top_p: Option<f64>,
    pub max_parallel_turns: u32,
    pub api_key: Option<String>,
    pub api_key_env_var: String,
    pub protocol: Option<String>,
    pub tool_mode: ToolMode,
    pub max_tool_iterations: u32,
    pub context_budget_tokens: u32,
    pub context_strategy: ContextStrategy,
    pub context_keep_recent_messages: u32,
    pub use_server_session: bool,
    pub use_responses_api: bool,
    pub store_responses: bool,
    pub auto_approve_safe_commands: bool,
    pub git_allow_writes: bool,
    pub transcript_wrap: bool,
    pub honesty_footer: bool,
    pub system_prompt: String,
    pub trusted_commands: Vec<String>,
    #[serde(alias = "mcpServers")]
    pub mcp_servers: Vec<baudet_core::mcp::McpServerConfig>,
    #[serde(alias = "mcpAllowServers")]
    pub mcp_allow_servers: Vec<String>,
    pub auto_allow_commands: Vec<String>,
    pub learned_context_window: u32,
    pub learned_context_key: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub learned_context_windows: BTreeMap<String, u32>,
    pub left_divider: i32,
    pub right_divider: i32,
    pub right_panel_width: i32,
    pub diff_window_size: Option<[f32; 2]>,
    pub files_divider: i32,
    #[serde(deserialize_with = "parse_language")]
    pub language: Language,
}

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
            base_url: "http://127.0.0.1:8000/v1".to_string(),
            model: None,
            reasoning: None,
            max_output_tokens: None,
            temperature: DEFAULT_TEMPERATURE,
            top_p: None,
            max_parallel_turns: DEFAULT_MAX_PARALLEL_TURNS,
            api_key: None,
            api_key_env_var: DEFAULT_KEY_ENV_VAR.to_string(),
            protocol: None,
            tool_mode: ToolMode::default(),
            max_tool_iterations: DEFAULT_MAX_TOOL_ITERATIONS,
            context_budget_tokens: 0,
            context_strategy: ContextStrategy::default(),
            context_keep_recent_messages: DEFAULT_KEEP_RECENT_MESSAGES,
            use_server_session: true,
            use_responses_api: true,
            store_responses: true,
            auto_approve_safe_commands: true,
            git_allow_writes: false,
            transcript_wrap: true,
            honesty_footer: true,
            system_prompt: String::new(),
            trusted_commands: Vec::new(),
            mcp_servers: Vec::new(),
            mcp_allow_servers: Vec::new(),
            auto_allow_commands: Vec::new(),
            learned_context_window: 0,
            learned_context_key: String::new(),
            learned_context_windows: BTreeMap::new(),
            left_divider: 0,
            right_divider: 0,
            right_panel_width: 0,
            diff_window_size: None,
            files_divider: 0,
            language: Language::default(),
        }
    }
}

pub const DEFAULT_KEY_ENV_VAR: &str = "OMLX_API_KEY";

pub const DEFAULT_MAX_TOOL_ITERATIONS: u32 = 8;

pub const DEFAULT_KEEP_RECENT_MESSAGES: u32 = 15;

pub const DEFAULT_TEMPERATURE: f64 = 0.7;

pub const DEFAULT_MAX_PARALLEL_TURNS: u32 = 1;

pub const DEFAULT_SYSTEM_PROMPT: &str = "You are Baudet, an accurate, critical coding assistant. Tests protect intended behavior. Change tests only when requirements changed or the test is objectively incorrect. Answer briefly and factually.";

impl Settings {
    pub fn resolved_key(&self) -> Option<String> {
        Self::resolve_key(self.api_key.as_deref(), &self.api_key_env_var)
    }

    pub fn resolve_key(typed: Option<&str>, env_name: &str) -> Option<String> {
        if let Some(typed) = typed.map(str::trim).filter(|k| !k.is_empty()) {
            return Some(typed.to_string());
        }
        let name = env_name.trim();
        let name = if name.is_empty() { DEFAULT_KEY_ENV_VAR } else { name };
        std::env::var(name).ok().filter(|value| !value.trim().is_empty())
    }

    pub fn has_key(&self) -> bool {
        self.resolved_key().is_some()
    }

    pub fn resolved_system_prompt(&self) -> &str {
        if self.system_prompt.trim().is_empty() {
            DEFAULT_SYSTEM_PROMPT
        } else {
            &self.system_prompt
        }
    }

    pub fn protocol_word(&self) -> Option<&str> {
        self.protocol.as_deref().map(str::trim).filter(|word| !word.is_empty())
    }

    pub fn model_name(&self) -> &str {
        self.model.as_deref().unwrap_or("")
    }

    pub fn context_limits(&self) -> baudet_store::ContextLimits {
        let endpoint = crate::Provider::from_base_url(&self.base_url);
        let base_url = endpoint.as_ref().map_or(self.base_url.as_str(), crate::Provider::base_url);
        self.context_limits_for(base_url, self.model_name())
    }

    pub fn context_limits_for(&self, base_url: &str, model: &str) -> baudet_store::ContextLimits {
        let key = baudet_store::window_key(base_url, model);
        baudet_store::ContextLimits {
            strategy: match self.context_strategy {
                ContextStrategy::RecentMessages => baudet_store::ContextStrategy::RecentMessages,
                ContextStrategy::RollingWindow => baudet_store::ContextStrategy::RollingWindow,
                ContextStrategy::StopAtLimit => baudet_store::ContextStrategy::StopAtLimit,
            },
            budget_tokens: self.context_budget_tokens,
            max_output_tokens: self.max_output_tokens.unwrap_or(0),
            learned_context_window: self.learned_window(&key).unwrap_or(0),
            learned_context_key: key,
            keep_recent_messages: usize::try_from(self.context_keep_recent_messages.max(1)).unwrap_or(usize::MAX),
            model: model.to_owned(),
            base_url: base_url.to_owned(),
        }
    }

    pub fn learned_window(&self, key: &str) -> Option<u32> {
        self.learned_context_windows.get(key).copied().or_else(|| {
            (self.learned_context_key == key).then_some(self.learned_context_window)
        }).filter(|tokens| *tokens > 0)
    }

    pub fn learn_context_window(&mut self, claimed: Option<u32>, base_url: &str) {
        let Some(claimed) = claimed.filter(|claimed| *claimed > 0) else { return };
        let key = baudet_store::window_key(base_url, self.model_name());
        self.record_context_window(key.clone(), claimed);
        self.learned_context_window = claimed;
        self.learned_context_key = key;
    }

    pub fn record_context_window(&mut self, key: String, tokens: u32) {
        if self.learned_context_window > 0 && !self.learned_context_key.is_empty() {
            self.learned_context_windows.entry(self.learned_context_key.clone())
                .or_insert(self.learned_context_window);
        }
        self.learned_context_windows.insert(key, tokens);
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
        assert_eq!(back.base_url, "http://127.0.0.1:8000/v1");
    }

    #[test]
    fn tool_mode_waits_for_approval_by_default_and_survives_the_file() {
        assert_eq!(Settings::default().tool_mode, ToolMode::Ask);
        assert_eq!(ToolMode::default(), ToolMode::Ask);

        let json = serde_json::to_string(&Settings::default()).unwrap();
        assert!(json.contains("\"tool_mode\":\"ask\""), "het bestand noemt het veld zoals het heet: {json}");
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Settings::default());
    }

    #[test]
    fn an_older_file_without_tool_mode_still_reads_as_ask() {
        let json = r#"{"base_url":"http://studio:8000/v1","model":"gemma"}"#;
        let read: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(read.tool_mode, ToolMode::Ask);
    }

    #[test]
    fn de_taal_is_engels_en_overleeft_het_bestand() {
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
        let json = r#"{"base_url":"http://studio:8000/v1","model":"gemma","tool_mode":"ask"}"#;
        let read: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(read.language, Language::En);
        assert_eq!(read.model.as_deref(), Some("gemma"));
    }

    #[test]
    fn een_onbekende_taal_in_het_bestand_valt_terug_naar_engels() {
        let json = r#"{"base_url":"http://studio:8000/v1","model":"gemma","language":"pools"}"#;
        let read: Settings = serde_json::from_str(json).expect("een onbekend taalwoord mag geen kapot bestand maken");
        assert_eq!(read.language, Language::En);
        assert_eq!(read.model.as_deref(), Some("gemma"), "de rest van het profiel blijft staan");
        let spaced: Settings =
            serde_json::from_str(r#"{"language":" NL "}"#).expect("hetzelfde woord met randen is hetzelfde woord");
        assert_eq!(spaced.language, Language::Nl);
        let wrong_kind: Result<Settings, _> = serde_json::from_str(r#"{"language":7}"#);
        assert!(wrong_kind.is_err(), "een getal is geen taal");
    }

    #[test]
    fn tool_mode_words_and_positions_match_the_reference_list() {
        let modes = [ToolMode::Off, ToolMode::Ask, ToolMode::Auto];
        for (expected, mode) in modes.iter().copied().enumerate() {
            assert_eq!(mode.index(), expected);
            assert_eq!(ToolMode::from_word(mode.word()), Some(mode));
        }
        assert_eq!(ToolMode::from_word("ToolsOn"), None);
        assert_eq!(ToolMode::from_word(" ASK "), Some(ToolMode::Ask), "spaties en kas zijn geen betekenis");
    }

    #[test]
    fn the_iteration_cap_defaults_to_eight_and_survives_the_file() {
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
        let json = r#"{"base_url":"http://studio:8000/v1","model":"gemma"}"#;
        let read: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(read.max_tool_iterations, DEFAULT_MAX_TOOL_ITERATIONS);
    }

    #[test]
    fn a_cap_of_zero_survives_the_file_and_is_the_loops_problem_to_floor() {
        let settings = Settings { max_tool_iterations: 0, ..Default::default() };
        let json = serde_json::to_string(&settings).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.max_tool_iterations, 0, "een 0 uit de hand blijft een 0, tot bij de lus");
    }

    #[test]
    fn a_typed_key_wins_and_a_blank_one_falls_back_to_the_named_variable() {
        let name = "BAUDET_SETTINGS_TEST_KEY";
        std::env::set_var(name, "from-the-environment");
        let from_env = Settings { api_key: None, api_key_env_var: name.into(), ..Default::default() };
        assert_eq!(from_env.resolved_key().as_deref(), Some("from-the-environment"));
        assert!(from_env.has_key());

        let typed = Settings { api_key: Some("typed-wins".into()), api_key_env_var: name.into(), ..Default::default() };
        assert_eq!(typed.resolved_key().as_deref(), Some("typed-wins"), "the key on screen must be the key sent");

        std::env::set_var(DEFAULT_KEY_ENV_VAR, "from-the-default-name");
        let blank_name = Settings { api_key: None, api_key_env_var: "   ".into(), ..Default::default() };
        assert_eq!(blank_name.api_key_env_var.trim(), "");
        assert_eq!(
            blank_name.resolved_key().as_deref(),
            Some("from-the-default-name"),
            "a blank name means I did not fill this in, not do not read any variable"
        );
        std::env::remove_var(DEFAULT_KEY_ENV_VAR);

        std::env::set_var(name, "   ");
        assert_eq!(from_env.resolved_key(), None);
        assert!(!from_env.has_key());
        std::env::remove_var(name);
    }

    #[test]
    fn the_context_defaults_are_the_reference_ones_and_survive_the_file() {
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
    fn de_generatie_parameters_zijn_de_default_en_overleven_het_bestand() {
        let settings = Settings::default();
        assert_eq!(settings.temperature, DEFAULT_TEMPERATURE);
        assert_eq!(DEFAULT_TEMPERATURE, 0.7, "de origineler begint op 0,7");
        assert_eq!(settings.top_p, None, "geen top_p ingetypt is geen 1,0 maar 'stuur hem niet'");
        assert_eq!(settings.max_parallel_turns, DEFAULT_MAX_PARALLEL_TURNS);
        assert_eq!(DEFAULT_MAX_PARALLEL_TURNS, 1, "de origineler draait één beurt tegelijk");

        let filled = Settings { temperature: 0.2, top_p: Some(0.9), max_parallel_turns: 4, ..Default::default() };
        let json = serde_json::to_string(&filled).unwrap();
        assert!(json.contains("\"temperature\":0.2"), "het bestand noemt de temperatuur: {json}");
        assert!(json.contains("\"top_p\":0.9"), "het bestand noemt de marge: {json}");
        assert!(json.contains("\"max_parallel_turns\":4"), "het bestand noemt de parallelstand: {json}");
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, filled);

        let no_top = Settings { temperature: 1.0, ..Default::default() };
        let json = serde_json::to_string(&no_top).unwrap();
        assert!(json.contains("\"top_p\":null"), "afwezig blijft afwezig in het bestand: {json}");
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap().top_p, None);
    }

    #[test]
    fn een_oud_bestand_zonder_de_generatie_velden_opent_met_de_default() {
        let json = r#"{"base_url":"http://studio:8000/v1","model":"gemma-3-4b-it"}"#;
        let read: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(read.temperature, DEFAULT_TEMPERATURE);
        assert_eq!(read.top_p, None);
        assert_eq!(read.max_parallel_turns, DEFAULT_MAX_PARALLEL_TURNS);
    }

    #[test]
    fn de_zeven_vinkjes_zijn_de_default_van_de_origineler_en_overleven_het_bestand() {
        let settings = Settings::default();
        assert!(settings.use_responses_api, "de origineler spreekt Responses");
        assert!(settings.store_responses);
        assert!(settings.auto_approve_safe_commands, "veilige alleen-lezen commando's gaan er automatisch door");
        assert!(!settings.git_allow_writes, "git mag níét schrijven tot de persoon het aanzet");
        assert!(settings.transcript_wrap);
        assert!(settings.honesty_footer);
        assert!(settings.use_server_session);

        let flipped = Settings {
            use_responses_api: false,
            store_responses: false,
            auto_approve_safe_commands: false,
            git_allow_writes: true,
            transcript_wrap: false,
            honesty_footer: false,
            use_server_session: false,
            ..Default::default()
        };
        let json = serde_json::to_string(&flipped).unwrap();
        assert!(json.contains("\"git_allow_writes\":true"), "het bestand noemt de git-vlag: {json}");
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), flipped);
    }

    #[test]
    fn een_ledige_systeemprompt_is_afwezigheid_en_geen_vergeten_tekst() {
        let default = Settings::default();
        assert!(default.system_prompt.is_empty(), "de default wijst niet af");
        assert_eq!(default.resolved_system_prompt(), DEFAULT_SYSTEM_PROMPT);

        let only_blanks = Settings { system_prompt: "   \n  ".into(), ..Default::default() };
        assert_eq!(only_blanks.resolved_system_prompt(), DEFAULT_SYSTEM_PROMPT, "spaties zijn geen prompt");

        let own = "Focus on Swift, ignore the tests.";
        let typed = Settings { system_prompt: own.into(), ..Default::default() };
        assert_eq!(typed.resolved_system_prompt(), own, "wat iemand schreef, wint");
    }

    #[test]
    fn de_ingebouwde_prompt_is_de_prompt_van_de_origineler() {
        assert_eq!(
            DEFAULT_SYSTEM_PROMPT,
            "You are Baudet, an accurate, critical coding assistant. Tests protect intended behavior. Change tests only when requirements changed or the test is objectively incorrect. Answer briefly and factually."
        );
    }

    #[test]
    fn de_eigen_systeemprompt_reist_door_het_bestand() {
        let dir = tempfile::tempdir().unwrap();
        use crate::store::SettingsFile;
        let store = SettingsFile::at(dir.path().join("settings.json"));
        let own = "Regel een.\nRegel twee met  spaties.\n\nEn een ledige erna.";
        store.write(&Settings { system_prompt: own.into(), ..Default::default() }).unwrap();
        let outcome = store.read();
        let read = outcome.settings();
        assert_eq!(read.system_prompt, own, "regeleinden reizen mee");
        assert_eq!(read.resolved_system_prompt(), own);
    }

    #[test]
    fn beide_vertrouwenslijsten_starten_ledig_en_reizen_als_lijst() {
        let default = Settings::default();
        assert!(default.trusted_commands.is_empty(), "ledig betekent uit");
        assert!(default.auto_allow_commands.is_empty());

        use crate::store::SettingsFile;
        let file = tempfile::tempdir().unwrap();
        let store = SettingsFile::at(file.path().join("settings.json"));
        let filled = Settings {
            trusted_commands: vec!["git commit".into(), "mvn".into()],
            auto_allow_commands: vec!["git diff --stat".into()],
            ..Default::default()
        };
        store.write(&filled).unwrap();
        let outcome = store.read();
        assert_eq!(outcome.settings().trusted_commands, filled.trusted_commands);
        assert_eq!(outcome.settings().auto_allow_commands, filled.auto_allow_commands);
    }

    #[test]
    fn de_verdeelstanden_zijn_eerst_niets_en_daarna_hun_getal() {
        let fresh = Settings::default();
        assert_eq!((fresh.left_divider, fresh.right_divider, fresh.files_divider), (0, 0, 0));
        assert_eq!(
            baudet_ui_divider_is_not_invented(fresh.left_divider, 280.0),
            280.0,
            "een null in het bestand is geen breedte"
        );

        let measured = Settings {
            left_divider: 311,
            right_divider: 940,
            right_panel_width: 300,
            files_divider: 168,
            ..Default::default()
        };
        let json = serde_json::to_string(&measured).unwrap();
        assert!(json.contains("\"left_divider\":311"), "het bestand noemt de stands: {json}");
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, measured);
        let legacy: Settings = serde_json::from_str(r#"{"right_divider":940}"#).unwrap();
        assert_eq!((legacy.right_divider, legacy.right_panel_width), (940, 0));
    }

    fn baudet_ui_divider_is_not_invented(persisted: i32, fallback: f32) -> f32 {
        if persisted > 0 {
            persisted as f32
        } else {
            fallback
        }
    }

    #[test]
    fn an_older_file_without_the_context_fields_still_opens() {
        let json = r#"{"base_url":"http://studio:8000/v1","model":"gemma-3-4b-it"}"#;
        let read: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(read.context_strategy, ContextStrategy::RecentMessages);
        assert_eq!(read.context_keep_recent_messages, DEFAULT_KEEP_RECENT_MESSAGES);
        assert_eq!(read.learned_context_window, 0);
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
        assert!(ContextStrategy::StopAtLimit.supports_server_session());
        assert!(!ContextStrategy::RollingWindow.supports_server_session());
        assert!(ContextStrategy::RecentMessages.trims_by_turn_count());
        assert!(!ContextStrategy::RollingWindow.trims_by_turn_count());
    }

    #[test]
    fn the_settings_hand_the_store_one_reading_of_every_number() {
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
        assert_eq!(limits.hard_ceiling(), 8_000);

        let bare = Settings { model: Some("gemma-3-4b-it".into()), ..Default::default() };
        assert_eq!(bare.context_limits().max_output_tokens, 0);

        let limits = bare.context_limits();
        let window = baudet_core::known_context_window("gemma-3-4b-it").unwrap();
        assert_eq!(window, 32_768);
        assert_eq!(limits.hard_ceiling(), window - baudet_store::WINDOW_MARGIN_TOKENS);
        assert_eq!(
            limits.context_ceiling(),
            u64::from(window)
                - u64::from(baudet_store::WINDOW_MARGIN_TOKENS)
                - u64::from(baudet_store::INPUT_OVERHEAD_TOKENS)
        );

        let blind = Settings { model: Some("helemaal-niets-bekends".into()), ..Default::default() };
        assert_eq!(blind.context_limits().hard_ceiling(), 0);
        assert_eq!(blind.context_limits().context_ceiling(), 0, "geen venster en geen budget, dus uit");
    }

    #[test]
    fn a_zero_turn_count_is_read_as_one_and_not_as_send_nothing() {
        let settings = Settings { context_keep_recent_messages: 0, ..Default::default() };
        assert_eq!(settings.context_limits().keep_recent_messages, 1);
    }

    #[test]
    fn a_learned_window_lands_under_the_identity_and_never_rises_again() {
        let mut settings =
            Settings { base_url: "http://a:8000/v1".into(), model: Some("gemma-3-4b-it".into()), ..Default::default() };
        settings.learn_context_window(None, "http://a:8000/v1");
        assert_eq!(settings.learned_context_window, 0);
        settings.learn_context_window(Some(0), "http://a:8000/v1");
        assert_eq!(settings.learned_context_window, 0);

        settings.learn_context_window(Some(12_288), "http://a:8000/v1");
        assert_eq!(settings.learned_context_window, 12_288);
        assert_eq!(settings.learned_context_key, "http://a:8000/v1|gemma-3-4b-it");

        let before = settings.clone();
        settings.learn_context_window(Some(12_288), "http://a:8000/v1");
        assert_eq!(settings, before);

        settings.learn_context_window(Some(4_096), "http://b:8000/v1");
        assert_eq!(settings.learned_context_key, "http://b:8000/v1|gemma-3-4b-it");
        assert_eq!(
            settings.context_limits().context_window(),
            Some(12_288),
            "learning on another endpoint preserves the first endpoint's limit"
        );
        settings.base_url = "http://b:8000/v1".into();
        assert_eq!(settings.context_limits().context_window(), Some(4_096));
        settings.model = Some("other-model".into());
        assert_eq!(settings.context_limits().context_window(), None);
    }

    #[test]
    fn an_unknown_field_survives_and_a_missing_one_default() {
        let json = r#"{"base_url":"http://studio:8000/v1","a_field_from_next_week":42}"#;
        let read: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(read.base_url, "http://studio:8000/v1");
        assert_eq!(read.model, None);
        assert_eq!(read, Settings { base_url: "http://studio:8000/v1".into(), ..Default::default() });
    }
}
