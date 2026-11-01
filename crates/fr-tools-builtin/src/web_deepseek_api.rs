// Rust guideline compliant 2026-11-01
//! The `DeepSeek` Search and Fetch API providers.
//!
//! One direct request per operation, without an auxiliary model
//! request (dsh `@deepseek-ai/dsh-web-deepseek-api`, 1:1 —
//! `deepseek-search-api` searches `/search`, `deepseek-fetch-api`
//! retrieves page Markdown through `/fetch`). Both read this section's
//! credential and API base.
//!
//! fr-side structural absences carry the same rulings as the search
//! provider: no account sign-in (Q57-B), environment-only credentials
//! resolved at option construction (Q66-B), no request-recording or
//! abort face (Q69-B), and the guidance suffix keeps the dsh settings
//! wording verbatim (the 19.2 clause-21 precedent).

#![forbid(unsafe_code)]

use std::io::Read as _;
use std::sync::Arc;

use fr_net::http::Fetch as _;

use serde_json::{Value, json};

use crate::web_seam::{
    WebError, WebFetchBody, WebFetchProvider, WebFetchRequest, WebFetchResult, WebSearchProvider,
    WebSearchRequest, WebSearchResult, WebSearchSource,
};

/// Stable id the Search API provider registers under in the search
/// registry.
pub const DEEPSEEK_SEARCH_API_PROVIDER_ID: &str = "deepseek-search-api";

/// Stable id the Fetch API provider registers under in the fetch
/// registry.
pub const DEEPSEEK_FETCH_API_PROVIDER_ID: &str = "deepseek-fetch-api";

/// Default Search and Fetch API base, including its `/v1/tools`
/// prefix; `/search` and `/fetch` are appended.
pub const DEEPSEEK_WEB_API_BASE_URL: &str = "https://api.deepseek.com/v1/tools";

/// Default Search API result count, the service's own default.
pub const DEEPSEEK_DEFAULT_COUNT: f64 = 10.0;

/// Default credential reference named by missing-key diagnostics.
pub const DEEPSEEK_DEFAULT_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";

/// Longest `ori_question` the Search API accepts, in characters.
const ORIGINAL_QUESTION_MAX_CHARS: usize = 128;

/// User-owned endpoint settings named by dispatch failures (dsh
/// `DeepSeekWebGuidance`).
#[derive(Debug, Clone)]
pub struct DeepSeekWebGuidance {
    /// Capability whose endpoint failed.
    pub subject: &'static str,
    /// Plugin configuration namespace.
    pub plugin: &'static str,
    /// Endpoint configuration field.
    pub field: &'static str,
    /// Local settings label.
    pub label: &'static str,
    /// Built-in settings page title.
    pub page: &'static str,
}

/// Settings named by endpoint failures of either API in this module
/// (dsh `DEEPSEEK_WEB_API_GUIDANCE`).
pub const DEEPSEEK_WEB_API_GUIDANCE: DeepSeekWebGuidance = DeepSeekWebGuidance {
    subject: "",
    plugin: "web-deepseek-api",
    field: "baseURL",
    label: "Search / Fetch API base URL",
    page: "Web search",
};

/// Authentication values resolved for one operation (dsh
/// `DeepSeekWebAuth` minus the dead account arm — the Q57-B ruling).
#[derive(Debug, Clone)]
pub struct DeepSeekWebAuth {
    /// Literal API key; `None` means the environment lookup of
    /// `api_key_env` already came up empty.
    pub api_key: Option<String>,
    /// Credential reference named by missing-key diagnostics.
    pub api_key_env: String,
}

/// The credential face is structurally present (environment
/// resolution), so the key does not gate availability (dsh
/// `hasDeepSeekWebCredential`: the resolver is always defined here).
#[must_use]
pub const fn has_deep_seek_web_credential(_auth: &DeepSeekWebAuth) -> bool {
    true
}

