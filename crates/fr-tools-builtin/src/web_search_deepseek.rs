// Rust guideline compliant 2026-11-01
//! `DeepSeek` search through an Anthropic-compatible Messages call.
//!
//! Each search carries the native `web_search_20250305` server tool
//! (dsh `@deepseek-ai/dsh-web-search-deepseek`, 1:1). It costs a model
//! turn, but returns structured result blocks; absence of those blocks
//! is an error rather than a prose-scraping fallback. The wire format
//! and the HTTP client are provider-private and do not use the LLM
//! seam.
//!
//! fr-side structural absences, each pinned by its ruling: the
//! `DeepSeek` account sign-in arm never resolves (no account service, the Q57-B
//! dead-arm ruling), so authentication is always the API key sent as
//! both `x-api-key` and `Authorization: Bearer`; the credential plane is
//! the launching environment alone (the Q66-B env-only ruling), resolved
//! once at option resolution; the request-recording face has no session
//! event surface in fr and stays absent (the recordRequest ruling); and
//! the abort arms have no caller signal above the blocking seam (Q69-B,
//! the deadline is owned by the tool pipeline). The endpoint guidance
//! suffix keeps the dsh settings-page wording verbatim even though fr
//! has no settings page (the 19.2 clause-21 precedent).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::io::Read as _;

use fr_net::http::Fetch as _;
use serde_json::{Value, json};

use crate::web_seam::{
    WebError, WebSearchProvider, WebSearchRequest, WebSearchResult, WebSearchSource,
};

/// Stable id this provider registers under.
pub const DEEPSEEK_PROVIDER_ID: &str = "deepseek-official";

/// Default auxiliary-search endpoint, including `/v1`; `/messages` is
/// appended. `$DEEPSEEK_SEARCH_BASE_URL` overrides it independently of
/// the conversation adapter's endpoint.
pub const DEEPSEEK_DEFAULT_BASE_URL: &str = "https://api.deepseek.com/anthropic/v1";

/// Default Anthropic-format model name.
pub const DEEPSEEK_DEFAULT_MODEL: &str = "deepseek-v4-flash";

/// Default `anthropic-version` header value.
pub const DEEPSEEK_DEFAULT_API_VERSION: &str = "2023-06-01";

/// Default upper bound on generated tokens for the Messages request.
pub const DEEPSEEK_DEFAULT_MAX_TOKENS: f64 = 4096.0;

/// Default maximum `web_search` server-tool uses per request.
pub const DEEPSEEK_DEFAULT_MAX_USES: f64 = 5.0;

/// Default credential reference named by missing-key diagnostics.
pub const DEEPSEEK_DEFAULT_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";

/// Environment variable overriding the search endpoint base.
pub const SEARCH_BASE_URL_ENV: &str = "DEEPSEEK_SEARCH_BASE_URL";

/// Attribution header sent on every request (the dsh constant under the
/// workspace brand mapping; the version rides
/// [`crate::attribution::APP_IDENTITY`]; not protocol wire).
fn user_agent() -> String {
    crate::attribution::product_agent()
}

/// Resolved provider options (dsh `DeepSeekSearchProviderOptions`).
///
/// fr resolves the whole struct once at construction: the dsh
/// per-operation snapshot closure exists because settings can change
/// mid-session there, while fr config is compiled once per process.
pub struct DeepSeekSearchProviderOptions {
    /// Literal `DeepSeek` API key; `None` means the environment lookup of
    /// `api_key_env` already came up empty.
    pub api_key: Option<String>,
    /// Credential reference named by missing-key diagnostics.
    pub api_key_env: String,
    /// Endpoint base; `/messages` is appended.
    pub base_url: String,
    /// Anthropic-format model name.
    pub model: String,
    /// `anthropic-version` header value.
    pub api_version: String,
    /// Upper bound on generated tokens for the Messages request.
    pub max_tokens: f64,
    /// Maximum `web_search` server-tool uses per request.
    pub max_uses: f64,
}

impl std::fmt::Debug for DeepSeekSearchProviderOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeepSeekSearchProviderOptions")
            .field("api_key", &self.api_key.as_ref().map(|_| "set"))
            .field("api_key_env", &self.api_key_env)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_version", &self.api_version)
            .field("max_tokens", &self.max_tokens)
            .field("max_uses", &self.max_uses)
            .finish()
    }
}

