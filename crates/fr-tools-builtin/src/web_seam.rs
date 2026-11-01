// Rust guideline compliant 2026-11-01
//! The web capability seam (dsh `@deepseek-ai/dsh-web`, 1:1): the
//! search/fetch vocabulary, the two provider traits, and the runtime
//! with its registries and execution-time provider selection.
//!
//! Search and fetch deliberately share one seam so provider selection,
//! errors, and product configuration have one owner, while retaining
//! separate request and result types. A configured provider must exist
//! and be usable; without one, exactly one usable provider is required,
//! so selection never depends on registration order. The seam is the
//! in-crate domain service (Q93-A: the tool bodies call the runtime
//! in-process; cross-crate consumers stay absent in M1), and the
//! providers run on the caller's worker thread (Q69-B: the network face
//! is blocking with the deadline owned above the seam).

#![forbid(unsafe_code)]

use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, json};

/// The harness-error class name the tool layer reports for a web failure
/// (dsh `WebError extends HarnessError`; the class name is the body
/// error's `name`).
pub const ERROR_NAME: &str = "WebError";

/// One typed web failure (dsh `WebError`): the machine-routable
/// open-string `code` plus the verbatim message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebError {
    /// The routable code (`WEB_*`).
    pub code: String,
    /// The failure message (verbatim dsh text).
    pub message: String,
}

impl WebError {
    /// One failure with the given code and message.
    #[must_use]
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for WebError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for WebError {}

impl From<WebError> for crate::tools::BodyError {
    fn from(error: WebError) -> Self {
        Self {
            code: Some(error.code),
            name: ERROR_NAME.to_owned(),
            message: error.message,
        }
    }
}

/// What one search-capable backend is asked to search (dsh
/// `WebSearchRequest`).
#[derive(Debug, Clone)]
pub struct WebSearchRequest {
    /// The search query.
    pub query: String,
    /// Upper bound on returned sources; the seam truncates to it.
    /// `None` = no bound.
    pub max_results: Option<usize>,
}

/// One citeable source (dsh `WebSearchSource`). A source always has a
/// URL; the rest are optional because not every provider returns them —
/// forcing adapters to invent them would make the seam lie.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WebSearchSource {
    /// The source URL.
    pub url: String,
    /// The provider title, when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The provider snippet, when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Publication/crawl timestamp as a provider-supplied ISO-8601
    /// string, when supplied.
    #[serde(rename = "publishedAt", skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
}

/// Normalized search outcome (dsh `WebSearchResult`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WebSearchResult {
    /// Optional provider-generated answer text or summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Citeable sources, truncated to the request's `max_results`.
    pub sources: Vec<WebSearchSource>,
    /// True when the seam dropped sources to honor `max_results`.
    pub truncated: bool,
}

/// What one fetch-capable backend is asked to retrieve (dsh
/// `WebFetchRequest`).
///
/// The request deliberately omits timeout, format, prompt, and
/// extraction controls: cancellation is owned above the seam, while
/// presentation and LLM concerns belong outside safe retrieval.
#[derive(Debug, Clone)]
pub struct WebFetchRequest {
    /// The URL to retrieve.
    pub url: String,
}

/// The decoded body of a fetched resource (dsh `WebFetchBody`): a
/// CLOSED union owned by this crate.
///
/// The provider decodes the kind and the tool renders it, so a new
/// kind is a coordinated change, not a plugin extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind")]
pub enum WebFetchBody {
    /// Decoded HTML markup.
    #[serde(rename = "html")]
    Html {
        /// The markup text.
        content: String,
    },
    /// Decoded plain text.
    #[serde(rename = "text")]
    Text {
        /// The text.
        content: String,
    },
}

