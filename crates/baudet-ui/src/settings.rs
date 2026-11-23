
use crate::palette;
use baudet_backend::protocol::Protocol;
use baudet_i18n::{Key, Language, Texts};
use baudet_settings::config::{ContextStrategy, ToolMode};
use baudet_settings::{ListingError, Model, ReadOutcome, Settings, SettingsFile, WriteError};
use egui::Ui;

pub enum SettingsCommand {
    ApplyMcp { servers: String, allowed: String },
    ClearMcpAllow,
    AllowMcp(String),
    Fetch {
        endpoint: String,
        api_key: Option<String>,
    },
    Probe { endpoint: String, api_key: Option<String> },
    Save(Settings),
    LearnContext { key: String, tokens: u32 },
    ApplyTrusted(Vec<String>),
    ClearTrusted,
    ClearAlways,
}

pub enum SettingsEvent {
    McpUpdated {
        servers: Vec<baudet_core::mcp::McpServerConfig>,
        allowed: Vec<String>,
        refresh_servers: bool,
        status: String,
        outcome: Result<(), WriteError>,
    },
    Listing(Result<Vec<Model>, ListingError>, String),
    Probed(Result<Option<Protocol>, String>, String),
    ConnectionChanged(Option<String>),
    Saved(Result<(), WriteError>),
    ContextLearned { key: String, tokens: u32 },
    TrustedApplied {
        trusted: Vec<String>,
        always: Vec<String>,
        dropped: usize,
        outcome: Result<(), WriteError>,
    },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SettingsView {
    base_url: String,
    model: Option<String>,
    protocol: Option<String>,
    tool_mode: ToolMode,
    reasoning: Reasoning,
    max_output_tokens: String,
    max_tool_iterations: String,
    stored_max_tool_iterations: u32,
    stored_context: Settings,
    context_strategy: ContextStrategy,
    context_budget: String,
    context_keep_recent: String,
    temperature: String,
    top_p: String,
    max_parallel_turns: String,
    use_responses_api: bool,
    store_responses: bool,
    use_server_session: bool,
    auto_approve_safe_commands: bool,
    git_allow_writes: bool,
    transcript_wrap: bool,
    honesty_footer: bool,
    system_prompt: String,
    trusted_text: String,
    mcp_servers_text: String,
    mcp_allow_text: String,
    auto_allow_text: String,
    api_key: String,
    api_key_env_var: String,

    models: Vec<Model>,
    fetching: bool,
    status: String,
    path: Option<std::path::PathBuf>,
    blocked: bool,
    model_window: Option<u32>,
    fetched_for: Option<String>,
    listing_answered: bool,
    probed: Option<(String, Protocol)>,
    probing: bool,
    reveal_key: bool,
    language: Language,
}

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
    const ALL: [Reasoning; 5] =
        [Reasoning::Unset, Reasoning::Minimal, Reasoning::Low, Reasoning::Medium, Reasoning::High];

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

    pub(crate) fn from_stored(stored: Option<&str>) -> Reasoning {
        match stored.map(str::trim).filter(|s| !s.is_empty()) {
            Some("minimal") => Reasoning::Minimal,
            Some("low") => Reasoning::Low,
            Some("medium") => Reasoning::Medium,
            Some("high") => Reasoning::High,
            _ => Reasoning::Unset,
        }
    }
}

impl SettingsView {
    pub(crate) fn transcript_wrap(&self) -> bool {
        self.transcript_wrap
    }

    pub(crate) fn honesty_footer(&self) -> bool {
        self.honesty_footer
    }

