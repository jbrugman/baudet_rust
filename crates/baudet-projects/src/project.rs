
use crate::permissions::{FolderPermission, Permissions};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_path: Option<String>,
    #[serde(default, deserialize_with = "read_permissions")]
    pub permissions: i32,
    #[serde(default)]
    pub archived: bool,
    pub created_at: i64,
}

fn read_permissions<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i32, D::Error> {
    i32::deserialize(deserializer).map(Permissions::normalize)
}

impl Project {
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_a_saved_project_with_read_and_execute_when_loaded_then_it_keeps_only_read() {
        let project: Project = serde_json::from_str(
            r#"{"id":"project","name":"Fixture","permissions":5,"created_at":0}"#,
        ).unwrap();
        assert_eq!(project.granted(), [FolderPermission::Read]);
        assert_eq!(project.permissions, FolderPermission::Read.bit());
    }
}
