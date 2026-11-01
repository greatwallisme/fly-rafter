// Rust guideline compliant 2026-11-01
//! `Perplexity` search over its OpenAI-compatible chat-completions
//! endpoint (dsh `@deepseek-ai/dsh-web-search-perplexity`, 1:1).
//!
//! The generated answer becomes `content`; sources prefer structured
//! `search_results[]` and fall back to URL-only `citations[]`. The
//! wire format and the HTTP client are provider-private and do not use
//! the LLM seam.

#![forbid(unsafe_code)]

use fr_net::http::Fetch as _;
use serde_json::{Value, json};

use crate::web_seam::{
    WebError, WebSearchProvider, WebSearchRequest, WebSearchResult, WebSearchSource,
};

/// Stable id this provider registers under.
pub const PERPLEXITY_PROVIDER_ID: &str = "perplexity";

/// Default Perplexity endpoint; `/chat/completions` is the operation.
pub const PERPLEXITY_DEFAULT_BASE_URL: &str = "https://api.perplexity.ai";

/// Default search model.
pub const PERPLEXITY_DEFAULT_MODEL: &str = "sonar";

/// Default upper bound on generated answer tokens.
pub const PERPLEXITY_DEFAULT_MAX_TOKENS: f64 = 1024.0;

/// Environment variable carrying the Perplexity API key (the literal
/// config key wins).
pub const PERPLEXITY_API_KEY_ENV: &str = "PERPLEXITY_API_KEY";

/// Recency filter values Perplexity accepts for
/// `search_recency_filter` (dsh `PerplexityRecency`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerplexityRecency {
    /// The last day.
    Day,
    /// The last week.
    Week,
    /// The last month.
    Month,
    /// The last year.
    Year,
}

impl PerplexityRecency {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }

    /// Parse one config value (dsh `z.union`).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "day" => Some(Self::Day),
            "week" => Some(Self::Week),
            "month" => Some(Self::Month),
            "year" => Some(Self::Year),
            _ => None,
        }
    }
}

/// Resolved provider options (dsh `PerplexitySearchProviderOptions`).
#[derive(Debug, Clone)]
pub struct PerplexitySearchProviderOptions {
    /// Perplexity API key. Empty makes the provider unavailable.
    pub api_key: String,
    /// Endpoint base; `/chat/completions` is appended.
    pub base_url: String,
    /// Search model name.
    pub model: String,
    /// Upper bound on generated answer tokens (`max_tokens`).
    pub max_tokens: f64,
    /// Optional recency window sent as `search_recency_filter`.
    pub search_recency: Option<PerplexityRecency>,
}