    pub fn new() -> SettingsView {
        SettingsView::default()
    }

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
                let _ = preserved;
            }
        }
    }

    fn adopt(&mut self, settings: &Settings) {
        self.base_url = settings.base_url.clone();
        self.model = settings.model.clone();
        self.reasoning = Reasoning::from_stored(settings.reasoning.as_deref());
        self.max_output_tokens = settings.max_output_tokens.map(|n| n.to_string()).unwrap_or_default();
        self.max_tool_iterations = settings.max_tool_iterations.to_string();
        self.stored_max_tool_iterations = settings.max_tool_iterations;
        self.api_key = settings.api_key.clone().unwrap_or_default();
        self.api_key_env_var = settings.api_key_env_var.clone();
        self.protocol = settings.protocol_word().map(str::to_string);
        self.tool_mode = settings.tool_mode;
        self.context_strategy = settings.context_strategy;
        self.context_budget = settings.context_budget_tokens.to_string();
        self.context_keep_recent = settings.context_keep_recent_messages.to_string();
        self.temperature = settings.temperature.to_string();
        self.top_p = settings.top_p.map(|p| p.to_string()).unwrap_or_default();
        self.max_parallel_turns = settings.max_parallel_turns.to_string();
        self.use_responses_api = settings.use_responses_api;
        self.store_responses = settings.store_responses;
        self.use_server_session = settings.use_server_session;
        self.auto_approve_safe_commands = settings.auto_approve_safe_commands;
        self.git_allow_writes = settings.git_allow_writes;
        self.transcript_wrap = settings.transcript_wrap;
        self.honesty_footer = settings.honesty_footer;
        self.system_prompt = settings.system_prompt.clone();
        self.trusted_text = settings.trusted_commands.join("\n");
        self.mcp_servers_text = mcp_text(&settings.mcp_servers);
        self.mcp_allow_text = settings.mcp_allow_servers.join("\n");
        self.stored_context.mcp_servers = settings.mcp_servers.clone();
        self.stored_context.mcp_allow_servers = settings.mcp_allow_servers.clone();
        self.auto_allow_text = settings.auto_allow_commands.join("\n");
        self.stored_context.use_server_session = settings.use_server_session;
        self.stored_context.learned_context_window = settings.learned_context_window;
        self.stored_context.learned_context_key = settings.learned_context_key.clone();
        self.stored_context.learned_context_windows = settings.learned_context_windows.clone();
        self.stored_context.left_divider = settings.left_divider;
        self.stored_context.right_divider = settings.right_divider;
        self.stored_context.right_panel_width = settings.right_panel_width;
        self.stored_context.diff_window_size = settings.diff_window_size;
        self.stored_context.files_divider = settings.files_divider;
        self.language = settings.language;
        self.fetched_for = None;
        self.listing_answered = false;
        self.model_window = settings.learned_window(&settings.context_limits().identity_key());
        self.probed = None;
        self.probing = false;
    }

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
            system_prompt: self.system_prompt.clone(),
            trusted_commands: self.stored_context.trusted_commands.clone(),
            mcp_servers: self.stored_context.mcp_servers.clone(),
            mcp_allow_servers: self.stored_context.mcp_allow_servers.clone(),
            auto_allow_commands: self.stored_context.auto_allow_commands.clone(),
            learned_context_window: self.stored_context.learned_context_window,
            learned_context_key: self.stored_context.learned_context_key.clone(),
            learned_context_windows: self.stored_context.learned_context_windows.clone(),
            left_divider: self.stored_context.left_divider,
            right_divider: self.stored_context.right_divider,
            right_panel_width: self.stored_context.right_panel_width,
            diff_window_size: self.stored_context.diff_window_size,
            files_divider: self.stored_context.files_divider,
            language: self.language,
        })
    }

    pub fn dividers(&self) -> crate::layout::Dividers {
        crate::layout::Dividers {
            left: self.stored_context.left_divider,
            right: self.stored_context.right_divider,
            right_width: self.stored_context.right_panel_width,
            files: self.stored_context.files_divider,
        }
    }

    pub fn set_dividers(&mut self, dividers: &crate::layout::Dividers) {
        self.stored_context.left_divider = dividers.left;
        self.stored_context.right_divider = dividers.right;
        self.stored_context.right_panel_width = dividers.right_width;
        self.stored_context.files_divider = dividers.files;
    }

    pub(crate) fn diff_window_size(&self) -> Option<[f32; 2]> {
        self.stored_context.diff_window_size
    }

    pub(crate) fn set_diff_window_size(&mut self, size: [f32; 2]) {
        self.stored_context.diff_window_size = Some(size);
    }

    pub fn language(&self) -> Language {
        self.language
    }

    pub fn file_path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    pub fn geometry_draft(&self, dividers: &crate::layout::Dividers) -> Result<Settings, String> {
        let mut settings = self.draft()?;
        settings.left_divider = dividers.left;
        settings.right_divider = dividers.right;
        settings.right_panel_width = dividers.right_width;
        settings.files_divider = dividers.files;
        Ok(settings)
    }

    pub fn apply_event(&mut self, event: SettingsEvent) {
        match event {
            SettingsEvent::Listing(result, endpoint) => {
                self.fetching = false;
                self.listing_answered = true;
                match result {
                    Ok(models) => {
                        self.models = models;
                        self.status = format!("{} models from {}", self.models.len(), self.base_url.trim());
                        self.fetched_for = Some(endpoint);
                    }
                    Err(error) => {
                        self.status = error.to_string();
                    }
                }
            }
            SettingsEvent::Probed(result, endpoint) => {
                self.probing = false;
                match result {
                    Ok(Some(dialect)) => {
                        self.protocol = Some(dialect.word().to_string());
                        self.probed = Some((endpoint, dialect));
                        self.status = format!("{}: {}", self.base_url.trim(), dialect.word());
                    }
                    Ok(None) => self.status = format!("no dialect identified: {endpoint}"),
                    Err(problem) => self.status = problem,
                }
            }
            SettingsEvent::ConnectionChanged(_) => {}
            SettingsEvent::ContextLearned { key, tokens } => {
                if let Ok(endpoint) = baudet_settings::Provider::from_base_url(&self.base_url) {
                    if key == baudet_store::window_key(endpoint.base_url(), self.model.as_deref().unwrap_or("")) {
                        self.model_window = Some(tokens);
                    }
                }
                self.stored_context.record_context_window(key, tokens);
            }
            SettingsEvent::McpUpdated { servers, allowed, refresh_servers, status, outcome } => {
                match outcome {
                    Ok(()) => {
                        if refresh_servers {
                            self.mcp_servers_text = mcp_text(&servers);
                        }
                        self.mcp_allow_text = allowed.join("\n");
                        self.stored_context.mcp_servers = servers;
                        self.stored_context.mcp_allow_servers = allowed;
                        self.status = status;
                    }
                    Err(error) => self.status = error.to_string(),
                }
            }
            SettingsEvent::Saved(result) => {
                self.status = match result {
                    Ok(()) => format!("saved to {}", self.path.as_deref().map(describe_path).unwrap_or_default()),
                    Err(WriteError::WouldDiscard { reason, .. }) => {
                        self.blocked = true;
                        reason
                    }
                    Err(other) => other.to_string(),
                };
            }
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
    pub fn set_endpoint(&mut self, address: &str) {
        self.base_url = address.trim().to_string();
        self.fetched_for = None;
        self.listing_answered = false;
        self.probed = None;
    }

    pub fn protocol_word(&self) -> Option<&str> {
        self.probed
            .as_ref()
            .filter(|(probed, _)| *probed == self.base_url.trim())
            .map(|(_, dialect)| dialect.word())
            .or_else(|| self.protocol.as_deref().map(str::trim).filter(|w| !w.is_empty()))
    }

    pub fn request_fetch(&mut self, tx: &std::sync::mpsc::Sender<SettingsCommand>) {
        if self.fetching {
            return;
        }
        let _ = tx.send(self.fetch_command());
    }

    fn fetch_command(&mut self) -> SettingsCommand {
        self.fetching = true;
        self.status = format!("asking {}…", self.base_url.trim());
        SettingsCommand::Fetch {
            endpoint: self.base_url.trim().to_string(),
            api_key: Settings::resolve_key(Some(&self.api_key), &self.api_key_env_var),
        }
    }

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

    pub fn listing_answered(&self) -> bool {
        self.listing_answered
    }

    pub fn endpoint(&self) -> String {
        self.base_url.trim().to_string()
    }

    pub fn models(&self) -> &[Model] {
        &self.models
    }

    pub fn chosen_model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn choose_model(&mut self, id: &str) {
        self.model = Some(id.to_string());
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub(crate) fn report(&mut self, problem: String) {
        self.status = problem;
    }

    pub fn is_fetching(&self) -> bool {
        self.fetching
    }

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

pub const FIELD_ENDPOINT: &str = "settings-endpoint-field";

const LABEL_COLUMN: f32 = 168.0;

pub const ROW_GAP: f32 = 8.0;

fn lucht_voor_kader(g: f32) -> f32 {
    if g <= 0.0 {
        0.0
    } else {
        g + 5.0
    }
}

pub const GROUP_GAP: f32 = 16.0;

fn form_row(ui: &mut Ui, label: impl Into<String>, build: impl FnOnce(&mut Ui)) {
    form_row_with_gap(ui, ROW_GAP, label, build);
}

fn form_row_with_gap(ui: &mut Ui, boven: f32, label: impl Into<String>, build: impl FnOnce(&mut Ui)) {
    let text = dim(label);
    let taken = ui.available_size_before_wrap();
    if boven > 0.0 {
        ui.add_space(lucht_voor_kader(boven));
    }
    let inkling = text.clone().into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body);
    if inkling.size().x + ROW_GAP <= LABEL_COLUMN {
        ui.horizontal(|ui| {
            ui.add_sized([LABEL_COLUMN, 20.0], egui::Label::new(text).selectable(false));
            build(ui);
        });
        ui.set_min_width(taken.x);
    } else {
        ui.horizontal(|ui| {
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

pub fn body(ui: &mut Ui, view: &mut SettingsView, tx: &std::sync::mpsc::Sender<SettingsCommand>) {
    let mut commands: Vec<SettingsCommand> = Vec::new();
    ui.set_width(ui.available_width().min(360.0));
    language_row(ui, view, 0.0);
    group_break(ui);
    endpoint_row(ui, view, &mut commands, 0.0);
    group_break(ui);
    model_row(ui, view, 0.0);
    group_break(ui);
    sampling_rows(ui, view);
    reasoning_row(ui, view, ROW_GAP);
    tokens_row(ui, view);
    iterations_row(ui, view);
    parallel_row(ui, view);
    group_break(ui);
    toggle_rows(ui, view);
    system_prompt_row(ui, view);
    group_break(ui);
    command_rows(ui, view, &mut commands);
    group_break(ui);
    mcp_rows(ui, view, &mut commands);
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
    if !view.status.is_empty() {
        group_break(ui);
        ui.label(dim(&view.status));
    }
    for command in commands {
        let _ = tx.send(command);
    }
}

fn mcp_text(servers: &[baudet_core::mcp::McpServerConfig]) -> String {
    servers.iter().map(|server| serde_json::to_string(server).expect("serializable MCP configuration"))
        .collect::<Vec<_>>().join("\n")
}

fn mcp_rows(ui: &mut Ui, view: &mut SettingsView, commands: &mut Vec<SettingsCommand>) {
    ui.strong(Texts::get(Key::McpManagerTitle));
    ui.label(Texts::get(Key::McpServersLabel));
    ui.add(egui::TextEdit::multiline(&mut view.mcp_servers_text).code_editor().desired_rows(4).desired_width(f32::INFINITY))
        .on_hover_text(Texts::get(Key::McpServersHint));
    ui.label(Texts::get(Key::McpAllowLabel));
    ui.add(egui::TextEdit::multiline(&mut view.mcp_allow_text).desired_rows(2).desired_width(f32::INFINITY))
        .on_hover_text(Texts::get(Key::McpAllowHint));
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(!view.blocked, egui::Button::new(Texts::get(Key::ApplyMcpButton))).clicked() {
            commands.push(SettingsCommand::ApplyMcp { servers: view.mcp_servers_text.clone(), allowed: view.mcp_allow_text.clone() });
        }
        if ui.add_enabled(!view.blocked, egui::Button::new(Texts::get(Key::ClearMcpAllowButton))).clicked() {
            commands.push(SettingsCommand::ClearMcpAllow);
        }
    });
}

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
    let mut focused = false;
    let rij_breed = ui.available_size_before_wrap().x;
    form_row_with_gap(ui, boven, Texts::get(Key::BaseUrlLabel), |ui| {
        let response = ui
            .add_sized(
                [ui.available_width(), 20.0],
                egui::TextEdit::singleline(&mut view.base_url)
                    .id_salt(FIELD_ENDPOINT)
                    .hint_text("http://127.0.0.1:8000/v1")
                    .desired_width(f32::INFINITY),
            )
            .on_hover_text(Texts::get(Key::TipBaseUrl));
        if response.changed() {
            view.fetched_for = None;
        }
        focused = response.has_focus();
    });
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
    ui.add_space(lucht_voor_kader(ROW_GAP));
    ui.horizontal(|ui| {
        ui.add_sized([LABEL_COLUMN, 20.0], egui::Label::new("").selectable(false));
        let scan = ui
            .add_enabled(!view.probing, egui::Button::new(if view.probing { "scanning…" } else { "Scan endpoint" }))
            .clicked();
        if scan {
            view.probing = true;
            view.status = format!("scanning {}…", view.base_url.trim());
            commands.push(SettingsCommand::Probe {
                endpoint: view.base_url.trim().to_string(),
                api_key: Settings::resolve_key(Some(&view.api_key), &view.api_key_env_var),
            });
        }
        if let Some(word) = view.protocol_word() {
            ui.label(dim(format!("protocol: {word}")));
        }
    });
    ui.set_min_width(rij_breed);
    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter)) && focused;
    if (fetch_now || enter) && !view.fetching {
        commands.push(view.fetch_command());
    }
}

fn model_row(ui: &mut Ui, view: &mut SettingsView, boven: f32) {
    let saved = view.model.clone().filter(|saved| !saved.trim().is_empty());
    if view.models.is_empty() {
        if let Some(saved) = saved {
            let listed = view.fetched_for.is_some();
            form_row_with_gap(ui, boven, Texts::get(Key::ModelLabel), |ui| {
                ui.monospace(saved_model_label(&saved, view.model_window, listed));
            });
            return;
        }
        form_row_with_gap(ui, boven, Texts::get(Key::ModelLabel), |ui| {
            let note = if view.fetched_for.is_some() { "this endpoint lists none" } else { "not fetched yet" };
            ui.label(dim(note));
        });
        return;
    }
    form_row(ui, Texts::get(Key::ModelLabel), |ui| {
        let row_labels: Vec<String> = view.models.iter().map(row_for).collect();
        let current = view.model.clone().unwrap_or_default();
        let mut labels: Vec<String> = row_labels.clone();
        if let Some(saved) = saved.as_ref().filter(|saved| !view.models.iter().any(|m| m.id == **saved)) {
            labels.insert(0, saved_model_label(saved, view.model_window, view.fetched_for.is_some()));
        }
        let mut chosen = current.clone();
        let selected = view
            .models
            .iter()
            .find(|m| m.id == current)
            .map(row_for)
            .or_else(|| labels.iter().find(|l| *l == &current).cloned())
            .or_else(|| saved.as_ref().map(|s| saved_model_label(s, view.model_window, view.fetched_for.is_some())))
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
        if chosen != current && view.models.iter().any(|m| m.id == chosen) {
            view.model = Some(chosen);
        }
    });
}

fn saved_model_label(saved: &str, window: Option<u32>, listed: bool) -> String {
    let venster = match window {
        Some(tokens) if tokens >= 1024 => {
            let mut power: u64 = 1024;
            while power < tokens as u64 {
                power *= 2;
            }
            let lager = power / 2;
            let dichtstbij =
                if lager >= 1024 && (tokens as u64) - lager < power - (tokens as u64) { lager } else { power };
            let kib = dichtstbij / 1024;
            if kib >= 1024 && kib % 1024 == 0 {
                format!("{}m", kib / 1024)
            } else {
                format!("{kib}k")
            }
        }
        Some(tokens) => tokens.to_string(),
        None => String::new(),
    };
    let bevestiging = if listed { "not listed" } else { "not fetched yet" };
    if venster.is_empty() {
        return format!("{saved}  ({bevestiging})");
    }
    format!("{saved} · {venster}  ({bevestiging})")
}

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

fn split_trusted_lines(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_string).collect()
}

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

