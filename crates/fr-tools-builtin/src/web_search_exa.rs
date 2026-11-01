// Rust guideline compliant 2026-11-01
//! `ExaSearchProvider`: a search backend on the `Exa` search API.
//!
//! `POST /search` with highlight contents (dsh
//! `@deepseek-ai/dsh-web-search-exa`, 1:1): the first non-blank
//! highlight maps to `snippet`, `publishedDate` maps to `publishedAt`,
//! entries without a snippet drop, and `content` is omitted because
//! `Exa` returns no generated answer. Errors carry no endpoint
//! guidance — this provider's failures name themselves.

#![forbid(unsafe_code)]

use fr_net::http::Fetch as _;
use serde_json::{Value, json};

use crate::web_seam::{
    WebError, WebSearchProvider, WebSearchRequest, WebSearchResult, WebSearchSource,
};

/// Stable id this provider registers under.
pub const EXA_PROVIDER_ID: &str = "exa";

/// Default Exa search endpoint; `/search` is the operation.
pub const EXA_DEFAULT_BASE_URL: &str = "https://api.exa.ai";

/// Default retrieval mode: let Exa pick between keyword and neural
/// search.
pub const EXA_DEFAULT_SEARCH_TYPE: &str = "auto";

/// Default number of highlight sentences requested per result.
pub const EXA_DEFAULT_HIGHLIGHTS_PER_RESULT: f64 = 1.0;

/// Environment variable carrying the Exa API key (the literal config
/// key wins).
pub const EXA_API_KEY_ENV: &str = "EXA_API_KEY";

/// Resolved provider options (dsh `ExaSearchProviderOptions`).
#[derive(Debug, Clone)]
pub struct ExaSearchProviderOptions {
    /// Exa API key. Empty makes the provider unavailable.
    pub api_key: String,
    /// Endpoint base; `/search` is appended.
    pub base_url: String,
    /// Retrieval mode sent as Exa's `type`.
    pub search_type: String,
    /// Default result count when a request carries no `maxResults`.
    pub num_results: Option<f64>,
    /// Highlight sentences requested per result (Exa's
    /// `highlightsPerUrl`).
    pub highlights_per_result: f64,
}

/// Map one Exa result to a normalized source, or `None`.
///
/// `None` when the entry carries no portable snippet (an entry with
/// no highlight is dropped — the seam has no other field to derive a
/// snippet from, and inventing one would lie). An entry without a
/// string `url` drops too: the seam requires one, and the dsh loose
/// pass-through would emit an `undefined` url the fr type cannot
/// represent.
///
/// # Errors
///
/// The unprocessable message when `highlights` is present but not an
/// array (the dsh `.find` `TypeError` shape).
pub fn map_exa_result(result: &Value) -> Result<Option<WebSearchSource>, String> {
    let unprocessable =
        || "Exa returned an unprocessable response body: highlights is not an array".to_owned();
    let Some(url) = result.get("url").and_then(Value::as_str) else {
        return Ok(None);
    };
    let highlights = match result.get("highlights") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => Some(items),
        Some(_) => return Err(unprocessable()),
    };
    let snippet = highlights.and_then(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .find(|highlight| !highlight.trim().is_empty())
            .map(str::to_owned)
    });
    let Some(snippet) = snippet else {
        return Ok(None);
    };
    let optional = |name: &str| {
        result
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    Ok(Some(WebSearchSource {
        url: url.to_owned(),
        title: optional("title"),
        snippet: Some(snippet),
        published_at: optional("publishedDate"),
    }))
}