/// Normalized fetch outcome (dsh `WebFetchResult`).
///
/// A successful network fetch of a non-2xx response is a result, not
/// an error — the status code is part of the fetched resource state.
/// [`WebError`] is reserved for failures to safely retrieve or
/// represent the resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WebFetchResult {
    /// The final URL after allowed redirects.
    pub url: String,
    /// HTTP status code of the fetched response.
    #[serde(rename = "statusCode")]
    pub status_code: u16,
    /// Decoded body, classified by content kind.
    pub body: WebFetchBody,
    /// True when the provider capped the decoded body.
    pub truncated: bool,
}

/// A search-capable backend (dsh `WebSearchProvider`). `id` is a stable
/// string, unique within the search capability kind.
pub trait WebSearchProvider: Send + Sync {
    /// The stable provider id.
    fn id(&self) -> &str;
    /// Cheap local usability check; must not make network calls.
    fn available(&self) -> bool;
    /// Run one search.
    ///
    /// # Errors
    ///
    /// [`WebError`] with the provider's own code family.
    fn search(&self, request: &WebSearchRequest) -> Result<WebSearchResult, WebError>;
}

/// A fetch-capable backend (dsh `WebFetchProvider`). `id` is a stable
/// string, unique within the fetch capability kind.
pub trait WebFetchProvider: Send + Sync {
    /// The stable provider id.
    fn id(&self) -> &str;
    /// Cheap local usability check; must not make network calls.
    fn available(&self) -> bool;
    /// Retrieve one URL.
    ///
    /// # Errors
    ///
    /// [`WebError`] with the provider's own code family.
    fn fetch(&self, request: &WebFetchRequest) -> Result<WebFetchResult, WebError>;
}

/// The web access runtime (dsh `WebRuntime`, one instance per plugin
/// activation): two registries and the execution-time selection.
///
/// Selection semantics (resolved at execution time, never
/// order-dependent):
/// - A configured id that is registered and `available` → that provider.
/// - A configured id not registered → `WEB_PROVIDER_CONFIGURED_MISSING`.
/// - A configured id registered but unavailable →
///   `WEB_PROVIDER_CONFIGURED_UNAVAILABLE`.
/// - No id configured, exactly one registered usable provider → that
///   provider.
/// - No id configured, multiple usable providers →
///   `WEB_PROVIDER_AMBIGUOUS`.
/// - No id configured, no usable provider → `WEB_PROVIDER_UNAVAILABLE`.
pub struct WebRuntime {
    search_providers: Vec<Arc<dyn WebSearchProvider>>,
    fetch_providers: Vec<Arc<dyn WebFetchProvider>>,
    /// The environment fallbacks (dsh `$DSH_WEB_SEARCH_PROVIDER` /
    /// `$DSH_WEB_FETCH_PROVIDER` under the FR_ prefix): operational
    /// overrides feeding the SAME fields as config, not a hidden
    /// priority chain.
    search_provider_env: Option<String>,
    fetch_provider_env: Option<String>,
}

impl std::fmt::Debug for WebRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebRuntime")
            .field("search_providers", &self.search_provider_ids())
            .field("fetch_providers", &self.fetch_provider_ids())
            .finish_non_exhaustive()
    }
}

impl WebRuntime {
    /// Build the runtime; the environment fallbacks are read once here
    /// (dsh reads `process.env` in the constructor).
    #[must_use]
    pub fn new() -> Self {
        Self {
            search_providers: Vec::new(),
            fetch_providers: Vec::new(),
            search_provider_env: std::env::var("FR_WEB_SEARCH_PROVIDER").ok(),
            fetch_provider_env: std::env::var("FR_WEB_FETCH_PROVIDER").ok(),
        }
    }

    /// Register a search provider; a duplicate id refuses with
    /// `WEB_DUPLICATE_PROVIDER`.
    ///
    /// # Errors
    ///
    /// [`WebError`] `WEB_DUPLICATE_PROVIDER` when the id is already
    /// registered for search.
    pub fn register_search_provider(
        &mut self,
        provider: Arc<dyn WebSearchProvider>,
    ) -> Result<(), WebError> {
        Self::register(self.search_provider_ids().as_slice(), provider.id())?;
        self.search_providers.push(provider);
        Ok(())
    }