/// Resolve one operation's API key without retaining it beyond the call
/// (dsh `resolveApiKey`; the credential-service leg stays absent with
/// the fr credentials seam).
///
/// # Errors
///
/// [`WebError`] `WEB_PROVIDER_CREDENTIAL_MISSING` with the verbatim
/// message when no key resolves.
pub fn resolve_api_key(auth: &DeepSeekWebAuth, plugin: &str) -> Result<String, WebError> {
    if let Some(api_key) = auth.api_key.as_deref()
        && !api_key.is_empty()
    {
        return Ok(api_key.to_owned());
    }
    Err(WebError::new(
        "WEB_PROVIDER_CREDENTIAL_MISSING",
        format!(
            "DeepSeek web access has no API key for \"{}\"; store it through the credentials service \
             (the web Models page writes it), export it in the launching environment, or set a literal \
             \"apiKey\" in the {plugin} config; a conversation using a DeepSeek Account model \
             searches with the account sign-in instead",
            auth.api_key_env
        ),
    ))
}

/// Report malformed upstream JSON without embedding response content
/// (dsh `invalidResponse`).
#[must_use]
pub fn invalid_response() -> String {
    "DeepSeek returned an invalid response body".to_owned()
}

/// Add endpoint recovery instructions to failures that occur after
/// request dispatch begins (dsh `searchEndpointError`; the settings
/// wording stays verbatim under the 19.2 clause-21 precedent).
#[must_use]
pub fn endpoint_guidance(guidance: &DeepSeekWebGuidance, endpoint: &str, message: &str) -> String {
    let endpoint = Value::String(endpoint.to_owned()).to_string();
    format!(
        "{message}\n\nThe web request used endpoint {endpoint}. \
         {subject} endpoint configuration is separate from chat. If that endpoint is not intended, \
         guide the user to Plugins > Built-in plugin settings > {page}, where they can \
         change and save {label}. If that settings page is unavailable, the user can set \
         {plugin}.{field} to a trusted DeepSeek endpoint. Only the user should choose or change the endpoint.",
        subject = guidance.subject,
        page = guidance.page,
        label = guidance.label,
        plugin = guidance.plugin,
        field = guidance.field
    )
}

/// Extract the best-effort error detail from one error envelope (dsh
/// `isRecord` narrowing over `error` as a string, `error.message`
/// through `??`, then `message`): only a nonempty string appends.
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

/// One dispatch (dsh `dispatchDeepSeekWeb`).
///
/// Resolves the key, POSTs the JSON body with redirects refused, maps
/// transport and HTTP failures onto the guidance-bearing error, and
/// decodes the success body. The abort arms stay absent with the fr
/// caller signal.
///
/// # Errors
///
/// [`WebError`] with the provider's code family; every post-dispatch
/// failure carries the endpoint guidance suffix.
pub fn dispatch_deep_seek_web<T>(
    auth: &DeepSeekWebAuth,
    endpoint: &str,
    body: &Value,
    guidance: &DeepSeekWebGuidance,
    decode: impl Fn(&Value) -> Result<T, String>,
) -> Result<T, WebError> {
    let api_key = resolve_api_key(auth, guidance.plugin)?;
    let payload = serde_json::to_vec(body).map_err(|serde_failure| {
        WebError::new(
            "WEB_PROVIDER_ERROR",
            format!("DeepSeek web request failed: {serde_failure}"),
        )
    })?;
    let response = fr_net::http::UreqFetch::shared()
        .fetch(fr_net::http::HttpRequest {
            method: fr_net::http::HttpMethod::Post,
            url: endpoint.to_owned(),
            headers: vec![
                ("authorization".to_owned(), format!("Bearer {api_key}")),
                ("content-type".to_owned(), "application/json".to_owned()),
                ("accept".to_owned(), "application/json".to_owned()),
                ("user-agent".to_owned(), crate::attribution::product_agent()),
            ],
            body: Some(payload),
        })
        .map_err(|failure| {
            WebError::new(
                "WEB_PROVIDER_ERROR",
                endpoint_guidance(
                    guidance,
                    endpoint,
                    &format!("DeepSeek web request failed: {}", failure.message()),
                ),
            )
        })?;
    if !(200..300).contains(&response.status) {
        let status = response.status;
        let mut message = format!("DeepSeek API error (HTTP {status})");
        let mut response_body = response.body;
        let mut bytes = Vec::new();
        if response_body.read_to_end(&mut bytes).is_ok()
            && let Ok(parsed) = serde_json::from_slice::<Value>(&bytes)
            && let Some(detail) = error_detail(&parsed)
        {
            message.push_str(": ");
            message.push_str(&detail);
        }
        return Err(WebError::new(
            "WEB_PROVIDER_ERROR",
            endpoint_guidance(guidance, endpoint, &message),
        ));
    }
    let mut bytes = Vec::new();
    let decode_failure = |failure: String| {
        WebError::new(
            "WEB_PROVIDER_ERROR",
            endpoint_guidance(
                guidance,
                endpoint,
                &format!("DeepSeek returned an unprocessable response body: {failure}"),
            ),
        )
    };
    let mut response_body = response.body;
    response_body
        .read_to_end(&mut bytes)
        .map_err(|read_failure| decode_failure(read_failure.to_string()))?;
    let parsed: Value = serde_json::from_slice(&bytes)
        .map_err(|parse_failure| decode_failure(parse_failure.to_string()))?;
    // The decode supplies the final message (the dsh
    // `error instanceof WebError` arm hands `error.message` to
    // searchEndpointError verbatim); only the read/parse failures
    // above take the unprocessable wrapper.
    decode(&parsed).map_err(|message| {
        WebError::new(
            "WEB_PROVIDER_ERROR",
            endpoint_guidance(guidance, endpoint, &message),
        )
    })
}