fn iterations_row(ui: &mut Ui, view: &mut SettingsView) {
    let label = |mode| Texts::get(match mode {
        ToolMode::Off => Key::ToolModeOff,
        ToolMode::Ask => Key::ToolModeAsk,
        ToolMode::Auto => Key::ToolModeAuto,
    });
    form_row(ui, Texts::get(Key::ToolModeLabel), |ui| {
        egui::ComboBox::from_id_salt("settings-tool-mode")
            .selected_text(label(view.tool_mode))
            .show_ui(ui, |ui| {
                for mode in [ToolMode::Off, ToolMode::Ask, ToolMode::Auto] {
                    ui.selectable_value(&mut view.tool_mode, mode, label(mode));
                }
            })
            .response
            .on_hover_text(Texts::get(Key::TipToolMode));
    });
    form_row(ui, Texts::get(Key::MaxToolIterationsLabel), |ui| {
        ui.add_enabled(
            view.tool_mode != ToolMode::Off,
            egui::TextEdit::singleline(&mut view.max_tool_iterations)
                .hint_text("e.g. 8")
                .desired_width(ui.available_width()),
        )
        .on_hover_text(Texts::get(Key::TipMaxIterations));
    });
}

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

fn strategy_label(strategy: ContextStrategy) -> &'static str {
    match strategy {
        ContextStrategy::RecentMessages => Texts::get(Key::StrategyRecent),
        ContextStrategy::RollingWindow => Texts::get(Key::StrategyRolling),
        ContextStrategy::StopAtLimit => Texts::get(Key::StrategyStop),
    }
}