    /// Register a fetch provider; a duplicate id refuses with
    /// `WEB_DUPLICATE_PROVIDER`.
    ///
    /// # Errors
    ///
    /// [`WebError`] `WEB_DUPLICATE_PROVIDER` when the id is already
    /// registered for fetch.
    pub fn register_fetch_provider(
        &mut self,
        provider: Arc<dyn WebFetchProvider>,
    ) -> Result<(), WebError> {
        Self::register(self.fetch_provider_ids().as_slice(), provider.id())?;
        self.fetch_providers.push(provider);
        Ok(())
    }

    fn register(ids: &[String], id: &str) -> Result<(), WebError> {
        if ids.iter().any(|known| known == id) {
            return Err(WebError::new(
                "WEB_DUPLICATE_PROVIDER",
                format!("a web provider with id \"{id}\" is already registered"),
            ));
        }
        Ok(())
    }

    fn search_provider_ids(&self) -> Vec<String> {
        self.search_providers
            .iter()
            .map(|provider| provider.id().to_owned())
            .collect()
    }

    fn fetch_provider_ids(&self) -> Vec<String> {
        self.fetch_providers
            .iter()
            .map(|provider| provider.id().to_owned())
            .collect()
    }

    /// Run one search through the selected provider.
    ///
    /// Resolution happens at call time (`config_value` wins over the
    /// environment fallback — dsh `config.get() ?? env`); the seam
    /// enforces `request.max_results` on the result: an over-returning
    /// provider has its `sources` truncated and `truncated` set.
    ///
    /// # Errors
    ///
    /// [`WebError`] when the capability cannot run, or the provider's
    /// own failure.
    pub fn search(
        &self,
        request: &WebSearchRequest,
        config_value: Option<&str>,
    ) -> Result<WebSearchResult, WebError> {
        let configured_id = config_value.or(self.search_provider_env.as_deref());
        let provider = resolve_search(self.search_providers.as_slice(), configured_id)?;
        let result = provider.search(request)?;
        Ok(cap_sources(result, request.max_results))
    }

    /// Retrieve one URL through the selected provider.
    ///
    /// Resolution rules as for [`WebRuntime::search`]; a non-2xx
    /// response is a result, not a failure.
    ///
    /// # Errors
    ///
    /// [`WebError`] when the capability cannot run, or the provider's
    /// own failure.
    pub fn fetch(
        &self,
        request: &WebFetchRequest,
        config_value: Option<&str>,
    ) -> Result<WebFetchResult, WebError> {
        let configured_id = config_value.or(self.fetch_provider_env.as_deref());
        let provider = resolve_fetch(self.fetch_providers.as_slice(), configured_id)?;
        provider.fetch(request)
    }
}

impl Default for WebRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve the selected search provider or fail with the matching
/// [`WebError`] (the dsh `resolveProvider` table over one capability
/// kind, in registration order).
fn resolve_search(
    providers: &[Arc<dyn WebSearchProvider>],
    configured_id: Option<&str>,
) -> Result<Arc<dyn WebSearchProvider>, WebError> {
    if let Some(configured_id) = configured_id {
        let Some(provider) = providers
            .iter()
            .find(|provider| provider.id() == configured_id)
        else {
            return Err(WebError::new(
                "WEB_PROVIDER_CONFIGURED_MISSING",
                format!("configured web provider \"{configured_id}\" is not registered"),
            ));
        };
        if !provider.available() {
            return Err(WebError::new(
                "WEB_PROVIDER_CONFIGURED_UNAVAILABLE",
                format!(
                    "configured web provider \"{configured_id}\" is registered but unavailable"
                ),
            ));
        }
        return Ok(Arc::clone(provider));
    }
    let usable: Vec<&Arc<dyn WebSearchProvider>> = providers
        .iter()
        .filter(|provider| provider.available())
        .collect();
    let Some(single) = usable.first() else {
        return Err(WebError::new(
            "WEB_PROVIDER_UNAVAILABLE",
            "no usable web provider is registered",
        ));
    };
    if usable.len() > 1 {
        let ids = usable
            .iter()
            .map(|provider| provider.id())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(WebError::new(
            "WEB_PROVIDER_AMBIGUOUS",
            format!(
                "multiple usable web providers are registered ({ids}); configure one explicitly"
            ),
        ));
    }
    Ok(Arc::clone(single))
}

