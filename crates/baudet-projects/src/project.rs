//! Een project groepeert gesprekken en mag naar een map wijzen, zoals `project/Project.kt`.

use crate::permissions::{FolderPermission, Permissions};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_path: Option<String>,
    #[serde(default)]
    pub permissions: i32,
    #[serde(default)]
    pub archived: bool,
    pub created_at: i64,
}

impl Project {
    /// Een project zonder map en zonder rechten, precies zoals `createProject` het aanmaakt.
    pub fn new(id: String, name: impl Into<String>, created_at: i64) -> Self {
        Self { id, name: name.into().trim().to_owned(), folder_path: None, permissions: 0, archived: false, created_at }
    }

    pub fn granted(&self) -> Vec<FolderPermission> {
        Permissions::decode(self.permissions)
    }

    pub fn is_granted(&self, permission: FolderPermission) -> bool {
        Permissions::has(self.permissions, permission)
    }
}