fn parse_context_budget(text: &str) -> Result<u32, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }
    trimmed.parse::<u32>().map_err(|_| format!("context budget is not a token count: {trimmed}"))
}

fn parse_keep_recent(text: &str) -> Result<u32, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(1);
    }
    trimmed.parse::<u32>().map(|n| n.max(1)).map_err(|_| format!("keep last turns is not a whole number: {trimmed}"))
}

fn parse_temperature(text: &str) -> Result<f64, String> {
    let trimmed = text.trim();
    let value = trimmed.parse::<f64>().map_err(|_| {
        format!("{} is not a number between 0 and 2", if trimmed.is_empty() { "temperature" } else { trimmed })
    })?;
    if (0.0..=2.0).contains(&value) {
        Ok(value)
    } else {
        Err(format!("temperature must be between 0 and 2, not {trimmed}"))
    }
}

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

fn parse_parallel_turns(text: &str) -> Result<u32, String> {
    let trimmed = text.trim();
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
    ui.add_space(lucht_voor_kader(ROW_GAP));
    let save = ui
        .add_enabled(!view.blocked, egui::Button::new(Texts::get(Key::SaveButton)))
        .on_hover_text(Texts::get(Key::TipSave))
        .clicked();
    if save {
        match view.draft() {
            Ok(settings) => {
                Texts::set_language(settings.language);
                view.status = "saving…".to_string();
                commands.push(SettingsCommand::Save(settings));
            }
            Err(problem) => view.status = problem,
        }
    }
}

