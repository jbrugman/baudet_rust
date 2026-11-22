//! De catalogus achter de linkersidebar: projecten en hun sessie-index.
//!
//! Dit is bewust niet de transcriptie. De oorspronkelijke H2Store bewaart beide in één database,
//! maar de Rust-poort scheidt de verantwoordelijkheden: `baudet-store` is de append-only bron van
//! één gesprek, terwijl deze crate alleen de kleine, herberekenbare catalogus bijhoudt die nodig is om
//! projecten en sessies te tekenen. Daardoor krijgt de sidebar een echte domain-laag zonder dat de
//! rolloutlaag een project-id door haar berichtenprotocol moet slepen.
//!
//! De grens is ook een migratiebesluit: H2 is JVM-only en rusqlite staat niet in de offline crate-
//! boom. JSON is hier geen toevlucht, maar de vorm die past bij de bestaande, leesbare rollouts.

pub mod catalog;
pub mod ids;
pub mod permissions;
pub mod project;
pub mod tool_gate;

pub use catalog::{Catalog, CatalogError, SessionSummary};
pub use ids::{new_project_id, new_session_id};
pub use permissions::{FolderPermission, Permissions};
pub use project::Project;
pub use tool_gate::{decide, gate_for_session, ToolGate};