/// Map one structured Perplexity result to a normalized source.
///
/// dsh `mapPerplexityResult`: blank fields are omitted rather than set
/// empty. An entry without a string `url` drops: the seam requires
/// one, and the dsh loose pass-through would emit an `undefined` url
/// the fr type cannot represent.
#[must_use]
pub fn map_perplexity_result(result: &Value) -> Option<WebSearchSource> {
    let url = result.get("url").and_then(Value::as_str)?;
    let optional = |name: &str| {
        result
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    Some(WebSearchSource {
        url: url.to_owned(),
        title: optional("title"),
        snippet: optional("snippet"),
        published_at: optional("date"),
    })
}

/// Map a `Perplexity` response envelope to a search result.
///
/// dsh `mapPerplexityResponse`: prefers structured `search_results[]`
/// and falls back to URL-only `citations[]` (those sources carry just
/// a `url`) only when `search_results` is absent; `content` is omitted
/// when the answer is empty.
///
/// # Errors
///
/// The unprocessable message on malformed shapes (a present-but-
/// non-array `search_results` or `citations` is the dsh `TypeError`
/// arm).
pub fn map_perplexity_response(response: &Value) -> Result<WebSearchResult, String> {
    let unprocessable = |field: &str| {
        format!("Perplexity returned an unprocessable response body: {field} is not an array")
    };
    let content = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .filter(|content| !content.is_empty())
        .map(str::to_owned);
    let sources = match response.get("search_results") {
        Some(Value::Array(items)) => items
            .iter()
            .filter(|item| !item.is_null())
            .filter_map(map_perplexity_result)
            .collect(),
        Some(Value::Null) | None => match response.get("citations") {
            Some(Value::Array(urls)) => urls
                .iter()
                .filter_map(Value::as_str)
                .map(|url| WebSearchSource {
                    url: url.to_owned(),
                    title: None,
                    snippet: None,
                    published_at: None,
                })
                .collect(),
            Some(_) => return Err(unprocessable("citations")),
            None => Vec::new(),
        },
        Some(_) => return Err(unprocessable("search_results")),
    };
    Ok(WebSearchResult {
        content,
        sources,
        truncated: false,
    })
}

/// The Perplexity-backed search provider (dsh
/// `PerplexitySearchProvider`); HTTP redirects fail as
/// `WEB_PROVIDER_ERROR`.
#[derive(Debug, Clone)]
pub struct PerplexitySearchProvider {
    options: PerplexitySearchProviderOptions,
}

impl PerplexitySearchProvider {
    /// Build the provider over one resolved option set.
    #[must_use]
    pub const fn new(options: PerplexitySearchProviderOptions) -> Self {
        Self { options }
    }
}

impl WebSearchProvider for PerplexitySearchProvider {
    fn id(&self) -> &str {
        PERPLEXITY_PROVIDER_ID
    }

    fn available(&self) -> bool {
        !self.options.api_key.is_empty()
            && url::Url::parse(&self.options.base_url).is_ok()
            && is_positive_integer(self.options.max_tokens)
    }

    /// Run one search against the `/chat/completions` endpoint.
    ///
    /// # Errors
    ///
    /// [`WebError`] with the provider's code family.
    fn search(&self, request: &WebSearchRequest) -> Result<WebSearchResult, WebError> {
        let body = match self.options.search_recency {
            Some(recency) => json!({
                "model": self.options.model,
                "max_tokens": integral(self.options.max_tokens)?,
                "messages": [{ "role": "user", "content": request.query }],
                "search_recency_filter": recency.as_str(),
            }),
            None => json!({
                "model": self.options.model,
                "max_tokens": integral(self.options.max_tokens)?,
                "messages": [{ "role": "user", "content": request.query }],
            }),
        };
        let endpoint = format!("{}/chat/completions", self.options.base_url);
        let payload = serde_json::to_vec(&body).map_err(|serde_failure| {
            WebError::new(
                "WEB_PROVIDER_ERROR",
                format!("Perplexity search request failed: {serde_failure}"),
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
                    format!("Perplexity search request failed: {}", failure.message()),
                )
            })?;
        if !(200..300).contains(&response.status) {
            let status = response.status;
            let mut message = format!("Perplexity API error (HTTP {status})");
            let mut response_body = response.body;
            let mut bytes = Vec::new();
            if response_body.read_to_end(&mut bytes).is_ok()
                && let Ok(parsed) = serde_json::from_slice::<Value>(&bytes)
                && let Some(detail) = error_detail(&parsed)
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
                    format!("Perplexity returned an unprocessable response body: {read_failure}"),
                )
            })?;
        let parsed: Value = serde_json::from_slice(&bytes).map_err(|parse_failure| {
            WebError::new(
                "WEB_PROVIDER_ERROR",
                format!("Perplexity returned an unprocessable response body: {parse_failure}"),
            )
        })?;
        map_perplexity_response(&parsed)
            .map_err(|message| WebError::new("WEB_PROVIDER_ERROR", message))
    }
}

/// Extract the best-effort error detail.
///
/// The dsh `typeof error === 'string' ? error : error?.message ??
/// message` narrowing: the `??` falls through only on a missing or
/// null field, and a non-string detail never replaces the default
/// message.
#[must_use]
pub fn error_detail(parsed: &Value) -> Option<String> {
    let object = parsed.as_object()?;
    let fallback = || object.get("message");
    let detail: Option<&Value> = object
        .get("error")
        .map_or_else(fallback, |error| match error {
            Value::String(_) => Some(error),
            Value::Object(inner) => match inner.get("message") {
                Some(Value::Null) | None => fallback(),
                present => present,
            },
            _ => fallback(),
        });
    detail
        .and_then(Value::as_str)
        .filter(|detail| !detail.is_empty())
        .map(str::to_owned)
}

