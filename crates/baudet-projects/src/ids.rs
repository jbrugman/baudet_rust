//! Lokale id's in de Kotlin-vorm: `p-` of `s-` plus acht kleine hex-tekens.
//!
//! UUID is hier niet nodig: de eis is lokale uniciteit, niet een niet-raadbare identifier. Een
//! monotone klok en een procesloper voorkomen dat twee sessies in hetzelfde proces
//! dezelfde id krijgen; de vertrouwde `s-...`-vorm van de rolloutlaag blijft behouden.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Een id voor een nieuw project.
pub fn new_project_id() -> String {
    format!("p-{}", unique_suffix())
}

/// Een id voor een nieuw gesprek.
pub fn new_session_id() -> String {
    format!("s-{}", unique_suffix())
}

fn unique_suffix() -> String {
    let nanos =
        SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_nanos() as u64).unwrap_or_default();
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let raw = (nanos.rotate_left(17) ^ counter.wrapping_mul(0x9e37_79b9_7f4a_7c15)) as u32;
    format!("{raw:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn een_id_heeft_de_vertrouwde_voork_en_acht_tekens() {
        let project = new_project_id();
        let session = new_session_id();
        assert!(project.starts_with("p-"));
        assert_eq!(project[2..].len(), 8);
        assert!(session.starts_with("s-"));
        assert_eq!(session[2..].len(), 8);
    }

    #[test]
    fn snelle_opeenvolging_geeft_twee_id() {
        assert_ne!(new_session_id(), new_session_id());
    }
}
