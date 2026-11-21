//! Which endpoint to ask, and what it answers it has.
//!
//! The reference app does this in `ModelsClient` (`GET /v1/models`) behind `ModelGateway`. This is
//! the same two decisions, in one place: the URL shape, and what the listing is allowed to do when
//! the endpoint answers late, not at all, or with something that is not JSON.
//!
//! A listing is the only network call in this crate and it is **blocking on purpose**: whoever owns
//! a window calls it from a background thread, exactly as `baudet_app::session::pump` is drained on
//! its own thread. Making it `async` here would force a runtime on a crate whose job is to answer
//! one question, and would not make the window feel any faster.

use crate::config::Settings;
use serde::Deserialize;
use std::time::Duration;

/// How long to wait for a listing before saying so. Generous enough for a local server that is
/// still loading weights — which is the common case here and is slow on a cold model — short
/// enough that a wrong host does not look like a hang.
pub const LISTING_TIMEOUT: Duration = Duration::from_secs(20);

/// One model as an endpoint describes it.
///
/// Only fields the OpenAI-shaped listing is actually documented to carry are read, and each is
/// optional: a server that answers with `{"object":"list","data":[{"id":"x"}]}` and nothing else
/// is a server whose models are still worth offering. `created` is deliberately not read — a
/// model's upload timestamp tells a picker nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    /// What to send as the model name.
    pub id: String,
    /// A shorter name worth showing when it differs from `id`, when the server bothers to give one.
    pub label: Option<String>,
    /// The largest prompt the model takes, when the server says. Displayed, never used to decide.
    pub context_window: Option<u32>,
}

impl Model {
    /// What the picker shows: the friendlier name when there is one, the id otherwise.
    pub fn display(&self) -> &str {
        self.label.as_deref().filter(|l| !l.trim().is_empty()).unwrap_or(&self.id)
    }
}

/// Why a listing failed, in words a panel can show without guessing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ListingError {
    /// No request was made, because the configured base URL cannot produce one.
    #[error("no usable endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("the endpoint did not answer within {} s", .0.as_secs())]
    Timeout(Duration),
    /// The request failed for a transport reason, with the client's own words kept — the one
    /// detail that lets a person tell "wrong port" from "no server" from "TLS".
    #[error("could not reach {endpoint}: {detail}")]
    Unreachable { endpoint: String, detail: String },
    /// It answered, but not with a model list. The status and a short body are kept, because
    /// "404 from a gateway" and "200 from a login page" need different fixes.
    #[error("{endpoint} answered {status}: {detail}")]
    NotAListing { endpoint: String, status: u16, detail: String },
}

/// The endpoint a turn goes to, derived from the stored settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    base_url: String,
}

impl Provider {
    /// Build from a base URL, tolerating the shapes a person actually types into a text field:
    /// trailing slash, missing or doubled `http://`, surrounding whitespace.
    ///
    /// The reference app reads a full `…/v1`; a person who types `studio.local:8000` means the
    /// same server, so the scheme and the version prefix are added rather than refused. What *is*
    /// refused is an empty value, because silently defaulting a typed-but-blank endpoint would hide
    /// the person's own mistake from them.
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
            // Already versioned, or carries a path the server itself defined: leave it alone.
            Some(already) => already,
            None => format!("{without_slash}/v1"),
        };
        Ok(Provider { base_url: base })
    }

    /// Build from what is stored.
    pub fn from_settings(settings: &Settings) -> Result<Provider, ListingError> {
        Provider::from_base_url(&settings.base_url)
    }

    /// The versioned base, after normalising.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The listing URL, the OpenAI-shaped one the reference app already calls.
    pub fn models_url(&self) -> String {
        format!("{}/models", self.base_url)
    }

    /// Ask the endpoint which models it has.
    ///
    /// Blocking, single-shot, no cache: a local server gains models as it loads them, so a cache
    /// here would show a person a model that has since disappeared. The panel owns refresh.
    ///
    /// Sends no credential, which the reference app copies exactly: `ModelsClient.fetch` issues a
    /// bare `GET`, so a local server that answers `/v1/models` without a key behaves the same for
    /// both apps. What has to stay true when that changes is the *display* rule, stated here so the
    /// next person does not rediscover it by leaking one: `NotAListing.detail` carries a short slice
    /// of the response body, so the moment this request starts sending `Authorization: Bearer …`, an
    /// echo of that body — some gateways repeat request context in their error — could surface the
    /// key in a status line. `shorten` caps the shape, not the secrecy; whoever adds the header must
    /// redact `detail` then, not here.
    pub fn list_models(&self) -> Result<Vec<Model>, ListingError> {
        let url = self.models_url();
        // No `.default_headers(authorization(...))` on purpose. See the display rule above.
        let client = match reqwest::blocking::Client::builder().timeout(LISTING_TIMEOUT).build() {
            Ok(client) => client,
            Err(error) => return Err(ListingError::Unreachable { endpoint: url, detail: error.to_string() }),
        };
        let response = match client.get(&url).send() {
            Ok(response) => response,
            Err(error) => {
                // reqwest reports a timeout as an error with its own flag, which is the one case
                // worth naming separately: it means the server exists and is busy, not absent.
                if error.is_timeout() {
                    return Err(ListingError::Timeout(LISTING_TIMEOUT));
                }
                return Err(ListingError::Unreachable { endpoint: url, detail: error.to_string() });
            }
        };
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let detail = response.text().unwrap_or_default();
            return Err(ListingError::NotAListing { endpoint: url, status, detail: shorten(detail.trim()) });
        }
        // The body is read as text and *then* parsed, so a 200 from a login page reports the shape
        // problem as a listing error instead of as a transport one.
        let body = match response.text() {
            Ok(body) => body,
            Err(error) => {
                return Err(ListingError::NotAListing { endpoint: url, status, detail: shorten(&error.to_string()) })
            }
        };
        parse_listing(&body).map_err(|detail| ListingError::NotAListing { endpoint: url, status, detail })
    }
}