/// Resolve the selected fetch provider (the same table over the fetch
/// kind).
fn resolve_fetch(
    providers: &[Arc<dyn WebFetchProvider>],
    configured_id: Option<&str>,
) -> Result<Arc<dyn WebFetchProvider>, WebError> {
    if let Some(configured_id) = configured_id {
        let Some(provider) = providers
            .iter()
            .find(|provider| provider.id() == configured_id)
        else {
            return Err(WebError::new(
                "WEB_PROVIDER_CONFIGURED_MISSING",
                format!("configured web provider \"{configured_id}\" is not registered"),
            ));
        };
        if !provider.available() {
            return Err(WebError::new(
                "WEB_PROVIDER_CONFIGURED_UNAVAILABLE",
                format!(
                    "configured web provider \"{configured_id}\" is registered but unavailable"
                ),
            ));
        }
        return Ok(Arc::clone(provider));
    }
    let usable: Vec<&Arc<dyn WebFetchProvider>> = providers
        .iter()
        .filter(|provider| provider.available())
        .collect();
    let Some(single) = usable.first() else {
        return Err(WebError::new(
            "WEB_PROVIDER_UNAVAILABLE",
            "no usable web provider is registered",
        ));
    };
    if usable.len() > 1 {
        let ids = usable
            .iter()
            .map(|provider| provider.id())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(WebError::new(
            "WEB_PROVIDER_AMBIGUOUS",
            format!(
                "multiple usable web providers are registered ({ids}); configure one explicitly"
            ),
        ));
    }
    Ok(Arc::clone(single))
}

/// Enforce `max_results` on a search result: truncate `sources` and flag
/// it (dsh `capSources`).
#[must_use]
pub fn cap_sources(mut result: WebSearchResult, max_results: Option<usize>) -> WebSearchResult {
    match max_results {
        Some(max_results) if result.sources.len() > max_results => {
            result.sources.truncate(max_results);
            result.truncated = true;
            result
        }
        _ => result,
    }
}

/// The wire view of one search result (the dsh result value shape the
/// tool's execute returns; optional fields stay absent, not null).
#[must_use]
pub fn search_result_value(result: &WebSearchResult) -> Value {
    let sources: Vec<Value> = result
        .sources
        .iter()
        .map(|source| {
            let mut object = serde_json::Map::new();
            object.insert("url".to_owned(), json!(source.url));
            if let Some(title) = source.title.as_ref() {
                object.insert("title".to_owned(), json!(title));
            }
            if let Some(snippet) = source.snippet.as_ref() {
                object.insert("snippet".to_owned(), json!(snippet));
            }
            if let Some(published_at) = source.published_at.as_ref() {
                object.insert("publishedAt".to_owned(), json!(published_at));
            }
            Value::Object(object)
        })
        .collect();
    let mut object = serde_json::Map::new();
    if let Some(content) = result.content.as_ref() {
        object.insert("content".to_owned(), json!(content));
    }
    object.insert("sources".to_owned(), Value::Array(sources));
    object.insert("truncated".to_owned(), json!(result.truncated));
    Value::Object(object)
}