/// Map an Exa response envelope to a normalized search result.
///
/// dsh `mapExaResponse`: snippet-less entries drop, `Exa` returns no
/// generated answer so `content` is omitted, and the web service owns
/// the final `maxResults` truncation, so `truncated` is always
/// `false`.
///
/// # Errors
///
/// The unprocessable message on malformed shapes.
pub fn map_exa_response(response: &Value) -> Result<WebSearchResult, String> {
    let results = match response.get("results") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.clone(),
        Some(_) => {
            return Err(
                "Exa returned an unprocessable response body: results is not an array".to_owned(),
            );
        }
    };
    let mut sources = Vec::new();
    for result in &results {
        if result.is_null() {
            continue;
        }
        if let Some(source) = map_exa_result(result)? {
            sources.push(source);
        }
    }
    Ok(WebSearchResult {
        content: None,
        sources,
        truncated: false,
    })
}

/// The Exa-backed search provider (dsh `ExaSearchProvider`); HTTP
/// redirects fail as `WEB_PROVIDER_ERROR`.
#[derive(Debug, Clone)]
pub struct ExaSearchProvider {
    options: ExaSearchProviderOptions,
}

impl ExaSearchProvider {
    /// Build the provider over one resolved option set.
    #[must_use]
    pub const fn new(options: ExaSearchProviderOptions) -> Self {
        Self { options }
    }
}

impl WebSearchProvider for ExaSearchProvider {
    fn id(&self) -> &str {
        EXA_PROVIDER_ID
    }

    fn available(&self) -> bool {
        !self.options.api_key.is_empty()
            && url::Url::parse(&self.options.base_url).is_ok()
            && is_positive_integer(self.options.highlights_per_result)
            && self.options.num_results.is_none_or(is_positive_integer)
    }

    /// Run one search against the `/search` endpoint.
    ///
    /// # Errors
    ///
    /// [`WebError`] with the provider's code family.
    fn search(&self, request: &WebSearchRequest) -> Result<WebSearchResult, WebError> {
        // A per-request bound wins over the configured default; either
        // may be absent.
        let num_results = request.max_results.map_or(self.options.num_results, |max| {
            #[expect(
                clippy::cast_precision_loss,
                reason = "the request bound rides the same f64 shape as the JS spread"
            )]
            let max = max as f64;
            Some(max)
        });
        let body = match num_results {
            Some(count) => json!({
                "query": request.query,
                "type": self.options.search_type,
                "contents": { "highlights": { "highlightsPerUrl": integral(self.options.highlights_per_result)? } },
                "numResults": integral(count)?,
            }),
            None => json!({
                "query": request.query,
                "type": self.options.search_type,
                "contents": { "highlights": { "highlightsPerUrl": integral(self.options.highlights_per_result)? } },
            }),
        };
        let endpoint = format!("{}/search", self.options.base_url);
        let payload = serde_json::to_vec(&body).map_err(|serde_failure| {
            WebError::new(
                "WEB_PROVIDER_ERROR",
                format!("Exa search request failed: {serde_failure}"),
            )
        })?;
        let response = fr_net::http::UreqFetch::shared()
            .fetch(fr_net::http::HttpRequest {
                method: fr_net::http::HttpMethod::Post,
                url: endpoint,
                headers: vec![
                    (
                        "authorization".to_owned(),
                        format!("Bearer {}", self.options.api_key),
                    ),
                    ("content-type".to_owned(), "application/json".to_owned()),
                    ("accept".to_owned(), "application/json".to_owned()),
                    ("user-agent".to_owned(), crate::attribution::product_agent()),
                ],
                body: Some(payload),
            })
            .map_err(|failure| {
                WebError::new(
                    "WEB_PROVIDER_ERROR",
                    format!("Exa search request failed: {}", failure.message()),
                )
            })?;
        if !(200..300).contains(&response.status) {
            let status = response.status;
            let mut message = format!("Exa API error (HTTP {status})");
            let mut response_body = response.body;
            let mut bytes = Vec::new();
            if response_body.read_to_end(&mut bytes).is_ok()
                && let Ok(parsed) = serde_json::from_slice::<Value>(&bytes)
                && let Some(detail) = exa_error_detail(&parsed)
            {
                message = detail;
            }
            return Err(WebError::new("WEB_PROVIDER_ERROR", message));
        }
        let mut bytes = Vec::new();
        let mut response_body = response.body;
        response_body
            .read_to_end(&mut bytes)
            .map_err(|read_failure| {
                WebError::new(
                    "WEB_PROVIDER_ERROR",
                    format!("Exa returned an unprocessable response body: {read_failure}"),
                )
            })?;
        let parsed: Value = serde_json::from_slice(&bytes).map_err(|parse_failure| {
            WebError::new(
                "WEB_PROVIDER_ERROR",
                format!("Exa returned an unprocessable response body: {parse_failure}"),
            )
        })?;
        map_exa_response(&parsed).map_err(|message| WebError::new("WEB_PROVIDER_ERROR", message))
    }
}