/// Bound the triggering question to what `ori_question` accepts.
///
/// dsh `boundedQuestion`: JavaScript `\s+` collapses to single
/// spaces, the result trims, and the first 128 characters remain (the
/// `Array.from` character semantics).
#[must_use]
pub fn bounded_question(question: Option<&str>) -> Option<String> {
    let question = question?;
    // The collapse trims the leading and trailing JavaScript-whitespace
    // runs (the dsh `replace` + `trim` pair; `trim` must not re-run
    // after it — Rust's trim would also strip a NEL the JS one keeps).
    let collapsed = collapse_js_spaces(question);
    if collapsed.is_empty() {
        return None;
    }
    Some(
        collapsed
            .chars()
            .take(ORIGINAL_QUESTION_MAX_CHARS)
            .collect(),
    )
}

/// Replace every run of JavaScript `\s` whitespace with one space.
///
/// The dsh `replace(/\s+/gu, ' ')`; the class is the Unicode set plus
/// `\u{feff}`, minus `is_whitespace`'s `NEL`.
fn collapse_js_spaces(text: &str) -> String {
    const fn is_js_space(character: char) -> bool {
        matches!(
            character,
            '\t' | '\n'
                | '\u{000B}'
                | '\u{000C}'
                | '\r'
                | ' '
                | '\u{00A0}'
                | '\u{1680}'
                | '\u{2000}'
                ..='\u{200A}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202F}'
                    | '\u{205F}'
                    | '\u{3000}'
                    | '\u{FEFF}'
        )
    }
    let mut out = String::with_capacity(text.len());
    let mut in_run = false;
    for character in text.chars() {
        if is_js_space(character) {
            in_run = true;
        } else {
            if in_run && !out.is_empty() {
                out.push(' ');
            }
            in_run = false;
            out.push(character);
        }
    }
    out
}

/// The type of the injected trigger-question source.
///
/// dsh `latestUserQuestion` reads the text of the latest human prompt,
/// the question that triggered the current turn; fr receives it
/// through this injected source — the agent-loop trigger message
/// carrier, absent until wired (the dead arm sends nothing).
pub type QuestionSource = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Authentication, endpoint, and result settings for one search (dsh
/// `DeepSeekSearchApiOptions`).
pub struct DeepSeekSearchApiOptions {
    /// Credential sources for the operation.
    pub auth: DeepSeekWebAuth,
    /// API base including its path prefix; `/search` is appended.
    pub base_url: String,
    /// Results requested per search, from 1 through 50; a smaller
    /// request bound wins.
    pub count: f64,
    /// Whether the triggering user question rides the request.
    pub send_original_question: bool,
    /// Read the user question that triggered the search; `None` or a
    /// blank result sends none.
    pub original_question: Option<QuestionSource>,
}

impl std::fmt::Debug for DeepSeekSearchApiOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeepSeekSearchApiOptions")
            .field("auth", &self.auth)
            .field("base_url", &self.base_url)
            .field("count", &self.count)
            .field("send_original_question", &self.send_original_question)
            .finish_non_exhaustive()
    }
}