/// Resolve one provider option set from one config object.
///
/// The dsh `resolveOptions` defaults: config value, then the
/// search-endpoint environment override, then the constants; the
/// literal key, then the credential-ref-named variable.
#[must_use]
pub fn resolve_options(
    config: &Value,
    env: &dyn Fn(&str) -> Option<String>,
) -> DeepSeekSearchProviderOptions {
    let api_key_env = config
        .get("api-key-env")
        .and_then(Value::as_str)
        .unwrap_or(DEEPSEEK_DEFAULT_API_KEY_ENV)
        .to_owned();
    let api_key = config
        .get("api-key")
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
        .or_else(|| env(&api_key_env).filter(|key| !key.is_empty()));
    let base_url = config
        .get("base-url")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| env(SEARCH_BASE_URL_ENV))
        .unwrap_or_else(|| DEEPSEEK_DEFAULT_BASE_URL.to_owned());
    DeepSeekSearchProviderOptions {
        api_key,
        api_key_env,
        base_url,
        model: config
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(DEEPSEEK_DEFAULT_MODEL)
            .to_owned(),
        api_version: config
            .get("api-version")
            .and_then(Value::as_str)
            .unwrap_or(DEEPSEEK_DEFAULT_API_VERSION)
            .to_owned(),
        max_tokens: config
            .get("max-tokens")
            .and_then(Value::as_f64)
            .unwrap_or(DEEPSEEK_DEFAULT_MAX_TOKENS),
        max_uses: config
            .get("max-uses")
            .and_then(Value::as_f64)
            .unwrap_or(DEEPSEEK_DEFAULT_MAX_USES),
    }
}

/// Build a `url → cited_text` map from every `text` block's
/// `citations[]`.
///
/// This is the snippet source: Anthropic `web_search_result` items
/// carry `url`/`title`/`page_age` but typically NO inline snippet —
/// the excerpt lives in a separate `text` block's citation, keyed by
/// `url` (first occurrence wins). A `text` block whose `citations` is
/// present but not an array is the dsh `for..of` `TypeError` shape and
/// reports through the unprocessable arm.
///
/// # Errors
///
/// The unprocessable message when a `citations` field is not an array.
pub fn citation_snippets(blocks: &[Value]) -> Result<HashMap<String, String>, String> {
    let mut map = HashMap::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        let Some(citations) = block.get("citations").filter(|value| !value.is_null()) else {
            continue;
        };
        let Some(citations) = citations.as_array() else {
            return Err("text block citations is not an array".to_owned());
        };
        for cite in citations {
            let url = cite.get("url").and_then(Value::as_str);
            let cited_text = cite.get("cited_text").and_then(Value::as_str);
            if let (Some(url), Some(cited_text)) = (url, cited_text)
                && !url.is_empty()
                && !cited_text.is_empty()
                && !map.contains_key(url)
            {
                map.insert(url.to_owned(), cited_text.to_owned());
            }
        }
    }
    Ok(map)
}

/// Map a `DeepSeek` Anthropic Messages response to a search result.
///
/// dsh `mapAnthropicResponse`: walks `web_search_tool_result` blocks
/// for citeable `web_search_result` items, joins each to its citation
/// excerpt as `snippet`, and dedupes by `url` (a `max_uses > 1`
/// request can surface the same URL across searches). The web service
/// owns the final `maxResults` truncation, so `truncated` is always
/// `false` here.
///
/// # Errors
///
/// The failure message for the provider wrapper: the verbatim no-blocks
/// text, or the unprocessable-body text describing the malformed shape
/// (the dsh `TypeError` arm).
pub fn map_anthropic_response(response: &Value) -> Result<WebSearchResult, String> {
    let unprocessable =
        |detail: &str| format!("DeepSeek returned an unprocessable response body: {detail}");
    let content = response.get("content");
    let blocks: Vec<Value> = match content {
        None | Some(Value::Null) => Vec::new(),
        Some(content) => content
            .as_array()
            .cloned()
            .ok_or_else(|| unprocessable("content is not an array"))?,
    };
    let result_blocks: Vec<&Value> = blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("web_search_tool_result"))
        .collect();
    if result_blocks.is_empty() {
        return Err(
            "DeepSeek returned no web_search_tool_result blocks; the request may not have triggered native web search"
                .to_owned(),
        );
    }
    let snippets = citation_snippets(&blocks).map_err(|detail| unprocessable(&detail))?;
    let mut seen = std::collections::HashSet::new();
    let mut sources = Vec::new();
    for block in &result_blocks {
        let Some(items) = block.get("content").filter(|value| !value.is_null()) else {
            continue;
        };
        let Some(items) = items.as_array() else {
            return Err(unprocessable(
                "web_search_tool_result content is not an array",
            ));
        };
        for item in items {
            if item.is_null() {
                return Err(unprocessable("result item is null"));
            }
            let Some(item) = item.as_object() else {
                continue;
            };
            if item.get("type").and_then(Value::as_str) != Some("web_search_result") {
                continue;
            }
            // A typed item without a string url is the dsh
            // `item.url.length` TypeError shape: the url field is part
            // of the result-item contract.
            let Some(url) = item.get("url").and_then(Value::as_str) else {
                return Err(unprocessable("web_search_result item has no url string"));
            };
            if url.is_empty() || !seen.insert(url.to_owned()) {
                continue;
            }
            let optional = |name: &str| {
                item.get(name)
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            };
            sources.push(WebSearchSource {
                url: url.to_owned(),
                title: optional("title"),
                snippet: snippets.get(url).cloned(),
                published_at: optional("page_age"),
            });
        }
    }
    Ok(WebSearchResult {
        content: None,
        sources,
        truncated: false,
    })
}

