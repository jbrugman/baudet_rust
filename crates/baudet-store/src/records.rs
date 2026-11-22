//! What one line of a rollout file can be.
//!
//! The shape is the harness's own, which I measured rather than guessed: one JSON object per
//! line, `type` naming a `payload`, so a file stays readable with `head` and a writer never has
//! to rewrite it. Kotlin keeps the same facts in three H2 tables (`sessions`, `messages`, the
//! item trail behind `TranscriptStore`); here they are three record kinds, because the append
//! discipline is identical in both and one decoder per kind beats one decoder for everything.
//!
//! Every optional field is `#[serde(default)]`-tolerant on read and skips `None` on write. That is
//! the forward-compatibility rule the stored config already follows: a reader must not fail on a
//! file written by a newer build, and a file must not carry nulls for fields it never had.

use baudet_core::{context::GitMeta, Item, Msg};
use serde::Deserialize;
use serde::Serialize;

/// The identity and provenance of one conversation, written once, as the file's first line.
///
/// `cwd` and `git` are here because a transcript of a coding conversation is meaningless without
/// knowing which checkout it happened in, and because the harness does exactly this: its
/// `session_meta` carries `cwd` plus a `git` block. They are recorded, never inferred at read
/// time — a file opened three weeks later from a moved directory must still say where it ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub session_id: String,
    /// When the conversation started, as RFC 3339 UTC. Kept as text so a rollout file stays
    /// greppable; no date-time crate for one field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// The folder the conversation ran in, absolute. `None` only when the caller has no project
    /// in mind, which is the "chat about nothing in particular" case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The checkout `cwd` pointed at, as observed when the session opened. `None` outside a work
    /// tree, and also `None` when git was missing or slow: an unreadable repo is not a reason to
    /// lose a transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitMeta>,
    /// Which model answered. Optional because the GUI phase has none: a scripted turn must stay
    /// recordable without inventing a model name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The app-server thread this conversation maps onto, once a real transport exists. Recorded
    /// so a session can be resumed server-side instead of only replayed from here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

impl SessionMeta {
    /// A session identified only by its id, for callers with no project context to record.
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

    /// The same session in the shape the rest of the app works with. Splitting identity from
    /// write-once provenance keeps [`crate::rollout::Rollout`] from re-deriving fields it was handed.
    pub fn session(&self) -> Session {
        Session {
            session_id: self.session_id.clone(),
            cwd: self.cwd.clone(),
            git: self.git.clone(),
            model: self.model.clone(),
        }
    }
}

/// The parts of [`SessionMeta`] a reader still needs after the first line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub session_id: String,
    pub cwd: Option<String>,
    pub git: Option<GitMeta>,
    pub model: Option<String>,
}

/// What one line holds.
///
/// The tags are the file's contract, so they are lowercase and stable: `session_meta` keeps the
/// harness's spelling, `item` and `msg` name what they carry. Adding a kind later is additive; a
/// reader already ignores what it cannot decode, so an old build opens a newer file by dropping
/// the kinds it does not know rather than refusing the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum Record {
    /// Write-once identity, first line only.
    SessionMeta(SessionMeta),
    /// A display row, exactly as the interface rendered it. Reasoning included on purpose: the
    /// trail is an audit of what was shown. What is *replayed* is decided by
    /// [`crate::history::history_of`], not by what is stored.
    Item(Item),
    /// A turn in the model's own vocabulary: what actually went over the wire.
    Msg(Msg),
    /// The mutable head of a conversation: model, server-side session wire, compaction summary.
    /// A separate kind rather than more fields on [`SessionMeta`], because identity is written
    /// once and this line is rewritten on every persist; a whole-file rewrite would put a
    /// turn's outcome in the same line as the provenance that must never change.
    SessionDetail(SessionDetail),
}

/// The mutable facts about a conversation, one rewritten line per persist.
///
/// Kotlin keeps these as columns of its `sessions` row and updates them with a `MERGE`
/// (`H2Store.kt:67`). Here it is the last `session_detail` line of the session directory, so the
/// append-only rule of the other files is not broken and a crash leaves the previous head
/// readable rather than a half-written replacement.
///
/// `seq` is a count, not an offset into a file: it is the number of turns the conversation had
/// when this line was written, which is the boundary a later [`crate::session::Session`] needs
/// to know in order to append only its own tail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDetail {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The wire to the server-side session (`previous_response_id`). `None` means this app sends
    /// the history itself, which is the honest default rather than a missing value.
    #[serde(default, rename = "prevResponseId", skip_serializing_if = "Option::is_none")]
    pub prev_response_id: Option<String>,
    #[serde(default)]
    pub seq: u64,
    /// The compaction digest. Written on every persist and not only after a compaction, because
    /// the older turns it replaces are already gone: losing only the summary would drop that
    /// context for good.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Het project waaraan dit gesprek is gekoppeld; `None` is de Unassigned-bak.
    ///
    /// Het veld bestond niet, dus `AppState::on_session` moest het op `None` zetten en wist een
    /// bewuste koppeling weg bij elke herstart — de kopregel was de enige plaats waar het had
    /// kunnen staan. `serde(default)` houdt een bestand van toen-diens leesbaar: een ontbrekend
    /// veld is "in de bak", niet "kapot", en dat is precies wat het betekende.
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
        // `seq` is a count and so always present; the three optional fields are absent rather
        // than null, which is the file-shape rule of this crate.
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
        // A line written before the summary existed must read as "no summary", not fail: the
        // tolerance that lets a new build open an old conversation.
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
        // `tag` + `content` puts the payload under `payload`, so `jq '.type'` alone sorts a
        // rollout file by kind — the property that makes flat text worth having at all.
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
        // An absent model is absent in the file, not null.
        assert!(!json.contains("model"));
        assert!(!json.contains("null"));
    }

    #[test]
    fn a_known_kind_still_decodes_beside_an_unknown_field() {
        // The line-level tolerance is asserted in rollout.rs. What is worth pinning here is that a
        // known kind survives an extra field, because a newer build will add fields to these very
        // payloads and an old reader must keep its own records rather than drop the line.
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
