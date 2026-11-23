
use crate::config::Settings;
use serde::Deserialize;
use std::time::Duration;

pub const LISTING_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub id: String,
    pub label: Option<String>,
    pub context_window: Option<u32>,
}

impl Model {
    pub fn display(&self) -> &str {
        self.label.as_deref().filter(|l| !l.trim().is_empty()).unwrap_or(&self.id)
    }

    pub fn window_label(&self) -> Option<String> {
        let tokens = self.context_window?;
        if tokens < 1024 {
            return Some(tokens.to_string());
        }
        let mut power: u64 = 1024;
        while power < tokens as u64 {
            power *= 2;
        }
        let lower = power / 2;
        let nearest = if lower >= 1024 && (tokens as u64) - lower < power - (tokens as u64) { lower } else { power };
        let kibibytes = nearest / 1024;
        if kibibytes >= 1024 && kibibytes % 1024 == 0 {
            return Some(format!("{}m", kibibytes / 1024));
        }
        Some(format!("{kibibytes}k"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ListingError {
    #[error("no usable endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("the endpoint did not answer within {} s", .0.as_secs())]
    Timeout(Duration),
    #[error("could not reach {endpoint}: {detail}")]
    Unreachable { endpoint: String, detail: String },
    #[error("{endpoint} answered {status}: {detail}")]
    NotAListing { endpoint: String, status: u16, detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    base_url: String,
    api_key: Option<String>,
}

impl Provider {
    pub fn from_base_url(raw: &str) -> Result<Provider, ListingError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ListingError::InvalidEndpoint("the endpoint field is empty".into()));
        }
        let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            trimmed.to_string()
        } else {
            format!("http://{trimmed}")
        };
        let without_slash = with_scheme.trim_end_matches('/').to_string();
        let base = match url_without_version(&without_slash) {
            Some(already) => already,
            None => format!("{without_slash}/v1"),
        };
        Ok(Provider { base_url: base, api_key: None })
    }

    pub fn from_settings(settings: &Settings) -> Result<Provider, ListingError> {
        let mut provider = Provider::from_base_url(&settings.base_url)?;
        provider.api_key = settings.resolved_key();
        Ok(provider)
    }

    pub fn with_key(base_url: &str, api_key: Option<String>) -> Result<Provider, ListingError> {
        let mut provider = Provider::from_base_url(base_url)?;
        provider.api_key = api_key.filter(|k| !k.trim().is_empty());
        Ok(provider)
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    pub fn models_url(&self) -> String {
        format!("{}/models", self.base_url)
    }

    pub fn list_models(&self) -> Result<Vec<Model>, ListingError> {
        let url = self.models_url();
        let client = match reqwest::blocking::Client::builder().timeout(LISTING_TIMEOUT).build() {
            Ok(client) => client,
            Err(error) => return Err(ListingError::Unreachable { endpoint: url, detail: error.to_string() }),
        };
        let mut request = client.get(&url);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let response = match request.send() {
            Ok(response) => response,
            Err(error) => {
                if error.is_timeout() {
                    return Err(ListingError::Timeout(LISTING_TIMEOUT));
                }
                return Err(ListingError::Unreachable { endpoint: url, detail: error.to_string() });
            }
        };
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let detail = response.text().unwrap_or_default();
            let detail = self.redact(detail.trim());
            return Err(ListingError::NotAListing { endpoint: url, status, detail: shorten(&detail) });
        }
        let body = match response.text() {
            Ok(body) => body,
            Err(error) => {
                return Err(ListingError::NotAListing { endpoint: url, status, detail: shorten(&error.to_string()) })
            }
        };
        parse_listing(&body).map_err(|detail| ListingError::NotAListing {
            endpoint: url,
            status,
            detail: shorten(&self.redact(&detail)),
        })
    }
}

#[derive(Deserialize)]
struct Listing {
    data: Option<Vec<Entry>>,
    models: Option<Vec<Entry>>,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "context_length", alias = "max_context_length", alias = "max_model_len")]
    context_window: Option<u32>,
}

fn parse_listing(body: &str) -> Result<Vec<Model>, String> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|e| format!("not JSON: {e}"))?;
    let entries: Vec<Entry> = if value.is_array() {
        serde_json::from_value(value).map_err(|e| format!("not a model list: {e}"))?
    } else {
        let listing = serde_json::from_value::<Listing>(value).map_err(|e| format!("not a model list: {e}"))?;
        match (listing.data, listing.models) {
            (Some(entries), _) | (None, Some(entries)) => entries,
            (None, None) => Vec::new(),
        }
    };
    let mut models: Vec<Model> = entries
        .into_iter()
        .map(|entry| Model {
            id: entry.id,
            label: entry.name.filter(|n| !n.trim().is_empty()),
            context_window: entry.context_window,
        })
        .filter(|model| !model.id.trim().is_empty())
        .collect();
    models.sort_by_key(|model| model.id.to_lowercase());
    models.dedup_by(|a, b| a.id == b.id);
    Ok(models)
}