/// The DeepSeek-backed search provider (dsh `DeepSeekSearchProvider`).
/// HTTP redirects fail as `WEB_PROVIDER_ERROR`; failures after dispatch
/// name the endpoint and tell the model how the user can configure it.
pub struct DeepSeekSearchProvider {
    options: DeepSeekSearchProviderOptions,
}

impl std::fmt::Debug for DeepSeekSearchProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeepSeekSearchProvider")
            .field("options", &self.options)
            .finish()
    }
}

impl DeepSeekSearchProvider {
    /// Build the provider over one resolved option set.
    #[must_use]
    pub const fn new(options: DeepSeekSearchProviderOptions) -> Self {
        Self { options }
    }

    /// Resolve the operation's API key without retaining it beyond the
    /// call (dsh `apiKey`; the credential-service leg stays absent with
    /// the fr credentials seam — the environment is the whole plane).
    fn api_key(&self) -> Result<&str, WebError> {
        let resolved = self
            .options
            .api_key
            .as_deref()
            .filter(|api_key| !api_key.is_empty());
        if let Some(api_key) = resolved {
            return Ok(api_key);
        }
        Err(WebError::new(
            "WEB_PROVIDER_CREDENTIAL_MISSING",
            format!(
                "DeepSeek search has no API key for \"{}\"; store it through the credentials service \
                 (the web Models page writes it), export it in the launching environment, or set a literal \
                 \"apiKey\" in the web-search-deepseek config; a conversation using a DeepSeek Account model \
                 searches with the account sign-in instead",
                self.options.api_key_env
            ),
        ))
    }

    /// One dispatch: POST the Messages request, map transport and HTTP
    /// failures onto the provider error family (dsh `search`, from the
    /// fetch call through the response decode; the abort arms stay
    /// absent with the fr caller signal).
    fn dispatch(&self, endpoint: &str, api_key: &str, body: &Value) -> Result<Value, WebError> {
        let payload = serde_json::to_vec(body).map_err(|serde_failure| {
            WebError::new(
                "WEB_PROVIDER_ERROR",
                format!("DeepSeek search request failed: {serde_failure}"),
            )
        })?;
        let response = fr_net::http::UreqFetch::shared()
            .fetch(fr_net::http::HttpRequest {
                method: fr_net::http::HttpMethod::Post,
                url: endpoint.to_owned(),
                headers: vec![
                    ("x-api-key".to_owned(), api_key.to_owned()),
                    ("authorization".to_owned(), format!("Bearer {api_key}")),
                    (
                        "anthropic-version".to_owned(),
                        self.options.api_version.clone(),
                    ),
                    ("content-type".to_owned(), "application/json".to_owned()),
                    ("accept".to_owned(), "application/json".to_owned()),
                    ("user-agent".to_owned(), user_agent()),
                ],
                body: Some(payload),
            })
            .map_err(|failure| {
                WebError::new(
                    "WEB_PROVIDER_ERROR",
                    endpoint_guidance(
                        endpoint,
                        &format!("DeepSeek search request failed: {}", failure.message()),
                    ),
                )
            })?;
        if !(200..300).contains(&response.status) {
            let status = response.status;
            let mut message = format!("DeepSeek API error (HTTP {status})");
            let mut body = response.body;
            let mut bytes = Vec::new();
            if body.read_to_end(&mut bytes).is_ok()
                && let Ok(parsed) = serde_json::from_slice::<Value>(&bytes)
                && let Some(detail) = anthropic_error_detail(&parsed)
            {
                message.push_str(": ");
                message.push_str(&detail);
            }
            return Err(WebError::new(
                "WEB_PROVIDER_ERROR",
                endpoint_guidance(endpoint, &message),
            ));
        }
        read_and_parse(response).map_err(|message| {
            WebError::new("WEB_PROVIDER_ERROR", endpoint_guidance(endpoint, &message))
        })
    }
}

