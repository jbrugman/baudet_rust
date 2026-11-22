//! De bitwaaier achter de maprechten, exact zoals `project/Permissions.kt`.
//!
//! De rechten zijn een eigendom van deze app, geen OS- of TCC-recht. Read wordt nu afgedwongen;
//! Write en Execute worden vastgelegd zodat de tools later dezelfde catalogus lezen zonder het
//! schema te verbouwen.

use serde::{Deserialize, Serialize};

/// Een recht dat Baudet zichzelf op een projectmap geeft. De bit is de opslag, niet een volgorde.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FolderPermission {
    Read,
    Write,
    Execute,
}

impl FolderPermission {
    pub const fn bit(self) -> i32 {
        match self {
            Self::Read => 1,
            Self::Write => 2,
            Self::Execute => 4,
        }
    }

    pub const fn all() -> [Self; 3] {
        [Self::Read, Self::Write, Self::Execute]
    }
}

/// Zuivere bitmutatie: exact de contracten van de Kotlin-objecten, zonder database.
pub struct Permissions;

impl Permissions {
    pub fn encode<const N: usize>(permissions: [FolderPermission; N]) -> i32 {
        permissions.into_iter().fold(0, |acc, permission| acc | permission.bit())
    }

    pub fn decode(bits: i32) -> Vec<FolderPermission> {
        FolderPermission::all().into_iter().filter(|permission| Self::has(bits, *permission)).collect()
    }

    pub const fn has(bits: i32, permission: FolderPermission) -> bool {
        bits & permission.bit() != 0
    }

    pub const fn with(bits: i32, permission: FolderPermission, granted: bool) -> i32 {
        if granted {
            bits | permission.bit()
        } else {
            bits & !permission.bit()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leege_rechten_zijn_nul_en_nul_blijft_leeg() {
        assert_eq!(Permissions::encode([]), 0);
        assert!(Permissions::decode(0).is_empty());
    }

    #[test]
    fn read_write_execute_houden_elk_hun_eigen_bit() {
        assert_eq!(FolderPermission::Read.bit(), 1);
        assert_eq!(FolderPermission::Write.bit(), 2);
        assert_eq!(FolderPermission::Execute.bit(), 4);
        for permission in FolderPermission::all() {
            assert_eq!(Permissions::decode(Permissions::encode([permission])), vec![permission]);
        }
    }

    #[test]
    fn een_toegevoegd_recht_blijft_terwijl_ander_uitgaat() {
        let read = Permissions::encode([FolderPermission::Read]);
        let write = Permissions::with(read, FolderPermission::Write, true);
        let without_read = Permissions::with(write, FolderPermission::Read, false);
        assert!(Permissions::has(write, FolderPermission::Write));
        assert!(!Permissions::has(without_read, FolderPermission::Read));
        assert!(Permissions::has(without_read, FolderPermission::Write));
    }
}
