
use serde::{Deserialize, Serialize};

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

pub struct Permissions;

impl Permissions {
    pub fn encode<const N: usize>(permissions: [FolderPermission; N]) -> i32 {
        permissions.into_iter().fold(0, |bits, permission| Self::with(bits, permission, true))
    }

    pub fn decode(bits: i32) -> Vec<FolderPermission> {
        FolderPermission::all().into_iter().filter(|permission| Self::has(bits, *permission)).collect()
    }

    pub const fn normalize(bits: i32) -> i32 {
        let read = FolderPermission::Read.bit();
        let write = FolderPermission::Write.bit();
        let execute = FolderPermission::Execute.bit();
        if bits & read == 0 {
            0
        } else if bits & write == 0 {
            read
        } else {
            bits & (read | write | execute)
        }
    }

    pub const fn has(bits: i32, permission: FolderPermission) -> bool {
        Self::normalize(bits) & permission.bit() != 0
    }

    pub const fn with(bits: i32, permission: FolderPermission, granted: bool) -> i32 {
        let bits = Self::normalize(bits);
        if granted {
            bits | (permission.bit() * 2 - 1)
        } else {
            Self::normalize(bits & !permission.bit())
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
    fn given_a_grant_when_write_or_execute_is_enabled_then_prerequisites_are_granted() {
        use FolderPermission::*;
        assert_eq!(Permissions::decode(Permissions::encode([Write])), [Read, Write]);
        assert_eq!(Permissions::decode(Permissions::encode([Execute])), [Read, Write, Execute]);
    }

    #[test]
    fn given_execute_access_when_a_prerequisite_is_revoked_then_dependent_access_is_removed() {
        use FolderPermission::*;
        let bits = Permissions::encode([Execute]);
        assert_eq!(Permissions::decode(Permissions::with(bits, Write, false)), [Read]);
        assert!(Permissions::decode(Permissions::with(bits, Read, false)).is_empty());
    }

    #[test]
    fn given_legacy_independent_bits_when_read_then_no_ungranted_access_is_added() {
        use FolderPermission::*;
        assert_eq!(Permissions::decode(Read.bit() | Execute.bit()), [Read]);
        assert!(Permissions::decode(Write.bit() | Execute.bit()).is_empty());
    }
}