fn dim(text: impl Into<String>) -> egui::WidgetText {
    egui::RichText::new(text.into()).color(palette::DIM).into()
}

fn group_break(ui: &mut Ui) {
    ui.add(egui::Separator::default().spacing(GROUP_GAP));
}

fn describe_path(path: &std::path::Path) -> String {
    format!("{}", path.display())
}

#[cfg(test)]
mod tests {
    use super::{split_trusted_lines, Language, Reasoning, SettingsCommand, SettingsEvent, SettingsView, ToolMode};
    use baudet_settings::{ListingError, Model};
    use std::path::PathBuf;

    fn model(id: &str) -> Model {
        Model { id: id.to_string(), label: None, context_window: None }
    }

    #[test]
    fn bewaard_model_staat_er_zonder_listing() {
        use baudet_settings::Settings;
        let mut bestand = Settings::default();
        bestand.base_url = "http://127.0.0.1:1234/v1".to_string();
        bestand.model = Some("google/gemma-4-26b-a4b-qat".to_string());
        let mut paneel = SettingsView::new();
        paneel.adopt(&bestand);
        assert!(paneel.models.is_empty(), "dit scenario is zónder listing");
        assert_eq!(paneel.fetched_for, None, "er is nooit iets gevraagd");
        assert_eq!(paneel.chosen_model(), Some("google/gemma-4-26b-a4b-qat"));

        let drafted = paneel.draft().expect("een bewaard model mag niet geweigerd worden");
        assert_eq!(drafted.model.as_deref(), Some("google/gemma-4-26b-a4b-qat"));
        let mut terug = SettingsView::new();
        terug.adopt(&drafted);
        assert_eq!(terug.model.as_deref(), Some("google/gemma-4-26b-a4b-qat"));
    }