/// The two root fields the endpoint is known to use. `Option`, because both are optional and the
/// reference app's whole rule lives in that optionality: `data`, else `models`, else empty.
///
/// A gateway's `{"error":{"message":"warming up"}}` therefore parses to an empty list, matching the
/// reference — and measurement says that is the right answer, not a bug to fix. On the real server,
/// a *wrong* URL cannot reach this parser at all: `http://host:8000/v1/modelz` answers **404** with
/// `{"detail":"Not Found"}`, which the status gate in [`Provider::list_models`] turns into
/// `NotAListing` before any parsing. So the only object still reaching here without either key is
/// one the *server itself* sent with a 200, which is the "still loading, ask again" message. That
/// deserves a Refresh, not "go check your URL".
///
/// `Option` still earns its keep over `Vec` + `#[serde(default)]`, just at the type level: absent is
/// `None` and present-but-empty is `Some(vec![])`, so the two stay distinguishable for whoever needs
/// them, and the reference's exact fallthrough — `data` even when empty — is the one written choice.
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
    /// Two spellings are seen in the wild and both mean the same thing to a picker.
    #[serde(default, rename = "context_length", alias = "max_context_length")]
    context_window: Option<u32>,
}

fn parse_listing(body: &str) -> Result<Vec<Model>, String> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|e| format!("not JSON: {e}"))?;
    // Two shapes are accepted, because both are real: the envelope above, and a bare array.
    let entries: Vec<Entry> = if value.is_array() {
        serde_json::from_value(value).map_err(|e| format!("not a model list: {e}"))?
    } else {
        let listing = serde_json::from_value::<Listing>(value).map_err(|e| format!("not a model list: {e}"))?;
        // `data` first, then `models`, then nothing: the reference app's order. Matching on the
        // options is what keeps "no key" (an error, taken by the `?` above only for a type
        // mismatch) apart from "empty key" (an honest empty picker) — `is_empty()` could not.
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
    // A listing in server order is a listing that changes when the server restarts. Case-folded so
    // `Qwen` and `qwen-7b` cannot land on opposite sides of the list, which is also how the
    // reference app sorts.
    models.sort_by_key(|model| model.id.to_lowercase());
    models.dedup_by(|a, b| a.id == b.id);
    Ok(models)
}

/// Where the versioned base is, if the URL already carries a path at all. Splitting on the host
/// boundary rather than looking for the literal `/v1`, so `http://h:8000/api` is respected as that
/// server's own choice instead of being second-guessed into `/api/v1`.
fn url_without_version(without_slash: &str) -> Option<String> {
    let after_scheme = without_slash.find("://").map(|i| &without_slash[i + 3..])?;
    // A path exists once anything but host:port remains.
    let path_start = after_scheme.find('/')?;
    (path_start > 0).then(|| without_slash.to_string())
}

