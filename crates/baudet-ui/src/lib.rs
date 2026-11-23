//! The egui desktop application.

mod app;
mod approval;
mod diff_view;
mod markdown;
pub mod clipboard;
pub mod palette;
pub mod panels;
mod rows;
mod settings;
pub mod layout;
pub mod sidebar;

pub use app::{chosen_backend, run, run_from, status_balk, with_chosen_backend, Startup, TranscriptApp};

pub use app::{root_ui_id, tab_size, Tab, TABSTRIP, TAB_GAP};
pub use panels::{NO_FOLDER, NO_READ, ROW_MENU, ROW_MENU_LINK, TREE_REFRESH};
pub use layout::{resolve_divider, worth_remembering, Dividers};
pub use palette::DiffLine;
pub use panels::{SidebarIntent, ASK_OK, CONFIRM_YES, PANEL_TOGGLES, SIDEBAR_ADD};
pub use rows::Row;
pub use sidebar::{Confirm, NameAsk, Selection, SidebarModel, SidebarOutcome};

pub use settings::{
    body as settings_body, SettingsCommand, SettingsEvent, SettingsView, FIELD_ENDPOINT, GROUP_GAP, ROW_GAP,
};
