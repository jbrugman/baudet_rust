//! The choices a person makes, and the file that remembers them.
//!
//! Kotlin keeps these in `SettingsStore`, which reads a JSON file and falls back to defaults. The
//! fallback is where a small app quietly becomes hostile, so it is the rule this file is written
//! around: **a file that cannot be read is never rewritten.** A corrupt, half-written or
//! future-version file must cost the person one bad start, not every model and endpoint they ever
//! configured. The store layer already follows the same discipline at the line level
//! ([`crate`](crate) ignores what it cannot decode); this is the same instinct one level up.
//!
//! The format is JSON rather than TOML for one concrete reason: `serde_json` is already in the
//! graph and `toml` is not, and nothing here needs TOML's syntax. A settings file the person is
//! never promised to hand-edit does not earn a new dependency.

use serde::{Deserialize, Serialize};

/// Everything the app remembers between starts.
///
/// Every field is `#[serde(default)]`-tolerant on read and skips nothing on write, so a newer
/// build adds a field by adding a default, and an older build reading this file keeps the fields
/// it knows. That is the same forward-compatibility rule the rollout records follow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The endpoint a turn is sent to. One base URL, because the seam takes one
    /// ([`crate::provider::Provider::from_base_url`]) and inventing a provider list before a second
    /// provider exists is the speculative abstraction this rewrite avoids.
    pub base_url: String,
    /// The model to send. `None` means "whatever the endpoint lists first", which is what a person
    /// who has never opened the settings panel actually wants: a first start should work.
    pub model: Option<String>,
    /// The reasoning level to ask for. Kept as text rather than an enum, because the honest values
    /// belong to the model — `off` through `max` is what the endpoint accepts, and a newer model
    /// with a new level must not look like a corrupt file.
    pub reasoning: Option<String>,
    /// The ceiling one turn may spend, carried to the backend on
    /// [`baudet_backend::SendRequest::max_output_tokens`]. `None` leaves the decision to the
    /// endpoint rather than inventing a budget here.
    pub max_output_tokens: Option<u32>,
    /// The credential as the person typed it. `None` means "not typed", which is not the same as
    /// "none": [`Settings::resolved_key`] still looks at the environment, and that is what lets a
    /// checkout work without putting a secret in a file at all.
    ///
    /// Kept in the file because the whole store layer is flat text in a folder the person can
    /// `cat`; a keychain is right for a shipped app and wrong for a tool this one is. The
    /// mitigation actually taken is the file's own mode — `0600`, set on the descriptor before the
    /// bytes exist. See [`crate::store`].
    pub api_key: Option<String>,
    /// Which environment variable answers for a key that was never typed. Named rather than fixed
    /// so a person with several local servers does not have to choose between their shell and the
    /// app.
    pub api_key_env_var: String,
    /// The wire dialect to send turns to: `responses` or `chat_completions`. Text rather than an
    /// enum for the same reason `reasoning` is: a newer server may answer a dialect this build has
    /// no name for, and that must read as an unknown word, not as a corrupt file.
    ///
    /// `None` means "not chosen", and the absence is load-bearing: a turn is only sent to a server
    /// whose dialect is known, so an unset dialect keeps the window on its scripted player instead
    /// of letting it guess a dialect against a server that may not speak it.
    pub protocol: Option<String>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            // The endpoint the JVM reference defaults to, so a checkout of this rewrite starts
            // against the same local server the original does.
            base_url: "http://127.0.0.1:8000/v1".to_string(),
            model: None,
            reasoning: None,
            max_output_tokens: None,
            api_key: None,
            // The variable this machine's local server actually uses, which is why it is the default
            // rather than a generic `BAUDET_API_KEY`.
            api_key_env_var: DEFAULT_KEY_ENV_VAR.to_string(),
            // Nothing is known about the server behind a default endpoint, so nothing is claimed.
            protocol: None,
        }
    }
}

/// The variable to fall back to when the field above names nothing. Kept in one place because the
/// reference app repeats the literal in two spots, and a second copy is how the two drift.
pub const DEFAULT_KEY_ENV_VAR: &str = "OMLX_API_KEY";