/// The wire view of one fetch result (the dsh result value shape).
#[must_use]
pub fn fetch_result_value(result: &WebFetchResult) -> Value {
    let (kind, content) = match &result.body {
        WebFetchBody::Html { content } => ("html", content),
        WebFetchBody::Text { content } => ("text", content),
    };
    json!({
        "url": result.url,
        "statusCode": result.status_code,
        "body": { "kind": kind, "content": content },
        "truncated": result.truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedSearch {
        id: String,
        usable: bool,
        result: Result<WebSearchResult, String>,
    }

    impl WebSearchProvider for FixedSearch {
        fn id(&self) -> &str {
            &self.id
        }

        fn available(&self) -> bool {
            self.usable
        }

        fn search(&self, _request: &WebSearchRequest) -> Result<WebSearchResult, WebError> {
            self.result
                .clone()
                .map_err(|message| WebError::new("WEB_PROVIDER_ERROR", message))
        }
    }

    struct FixedFetch {
        id: String,
        usable: bool,
    }

    impl WebFetchProvider for FixedFetch {
        fn id(&self) -> &str {
            &self.id
        }

        fn available(&self) -> bool {
            self.usable
        }

        fn fetch(&self, request: &WebFetchRequest) -> Result<WebFetchResult, WebError> {
            Ok(WebFetchResult {
                url: request.url.clone(),
                status_code: 200,
                body: WebFetchBody::Text {
                    content: "body".to_owned(),
                },
                truncated: false,
            })
        }
    }

    fn search_provider(id: &str, usable: bool) -> Arc<dyn WebSearchProvider> {
        Arc::new(FixedSearch {
            id: id.to_owned(),
            usable,
            result: Ok(WebSearchResult {
                content: None,
                sources: Vec::new(),
                truncated: false,
            }),
        })
    }

    fn fetch_provider(id: &str, usable: bool) -> Arc<dyn WebFetchProvider> {
        Arc::new(FixedFetch {
            id: id.to_owned(),
            usable,
        })
    }

    /// Registration refuses a duplicate id per capability kind, and the
    /// same id may register across the two kinds.
    #[test]
    fn registration_refuses_duplicates_per_kind() {
        let mut runtime = WebRuntime::new();
        runtime
            .register_search_provider(search_provider("one", true))
            .expect("first registration");
        let duplicate = runtime
            .register_search_provider(search_provider("one", true))
            .unwrap_err();
        assert_eq!(duplicate.code, "WEB_DUPLICATE_PROVIDER");
        assert_eq!(
            duplicate.message,
            "a web provider with id \"one\" is already registered"
        );
        runtime
            .register_fetch_provider(fetch_provider("one", true))
            .expect("the same id registers under the other kind");
    }

    /// The five selection arms over the configured-id path.
    #[test]
    fn selection_pins_the_configured_arms() {
        let mut runtime = WebRuntime::new();
        runtime
            .register_search_provider(search_provider("one", false))
            .expect("registration");
        let missing = runtime
            .search(&sample_request(), Some("absent"))
            .unwrap_err();
        assert_eq!(missing.code, "WEB_PROVIDER_CONFIGURED_MISSING");
        assert_eq!(
            missing.message,
            "configured web provider \"absent\" is not registered"
        );
        let unavailable = runtime.search(&sample_request(), Some("one")).unwrap_err();
        assert_eq!(unavailable.code, "WEB_PROVIDER_CONFIGURED_UNAVAILABLE");
        assert_eq!(
            unavailable.message,
            "configured web provider \"one\" is registered but unavailable"
        );
        runtime
            .register_search_provider(search_provider("usable", true))
            .expect("second registration");
        let selected = runtime
            .search(&sample_request(), Some("usable"))
            .expect("runs");
        assert_eq!(selected.sources, Vec::new());
    }

    /// The three no-config arms: single usable auto-selects, several
    /// refuse ambiguous with the id list, none refuse unavailable.
    #[test]
    fn selection_pins_the_auto_arms() {
        let mut runtime = WebRuntime::new();
        runtime
            .register_search_provider(search_provider("down", false))
            .expect("registration");
        let none = runtime.search(&sample_request(), None).unwrap_err();
        assert_eq!(none.code, "WEB_PROVIDER_UNAVAILABLE");
        assert_eq!(none.message, "no usable web provider is registered");

        runtime
            .register_search_provider(search_provider("first", true))
            .expect("second registration");
        runtime
            .register_search_provider(search_provider("second", true))
            .expect("third registration");
        let ambiguous = runtime.search(&sample_request(), None).unwrap_err();
        assert_eq!(ambiguous.code, "WEB_PROVIDER_AMBIGUOUS");
        // Registration order, not sorted keys.
        assert_eq!(
            ambiguous.message,
            "multiple usable web providers are registered (first, second); configure one explicitly"
        );

        let mut single = WebRuntime::new();
        single
            .register_search_provider(search_provider("only", true))
            .expect("registration");
        single
            .search(&sample_request(), None)
            .expect("the single usable provider auto-selects");
    }

    /// The seam enforces `max_results` regardless of the provider's own
    /// return size, and the flag rides only when sources dropped.
    #[test]
    fn seam_caps_over_returning_sources() {
        let over = WebSearchResult {
            content: None,
            sources: vec![
                WebSearchSource {
                    url: "https://a.example/1".to_owned(),
                    title: None,
                    snippet: None,
                    published_at: None,
                },
                WebSearchSource {
                    url: "https://a.example/2".to_owned(),
                    title: None,
                    snippet: None,
                    published_at: None,
                },
                WebSearchSource {
                    url: "https://a.example/3".to_owned(),
                    title: None,
                    snippet: None,
                    published_at: None,
                },
            ],
            truncated: false,
        };
        let capped = cap_sources(over.clone(), Some(2));
        assert_eq!(capped.sources.len(), 2);
        assert_eq!(capped.sources[0].url, "https://a.example/1");
        assert!(capped.truncated);
        let exact = cap_sources(over, Some(3));
        assert!(!exact.truncated);
    }

    /// The environment fallback rides only when the config value is
    /// absent (one field, not a priority chain), and a wire view keeps
    /// optional fields absent while the required ones render.
    #[test]
    fn env_fallback_and_wire_views_pin_the_shapes() {
        let mut runtime = WebRuntime::new();
        runtime
            .register_fetch_provider(fetch_provider("env-picked", true))
            .expect("registration");
        runtime.fetch_provider_env = Some("env-picked".to_owned());
        let outcome = runtime
            .fetch(
                &WebFetchRequest {
                    url: "https://example.invalid/page".to_owned(),
                },
                None,
            )
            .expect("the env fallback selects");
        assert_eq!(outcome.url, "https://example.invalid/page");
        let via_config = runtime
            .fetch(
                &WebFetchRequest {
                    url: "https://example.invalid/other".to_owned(),
                },
                Some("absent"),
            )
            .unwrap_err();
        assert_eq!(via_config.code, "WEB_PROVIDER_CONFIGURED_MISSING");

        let wire = fetch_result_value(&outcome);
        assert_eq!(wire["statusCode"], json!(200));
        assert_eq!(wire["body"]["kind"], json!("text"));
        assert_eq!(wire["truncated"], json!(false));

        let sources = search_result_value(&WebSearchResult {
            content: Some("answer".to_owned()),
            sources: vec![WebSearchSource {
                url: "https://a.example/".to_owned(),
                title: Some("A".to_owned()),
                snippet: None,
                published_at: Some("2026-01-01".to_owned()),
            }],
            truncated: false,
        });
        assert_eq!(sources["content"], json!("answer"));
        assert_eq!(sources["sources"][0]["url"], json!("https://a.example/"));
        assert_eq!(sources["sources"][0]["publishedAt"], json!("2026-01-01"));
        assert!(sources["sources"][0].get("snippet").is_none());
    }

    fn sample_request() -> WebSearchRequest {
        WebSearchRequest {
            query: "rust SSRF".to_owned(),
            max_results: None,
        }
    }
}