/// Validate a Search API response and project its sources.
///
/// dsh `mapSearchResponse`: strict narrowing — every item needs a
/// nonempty `url` string, `title`/`snippet` strings, and a
/// null-or-string `date`.
///
/// # Errors
///
/// The verbatim [`invalid_response`] message on any violation.
pub fn map_search_response(response: &Value) -> Result<WebSearchResult, String> {
    let Some(results) = response.get("results").and_then(Value::as_array) else {
        return Err(invalid_response());
    };
    let mut sources = Vec::new();
    for item in results {
        let Some(item) = item.as_object() else {
            return Err(invalid_response());
        };
        let url = item.get("url").and_then(Value::as_str);
        let title = item.get("title").and_then(Value::as_str);
        let snippet = item.get("snippet").and_then(Value::as_str);
        let date = item.get("date");
        let (Some(url), Some(title), Some(snippet)) = (url, title, snippet) else {
            return Err(invalid_response());
        };
        if url.is_empty()
            || !(date.is_none()
                || date == Some(&Value::Null)
                || date.and_then(Value::as_str).is_some())
        {
            return Err(invalid_response());
        }
        let optional = |value: &str| (!value.is_empty()).then(|| value.to_owned());
        sources.push(WebSearchSource {
            url: url.to_owned(),
            title: optional(title),
            snippet: optional(snippet),
            published_at: date
                .and_then(Value::as_str)
                .filter(|date| !date.is_empty())
                .map(str::to_owned),
        });
    }
    Ok(WebSearchResult {
        content: None,
        sources,
        truncated: false,
    })
}

/// Search through the `DeepSeek` Search API.
///
/// dsh `DeepSeekSearchApiProvider`: HTTP redirects fail as
/// `WEB_PROVIDER_ERROR`; failures after dispatch name the endpoint and
/// tell the model how the user can configure it.
pub struct DeepSeekSearchApiProvider {
    options: DeepSeekSearchApiOptions,
}

impl std::fmt::Debug for DeepSeekSearchApiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeepSeekSearchApiProvider")
            .field("options", &self.options)
            .finish()
    }
}

impl DeepSeekSearchApiProvider {
    /// Build the provider over one resolved option set.
    #[must_use]
    pub const fn new(options: DeepSeekSearchApiOptions) -> Self {
        Self { options }
    }
}

impl WebSearchProvider for DeepSeekSearchApiProvider {
    fn id(&self) -> &str {
        DEEPSEEK_SEARCH_API_PROVIDER_ID
    }

    fn available(&self) -> bool {
        url::Url::parse(&self.options.base_url).is_ok() && is_positive_integer(self.options.count)
    }

    /// Run one search against the `/search` endpoint.
    ///
    /// # Errors
    ///
    /// [`WebError`] with the provider's code family; post-dispatch
    /// failures carry the endpoint guidance suffix.
    fn search(&self, request: &WebSearchRequest) -> Result<WebSearchResult, WebError> {
        let options = &self.options;
        let guidance = DeepSeekWebGuidance {
            subject: "Search",
            ..DEEPSEEK_WEB_API_GUIDANCE.clone()
        };
        let endpoint = format!("{}/search", options.base_url.trim_end_matches('/'));
        let count = request.max_results.map_or(options.count, |max| {
            #[expect(
                clippy::cast_precision_loss,
                reason = "the request bound rides the same f64 compare as the JS Math.min"
            )]
            let max = max as f64;
            options.count.min(max)
        });
        let question = if options.send_original_question {
            options
                .original_question
                .as_ref()
                .and_then(|source| source())
                .and_then(|question| bounded_question(Some(&question)))
        } else {
            None
        };
        let body = json!({
            "query": request.query,
            "count": integral(count)?,
            "need_content": false,
            "ori_question": question,
        });
        let payload = if question.is_some() {
            body
        } else {
            let mut body = body;
            if let Value::Object(fields) = &mut body {
                fields.remove("ori_question");
            }
            body
        };
        dispatch_deep_seek_web(
            &options.auth,
            &endpoint,
            &payload,
            &guidance,
            map_search_response,
        )
    }
}

/// Authentication and endpoint settings snapshotted for one fetch (dsh
/// `DeepSeekFetchApiOptions`).
#[derive(Debug, Clone)]
pub struct DeepSeekFetchApiOptions {
    /// Credential sources for the operation.
    pub auth: DeepSeekWebAuth,
    /// API base including its path prefix; `/fetch` is appended.
    pub base_url: String,
}