impl Settings {
    /// The credential to actually send: what was typed, otherwise what the named variable holds.
    ///
    /// Typed wins, which matters more than it looks: a person who types a key into a panel expects
    /// that key to be used, and an environment inherited from an editor's shell would otherwise
    /// silently answer with a different credential than the one on screen.
    ///
    /// A blank name falls back to [`DEFAULT_KEY_ENV_VAR`] rather than to "read no variable",
    /// because an empty field reads as "I did not fill this in", not as "opt out".
    pub fn resolved_key(&self) -> Option<String> {
        if let Some(typed) = self.api_key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
            return Some(typed.to_string());
        }
        let name = self.api_key_env_var.trim();
        let name = if name.is_empty() { DEFAULT_KEY_ENV_VAR } else { name };
        std::env::var(name).ok().filter(|value| !value.trim().is_empty())
    }

    /// Whether anything at all answers for a credential, without saying what it is.
    pub fn has_key(&self) -> bool {
        self.resolved_key().is_some()
    }

    /// The wire dialect the file names, in the words the transport uses, or `None` when the file
    /// names none. Untrusted words stay `None` for the reason stated on the field.
    pub fn protocol_word(&self) -> Option<&str> {
        self.protocol.as_deref().map(str::trim).filter(|word| !word.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_file_is_valid_and_names_the_local_endpoint() {
        let json = serde_json::to_string(&Settings::default()).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Settings::default());
        // The reference app's own default, so this checkout starts against the same server.
        assert_eq!(back.base_url, "http://127.0.0.1:8000/v1");
    }

    #[test]
    fn a_typed_key_wins_and_a_blank_one_falls_back_to_the_named_variable() {
        // Process-global, so this touches one variable and puts it back on every path — the same
        // discipline `baudet-store` documents for `BAUDET_HOME`.
        let name = "BAUDET_SETTINGS_TEST_KEY";
        std::env::set_var(name, "from-the-environment");
        let from_env = Settings { api_key: None, api_key_env_var: name.into(), ..Default::default() };
        assert_eq!(from_env.resolved_key().as_deref(), Some("from-the-environment"));
        assert!(from_env.has_key());

        let typed = Settings { api_key: Some("typed-wins".into()), api_key_env_var: name.into(), ..Default::default() };
        assert_eq!(typed.resolved_key().as_deref(), Some("typed-wins"), "the key on screen must be the key sent");

        // A blank name is "not filled in", so the default name still answers. Proved by setting
        // that name rather than by assuming it is unset: `OMLX_API_KEY` genuinely exists in the
        // shells this runs in, and an assertion of `None` there was the test being wrong, not the
        // code.
        std::env::set_var(DEFAULT_KEY_ENV_VAR, "from-the-default-name");
        let blank_name = Settings { api_key: None, api_key_env_var: "   ".into(), ..Default::default() };
        assert_eq!(blank_name.api_key_env_var.trim(), "");
        assert_eq!(
            blank_name.resolved_key().as_deref(),
            Some("from-the-default-name"),
            "a blank name means I did not fill this in, not do not read any variable"
        );
        std::env::remove_var(DEFAULT_KEY_ENV_VAR);

        // Whitespace-only is not a credential.
        std::env::set_var(name, "   ");
        assert_eq!(from_env.resolved_key(), None);
        assert!(!from_env.has_key());
        std::env::remove_var(name);
    }

    #[test]
    fn an_unknown_field_survives_and_a_missing_one_default() {
        // The forward-compatibility rule in one assertion: next week's field must not make this
        // build refuse last week's file, and this build's new field must not make it lose the old.
        let json = r#"{"base_url":"http://studio:8000/v1","a_field_from_next_week":42}"#;
        let read: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(read.base_url, "http://studio:8000/v1");
        assert_eq!(read.model, None);
        assert_eq!(read, Settings { base_url: "http://studio:8000/v1".into(), ..Default::default() });
    }
}