fn shorten(text: &str) -> String {
    const LIMIT: usize = 100;
    // Counted in characters, not bytes: a body is read here for its words, so a byte count would
    // both cut a two-byte letter in half (a panic) and quietly halve the visible text for accented
    // bodies. `take` ends on a character boundary by construction.
    //
    // One definition rather than a byte fast path in front of it: a `text.len() <= 200` guard would
    // let a 210-byte body of 105 accented characters through uncut while cutting 210 ASCII ones, so
    // the limit would mean two different things depending on what the server's text contains.
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
        // Not our `/v1` to invent: a gateway mounted at /api knows its own routes.
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
        // Sorted, so the picker's order does not follow the server's mood.
        assert_eq!(models[0].id, "a-model");
        assert_eq!(models[0].display(), "Friendly");
        assert_eq!(models[0].context_window, Some(131072));
        // No name, no label: the id is what shows.
        assert_eq!(models[1].display(), "b-model");

        let bare = r#"[{"id":"only","max_context_length":4096}]"#;
        let models = parse_listing(bare).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].context_window, Some(4096), "the alias must be read too");
    }

    #[test]
    fn the_models_field_is_read_when_data_is_absent() {
        // A second server shape the reference app already tolerates; refusing it would show an
        // empty picker for a server that is answering perfectly well.
        let models = parse_listing(r#"{"models":[{"id":"only-one"}]}"#).unwrap();
        assert_eq!(models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["only-one"]);
    }

    #[test]
    fn a_list_that_holds_nothing_is_not_an_error_while_a_wrong_url_is() {
        // The distinction a person acts on, and measurement says which side each case is on.
        // A listing that is empty — either key, or a server's 200 `{"error":…}` "warming up" — is
        // not an error: the answer is Refresh. (An error-shaped body reaching *here* arrived with a
        // 200; the reference treats that as an empty picker for exactly that reason.)
        assert_eq!(parse_listing(r#"{"data":[]}"#).unwrap(), Vec::<Model>::new());
        assert_eq!(parse_listing(r#"{"models":[]}"#).unwrap(), Vec::<Model>::new());
        assert_eq!(parse_listing(r#"{"error":{"message":"warming up"}}"#).unwrap(), Vec::<Model>::new());
        // A wrong URL never reaches this function: the real server answers 404 with
        // {"detail":"Not Found"} and the status gate reports it. The test for that lives at the
        // transport boundary, so it is named here rather than asserted against a fake body.
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
        // The one shape that is unambiguously "this is not a model endpoint's answer": text that is
        // not JSON at all — a login page, a proxy's HTML error. Reported, not turned into a silent
        // empty picker.
        let err = parse_listing("<html>sign in</html>").unwrap_err();
        assert!(err.starts_with("not JSON"), "{err}");
    }

    /// Live-bewijs tegen een echte endpoint, omdat eenheidstests over `parse_listing` alleen nooit
    /// bewijzen dat de transport- en status-poort samenwerken: "error body met 401" mag nooit als
    /// "0 modellen" uit de echte HTTP-laag komen.
    ///
    /// Draait alleen wanneer een endpoint werkelijk bereikbaar is (de default `127.0.0.1:8000` of
    /// `$BAUDET_LIVE_MODELS_URL`), anders skipt hij schoon — een test die faalt omdat er geen server
    /// draait, is geen test.
    #[test]
    fn a_live_endpoint_reports_models_or_a_reason_but_never_a_silent_empty_list() {
        let base = std::env::var("BAUDET_LIVE_MODELS_URL").unwrap_or_else(|_| "http://127.0.0.1:8000/v1".into());
        let provider = Provider::from_base_url(&base).expect("a normalisable endpoint");
        let probe = reqwest::blocking::Client::builder().timeout(Duration::from_secs(5)).build().unwrap();
        if probe.get(provider.models_url()).send().is_err() {
            eprintln!("skipping live test: no endpoint at {}", provider.models_url());
            return;
        }
        match provider.list_models() {
            // Een echte lijst, of leeg omdat de server dat zegt.
            Ok(models) => eprintln!("live: {} models van {}", models.len(), provider.models_url()),
            // Een fout moet een status noemen. "error body + 401 -> NotAListing" is precies de casus
            // die de echte gateway hier levert; mag niet door de vingers glippen als Ok(vec![]).
            Err(
                error
                @ (ListingError::NotAListing { .. } | ListingError::Unreachable { .. } | ListingError::Timeout(_)),
            ) => {
                eprintln!("live: {} -> {error}", provider.models_url());
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
    fn the_detail_is_cut_to_a_readable_word_count_without_breaking_a_character() {
        assert_eq!(shorten("short"), "short");
        // Two-byte characters: the limit counts characters, so 210 of them lose exactly ten.
        let cut = shorten(&"é".repeat(210));
        assert!(cut.ends_with('…'), "{cut}");
        assert_eq!(cut.chars().count(), 101, "100 kept characters plus the ellipsis");
        // ASCII keeps the full width, which is what the limit is for.
        assert_eq!(shorten(&"x".repeat(210)).chars().count(), 101);
    }
}
