//! The turn as a model is asked to remember it.
//!
//! [`Msg`] is the *wire* vector; [`crate::Item`] is the *display* vector. They are deliberately
//! not one type, and the reason is measured rather than theoretical: in a real harness rollout,
//! 587 stored response items correspond to 399 completed display rows. The difference is tool
//! calls and their output — work that is real but never rendered as prose. Deriving a request from
//! rows therefore does not crash; it quietly under-sends, and every scripted test still passes.
//! Keeping two types is what makes that mistake impossible to make silently.
//!
//! Translated from `dev.baudet.session.Msg`, including its optionality: `response_id` and
//! `prompt_tokens` are known only when the backend actually reported them, and inventing a value
//! there would make a resumed chain look authoritative when it is a guess.

use serde::Deserialize;
use serde::Serialize;

/// One turn in the conversation a model is given.
///
/// `role` stays an open string rather than an enum: this crosses a wire whose vocabulary is not
/// ours to police, and a system or tool role arriving from a backend must round-trip instead of
/// failing to deserialise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Msg {
    pub role: String,
    pub content: String,
    /// The backend's id for the response this turn produced, kept so a chain can be resumed
    /// server-side rather than replayed from here.
    #[serde(default, rename = "responseId", skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    /// What the backend said this turn cost to prompt, when it said so. `None` stays `None`:
    /// an estimate reported as an accounting figure is the kind of lie this app exists to stop.
    #[serde(default, rename = "promptTokens", skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
}

impl Msg {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Msg {
        Msg { role: role.into(), content: content.into(), response_id: None, prompt_tokens: None }
    }

    pub fn user(content: impl Into<String>) -> Msg {
        Msg::new("user", content)
    }

    pub fn assistant(content: impl Into<String>) -> Msg {
        Msg::new("assistant", content)
    }

    /// Whether this turn was answered by a backend that reported an id, which is what makes a
    /// server-side chain possible at all.
    pub fn chained(&self) -> bool {
        self.response_id.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_without_backend_answers_stays_silent_about_them() {
        let json = serde_json::to_string(&Msg::user("hallo")).unwrap();
        assert_eq!(json, r#"{"role":"user","content":"hallo"}"#);
        assert!(!Msg::user("hallo").chained());
    }

    #[test]
    fn backend_answers_round_trip_under_their_wire_spelling() {
        let msg = Msg {
            role: "assistant".into(),
            content: "antwoord".into(),
            response_id: Some("resp_7".into()),
            prompt_tokens: Some(1234),
        };
        let json = serde_json::to_string(&msg).unwrap();
        // camelCase is the Codex wire spelling, matching how ItemStatus and the item tags are
        // already named in this crate; a resume must read back what the backend called it.
        assert!(json.contains("\"responseId\":\"resp_7\""), "{json}");
        assert!(json.contains("\"promptTokens\":1234"), "{json}");
        assert_eq!(serde_json::from_str::<Msg>(&json).unwrap(), msg);
        assert!(msg.chained());
    }

    #[test]
    fn an_unknown_role_survives_a_round_trip() {
        let msg: Msg = serde_json::from_str(r#"{"role":"developer","content":"x"}"#).unwrap();
        assert_eq!(msg.role, "developer");
        assert_eq!(msg.prompt_tokens, None, "absent is absent, not zero");
    }
}