    #[test]
    fn bewaard_model_woord_leeft_eerlijk() {
        use super::saved_model_label;
        assert_eq!(saved_model_label("gemma-4", None, false), "gemma-4  (not fetched yet)");
        assert_eq!(saved_model_label("gemma-4", None, true), "gemma-4  (not listed)");
        assert_eq!(saved_model_label("gemma-4", Some(131072), false), "gemma-4 · 128k  (not fetched yet)");
        assert_eq!(saved_model_label("gemma-4", Some(8194), true), "gemma-4 · 8k  (not listed)");
        assert_eq!(saved_model_label("gemma-4", Some(1048576), false), "gemma-4 · 1m  (not fetched yet)");
        assert_eq!(saved_model_label("gemma-4", Some(999), false), "gemma-4 · 999  (not fetched yet)");
        assert!(!saved_model_label("gemma-4", None, false).contains("·"));
    }

    #[test]
    fn geleerd_venster_mag_alleen_bij_dezelfde_combinatie() {
        use baudet_settings::Settings;

        let mut settings = Settings::default();
        settings.base_url = "http://127.0.0.1:1234/v1".to_string();
        settings.model = Some("gemma-4".to_string());
        settings.learned_context_window = 131072;
        settings.learned_context_key = baudet_store::window_key(&settings.base_url, "gemma-4");
        let mut eigen = SettingsView::new();
        eigen.adopt(&settings);
        assert_eq!(eigen.model_window, Some(131072), "het venster van deze combinatie blijft staan");

        let mut elders = settings.clone();
        elders.learned_context_key = baudet_store::window_key("http://elders:8000/v1", "gemma-4");
        let mut vreemd = SettingsView::new();
        vreemd.adopt(&elders);
        assert_eq!(vreemd.model_window, None, "een venster voor een ander adres is geen bewijs");

        let mut kaal = Settings::default();
        kaal.model = Some("gemma-4".to_string());
        let mut blanco = SettingsView::new();
        blanco.adopt(&kaal);
        assert_eq!(blanco.model_window, None);
    }

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