/// Extract the best-effort error detail (dsh `parsed.error ??
/// parsed.message`): the first present field wins and OVERWRITES the
/// default message; a non-string detail never replaces it.
#[must_use]
pub fn exa_error_detail(parsed: &Value) -> Option<String> {
    let object = parsed.as_object()?;
    let detail = object
        .get("error")
        .map_or_else(|| object.get("message"), Some);
    detail.and_then(Value::as_str).map(str::to_owned)
}

/// True for a request limit that can be sent to Exa (a positive whole
/// number).
const fn is_positive_integer(value: f64) -> bool {
    value.fract() == 0.0 && value > 0.0
}

/// Cast a validated positive-integral config number for the wire body.
///
/// # Errors
///
/// A defensive [`WebError`] when the number is not a positive integer —
/// the seam checks availability before selection.
fn integral(value: f64) -> Result<u64, WebError> {
    if !is_positive_integer(value) {
        return Err(WebError::new(
            "WEB_PROVIDER_ERROR",
            "Exa search request failed: a result limit is not a positive integer".to_owned(),
        ));
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "validated above as a positive integral number"
    )]
    Ok(value as u64)
}

/// Resolve one provider option set from the compiled config object and
/// an environment lookup (the dsh `apply` defaults: the literal key,
/// then `$EXA_API_KEY`, then empty = unavailable).
#[must_use]
pub fn resolve_options(
    config: &Value,
    env: &dyn Fn(&str) -> Option<String>,
) -> ExaSearchProviderOptions {
    let search_type = config
        .get("search-type")
        .and_then(Value::as_str)
        .unwrap_or(EXA_DEFAULT_SEARCH_TYPE)
        .to_owned();
    ExaSearchProviderOptions {
        api_key: config
            .get("api-key")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| env(EXA_API_KEY_ENV))
            .unwrap_or_default(),
        base_url: config
            .get("base-url")
            .and_then(Value::as_str)
            .unwrap_or(EXA_DEFAULT_BASE_URL)
            .to_owned(),
        search_type,
        num_results: config.get("num-results").and_then(Value::as_f64),
        highlights_per_result: config
            .get("highlights-per-result")
            .and_then(Value::as_f64)
            .unwrap_or(EXA_DEFAULT_HIGHLIGHTS_PER_RESULT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> ExaSearchProviderOptions {
        ExaSearchProviderOptions {
            api_key: "sk-exa".to_owned(),
            base_url: EXA_DEFAULT_BASE_URL.to_owned(),
            search_type: EXA_DEFAULT_SEARCH_TYPE.to_owned(),
            num_results: None,
            highlights_per_result: EXA_DEFAULT_HIGHLIGHTS_PER_RESULT,
        }
    }

    fn request(query: &str) -> WebSearchRequest {
        WebSearchRequest {
            query: query.to_owned(),
            max_results: None,
        }
    }

    /// Constants and the availability table: an empty key, a bad URL,
    /// and non-integral limits refuse.
    #[test]
    fn availability_pins_the_option_contract() {
        assert_eq!(EXA_PROVIDER_ID, "exa");
        assert_eq!(EXA_DEFAULT_BASE_URL, "https://api.exa.ai");
        assert_eq!(EXA_DEFAULT_SEARCH_TYPE, "auto");
        assert_eq!(EXA_DEFAULT_HIGHLIGHTS_PER_RESULT, 1.0);
        assert!(ExaSearchProvider::new(options()).available());
        assert!(
            !ExaSearchProvider::new(ExaSearchProviderOptions {
                api_key: String::new(),
                ..options()
            })
            .available()
        );
        assert!(
            !ExaSearchProvider::new(ExaSearchProviderOptions {
                base_url: "not a url".to_owned(),
                ..options()
            })
            .available()
        );
        assert!(
            !ExaSearchProvider::new(ExaSearchProviderOptions {
                highlights_per_result: 0.0,
                ..options()
            })
            .available()
        );
        assert!(
            !ExaSearchProvider::new(ExaSearchProviderOptions {
                num_results: Some(1.5),
                ..options()
            })
            .available()
        );
    }

    /// The request body pins the query, the type, the highlights
    /// contents, and the numResults presence rule (a request bound wins
    /// over the configured default; both absent omit the field), and
    /// the mapping keeps only entries with a non-blank highlight.
    #[test]
    fn search_pins_the_roundtrip_and_mapping() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            let mut buffer = Vec::new();
            let mut window = [0u8; 4096];
            let text = loop {
                let read = std::io::Read::read(&mut socket, &mut window).unwrap_or(0);
                buffer.extend_from_slice(&window[..read]);
                let text = String::from_utf8_lossy(&buffer).to_string();
                let payload = text.split("\r\n\r\n").nth(1).unwrap_or("");
                if serde_json::from_str::<Value>(payload).is_ok() || read == 0 {
                    break text;
                }
            };
            assert!(text.starts_with("POST /search HTTP/1.1\r\n"));
            assert!(text.contains("authorization: Bearer sk-exa\r\n"));
            let payload = text.split("\r\n\r\n").nth(1).unwrap();
            let body: Value = serde_json::from_str(payload).unwrap();
            assert_eq!(
                body,
                json!({
                    "query": "rust",
                    "type": "auto",
                    "contents": { "highlights": { "highlightsPerUrl": 1 } },
                    "numResults": 3,
                })
            );
            let body = r#"{"results":[
                {"url":"https://a.example/","title":"A","publishedDate":"2026-01-01","highlights":["","  the pick ","second"]},
                {"url":"https://b.example/","title":"B","highlights":["   "]},
                {"url":"https://c.example/","title":"C"}
            ]}"#;
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            std::io::Write::write_all(&mut socket, wire.as_bytes()).unwrap();
        });
        let outcome = ExaSearchProvider::new(ExaSearchProviderOptions {
            num_results: Some(10.0),
            base_url: format!("http://127.0.0.1:{port}"),
            ..options()
        })
        .search(&WebSearchRequest {
            query: "rust".to_owned(),
            max_results: Some(3),
        })
        .expect("search runs");
        // The blank-highlight and highlight-less entries drop; the
        // first non-blank highlight wins.
        assert_eq!(outcome.sources.len(), 1);
        assert_eq!(outcome.sources[0].url, "https://a.example/");
        assert_eq!(outcome.sources[0].title.as_deref(), Some("A"));
        assert_eq!(outcome.sources[0].snippet.as_deref(), Some("  the pick "));
        assert_eq!(
            outcome.sources[0].published_at.as_deref(),
            Some("2026-01-01")
        );
        assert!(!outcome.truncated);
    }

    /// The error family: transport and unprocessable keep their
    /// prefixes, and an HTTP error's string detail OVERWRITES the
    /// default message.
    #[test]
    fn error_family_pins_the_detail_overwrite() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for request in server.incoming() {
                let mut socket = request.unwrap();
                let mut buffer = Vec::new();
                let mut window = [0u8; 4096];
                loop {
                    let read = std::io::Read::read(&mut socket, &mut window).unwrap_or(0);
                    buffer.extend_from_slice(&window[..read]);
                    let text = String::from_utf8_lossy(&buffer).to_string();
                    let payload = text.split("\r\n\r\n").nth(1).unwrap_or("");
                    if serde_json::from_str::<Value>(payload).is_ok() || read == 0 {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&buffer).to_string();
                let body = if text.contains("/overwrite") {
                    r#"{"error":"quota exceeded","message":"ignored"}"#
                } else if text.contains("/message") {
                    r#"{"message":"fallback wins"}"#
                } else if text.contains("/nostring") {
                    r#"{"error":42}"#
                } else {
                    // The malformed-shape arm answers 200 so the
                    // mapping (not the status) refuses.
                    let body = r#"{"results":"not an array"}"#;
                    let wire = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    std::io::Write::write_all(&mut socket, wire.as_bytes()).unwrap();
                    continue;
                };
                let wire = format!(
                    "HTTP/1.1 402 Payment Required\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                std::io::Write::write_all(&mut socket, wire.as_bytes()).unwrap();
            }
        });
        let base = format!("http://127.0.0.1:{port}");
        let at = |path: &'static str| {
            ExaSearchProvider::new(ExaSearchProviderOptions {
                base_url: format!("{base}{path}"),
                ..options()
            })
        };
        assert_eq!(
            at("/overwrite").search(&request("q")).unwrap_err().message,
            "quota exceeded"
        );
        assert_eq!(
            at("/message").search(&request("q")).unwrap_err().message,
            "fallback wins"
        );
        assert_eq!(
            at("/nostring").search(&request("q")).unwrap_err().message,
            "Exa API error (HTTP 402)"
        );
        assert_eq!(
            at("/bad").search(&request("q")).unwrap_err().message,
            "Exa returned an unprocessable response body: results is not an array"
        );
    }

    /// The result mapping table over malformed and null shapes.
    #[test]
    fn map_exa_result_pins_the_narrowing_table() {
        assert_eq!(
            map_exa_result(&json!({"url": "https://a/", "highlights": 7})).unwrap_err(),
            "Exa returned an unprocessable response body: highlights is not an array"
        );
        assert_eq!(
            map_exa_result(&json!({"highlights": ["x"]}))
                .expect("no url drops")
                .is_none(),
            true
        );
        assert_eq!(
            map_exa_result(&json!({"url": "https://b/"}))
                .expect("no highlights drops")
                .is_none(),
            true
        );
        let mapped = map_exa_result(&json!({
            "url": "https://c/",
            "title": null,
            "publishedDate": null,
            "highlights": ["keep"]
        }))
        .expect("maps")
        .expect("has snippet");
        assert_eq!(mapped.title, None);
        assert_eq!(mapped.published_at, None);
        let empty_results = map_exa_response(&json!({})).expect("absent results are empty");
        assert!(empty_results.sources.is_empty());
    }

    /// The option resolution: config values over the `$EXA_API_KEY`
    /// environment fallback, then the defaults (empty key = the
    /// unavailable state).
    #[test]
    fn resolve_options_pins_the_defaults_and_env() {
        let resolved = resolve_options(&json!({}), &|_name| None);
        assert_eq!(resolved.api_key, "");
        assert_eq!(resolved.base_url, EXA_DEFAULT_BASE_URL);
        assert_eq!(resolved.search_type, "auto");
        assert_eq!(resolved.num_results, None);
        assert_eq!(resolved.highlights_per_result, 1.0);

        let from_env = resolve_options(&json!({}), &|name| {
            (name == EXA_API_KEY_ENV).then(|| "sk-env".to_owned())
        });
        assert_eq!(from_env.api_key, "sk-env");

        let config = resolve_options(
            &json!({
                "api-key": "sk-cfg",
                "base-url": "https://cfg.exa/api",
                "search-type": "neural",
                "num-results": 5,
                "highlights-per-result": 2,
            }),
            &|_name| None,
        );
        assert_eq!(config.api_key, "sk-cfg");
        assert_eq!(config.base_url, "https://cfg.exa/api");
        assert_eq!(config.search_type, "neural");
        assert_eq!(config.num_results, Some(5.0));
        assert_eq!(config.highlights_per_result, 2.0);
    }
}
