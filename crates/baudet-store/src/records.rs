
use baudet_core::{context::GitMeta, Item, Msg};
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

impl SessionMeta {
    pub fn new(session_id: impl Into<String>) -> SessionMeta {
        SessionMeta {
            session_id: session_id.into(),
            started_at: None,
            cwd: None,
            git: None,
            model: None,
            thread_id: None,
        }
    }

    pub fn session(&self) -> Session {
        Session {
            session_id: self.session_id.clone(),
            cwd: self.cwd.clone(),
            git: self.git.clone(),
            model: self.model.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub session_id: String,
    pub cwd: Option<String>,
    pub git: Option<GitMeta>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum Record {
    SessionMeta(SessionMeta),
    Item(Item),
    Msg(Msg),
    CompactionCheckpoint(String),
    SessionDetail(SessionDetail),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDetail {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, rename = "prevResponseId", skip_serializing_if = "Option::is_none")]
    pub prev_response_id: Option<String>,
    #[serde(default)]
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_detail_line_omits_what_was_never_said() {
        let detail =
            SessionDetail {
                session_id: "s-x".into(),
                model: None,
                prev_response_id: None,
                seq: 0,
                summary: None,
                project_id: None,
            };
        let json = serde_json::to_string(&Record::SessionDetail(detail)).unwrap();
        assert_eq!(json, r#"{"type":"session_detail","payload":{"session_id":"s-x","seq":0}}"#);
    }

    #[test]
    fn a_detail_line_survives_a_field_from_next_week() {
        let detail = SessionDetail {
            session_id: "s-x".into(),
            model: Some("gemma".into()),
            prev_response_id: Some("resp-9".into()),
            seq: 12,
            summary: Some("samenvatting".into()),
            project_id: Some("p-1".into()),
        };
        let mut value = serde_json::to_value(Record::SessionDetail(detail.clone())).unwrap();
        value["payload"]["a_field_from_next_week"] = serde_json::json!(true);
        assert_eq!(serde_json::from_value::<Record>(value).unwrap(), Record::SessionDetail(detail));
    }

    #[test]
    fn an_older_detail_line_reads_with_missing_fields_absent() {
        let record: Record =
            serde_json::from_str(r#"{"type":"session_detail","payload":{"session_id":"s-x","seq":3}}"#).unwrap();
        let Record::SessionDetail(detail) = record else { panic!("verwacht een detail-regel") };
        assert_eq!(detail.seq, 3);
        assert_eq!(detail.summary, None);
        assert_eq!(detail.prev_response_id, None);
        assert_eq!(detail.project_id, None);
    }

    use super::*;
    use baudet_core::{ItemStatus, UserMessageItem};

    #[test]
    fn a_record_is_one_tagged_object() {
        let line = serde_json::to_string(&Record::Msg(Msg::user("hallo"))).unwrap();
        assert_eq!(line, r#"{"type":"msg","payload":{"role":"user","content":"hallo"}}"#);
    }

    #[test]
    fn a_session_meta_round_trips_its_provenance() {
        let meta = SessionMeta {
            session_id: "s-abcd1234".into(),
            started_at: Some("2026-11-19T09:00:00Z".into()),
            cwd: Some("/tmp/repo".into()),
            git: Some(GitMeta {
                repository_url: Some("git@github.com:jbrugman/baudet.git".into()),
                branch: Some("main".into()),
                commit_hash: Some("4baee70".into()),
            }),
            model: None,
            thread_id: None,
        };
        let json = serde_json::to_string(&Record::SessionMeta(meta.clone())).unwrap();
        let back: Record = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Record::SessionMeta(meta));
        assert!(!json.contains("model"));
        assert!(!json.contains("null"));
    }

    #[test]
    fn a_known_kind_still_decodes_beside_an_unknown_field() {
        let item = Record::Item(Item::UserMessage(UserMessageItem {
            item_id: "tu_1/user".into(),
            text: "weetjes".into(),
            status: ItemStatus::Completed,
        }));
        let mut value = serde_json::to_value(&item).unwrap();
        value["payload"]["a_field_from_next_week"] = serde_json::json!(42);
        assert_eq!(serde_json::from_value::<Record>(value).unwrap(), item);
    }
}