/// Read one success body and parse it as JSON, mapping either failure
/// onto the unprocessable message.
fn read_and_parse(mut response: fr_net::http::HttpResponse) -> Result<Value, String> {
    let mut bytes = Vec::new();
    response
        .body
        .read_to_end(&mut bytes)
        .map_err(|read_failure| {
            format!("DeepSeek returned an unprocessable response body: {read_failure}")
        })?;
    serde_json::from_slice(&bytes).map_err(|parse_failure| {
        format!("DeepSeek returned an unprocessable response body: {parse_failure}")
    })
}

/// Extract the best-effort error detail from a `DeepSeek` envelope.
///
/// dsh `AnthropicError` narrowing: `error` as a string, else
/// `error.message ?? message` — the `??` falls through only on a
/// missing or null field, and a non-string detail never appends.
#[must_use]
pub fn anthropic_error_detail(parsed: &Value) -> Option<String> {
    let object = parsed.as_object()?;
    let fallback = || object.get("message");
    let detail: Option<&Value> = object
        .get("error")
        .map_or_else(fallback, |error| match error {
            // A string error is the detail itself.
            Value::String(_) => Some(error),
            Value::Object(inner) => match inner.get("message") {
                // `??` falls through only on a missing or null field.
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

/// Add endpoint recovery instructions to failures that occur after
/// request dispatch begins (dsh `searchEndpointError`; the settings-page
/// wording stays verbatim under the 19.2 clause-21 precedent).
#[must_use]
pub fn search_endpoint_error(endpoint: &str, message: &str) -> WebError {
    WebError::new("WEB_PROVIDER_ERROR", endpoint_guidance(endpoint, message))
}

/// The composed recovery message behind [`search_endpoint_error`]; the
/// endpoint renders the `JSON.stringify` way.
#[must_use]
pub fn endpoint_guidance(endpoint: &str, message: &str) -> String {
    let endpoint = Value::String(endpoint.to_owned()).to_string();
    format!(
        "{message}\n\nThe web request used endpoint {endpoint}. \
         Search endpoint configuration is separate from chat. If that endpoint is not intended, \
         guide the user to Plugins > Built-in plugin settings > Web search, where they can \
         change and save Messages API base URL. If that settings page is unavailable, the user can set \
         web-search-deepseek.baseURL to a trusted DeepSeek endpoint. Only the user should choose or change the endpoint."
    )
}

/// True for a request limit that can be sent to the Messages API (dsh
/// `isPositiveInteger`).
const fn is_positive_integer(value: f64) -> bool {
    value.fract() == 0.0 && value > 0.0
}

/// Cast a validated positive-integral config number for the wire body.
///
/// # Errors
///
/// A defensive [`WebError`] when the number is not a positive integer —
/// the seam checks [`WebSearchProvider::available`] before selection,
/// so this arm is structurally unreachable in operation.
fn integral(value: f64, name: &str) -> Result<u64, WebError> {
    if !is_positive_integer(value) {
        return Err(WebError::new(
            "WEB_PROVIDER_ERROR",
            format!("DeepSeek search request failed: {name} is not a positive integer"),
        ));
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "validated above as a positive integral number"
    )]
    Ok(value as u64)
}

impl WebSearchProvider for DeepSeekSearchProvider {
    fn id(&self) -> &str {
        DEEPSEEK_PROVIDER_ID
    }

    /// The credential face is structurally present (environment
    /// resolution), so the key does not gate availability — a missing
    /// key surfaces per search as `WEB_PROVIDER_CREDENTIAL_MISSING`
    /// (the dsh `resolveApiKey !== undefined` arm).
    fn available(&self) -> bool {
        url::Url::parse(&self.options.base_url).is_ok()
            && is_positive_integer(self.options.max_tokens)
            && is_positive_integer(self.options.max_uses)
    }

    /// Run one search against the Messages endpoint.
    ///
    /// # Errors
    ///
    /// [`WebError`] with the provider's code family; post-dispatch
    /// failures carry the endpoint guidance suffix.
    fn search(&self, request: &WebSearchRequest) -> Result<WebSearchResult, WebError> {
        let options = &self.options;
        let endpoint = format!("{}/messages", options.base_url);
        let api_key = self.api_key()?;
        let body = json!({
            "model": options.model,
            "max_tokens": integral(options.max_tokens, "maxTokens")?,
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": format!("Perform a web search for the query: {}", request.query),
                }],
            }],
            "tools": [{
                "type": "web_search_20250305",
                "name": "web_search",
                "max_uses": integral(options.max_uses, "maxUses")?,
            }],
            "thinking": { "type": "disabled" },
        });
        let payload = self.dispatch(&endpoint, api_key, &body)?;
        map_anthropic_response(&payload)
            .map_err(|message| search_endpoint_error(&endpoint, &message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> DeepSeekSearchProviderOptions {
        DeepSeekSearchProviderOptions {
            api_key: Some("sk-test".to_owned()),
            api_key_env: DEEPSEEK_DEFAULT_API_KEY_ENV.to_owned(),
            base_url: DEEPSEEK_DEFAULT_BASE_URL.to_owned(),
            model: DEEPSEEK_DEFAULT_MODEL.to_owned(),
            api_version: DEEPSEEK_DEFAULT_API_VERSION.to_owned(),
            max_tokens: DEEPSEEK_DEFAULT_MAX_TOKENS,
            max_uses: DEEPSEEK_DEFAULT_MAX_USES,
        }
    }

    fn provider_at(base_url: &str) -> DeepSeekSearchProvider {
        DeepSeekSearchProvider::new(DeepSeekSearchProviderOptions {
            base_url: base_url.to_owned(),
            ..options()
        })
    }

    fn request(query: &str) -> WebSearchRequest {
        WebSearchRequest {
            query: query.to_owned(),
            max_results: None,
        }
    }

    /// Read one full HTTP request off the socket: loopback segments may
    /// split the header and body writes, and answering before the body
    /// lands resets the client's pending write.
    fn read_request(socket: &mut std::net::TcpStream) -> String {
        let mut buffer = Vec::new();
        let mut window = [0u8; 4096];
        loop {
            let read = std::io::Read::read(socket, &mut window).unwrap_or(0);
            buffer.extend_from_slice(&window[..read]);
            let text = String::from_utf8_lossy(&buffer).to_string();
            if let Some(index) = text.find("\r\n\r\n") {
                let declared = text
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().ok())
                    })
                    .flatten()
                    .unwrap_or(0);
                if text.len() >= index + 4 + declared {
                    return text;
                }
            }
            if read == 0 {
                return text;
            }
        }
    }

    /// The loopback sample: two result blocks sharing one URL, one text
    /// block carrying the citation join.
    fn sample_response() -> &'static str {
        r#"{"content":[
            {"type":"web_search_tool_result","content":[
                {"type":"web_search_result","url":"https://docs.example/ssrf","title":"SSRF guide","page_age":"2 days ago"},
                {"type":"web_search_result","url":"https://docs.example/other"}
            ]},
            {"type":"text","text":"Here is what I found.","citations":[
                {"url":"https://docs.example/ssrf","cited_text":"the cited excerpt"}
            ]},
            {"type":"web_search_tool_result","content":[
                {"type":"web_search_result","url":"https://docs.example/ssrf","title":"dup","page_age":"old"}
            ]}
        ]}"#
    }

    /// Constants and the availability table: the structural credential
    /// face keeps a missing key available; bad URLs and non-positive
    /// limits refuse.
    #[test]
    fn availability_pins_the_option_contract() {
        assert_eq!(DEEPSEEK_PROVIDER_ID, "deepseek-official");
        assert_eq!(
            DEEPSEEK_DEFAULT_BASE_URL,
            "https://api.deepseek.com/anthropic/v1"
        );
        assert_eq!(DEEPSEEK_DEFAULT_MODEL, "deepseek-v4-flash");
        assert_eq!(DEEPSEEK_DEFAULT_API_VERSION, "2023-06-01");
        assert_eq!(DEEPSEEK_DEFAULT_MAX_TOKENS, 4096.0);
        assert_eq!(DEEPSEEK_DEFAULT_MAX_USES, 5.0);

        let no_key = DeepSeekSearchProvider::new(DeepSeekSearchProviderOptions {
            api_key: None,
            ..options()
        });
        assert!(no_key.available());
        assert!(
            !DeepSeekSearchProvider::new(DeepSeekSearchProviderOptions {
                base_url: "not a url".to_owned(),
                ..options()
            })
            .available()
        );
        assert!(
            !DeepSeekSearchProvider::new(DeepSeekSearchProviderOptions {
                max_tokens: 0.0,
                ..options()
            })
            .available()
        );
        assert!(
            !DeepSeekSearchProvider::new(DeepSeekSearchProviderOptions {
                max_uses: 1.5,
                ..options()
            })
            .available()
        );
    }

    /// The full loopback dispatch: endpoint path, method, every header,
    /// the exact body shape, and the response mapping (citation join,
    /// dedupe, publishedAt).
    #[test]
    fn dispatch_pins_the_messages_roundtrip() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            let text = read_request(&mut socket);
            assert!(text.starts_with("POST /v1/messages HTTP/1.1\r\n"));
            assert!(text.contains("x-api-key: sk-test\r\n"));
            assert!(text.contains("authorization: Bearer sk-test\r\n"));
            assert!(text.contains("anthropic-version: 2023-06-01\r\n"));
            assert!(text.contains("content-type: application/json\r\n"));
            assert!(text.contains("accept: application/json\r\n"));
            assert!(text.contains(&format!("user-agent: {}\r\n", user_agent())));
            let payload = text.split("\r\n\r\n").nth(1).unwrap();
            let body: Value = serde_json::from_str(payload).unwrap();
            assert_eq!(
                body,
                json!({
                    "model": "deepseek-v4-flash",
                    "max_tokens": 4096,
                    "messages": [{
                        "role": "user",
                        "content": [{
                            "type": "text",
                            "text": "Perform a web search for the query: rust SSRF",
                        }],
                    }],
                    "tools": [{
                        "type": "web_search_20250305",
                        "name": "web_search",
                        "max_uses": 5,
                    }],
                    "thinking": { "type": "disabled" },
                })
            );
            let payload = sample_response();
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
                payload.len()
            );
            std::io::Write::write_all(&mut socket, wire.as_bytes()).unwrap();
        });

        let outcome = provider_at(&format!("http://127.0.0.1:{port}/v1"))
            .search(&request("rust SSRF"))
            .expect("search runs");
        assert!(!outcome.truncated);
        assert_eq!(outcome.sources.len(), 2);
        assert_eq!(outcome.sources[0].url, "https://docs.example/ssrf");
        assert_eq!(outcome.sources[0].title.as_deref(), Some("SSRF guide"));
        assert_eq!(
            outcome.sources[0].snippet.as_deref(),
            Some("the cited excerpt")
        );
        assert_eq!(
            outcome.sources[0].published_at.as_deref(),
            Some("2 days ago")
        );
        // The same URL across result blocks dedupes.
        assert_eq!(outcome.sources[1].url, "https://docs.example/other");
        assert_eq!(outcome.sources[1].snippet, None);
    }

    /// A response without result blocks refuses with the verbatim
    /// message plus the endpoint guidance suffix.
    #[test]
    fn missing_result_blocks_pin_the_provider_error() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            let _ = read_request(&mut socket);
            let body = r#"{"content":[{"type":"text","text":"no search happened"}]}"#;
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            std::io::Write::write_all(&mut socket, wire.as_bytes()).unwrap();
        });
        let error = provider_at(&format!("http://127.0.0.1:{port}"))
            .search(&request("q"))
            .unwrap_err();
        assert_eq!(error.code, "WEB_PROVIDER_ERROR");
        assert_eq!(
            error.message,
            format!(
                "DeepSeek returned no web_search_tool_result blocks; the request may not have triggered native web search\n\n\
                 The web request used endpoint \"http://127.0.0.1:{port}/messages\". \
                 Search endpoint configuration is separate from chat. If that endpoint is not intended, \
                 guide the user to Plugins > Built-in plugin settings > Web search, where they can \
                 change and save Messages API base URL. If that settings page is unavailable, the user can set \
                 web-search-deepseek.baseURL to a trusted DeepSeek endpoint. Only the user should choose or change the endpoint."
            )
        );
    }

    /// A missing key refuses before any network traffic with the
    /// verbatim credential message naming the credential ref.
    #[test]
    fn missing_key_pins_the_credential_error() {
        let provider = DeepSeekSearchProvider::new(DeepSeekSearchProviderOptions {
            api_key: None,
            api_key_env: "MY_SEARCH_KEY".to_owned(),
            base_url: "http://127.0.0.1:9".to_owned(),
            ..options()
        });
        let error = provider.search(&request("q")).unwrap_err();
        assert_eq!(error.code, "WEB_PROVIDER_CREDENTIAL_MISSING");
        assert_eq!(
            error.message,
            concat!(
                "DeepSeek search has no API key for \"MY_SEARCH_KEY\"; store it through the credentials service",
                " (the web Models page writes it), export it in the launching environment, or set a literal",
                " \"apiKey\" in the web-search-deepseek config; a conversation using a DeepSeek Account model",
                " searches with the account sign-in instead",
            )
        );
    }

    /// An HTTP error status appends the envelope detail and the
    /// endpoint guidance; a non-JSON body and a non-string detail keep
    /// the bare status text.
    #[test]
    fn http_error_statuses_pin_the_detail_extraction() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for request in server.incoming() {
                let mut socket = request.unwrap();
                let text = read_request(&mut socket);
                let body = if text.contains("POST /rich") {
                    r#"{"error":{"message":"invalid api key"}}"#
                } else if text.contains("POST /string") {
                    r#"{"error":"quota exceeded"}"#
                } else if text.contains("POST /bare") {
                    r#"{"error":{"message":42},"message":"ignored"}"#
                } else {
                    "gateway 502 html"
                };
                let wire = format!(
                    "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                std::io::Write::write_all(&mut socket, wire.as_bytes()).unwrap();
            }
        });
        let base = format!("http://127.0.0.1:{port}");
        let suffix = |path: &str| {
            format!(
                "\n\nThe web request used endpoint \"{base}{path}/messages\". \
                 Search endpoint configuration is separate from chat. If that endpoint is not intended, \
                 guide the user to Plugins > Built-in plugin settings > Web search, where they can \
                 change and save Messages API base URL. If that settings page is unavailable, the user can set \
                 web-search-deepseek.baseURL to a trusted DeepSeek endpoint. Only the user should choose or change the endpoint."
            )
        };
        let at = |path: &str| provider_at(&format!("{base}{path}"));

        let rich = at("/rich").search(&request("q")).unwrap_err();
        assert_eq!(
            rich.message,
            format!(
                "DeepSeek API error (HTTP 401): invalid api key{}",
                suffix("/rich")
            )
        );
        let string_form = at("/string").search(&request("q")).unwrap_err();
        assert_eq!(
            string_form.message,
            format!(
                "DeepSeek API error (HTTP 401): quota exceeded{}",
                suffix("/string")
            )
        );
        // A non-string detail (a numeric message) never appends.
        let bare = at("/bare").search(&request("q")).unwrap_err();
        assert_eq!(
            bare.message,
            format!("DeepSeek API error (HTTP 401){}", suffix("/bare"))
        );
        // A non-JSON error body only costs the detail, never the status.
        let html = at("/html").search(&request("q")).unwrap_err();
        assert_eq!(
            html.message,
            format!("DeepSeek API error (HTTP 401){}", suffix("/html"))
        );
    }

    /// The envelope-detail narrowing table: string error, object error,
    /// message fallback, non-string ignored.
    #[test]
    fn anthropic_error_detail_pins_the_narrowing_table() {
        assert_eq!(
            anthropic_error_detail(&json!({"error": "plain"})).as_deref(),
            Some("plain")
        );
        assert_eq!(
            anthropic_error_detail(&json!({"error": {"message": "nested"}})).as_deref(),
            Some("nested")
        );
        assert_eq!(
            anthropic_error_detail(&json!({"error": {}, "message": "fallback"})).as_deref(),
            Some("fallback")
        );
        assert_eq!(
            anthropic_error_detail(&json!({"message": "only message"})).as_deref(),
            Some("only message")
        );
        assert_eq!(
            anthropic_error_detail(&json!({"error": {"message": 42}})),
            None
        );
        assert_eq!(anthropic_error_detail(&json!(null)), None);
        assert_eq!(anthropic_error_detail(&json!("text")), None);
    }

    /// A malformed success body reports through the unprocessable arm.
    #[test]
    fn malformed_shapes_pin_the_unprocessable_arm() {
        assert_eq!(
            map_anthropic_response(&json!({"content": 5})).unwrap_err(),
            "DeepSeek returned an unprocessable response body: content is not an array"
        );
        // The dsh order surfaces the no-blocks error before the
        // citations TypeError, so this shape needs a result block in
        // front to reach the citations arm.
        assert_eq!(
            map_anthropic_response(&json!({"content": [
                {"type": "web_search_tool_result", "content": []},
                {"type": "text", "citations": 7}
            ]}))
            .unwrap_err(),
            "DeepSeek returned an unprocessable response body: text block citations is not an array"
        );
        assert_eq!(
            map_anthropic_response(&json!({"content": [
                {"type": "web_search_tool_result", "content": [null]}
            ]}))
            .unwrap_err(),
            "DeepSeek returned an unprocessable response body: result item is null"
        );
        assert_eq!(
            map_anthropic_response(&json!({"content": [
                {"type": "web_search_tool_result", "content": [{"type": "web_search_result"}]}
            ]}))
            .unwrap_err(),
            "DeepSeek returned an unprocessable response body: web_search_result item has no url string"
        );
        // An entirely empty body is the no-blocks error (the dsh empty
        // resultBlocks arm); a lone result block with no items is the
        // empty-sources success.
        assert_eq!(
            map_anthropic_response(&json!({})).unwrap_err(),
            "DeepSeek returned no web_search_tool_result blocks; the request may not have triggered native web search"
        );
        let bare_block = map_anthropic_response(&json!({"content": [
            {"type": "web_search_tool_result"}
        ]}))
        .expect("a result block with no items is empty sources");
        assert!(bare_block.sources.is_empty());
    }

    /// First occurrence wins per URL; blank urls and cited texts skip.
    #[test]
    fn citation_snippets_pin_first_occurrence_and_blanks() {
        let blocks = json!([
            {"type": "text", "citations": [
                {"url": "https://a/", "cited_text": "first"},
                {"url": "https://a/", "cited_text": "second"},
                {"url": "", "cited_text": "blank url"},
                {"url": "https://b/", "cited_text": ""}
            ]},
            {"type": "text", "text": "no citations field"}
        ]);
        let map = citation_snippets(blocks.as_array().unwrap()).expect("parses");
        assert_eq!(map.get("https://a/").map(String::as_str), Some("first"));
        assert_eq!(map.len(), 1);
    }

    /// The option resolution table: config values, then the endpoint
    /// environment override, then the constants; the key reads the
    /// credential-ref-named variable.
    #[test]
    fn resolve_options_pins_defaults_and_env_fallbacks() {
        let empty = resolve_options(&json!({}), &|_name| None);
        assert_eq!(empty.base_url, DEEPSEEK_DEFAULT_BASE_URL);
        assert_eq!(empty.model, DEEPSEEK_DEFAULT_MODEL);
        assert_eq!(empty.api_version, DEEPSEEK_DEFAULT_API_VERSION);
        assert_eq!(empty.max_tokens, 4096.0);
        assert_eq!(empty.max_uses, 5.0);
        assert_eq!(empty.api_key_env, "DEEPSEEK_API_KEY");
        assert_eq!(empty.api_key, None);

        let env = |name: &str| {
            (name == SEARCH_BASE_URL_ENV).then(|| "https://search.example/anthropic/v1".to_owned())
        };
        let from_env = resolve_options(&json!({}), &env);
        assert_eq!(from_env.base_url, "https://search.example/anthropic/v1");

        let config = resolve_options(
            &json!({
                "api-key": "sk-cfg",
                "api-key-env": "CFG_KEY",
                "base-url": "https://cfg.example/v1",
                "model": "deepseek-v4",
                "api-version": "2024-01-01",
                "max-tokens": 8,
                "max-uses": 2,
            }),
            &env,
        );
        assert_eq!(config.api_key.as_deref(), Some("sk-cfg"));
        assert_eq!(config.api_key_env, "CFG_KEY");
        assert_eq!(config.base_url, "https://cfg.example/v1");
        assert_eq!(config.model, "deepseek-v4");
        assert_eq!(config.api_version, "2024-01-01");
        assert_eq!(config.max_tokens, 8.0);
        assert_eq!(config.max_uses, 2.0);
    }
}