/// Fetch page Markdown through the `DeepSeek` Fetch API.
///
/// dsh `DeepSeekFetchApiProvider`: the API reports neither the
/// target's status nor a final URL, and it fails when the target
/// yields no usable content, so a success is the requested page
/// retrieved with HTTP 200.
#[derive(Debug, Clone)]
pub struct DeepSeekFetchApiProvider {
    options: DeepSeekFetchApiOptions,
}

impl DeepSeekFetchApiProvider {
    /// Build the provider over one resolved option set.
    #[must_use]
    pub const fn new(options: DeepSeekFetchApiOptions) -> Self {
        Self { options }
    }
}

impl WebFetchProvider for DeepSeekFetchApiProvider {
    fn id(&self) -> &str {
        DEEPSEEK_FETCH_API_PROVIDER_ID
    }

    fn available(&self) -> bool {
        url::Url::parse(&self.options.base_url).is_ok()
    }

    /// Retrieve one URL as Markdown.
    ///
    /// # Errors
    ///
    /// [`WebError`] with the provider's code family; post-dispatch
    /// failures carry the endpoint guidance suffix.
    fn fetch(&self, request: &WebFetchRequest) -> Result<WebFetchResult, WebError> {
        let options = &self.options;
        let guidance = DeepSeekWebGuidance {
            subject: "Fetch",
            ..DEEPSEEK_WEB_API_GUIDANCE.clone()
        };
        let endpoint = format!("{}/fetch", options.base_url.trim_end_matches('/'));
        let url = request.url.clone();
        dispatch_deep_seek_web(
            &options.auth,
            &endpoint,
            &json!({ "url": url }),
            &guidance,
            move |parsed| {
                let content = parsed
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid_response)?;
                Ok(WebFetchResult {
                    url: url.clone(),
                    status_code: 200,
                    body: WebFetchBody::Text {
                        content: content.to_owned(),
                    },
                    truncated: false,
                })
            },
        )
    }
}

/// Resolve one Search API option set from one config object.
///
/// The dsh Config defaults: the credential ref default, the API base
/// default, the count default, and the question gate default.
#[must_use]
pub fn resolve_search_options(
    config: &Value,
    env: &dyn Fn(&str) -> Option<String>,
) -> DeepSeekSearchApiOptions {
    let auth = resolve_auth(config, env);
    let count = config
        .get("count")
        .and_then(Value::as_f64)
        .unwrap_or(DEEPSEEK_DEFAULT_COUNT);
    DeepSeekSearchApiOptions {
        auth,
        base_url: config
            .get("base-url")
            .and_then(Value::as_str)
            .unwrap_or(DEEPSEEK_WEB_API_BASE_URL)
            .to_owned(),
        count,
        send_original_question: config
            .get("send-original-question")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        original_question: None,
    }
}

/// Resolve one Fetch API option set (the same credential and base
/// surface without the search-only fields).
#[must_use]
pub fn resolve_fetch_options(
    config: &Value,
    env: &dyn Fn(&str) -> Option<String>,
) -> DeepSeekFetchApiOptions {
    DeepSeekFetchApiOptions {
        auth: resolve_auth(config, env),
        base_url: config
            .get("base-url")
            .and_then(Value::as_str)
            .unwrap_or(DEEPSEEK_WEB_API_BASE_URL)
            .to_owned(),
    }
}

/// The shared credential resolution: the literal key, then the
/// credential-ref-named environment variable.
fn resolve_auth(config: &Value, env: &dyn Fn(&str) -> Option<String>) -> DeepSeekWebAuth {
    let api_key_env = config
        .get("api-key-env")
        .and_then(Value::as_str)
        .unwrap_or(DEEPSEEK_DEFAULT_API_KEY_ENV)
        .to_owned();
    let literal = config
        .get("api-key")
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
        .map(str::to_owned);
    let api_key = literal.map_or_else(|| env(&api_key_env).filter(|key| !key.is_empty()), Some);
    DeepSeekWebAuth {
        api_key,
        api_key_env,
    }
}