        view.context_budget = "0".to_string();
        view.context_keep_recent = "0".to_string();
        let drafted = view.draft().expect("nul mag geen fout zijn");
        assert_eq!(drafted.context_budget_tokens, 0);
        assert_eq!(drafted.context_keep_recent_messages, 1);

        view.context_budget = "veel".to_string();
        assert!(view.draft().is_err(), "een woord als budget moet geweigerd worden");
    }

    #[test]
    fn sampling_en_parallel_rondje_en_vloeren() {
        let mut view = view();
        view.temperature = "0.3".to_string();
        view.top_p = "0.9".to_string();
        view.max_parallel_turns = "3".to_string();
        let drafted = view.draft().expect("een geldige sampling mag niet worden geweigerd");
        assert_eq!(drafted.temperature, 0.3);
        assert_eq!(drafted.top_p, Some(0.9));
        assert_eq!(drafted.max_parallel_turns, 3);

        view.top_p = String::new();
        let drafted = view.draft().expect("blanco top_p is een keuze en geen fout");
        assert_eq!(drafted.top_p, None, "blanco betekent niet sturen");

        view.temperature = String::new();
        assert!(view.draft().is_err(), "blanco temperatuur mag niet stilletjes op 0,7 terugvallen");
        view.temperature = "0.7".to_string();
        view.max_parallel_turns = String::new();
        assert!(view.draft().is_err(), "blanco parallelstand mag niet stilletjes op 1 terugvallen");
        view.max_parallel_turns = "1".to_string();

        view.temperature = "3".to_string();
        assert!(view.draft().is_err(), "temperatuur boven 2 is geen smakelijkheid maar een typefout");
        view.temperature = "0.7".to_string();
        view.top_p = "1.5".to_string();
        assert!(view.draft().is_err(), "top_p boven 1 moet geweigerd worden");
        view.top_p = String::new();
        view.max_parallel_turns = "0".to_string();
        assert!(view.draft().is_err(), "nul beurten tegelijk is geen 'serieel' maar 'niets'");
    }

    #[test]
    fn de_zeven_vinkjes_reizen_naar_hun_eigen_veld() {
        let mut view = view();
        view.use_responses_api = false;
        view.git_allow_writes = true;
        view.use_server_session = false;
        let drafted = view.draft().expect("vinkjes veranderen mogen `draft` niet breken");
        assert!(!drafted.use_responses_api);
        assert!(drafted.git_allow_writes, "git-schrijven aan komt als aan terug");
        assert!(!drafted.use_server_session, "de checkbox wint van de oude voorraad");
        assert!(drafted.auto_approve_safe_commands);
        assert!(drafted.transcript_wrap);
        assert!(drafted.honesty_footer);
    }


    #[test]
    fn een_lijst_knop_neemt_het_besluit_en_nooit_de_halve_tekst() {
        let mut view = view();
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

    fn view() -> SettingsView {
        SettingsView {
            base_url: "http://127.0.0.1:8000/v1".to_string(),
            max_output_tokens: String::new(),
            api_key_env_var: baudet_settings::DEFAULT_KEY_ENV_VAR.to_string(),
            max_tool_iterations: baudet_settings::DEFAULT_MAX_TOOL_ITERATIONS.to_string(),
            stored_max_tool_iterations: baudet_settings::DEFAULT_MAX_TOOL_ITERATIONS,
            temperature: baudet_settings::config::DEFAULT_TEMPERATURE.to_string(),
            max_parallel_turns: baudet_settings::DEFAULT_MAX_PARALLEL_TURNS.to_string(),
            use_responses_api: true,
            store_responses: true,
            use_server_session: true,
            auto_approve_safe_commands: true,
            git_allow_writes: false,
            transcript_wrap: true,
            honesty_footer: true,
            ..SettingsView::default()
        }
    }

    #[test]
    fn de_gekozen_taal_reist_mee_naar_het_bestand() {
        let mut panel = view();
        assert_eq!(panel.language, Language::En, "een paneel zonder bestand begins bij Engels");
        panel.language = Language::Nl;
        assert_eq!(panel.draft().unwrap().language, Language::Nl);
        panel.adopt(&panel.draft().unwrap());
        assert_eq!(panel.language, Language::Nl);
        let dividers = crate::layout::Dividers { left: 1, right: 2, right_width: 4, files: 3 };
        assert_eq!(panel.geometry_draft(&dividers).unwrap().language, Language::Nl);
    }

    #[test]
    fn an_empty_ceiling_says_nothing_rather_than_guessing_one() {
        let settings = view().draft().unwrap();
        assert_eq!(settings.max_output_tokens, None);
        assert_eq!(settings.model, None);
        assert_eq!(settings.api_key, None);
    }

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
        let name = "BAUDET_FETCH_KEY_TEST_ENV";
        // This fixture uses a dedicated environment variable.
        unsafe { std::env::set_var(name, "a-fake-value-for-tests-only") };
        let mut panel = view();
        panel.api_key_env_var = name.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        let fetched = fetch_of(&mut panel, &tx, &rx);
        assert_eq!(fetched.0, "http://127.0.0.1:8000/v1");
        assert_eq!(fetched.1.as_deref(), Some("a-fake-value-for-tests-only"));

        panel.api_key = "typed-wins".to_string();
        assert_eq!(fetch_of(&mut panel, &tx, &rx).1.as_deref(), Some("typed-wins"));

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
        assert_eq!(view.fetched_for, None, "a failure must not claim the picker is current");

        view.set_endpoint("http://192.168.1.2:8000/v1");
        assert!(!view.listing_answered());
    }

    #[test]
    fn the_iteration_cap_survives_a_panel_round_trip() {
        let mut panel = view();
        panel.max_tool_iterations = "12".into();
        assert_eq!(panel.draft().unwrap().max_tool_iterations, 12);
    }

    #[test]
    fn a_blank_or_nonsense_cap_is_refused_rather_than_repaired() {
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
        let mut panel = view();
        panel.max_tool_iterations = "3".into();
        panel.stored_max_tool_iterations = 3;
        panel.max_tool_iterations.clear();
        panel.tool_mode = ToolMode::Off;
        assert_eq!(panel.draft().unwrap().max_tool_iterations, 3, "lus uit mag een limiet niet wissen");

        panel.tool_mode = ToolMode::Ask;
        assert!(panel.draft().is_err(), "terug aan hoort het blanco vakje te horen");
    }

    #[test]
    fn a_ceiling_that_cannot_be_a_number_is_refused_loudly() {
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
    fn a_row_with_a_window_still_saves_the_bare_id() {
        let mut panel = view();
        panel.models =
            vec![Model { id: "Qwen3.8-Flash-Next-oQ4e-mtp".to_string(), label: None, context_window: Some(262144) }];
        panel.model = Some("Qwen3.8-Flash-Next-oQ4e-mtp".to_string());
        assert_eq!(super::row_for(&panel.models[0]), "Qwen3.8-Flash-Next-oQ4e-mtp · 256k");
        let settings = panel.draft().unwrap();
        assert_eq!(settings.model.as_deref(), Some("Qwen3.8-Flash-Next-oQ4e-mtp"));
        assert!(!settings.model.as_deref().unwrap().contains(" · "), "a stored model id carries no row suffix");
    }

    #[test]
    fn a_listing_only_counts_when_it_answers_the_endpoint_still_on_screen() {
        let mut panel = view();
        panel.fetching = true;
        panel.apply_event(SettingsEvent::Listing(Ok(vec![model("a"), model("b")]), panel.base_url.clone()));
        assert_eq!(panel.models.len(), 2);
        assert!(!panel.fetching, "a finished fetch must free the button");
        assert!(panel.status.contains("2 models"), "{}", panel.status);
        assert_eq!(panel.fetched_for.as_deref(), Some("http://127.0.0.1:8000/v1"));

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

    #[test]
    fn a_fetched_window_shows_beside_the_name_it_belongs_to() {
        let windowed = |id: &str, tokens| Model { id: id.to_string(), label: None, context_window: tokens };
        assert_eq!(
            super::row_for(&windowed("Qwen3.8-Flash-Next-oQ4e-mtp", Some(262144))),
            "Qwen3.8-Flash-Next-oQ4e-mtp · 256k"
        );
        assert_eq!(super::row_for(&windowed("google/gemma-4-26b-a4b", None)), "google/gemma-4-26b-a4b");
        let named =
            Model { id: "qwen-turbo".to_string(), label: Some("Turbo".to_string()), context_window: Some(131072) };
        assert_eq!(super::row_for(&named), "Turbo · 128k");
    }
}