fn url_without_version(without_slash: &str) -> Option<String> {
    let after_scheme = without_slash.find("://").map(|i| &without_slash[i + 3..])?;
    let path_start = after_scheme.find('/')?;
    (path_start > 0).then(|| without_slash.to_string())
}

impl Provider {
    fn redact(&self, detail: &str) -> String {
        redact(detail, self.api_key.as_deref())
    }
}

fn redact(detail: &str, key: Option<&str>) -> String {
    match key.map(str::trim).filter(|k| !k.is_empty()) {
        None => detail.to_string(),
        Some(key) => detail.replace(key, "[redacted]"),
    }
}

fn shorten(text: &str) -> String {
    const LIMIT: usize = 100;
    match text.char_indices().nth(LIMIT) {
        None => text.to_string(),
        Some(_) => format!("{}…", text.chars().take(LIMIT).collect::<String>()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(raw: &str) -> String {
        Provider::from_base_url(raw).expect("must normalise, not refuse").models_url()
    }

    #[test]
    fn the_shapes_a_person_types_all_reach_the_same_listing_url() {
        for raw in ["http://127.0.0.1:8000/v1", "http://127.0.0.1:8000/v1/", "  http://127.0.0.1:8000/v1  "] {
            assert_eq!(urls(raw), "http://127.0.0.1:8000/v1/models", "{raw}");
        }
    }

    #[test]
    fn a_missing_scheme_and_version_are_added_not_refused() {
        assert_eq!(urls("127.0.0.1:8000"), "http://127.0.0.1:8000/v1/models");
        assert_eq!(urls("127.0.0.1:8000/"), "http://127.0.0.1:8000/v1/models");
        assert_eq!(urls("https://api.example.com"), "https://api.example.com/v1/models");
    }

    #[test]
    fn a_server_defined_path_is_left_alone() {
        assert_eq!(urls("http://gw.internal/api"), "http://gw.internal/api/models");
    }

    #[test]
    fn an_empty_endpoint_says_so_instead_of_defaulting() {
        assert!(matches!(Provider::from_base_url("   "), Err(ListingError::InvalidEndpoint(_))));
    }

    #[test]
    fn an_envelope_and_a_bare_array_both_parse() {
        let envelope =
            r#"{"object":"list","data":[{"id":"b-model"},{"id":"a-model","name":"Friendly","context_length":131072}]}"#;
        let models = parse_listing(envelope).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "a-model");
        assert_eq!(models[0].display(), "Friendly");
        assert_eq!(models[0].context_window, Some(131072));
        assert_eq!(models[1].display(), "b-model");

        let bare = r#"[{"id":"only","max_model_len":4096}]"#;
        let models = parse_listing(bare).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].context_window, Some(4096), "the alias must be read too");
    }

    #[test]
    fn a_window_is_worded_the_way_a_picker_shows_it() {
        let windowed = |tokens| Model { id: "m".to_string(), label: None, context_window: tokens };
        assert_eq!(windowed(Some(262144)).window_label().as_deref(), Some("256k"));
        assert_eq!(windowed(Some(128000)).window_label().as_deref(), Some("128k"));
        assert_eq!(windowed(Some(1048576)).window_label().as_deref(), Some("1m"));
        assert_eq!(windowed(Some(8194)).window_label().as_deref(), Some("8k"));
        assert_eq!(windowed(Some(4095)).window_label().as_deref(), Some("4k"));
        assert_eq!(windowed(Some(2048)).window_label().as_deref(), Some("2k"));
        assert_eq!(windowed(Some(1023)).window_label().as_deref(), Some("1023"));
        assert_eq!(windowed(Some(1024)).window_label().as_deref(), Some("1k"));
        assert_eq!(windowed(None).window_label(), None);
    }

    #[test]
    fn the_models_field_is_read_when_data_is_absent() {
        let models = parse_listing(r#"{"models":[{"id":"only-one"}]}"#).unwrap();
        assert_eq!(models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["only-one"]);
    }

    #[test]
    fn a_list_that_holds_nothing_is_not_an_error_while_a_wrong_url_is() {
        assert_eq!(parse_listing(r#"{"data":[]}"#).unwrap(), Vec::<Model>::new());
        assert_eq!(parse_listing(r#"{"models":[]}"#).unwrap(), Vec::<Model>::new());
        assert_eq!(parse_listing(r#"{"error":{"message":"warming up"}}"#).unwrap(), Vec::<Model>::new());
    }

    #[test]
    fn the_sort_case_folds_so_the_order_is_stable() {
        let models = parse_listing(r#"{"data":[{"id":"Zeta"},{"id":"alpha"},{"id":"BETA"}]}"#).unwrap();
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "BETA", "Zeta"],
            "case must not decide which half of the list a model lands in"
        );
    }

    #[test]
    fn a_body_that_is_not_json_is_named_as_such() {
        let err = parse_listing("<html>sign in</html>").unwrap_err();
        assert!(err.starts_with("not JSON"), "{err}");
    }

    #[test]
    fn a_live_endpoint_reports_models_or_a_reason_but_never_a_silent_empty_list() {
        let base = std::env::var("BAUDET_LIVE_MODELS_URL").unwrap_or_else(|_| "http://127.0.0.1:8000/v1".into());
        let api_key = std::env::var("BAUDET_LIVE_MODELS_KEY_ENV")
            .ok()
            .and_then(|name| std::env::var(name).ok())
            .filter(|k| !k.trim().is_empty());
        let provider = Provider::with_key(&base, api_key.clone()).expect("a normalisable endpoint");
        let probe = reqwest::blocking::Client::builder().timeout(Duration::from_secs(5)).build().unwrap();
        if probe.get(provider.models_url()).send().is_err() {
            eprintln!("skipping live test: no endpoint at {}", provider.models_url());
            return;
        }
        match provider.list_models() {
            Ok(models) => eprintln!(
                "live: {} models van {} (key: {})",
                models.len(),
                provider.models_url(),
                if api_key.is_some() { "ja" } else { "nee" }
            ),
            Err(
                error
                @ (ListingError::NotAListing { .. } | ListingError::Unreachable { .. } | ListingError::Timeout(_)),
            ) => {
                eprintln!(
                    "live: {} -> {error} (key: {})",
                    provider.models_url(),
                    if api_key.is_some() { "ja" } else { "nee" }
                );
            }
            Err(other) => panic!("live listing faalde op de verkeerde plek: {other}"),
        }
    }

    #[test]
    fn a_blank_id_is_not_offered_as_a_model() {
        let models = parse_listing(r#"{"data":[{"id":"  "},{"id":"real"}]}"#).unwrap();
        assert_eq!(models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["real"]);
    }

    #[test]
    fn a_credential_reaches_the_header_only_when_one_is_known() {
        let provider = Provider::with_key("127.0.0.1:1234/v1", Some("typed-key".into())).unwrap();
        assert_eq!(provider.api_key.as_deref(), Some("typed-key"));
        let blank = Provider::with_key("127.0.0.1:1234/v1", Some("   ".into())).unwrap();
        assert_eq!(blank.api_key, None);
        assert_eq!(blank.base_url(), "http://127.0.0.1:1234/v1");
        assert_eq!(Provider::from_base_url("127.0.0.1:1234/v1").unwrap().api_key, None);
    }

    #[test]
    fn from_settings_offers_the_resolved_key_and_none_when_there_is_none() {
        let typed = Settings {
            base_url: "http://127.0.0.1:1234/v1".into(),
            api_key: Some("typed-key".into()),
            api_key_env_var: "BAUDET_UNSET_TEST_KEY".into(),
            ..Default::default()
        };
        assert_eq!(Provider::from_settings(&typed).unwrap().api_key.as_deref(), Some("typed-key"));
        let none = Settings {
            base_url: "http://127.0.0.1:1234/v1".into(),
            api_key: None,
            api_key_env_var: "BAUDET_UNSET_TEST_KEY".into(),
            ..Default::default()
        };
        assert_eq!(Provider::from_settings(&none).unwrap().api_key, None);
    }

    #[test]
    fn a_key_the_endpoint_echoed_back_never_survives_redaction() {
        let key = "secret-typed-key";
        let echo = format!(r#"{{"error":{{"message":"bad key {key} supplied"}}}}"#);
        let out = redact(&echo, Some(key));
        assert!(!out.contains(key), "the key must not survive: {out}");
        assert!(out.contains("[redacted]"), "and the shape must say so: {out}");
        assert!(out.contains("bad key") && out.contains("supplied"), "the words around it stay: {out}");
        let twice = format!("{key} and {key}");
        assert_eq!(redact(&twice, Some(key)), "[redacted] and [redacted]");
        assert_eq!(redact("Not Found", Some(key)), "Not Found");
        assert_eq!(redact("Not Found", None), "Not Found");
        assert_eq!(redact("Not Found", Some("")), "Not Found");
        assert_eq!(redact("Not Found", Some("   ")), "Not Found");
    }

    #[test]
    fn the_detail_is_cut_to_a_readable_word_count_without_breaking_a_character() {
        assert_eq!(shorten("short"), "short");
        let cut = shorten(&"é".repeat(210));
        assert!(cut.ends_with('…'), "{cut}");
        assert_eq!(cut.chars().count(), 101, "100 kept characters plus the ellipsis");
        assert_eq!(shorten(&"x".repeat(210)).chars().count(), 101);
    }
}
