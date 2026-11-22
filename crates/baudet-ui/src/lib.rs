//! Baudet's first phase: the transcript, with no backend process behind it.
//!
//! The reason this crate can exist before a transport does is the seam it drives. `Backend` is
//! object-safe and returns events over a channel, so a scripted player satisfies it exactly as a
//! socket would -- which is the same trick the JVM suite used to drive a whole turn without a
//! frame on screen. Swapping in a real adapter later is one `impl Backend` and no change here.
//!
//! Two boundaries are deliberate:
//!
//!  * rendering reads folded rows and nothing else. It never accumulates deltas into its own copy
//!    of the truth, because the reducer already does that and two copies disagree eventually;
//!  * the conversation owner lives on one worker thread, not in the paint pass. eframe's closure
//!    borrows the app for the duration of a frame, so an owner reachable only behind `&mut self`
//!    could not be shared with the thread that drains the stream; one owner plus two message
//!    channels is both simpler and free of lock ordering.

mod app;
// Publiek zodat tests/tabs.rs dat palet tekent en niet een standaard-`Visuals`: de vormen waarmee
// die test maten vergelijkt, zijn de vormen van dit venster.
pub mod palette;
// Publiek zodat tests/folder_open.rs het commit-pad (Workspace::open) buiten de lay-out
// kan meten: het mappadveld zit in een SidePanel-kolom en is niet root-adresseerbaar om in te
// typen, dus het commit-pad wordt hier rechtstreeks aangesproken.
pub mod panels;
mod rows;
mod settings;
// De linkersidebar: het domeinmodel erachter (sidebar.rs) en de riemen die het tekenen
// (panels::project_sidebar). Het model is publiek voor tests: een test die de rechtenregels, de
// cascade en de titelregels wil meten, doet dat op het model, niet door duizend pixels te klikken.
pub mod sidebar;

pub use app::{run, run_from, Startup, TranscriptApp};

// De adressen die tests/tabs.rs nodig heeft om te vragen "staat díe widget getekend". Alleen widgets
// met een expliciet id zijn van buiten te vinden — een `UiBuilder::id(..)`-wikkel met click-sense op
// `root_ui_id(ctx).with(..)`, zoals de tabknoppen (`TABSTRIP`) en de modelknop (`SELECTED_ROW`). Een
// `.id_salt`-veld adresseert relatief aan zijn ouder-`Ui` en is die keten hier bewust níét: geen
// enkele root-keten gaf dat adres (gemeten), dus de velden worden niet op adres getest.
pub use app::{root_ui_id, TABSTRIP};
pub use panels::FOLDER_OPEN;
// De intenties en het adres van de `Add project`-knop: tests/folder_open.rs en tests/project_tree.rs
// klikken op dat adres, dus het moet van buiten te noemen zijn, zoals `FOLDER_OPEN`.
pub use palette::DiffLine;
pub use panels::{SidebarIntent, SIDEBAR_ADD};
pub use rows::Row;
pub use sidebar::{Confirm, NameAsk, Selection, SidebarModel, SidebarOutcome};

// Het tabbladtype hoort bij de publiek testbare grens: tests/tabs.rs tekent er een na met dezelfde
// knoppen die een persoon indrukt, en heeft daar zowel de startkeus (`with_tab`) als de lezing van
// het antwoord (`tab()`) voor nodig.
pub use app::Tab;
pub use settings::{body as settings_body, SettingsCommand, SettingsEvent, SettingsView};
