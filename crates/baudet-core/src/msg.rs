
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// Model history and the display trail have different contents.
pub struct Msg {
    pub role: String,
    pub content: String,
    #[serde(default, rename = "responseId", skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
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