/// True for positive integral request limits (dsh
/// `isPositiveInteger`).
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
            "DeepSeek web request failed: count is not a positive integer".to_owned(),
        ));
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "validated above as a positive integral number"
    )]
    Ok(value as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth() -> DeepSeekWebAuth {
        DeepSeekWebAuth {
            api_key: Some("sk-test".to_owned()),
            api_key_env: DEEPSEEK_DEFAULT_API_KEY_ENV.to_owned(),
        }
    }

    fn search_options(base_url: &str) -> DeepSeekSearchApiOptions {
        DeepSeekSearchApiOptions {
            auth: auth(),
            base_url: base_url.to_owned(),
            count: DEEPSEEK_DEFAULT_COUNT,
            send_original_question: false,
            original_question: None,
        }
    }

    fn request(query: &str) -> WebSearchRequest {
        WebSearchRequest {
            query: query.to_owned(),
            max_results: None,
        }
    }

    fn fetch_options(base_url: &str) -> DeepSeekFetchApiOptions {
        DeepSeekFetchApiOptions {
            auth: auth(),
            base_url: base_url.to_owned(),
        }
    }

    /// Constants and the availability table: the structural credential
    /// face keeps a missing key available; bad URLs and non-integral
    /// counts refuse.
    #[test]
    fn availability_pins_the_option_contract() {
        assert_eq!(DEEPSEEK_SEARCH_API_PROVIDER_ID, "deepseek-search-api");
        assert_eq!(DEEPSEEK_FETCH_API_PROVIDER_ID, "deepseek-fetch-api");
        assert_eq!(
            DEEPSEEK_WEB_API_BASE_URL,
            "https://api.deepseek.com/v1/tools"
        );
        assert_eq!(DEEPSEEK_DEFAULT_COUNT, 10.0);
        assert!(has_deep_seek_web_credential(&DeepSeekWebAuth {
            api_key: None,
            api_key_env: String::new(),
        }));

        let no_key = DeepSeekSearchApiProvider::new(DeepSeekSearchApiOptions {
            auth: DeepSeekWebAuth {
                api_key: None,
                api_key_env: DEEPSEEK_DEFAULT_API_KEY_ENV.to_owned(),
            },
            ..search_options("https://api.example/v1/tools")
        });
        assert!(no_key.available());
        assert!(!DeepSeekSearchApiProvider::new(search_options("not a url")).available());
        assert!(
            !DeepSeekSearchApiProvider::new(DeepSeekSearchApiOptions {
                count: 1.5,
                ..search_options("https://api.example/v1/tools")
            })
            .available()
        );
        // The fetch provider carries no count contract.
        assert!(DeepSeekFetchApiProvider::new(fetch_options("not a url")).available() == false);
        assert!(
            DeepSeekFetchApiProvider::new(fetch_options("https://api.example/v1/tools"))
                .available()
        );
    }

    /// The search loopback: base trailing slash strips, the endpoint is
    /// `/search`, the key rides `authorization` alone, the body carries
    /// the count min and no `ori_question`, and strict mapping projects
    /// the sources.
    #[test]
    fn search_pins_the_roundtrip() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            let text = read_json_request(&mut socket);
            assert!(text.starts_with("POST /v1/tools/search HTTP/1.1\r\n"));
            assert!(text.contains("authorization: Bearer sk-test\r\n"));
            assert!(!text.contains("x-api-key"));
            assert!(text.contains(&format!(
                "user-agent: {}\r\n",
                crate::attribution::product_agent()
            )));
            let payload = text.split("\r\n\r\n").nth(1).unwrap();
            let body: Value = serde_json::from_str(payload).unwrap();
            assert_eq!(
                body,
                json!({
                    "query": "rust SSRF",
                    "count": 10,
                    "need_content": false,
                })
            );
            let body = r#"{"results":[
                {"url":"https://a.example/","title":"A","snippet":"first","date":"2026-01-01"},
                {"url":"https://b.example/","title":"","snippet":"keep","date":null}
            ]}"#;
            let wire = format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            std::io::Write::write_all(&mut socket, wire.as_bytes()).unwrap();
        });
        let outcome = DeepSeekSearchApiProvider::new(search_options(&format!(
            "http://127.0.0.1:{port}/v1/tools/"
        )))
        .search(&request("rust SSRF"))
        .expect("search runs");
        assert_eq!(outcome.sources.len(), 2);
        assert_eq!(outcome.sources[0].url, "https://a.example/");
        assert_eq!(outcome.sources[0].title.as_deref(), Some("A"));
        assert_eq!(outcome.sources[0].snippet.as_deref(), Some("first"));
        assert_eq!(
            outcome.sources[0].published_at.as_deref(),
            Some("2026-01-01")
        );
        // An empty title omits the field rather than lying.
        assert_eq!(outcome.sources[1].title, None);
        assert_eq!(outcome.sources[1].published_at, None);
    }

    /// The triggering question rides when the gate is on and the source
    /// answers; the request bound shrinks the count.
    #[test]
    fn original_question_pins_the_bounded_injection() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            let text = read_json_request(&mut socket);
            let payload = text.split("\r\n\r\n").nth(1).unwrap();
            let body: Value = serde_json::from_str(payload).unwrap();
            assert_eq!(
                body,
                json!({
                    "query": "q",
                    "count": 3,
                    "need_content": false,
                    "ori_question": "what is SSRF?",
                })
            );
            let body = r#"{"results":[]}"#;
            let wire = format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            std::io::Write::write_all(&mut socket, wire.as_bytes()).unwrap();
        });
        let provider = DeepSeekSearchApiProvider::new(DeepSeekSearchApiOptions {
            count: 50.0,
            send_original_question: true,
            original_question: Some(Arc::new(|| Some("  what\tis\u{00A0}SSRF?  ".to_owned()))),
            ..search_options(&format!("http://127.0.0.1:{port}"))
        });
        let outcome = provider
            .search(&WebSearchRequest {
                query: "q".to_owned(),
                max_results: Some(3),
            })
            .expect("search runs");
        assert!(outcome.sources.is_empty());

        // The gate off (the dsh config default is on) sends nothing —
        // pinned by the first loopback, the gate on with no source
        // sends none too.
        let gateless = DeepSeekSearchApiProvider::new(DeepSeekSearchApiOptions {
            send_original_question: true,
            original_question: None,
            ..search_options("https://api.example/v1/tools")
        });
        assert_eq!(gateless.options.send_original_question, true);
    }

    /// Read one full HTTP request off the socket: complete when the
    /// JSON body parses (the provider requests all carry JSON bodies,
    /// and loopback segments may split the header and body writes).
    fn read_json_request(socket: &mut std::net::TcpStream) -> String {
        let mut buffer = Vec::new();
        let mut window = [0u8; 4096];
        loop {
            let read = std::io::Read::read(socket, &mut window).unwrap_or(0);
            buffer.extend_from_slice(&window[..read]);
            let text = String::from_utf8_lossy(&buffer).to_string();
            let payload = text.split("\r\n\r\n").nth(1).unwrap_or("");
            if serde_json::from_str::<Value>(payload).is_ok() || read == 0 {
                return text;
            }
        }
    }

    /// The question bounding: JavaScript `\s` runs collapse (including
    /// the byte-order mark, excluding NEL), edges trim, blanks send
    /// none, and the cap counts characters.
    #[test]
    fn bounded_question_pins_the_js_semantics() {
        assert_eq!(bounded_question(None), None);
        assert_eq!(bounded_question(Some("   ")), None);
        assert_eq!(
            bounded_question(Some("  what\tis\u{00A0}SSRF?  ")),
            Some("what is SSRF?".to_owned())
        );
        // \u{FEFF} is JavaScript whitespace; \u{0085} (NEL) is not.
        assert_eq!(
            bounded_question(Some("a\u{FEFF}b\u{0085}c")),
            Some("a b\u{0085}c".to_owned())
        );
        assert_eq!(
            bounded_question(Some("x".repeat(130).as_str())),
            Some("x".repeat(128))
        );
        assert_eq!(
            bounded_question(Some("短".repeat(140).as_str())).map(|q| q.chars().count()),
            Some(128)
        );
    }

    /// A missing key refuses before any network traffic with the
    /// verbatim credential message naming the plugin config.
    #[test]
    fn missing_key_pins_the_credential_error() {
        let provider = DeepSeekSearchApiProvider::new(DeepSeekSearchApiOptions {
            auth: DeepSeekWebAuth {
                api_key: None,
                api_key_env: "MY_API_KEY".to_owned(),
            },
            base_url: "http://127.0.0.1:9/v1/tools".to_owned(),
            ..search_options("http://127.0.0.1:9/v1/tools")
        });
        let error = provider.search(&request("q")).unwrap_err();
        assert_eq!(error.code, "WEB_PROVIDER_CREDENTIAL_MISSING");
        assert_eq!(
            error.message,
            concat!(
                "DeepSeek web access has no API key for \"MY_API_KEY\"; store it through the credentials service",
                " (the web Models page writes it), export it in the launching environment, or set a literal",
                " \"apiKey\" in the web-deepseek-api config; a conversation using a DeepSeek Account model",
                " searches with the account sign-in instead",
            )
        );
    }

    /// The fetch loopback: the `/fetch` endpoint, the `{url}` body, the
    /// 200/text decode, and the Fetch-subject guidance on an invalid
    /// decode.
    #[test]
    fn fetch_pins_the_roundtrip_and_decode() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for request in server.incoming() {
                let mut socket = request.unwrap();
                let text = read_json_request(&mut socket);
                let body = if text.contains("target.example/page") {
                    r#"{"content":"rendered page"}"#
                } else {
                    r#"{"content":42}"#
                };
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                std::io::Write::write_all(&mut socket, wire.as_bytes()).unwrap();
            }
        });
        let base = format!("http://127.0.0.1:{port}/v1/tools");
        let good = DeepSeekFetchApiProvider::new(fetch_options(&base))
            .fetch(&WebFetchRequest {
                url: "https://target.example/page".to_owned(),
            })
            .expect("fetch runs");
        assert_eq!(good.status_code, 200);
        assert_eq!(
            good.body,
            WebFetchBody::Text {
                content: "rendered page".to_owned()
            }
        );
        assert_eq!(good.url, "https://target.example/page");
        assert!(!good.truncated);

        let bad = DeepSeekFetchApiProvider::new(fetch_options(&base))
            .fetch(&WebFetchRequest {
                url: "https://target.example/other".to_owned(),
            })
            .unwrap_err();
        assert_eq!(bad.code, "WEB_PROVIDER_ERROR");
        assert_eq!(
            bad.message,
            format!(
                "DeepSeek returned an invalid response body\n\nThe web request used endpoint \"{base}/fetch\". \
                 Fetch endpoint configuration is separate from chat. If that endpoint is not intended, \
                 guide the user to Plugins > Built-in plugin settings > Web search, where they can \
                 change and save Search / Fetch API base URL. If that settings page is unavailable, the user can set \
                 web-deepseek-api.baseURL to a trusted DeepSeek endpoint. Only the user should choose or change the endpoint."
            )
        );
    }

    /// The strict response table: a missing results array, a
    /// mistyped field, and a non-string date each refuse; empty
    /// results pass.
    #[test]
    fn map_search_response_pins_the_strict_table() {
        assert_eq!(
            map_search_response(&json!({})).unwrap_err(),
            "DeepSeek returned an invalid response body"
        );
        assert_eq!(
            map_search_response(&json!({"results": [{"url": "https://a/"}]})).unwrap_err(),
            "DeepSeek returned an invalid response body"
        );
        assert_eq!(
            map_search_response(&json!({"results": [
                {"url": "https://a/", "title": "t", "snippet": "s", "date": 5}
            ]}))
            .unwrap_err(),
            "DeepSeek returned an invalid response body"
        );
        let empty = map_search_response(&json!({"results": []})).expect("empty results pass");
        assert!(empty.sources.is_empty());
    }

    /// The option resolution: config values, the credential-ref-named
    /// environment variable, and the defaults.
    #[test]
    fn resolve_options_pin_the_defaults_and_env() {
        let empty_env = |_name: &str| None;
        let search = resolve_search_options(&json!({}), &empty_env);
        assert_eq!(search.base_url, DEEPSEEK_WEB_API_BASE_URL);
        assert_eq!(search.count, 10.0);
        assert!(search.send_original_question);
        assert_eq!(search.auth.api_key, None);
        assert_eq!(search.auth.api_key_env, "DEEPSEEK_API_KEY");
        let fetch = resolve_fetch_options(&json!({}), &empty_env);
        assert_eq!(fetch.base_url, DEEPSEEK_WEB_API_BASE_URL);

        let env = |name: &str| (name == "CFG_KEY").then(|| "sk-env".to_owned());
        let resolved = resolve_search_options(
            &json!({
                "api-key-env": "CFG_KEY",
                "base-url": "https://cfg.example/v1/tools",
                "count": 25,
                "send-original-question": false,
            }),
            &env,
        );
        assert_eq!(resolved.auth.api_key.as_deref(), Some("sk-env"));
        assert_eq!(resolved.auth.api_key_env, "CFG_KEY");
        assert_eq!(resolved.base_url, "https://cfg.example/v1/tools");
        assert_eq!(resolved.count, 25.0);
        assert!(!resolved.send_original_question);
    }
}