/// True for a request limit that can be sent to Perplexity (a positive
/// whole number).
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
            "Perplexity search request failed: maxTokens is not a positive integer".to_owned(),
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
/// then `$PERPLEXITY_API_KEY`, then empty = unavailable).
#[must_use]
pub fn resolve_options(
    config: &Value,
    env: &dyn Fn(&str) -> Option<String>,
) -> PerplexitySearchProviderOptions {
    let search_recency = config
        .get("search-recency")
        .and_then(Value::as_str)
        .and_then(PerplexityRecency::parse);
    PerplexitySearchProviderOptions {
        api_key: config
            .get("api-key")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| env(PERPLEXITY_API_KEY_ENV))
            .unwrap_or_default(),
        base_url: config
            .get("base-url")
            .and_then(Value::as_str)
            .unwrap_or(PERPLEXITY_DEFAULT_BASE_URL)
            .to_owned(),
        model: config
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(PERPLEXITY_DEFAULT_MODEL)
            .to_owned(),
        max_tokens: config
            .get("max-tokens")
            .and_then(Value::as_f64)
            .unwrap_or(PERPLEXITY_DEFAULT_MAX_TOKENS),
        search_recency,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> PerplexitySearchProviderOptions {
        PerplexitySearchProviderOptions {
            api_key: "sk-pplx".to_owned(),
            base_url: PERPLEXITY_DEFAULT_BASE_URL.to_owned(),
            model: PERPLEXITY_DEFAULT_MODEL.to_owned(),
            max_tokens: PERPLEXITY_DEFAULT_MAX_TOKENS,
            search_recency: None,
        }
    }

    fn request(query: &str) -> WebSearchRequest {
        WebSearchRequest {
            query: query.to_owned(),
            max_results: None,
        }
    }

    /// Constants, the recency parse table, and the availability
    /// contract (empty key, bad URL, and non-integral tokens refuse).
    #[test]
    fn availability_pins_the_option_contract() {
        assert_eq!(PERPLEXITY_PROVIDER_ID, "perplexity");
        assert_eq!(PERPLEXITY_DEFAULT_BASE_URL, "https://api.perplexity.ai");
        assert_eq!(PERPLEXITY_DEFAULT_MODEL, "sonar");
        assert_eq!(PERPLEXITY_DEFAULT_MAX_TOKENS, 1024.0);
        for (raw, parsed) in [
            ("day", PerplexityRecency::Day),
            ("week", PerplexityRecency::Week),
            ("month", PerplexityRecency::Month),
            ("year", PerplexityRecency::Year),
        ] {
            assert_eq!(PerplexityRecency::parse(raw), Some(parsed));
            assert_eq!(parsed.as_str(), raw);
        }
        assert_eq!(PerplexityRecency::parse("hour"), None);

        assert!(PerplexitySearchProvider::new(options()).available());
        assert!(
            !PerplexitySearchProvider::new(PerplexitySearchProviderOptions {
                api_key: String::new(),
                ..options()
            })
            .available()
        );
        assert!(
            !PerplexitySearchProvider::new(PerplexitySearchProviderOptions {
                base_url: "not a url".to_owned(),
                ..options()
            })
            .available()
        );
        assert!(
            !PerplexitySearchProvider::new(PerplexitySearchProviderOptions {
                max_tokens: 0.0,
                ..options()
            })
            .available()
        );
    }

    /// The loopback roundtrip: the chat-completions endpoint and body
    /// (with the recency filter when configured), the content mapping,
    /// and the structured-sources preference over the citations
    /// fallback.
    #[test]
    fn search_pins_the_roundtrip_and_mapping() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for request in server.incoming() {
                let mut socket = request.unwrap();
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
                let recency = text.contains("/recency");
                let body: Value =
                    serde_json::from_str(text.split("\r\n\r\n").nth(1).unwrap()).unwrap();
                let mut expected = json!({
                    "model": "sonar",
                    "max_tokens": 1024,
                    "messages": [{ "role": "user", "content": "rust" }],
                });
                if recency {
                    expected["search_recency_filter"] = json!("week");
                }
                assert_eq!(body, expected);
                assert!(text.contains("authorization: Bearer sk-pplx\r\n"));
                let payload = if recency {
                    r#"{"choices":[{"message":{"content":"an answer"}}],"search_results":[
                        {"url":"https://a.example/","title":"A","snippet":"s","date":"2026-01-01"},
                        {"url":"https://b.example/","title":null,"snippet":"","date":null}
                    ]}"#
                } else {
                    r#"{"choices":[{"message":{"content":""}}],"citations":["https://c.example/","https://d.example/"]}"#
                };
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
                    payload.len()
                );
                std::io::Write::write_all(&mut socket, wire.as_bytes()).unwrap();
            }
        });
        let base = format!("http://127.0.0.1:{port}");
        let structured = PerplexitySearchProvider::new(PerplexitySearchProviderOptions {
            base_url: format!("{base}/recency"),
            search_recency: Some(PerplexityRecency::Week),
            ..options()
        })
        .search(&request("rust"))
        .expect("search runs");
        assert_eq!(structured.content.as_deref(), Some("an answer"));
        assert_eq!(structured.sources.len(), 2);
        assert_eq!(structured.sources[0].url, "https://a.example/");
        assert_eq!(structured.sources[0].snippet.as_deref(), Some("s"));
        assert_eq!(
            structured.sources[0].published_at.as_deref(),
            Some("2026-01-01")
        );
        // Blank fields omit rather than lie; the URL-only shape holds.
        assert_eq!(structured.sources[1].title, None);
        assert_eq!(structured.sources[1].snippet, None);

        let citations = PerplexitySearchProvider::new(PerplexitySearchProviderOptions {
            base_url: format!("{base}/noc"),
            ..options()
        })
        .search(&request("rust"))
        .expect("fallback runs");
        // An empty answer omits content; the citations fallback is
        // URL-only.
        assert_eq!(citations.content, None);
        assert_eq!(citations.sources.len(), 2);
        assert_eq!(citations.sources[0].url, "https://c.example/");
        assert_eq!(citations.sources[0].title, None);
    }

    /// The error family: the string error and the nested message
    /// overwrite the default, a non-string detail keeps it, and the
    /// malformed arrays read as unprocessable.
    #[test]
    fn error_family_pins_the_detail_table() {
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
                let (status, body) = if text.contains("/string") {
                    ("401 Unauthorized", r#"{"error":"quota exceeded"}"#)
                } else if text.contains("/nested") {
                    (
                        "401 Unauthorized",
                        r#"{"error":{"message":"bad key"},"message":"ignored"}"#,
                    )
                } else if text.contains("/nostring") {
                    ("429 Too Many Requests", r#"{"error":42}"#)
                } else if text.contains("/results") {
                    ("200 OK", r#"{"search_results":"not an array"}"#)
                } else {
                    ("200 OK", r#"{"citations":"not an array"}"#)
                };
                let wire = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                std::io::Write::write_all(&mut socket, wire.as_bytes()).unwrap();
            }
        });
        let base = format!("http://127.0.0.1:{port}");
        let at = |path: &'static str| {
            PerplexitySearchProvider::new(PerplexitySearchProviderOptions {
                base_url: format!("{base}{path}"),
                ..options()
            })
        };
        assert_eq!(
            at("/string").search(&request("q")).unwrap_err().message,
            "quota exceeded"
        );
        assert_eq!(
            at("/nested").search(&request("q")).unwrap_err().message,
            "bad key"
        );
        assert_eq!(
            at("/nostring").search(&request("q")).unwrap_err().message,
            "Perplexity API error (HTTP 429)"
        );
        assert_eq!(
            at("/results").search(&request("q")).unwrap_err().message,
            "Perplexity returned an unprocessable response body: search_results is not an array"
        );
        assert_eq!(
            at("/citations").search(&request("q")).unwrap_err().message,
            "Perplexity returned an unprocessable response body: citations is not an array"
        );
    }

    /// The mapping table: absent sources, a non-object result entry,
    /// and the citations-only shape.
    #[test]
    fn map_perplexity_response_pins_the_narrowing_table() {
        let bare = map_perplexity_response(&json!({})).expect("bare envelope");
        assert_eq!(bare.content, None);
        assert!(bare.sources.is_empty());
        let mapped = map_perplexity_response(&json!({
            "choices": "not an array",
            "citations": ["https://a/", 42, "https://b/"]
        }))
        .expect("loose choices and non-string citations skip");
        assert_eq!(mapped.sources.len(), 2);
        assert_eq!(mapped.sources[0].url, "https://a/");
    }

    /// The option resolution: config values over the
    /// `$PERPLEXITY_API_KEY` fallback, then the defaults.
    #[test]
    fn resolve_options_pins_the_defaults_and_env() {
        let resolved = resolve_options(&json!({}), &|_name| None);
        assert_eq!(resolved.api_key, "");
        assert_eq!(resolved.base_url, PERPLEXITY_DEFAULT_BASE_URL);
        assert_eq!(resolved.model, "sonar");
        assert_eq!(resolved.max_tokens, 1024.0);
        assert_eq!(resolved.search_recency, None);

        let from_env = resolve_options(&json!({}), &|name| {
            (name == PERPLEXITY_API_KEY_ENV).then(|| "sk-env".to_owned())
        });
        assert_eq!(from_env.api_key, "sk-env");

        let config = resolve_options(
            &json!({
                "api-key": "sk-cfg",
                "base-url": "https://cfg.pplx/api",
                "model": "sonar-pro",
                "max-tokens": 2048,
                "search-recency": "month",
            }),
            &|_name| None,
        );
        assert_eq!(config.api_key, "sk-cfg");
        assert_eq!(config.base_url, "https://cfg.pplx/api");
        assert_eq!(config.model, "sonar-pro");
        assert_eq!(config.max_tokens, 2048.0);
        assert_eq!(config.search_recency, Some(PerplexityRecency::Month));
    }
}
