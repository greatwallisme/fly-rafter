// Rust guideline compliant 2026-11-01
//! The anonymous public HTTP(S) fetch provider (dsh
//! `@deepseek-ai/dsh-web-fetch-http`, 1:1).
//!
//! Requests carry no browser cookies or ambient credentials. The
//! transport granularity is the workspace carrier's (Q69-B): the
//! deadline rides each hop's per-read window, so a stalled read
//! surfaces at the deadline instead of the default window; the dsh
//! entry abort check has no caller signal in fr and stays absent by
//! construction. The proxied-hop arm (dsh `proxyRouteFor`) stays
//! absent with the fr proxy seam itself (M1 routes every hop through
//! resolution and pinning directly).

#![forbid(unsafe_code)]

use std::io::Read as _;
use std::time::{Duration, Instant};

use fr_net::http::{HttpMethod, HttpRequest, UreqFetch};
use fr_net::pinned::{
    PinError, is_non_public_ip_literal, resolve_public_addresses, strip_ipv6_brackets,
};
use serde_json::Value;
use url::Url;

use crate::web_seam::{WebError, WebFetchBody, WebFetchProvider, WebFetchRequest, WebFetchResult};

/// Maximum accepted request URL length enforced by the public fetch
/// provider (dsh `WEB_FETCH_MAX_URL_LENGTH`).
pub const WEB_FETCH_MAX_URL_LENGTH: usize = 2048;

/// The default `User-Agent`: an explicit product agent, never a browser
/// disguise (the dsh default under the workspace brand mapping; the
/// version rides [`crate::attribution::APP_IDENTITY`]).
#[must_use]
pub fn default_user_agent() -> String {
    crate::attribution::user_agent()
}

/// Node coerces larger timer delays to 1 ms, so reject them at
/// configuration time (dsh `MAX_NODE_TIMER_DELAY_MS`).
const MAX_TIMER_DELAY_MS: f64 = 2_147_483_647.0;

/// The redirect statuses that carry a `Location` (dsh
/// `isRedirectStatus`: the five codes only — the remaining 3xx read as
/// bodies and fail at content classification).
const fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Resolved provider limits (dsh `HttpFetchLimits`): the fr config
/// layer resolves the defaults, this struct carries them.
#[derive(Debug, Clone)]
pub struct HttpFetchLimits {
    /// Maximum response body size in bytes.
    pub max_response_bytes: u64,
    /// Maximum decoded body length in characters.
    pub max_body_chars: usize,
    /// Default fetch timeout in milliseconds.
    pub timeout_ms: f64,
    /// Maximum number of (same-origin) redirect hops to follow.
    pub max_redirects: usize,
    /// `User-Agent` header sent on every request.
    pub user_agent: String,
}

/// Resolve the provider limits from one config object (the dsh
/// schemastery defaults plus the verbatim configuration refusals).
///
/// # Errors
///
/// The dsh configuration error texts, verbatim.
pub fn resolve_limits(config: &Value) -> Result<HttpFetchLimits, String> {
    let max_response_bytes = config_number(config, "max-response-bytes", 5_000_000.0)?;
    let max_body_chars = config_number(config, "max-body-chars", 100_000.0)?;
    let timeout_ms = config_number(config, "timeout-ms", 30_000.0)?;
    let max_redirects = config_number(config, "max-redirects", 5.0)?;
    let user_agent = config
        .get("user-agent")
        .and_then(Value::as_str)
        .unwrap_or(&default_user_agent())
        .to_owned();
    assert_positive_finite("max-response-bytes", max_response_bytes)?;
    assert_positive_finite("max-body-chars", max_body_chars)?;
    assert_positive_finite("timeout-ms", timeout_ms)?;
    if timeout_ms > MAX_TIMER_DELAY_MS {
        return Err(format!(
            "web-fetch-http: timeoutMs must be no greater than {MAX_TIMER_DELAY_MS}"
        ));
    }
    if max_redirects.fract() != 0.0 || max_redirects < 0.0 {
        return Err("web-fetch-http: maxRedirects must be a non-negative integer".to_owned());
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "validated above as a non-negative integral number; the caps stay far below the lossy ranges"
    )]
    Ok(HttpFetchLimits {
        max_response_bytes: max_response_bytes as u64,
        max_body_chars: max_body_chars as usize,
        timeout_ms,
        max_redirects: max_redirects as usize,
        user_agent,
    })
}

fn config_number(config: &Value, key: &str, default: f64) -> Result<f64, String> {
    match config.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value.as_f64().map_or_else(
            || Err(format!("web-fetch-http: {key} must be a number")),
            Ok,
        ),
    }
}

fn assert_positive_finite(name: &str, value: f64) -> Result<(), String> {
    if !value.is_finite() || value <= 0.0 {
        return Err(format!(
            "web-fetch-http: {name} must be a positive finite number"
        ));
    }
    Ok(())
}

/// Parse a request URL and enforce the network-independent transport
/// restrictions: HTTP(S) only and no embedded credentials (dsh
/// `parseFetchUrl`).
///
/// # Errors
///
/// [`WebError`] `WEB_INVALID_URL` / `WEB_BLOCKED_URL` verbatim.
pub fn parse_fetch_url(input: &str) -> Result<Url, WebError> {
    let url = Url::parse(input).map_err(|_parse_failure| {
        WebError::new("WEB_INVALID_URL", format!("invalid URL: {input}"))
    })?;
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(WebError::new(
            "WEB_INVALID_URL",
            format!("unsupported URL scheme \"{scheme}:\" (only http and https are allowed)"),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(WebError::new(
            "WEB_BLOCKED_URL",
            "credentials in URLs are not allowed",
        ));
    }
    Ok(url)
}

/// Validate a request URL against the complete pre-network policy:
/// bounded length plus [`parse_fetch_url`] (dsh `validateFetchUrl`).
/// The URL length counts UTF-16 code units, the JavaScript `.length`
/// semantics.
///
/// # Errors
///
/// [`WebError`] `WEB_INVALID_URL` / `WEB_BLOCKED_URL` verbatim.
pub fn validate_fetch_url(input: &str) -> Result<Url, WebError> {
    if input.encode_utf16().count() > WEB_FETCH_MAX_URL_LENGTH {
        return Err(WebError::new(
            "WEB_INVALID_URL",
            format!("URL exceeds the maximum length of {WEB_FETCH_MAX_URL_LENGTH}"),
        ));
    }
    parse_fetch_url(input)
}

/// Two URLs are same-origin when scheme, hostname, and port match (dsh
/// `isSameOrigin`; the port compares post-normalization — a URL's
/// default port normalizes away in both implementations).
#[must_use]
pub fn is_same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// Classify a response `Content-Type` into a decodable body kind, or
/// `None` for an unsupported (e.g. binary) type (dsh
/// `classifyContentType`).
#[must_use]
pub fn classify_content_type(content_type: Option<&str>) -> Option<WebFetchKind> {
    let mime = content_type
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if mime == "text/html" || mime == "application/xhtml+xml" {
        return Some(WebFetchKind::Html);
    }
    if mime.starts_with("text/") {
        return Some(WebFetchKind::Text);
    }
    if mime == "application/json"
        || mime == "application/xml"
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
    {
        return Some(WebFetchKind::Text);
    }
    None
}

/// The body kinds this provider decodes (dsh `FetchableKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebFetchKind {
    /// Markup.
    Html,
    /// Plain text.
    Text,
}

/// Extract the `charset` parameter from a response `Content-Type`,
/// lower-cased, or `None` when absent (dsh `parseCharset`; the scan
/// mirrors the dsh regex over the first `;` parameter list).
#[must_use]
pub fn parse_charset(content_type: Option<&str>) -> Option<String> {
    let raw = content_type?;
    let mut params = raw.splitn(2, ';');
    params.next()?;
    let tail = params.next()?.trim_start();
    let rest = tail.strip_prefix("charset")?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('"').unwrap_or(rest);
    let value = rest.split('"').next().unwrap_or(rest);
    let value = value.split(';').next().unwrap_or(value).trim();
    (!value.is_empty()).then(|| value.to_ascii_lowercase())
}

/// Build a decoder for the declared charset, defaulting to UTF-8 (dsh
/// `decoderForCharset`; the label table is the WHATWG set via
/// `encoding_rs`, the same labels `TextDecoder` accepts).
///
/// # Errors
///
/// [`WebError`] `WEB_UNSUPPORTED_CONTENT_TYPE` for a label present but
/// not a known charset.
pub fn decoder_for_charset(
    charset: Option<&str>,
) -> Result<&'static encoding_rs::Encoding, WebError> {
    charset.map_or_else(
        || Ok(encoding_rs::UTF_8),
        |label| {
            encoding_rs::Encoding::for_label(label.as_bytes()).map_or_else(
                || {
                    Err(WebError::new(
                        "WEB_UNSUPPORTED_CONTENT_TYPE",
                        format!("unsupported charset \"{label}\""),
                    ))
                },
                Ok,
            )
        },
    )
}

/// Resolve a (possibly relative) `Location` against the current URL
/// (dsh `resolveRedirect`).
///
/// # Errors
///
/// [`WebError`] `WEB_PROVIDER_ERROR` verbatim.
pub fn resolve_redirect(location: &str, base: &Url) -> Result<Url, WebError> {
    base.join(location).map_err(|_join_failure| {
        WebError::new(
            "WEB_PROVIDER_ERROR",
            format!("invalid redirect Location \"{location}\""),
        )
    })
}

/// The resolver seam (dsh `HttpFetchResolver`, blocking face): resolve
/// one hostname to an already policy-validated address set.
pub type AddressResolver =
    std::sync::Arc<dyn Fn(&str) -> Result<Vec<std::net::IpAddr>, PinError> + Send + Sync>;

/// The anonymous public HTTP(S) fetch provider (dsh
/// `HttpFetchProvider`).
pub struct HttpFetchProvider {
    limits: HttpFetchLimits,
    resolve_addresses: AddressResolver,
}

impl std::fmt::Debug for HttpFetchProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpFetchProvider")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl HttpFetchProvider {
    /// Build the provider over resolved limits and the production
    /// resolver.
    #[must_use]
    pub fn new(limits: HttpFetchLimits) -> Self {
        Self::with_resolver(limits, std::sync::Arc::new(resolve_public_addresses))
    }

    /// Build the provider over resolved limits and an injected resolver
    /// (the focused-test shape, dsh's constructor default overridden).
    #[must_use]
    pub fn with_resolver(limits: HttpFetchLimits, resolve_addresses: AddressResolver) -> Self {
        Self {
            limits,
            resolve_addresses,
        }
    }

    /// The stable id this provider registers under (dsh
    /// `LOCAL_FETCH_PROVIDER_ID`).
    pub const LOCAL_FETCH_PROVIDER_ID: &'static str = "http";
}

impl WebFetchProvider for HttpFetchProvider {
    fn id(&self) -> &str {
        Self::LOCAL_FETCH_PROVIDER_ID
    }

    /// No credentials to check — an anonymous public fetcher is always
    /// usable.
    fn available(&self) -> bool {
        true
    }

    /// Retrieve one URL: validate, resolve and pin the public address
    /// set, follow same-origin redirects up to the hop cap, then
    /// byte-cap, classify, and decode the final response.
    ///
    /// # Errors
    ///
    /// [`WebError`] under the provider's code family.
    fn fetch(&self, request: &WebFetchRequest) -> Result<WebFetchResult, WebError> {
        let started = Instant::now();
        self.follow_and_read(&request.url, started)
    }
}

impl HttpFetchProvider {
    /// Follow same-origin redirects up to the hop cap, then read the
    /// final response (dsh `followAndRead`; the deadline signal is the
    /// per-hop read window computed from the wall clock).
    fn follow_and_read(
        &self,
        initial_url: &str,
        started: Instant,
    ) -> Result<WebFetchResult, WebError> {
        let mut current_url = validate_fetch_url(initial_url)?;
        let mut redirects_followed = 0usize;
        loop {
            let response = self.request_once(&current_url, started)?;
            if is_redirect_status(response.status) {
                if redirects_followed >= self.limits.max_redirects {
                    return Err(WebError::new(
                        "WEB_REDIRECT_BLOCKED",
                        format!(
                            "exceeded the maximum of {} redirects",
                            self.limits.max_redirects
                        ),
                    ));
                }
                let location = response.header("location").map(str::to_owned);
                let Some(location) = location else {
                    return Err(WebError::new(
                        "WEB_PROVIDER_ERROR",
                        format!(
                            "redirect response (HTTP {}) without a Location header",
                            response.status
                        ),
                    ));
                };
                let target = resolve_redirect(&location, &current_url)?;
                // Re-validate the target against the same transport
                // hygiene a direct request gets: a redirect must not be
                // a back door to a credentialed, non-http(s), or
                // over-long URL.
                let validated_target = validate_fetch_url(target.as_str())?;
                if !is_same_origin(&validated_target, &current_url) {
                    return Err(WebError::new(
                        "WEB_REDIRECT_BLOCKED",
                        format!(
                            "cross-origin redirect to {} is not followed automatically; retry against that URL directly",
                            origin(&validated_target)
                        ),
                    ));
                }
                current_url = validated_target;
                redirects_followed += 1;
                continue;
            }
            return self.read_body(response, &current_url);
        }
    }

    /// One hop: resolve and pin the hostname, then GET through a
    /// per-request agent whose read window is the remaining deadline.
    fn request_once(
        &self,
        url: &Url,
        started: Instant,
    ) -> Result<fr_net::http::HttpResponse, WebError> {
        let remaining = self.remaining_ms(started);
        if elapsed_ms(started) >= self.limits.timeout_ms {
            return Err(WebError::new("WEB_FETCH_TIMEOUT", "web fetch timed out"));
        }
        let addresses =
            (self.resolve_addresses)(url.host_str().unwrap_or_default()).map_err(pin_error)?;
        let fetch =
            UreqFetch::pinned_with_read_window(addresses, Some(Duration::from_millis(remaining)));
        let response = fetch
            .fetch_raw(HttpRequest {
                method: HttpMethod::Get,
                url: url.to_string(),
                headers: vec![
                    ("user-agent".to_owned(), self.limits.user_agent.clone()),
                    (
                        "accept".to_owned(),
                        "text/html,application/xhtml+xml,text/*;q=0.9,application/json;q=0.8"
                            .to_owned(),
                    ),
                ],
                body: None,
            })
            .map_err(|failure| {
                if elapsed_ms(started) >= self.limits.timeout_ms {
                    WebError::new("WEB_FETCH_TIMEOUT", "web fetch timed out")
                } else {
                    WebError::new(
                        "WEB_PROVIDER_ERROR",
                        format!("web fetch failed: {}", failure.message()),
                    )
                }
            })?;
        Ok(response)
    }

    /// Read, byte-cap, classify, and decode the final response body
    /// (dsh `readBody`).
    fn read_body(
        &self,
        mut response: fr_net::http::HttpResponse,
        final_url: &Url,
    ) -> Result<WebFetchResult, WebError> {
        let content_type = response.header("content-type").map(str::to_owned);
        let kind = classify_content_type(content_type.as_deref()).ok_or_else(|| {
            WebError::new(
                "WEB_UNSUPPORTED_CONTENT_TYPE",
                format!(
                    "unsupported content type \"{}\"",
                    content_type.as_deref().unwrap_or("unknown")
                ),
            )
        })?;
        let encoding = decoder_for_charset(parse_charset(content_type.as_deref()).as_deref())?;
        let (bytes, truncated_by_bytes) = self.read_capped(&mut response)?;
        let (decoded, _encoding_used, _malformed) = encoding.decode(&bytes);
        let decoded_len = decoded.encode_utf16().count();
        let truncated_by_chars = decoded_len > self.limits.max_body_chars;
        let content = if truncated_by_chars {
            slice_to_utf16_len(&decoded, self.limits.max_body_chars)
        } else {
            decoded.into_owned()
        };
        let body = match kind {
            WebFetchKind::Html => WebFetchBody::Html { content },
            WebFetchKind::Text => WebFetchBody::Text { content },
        };
        Ok(WebFetchResult {
            url: final_url.to_string(),
            status_code: response.status,
            body,
            truncated: truncated_by_bytes || truncated_by_chars,
        })
    }

    /// Read the response stream up to `max_response_bytes`. A
    /// `Content-Length` over the cap rejects immediately; a stream that
    /// grows past the cap is cut short (`truncated_by_bytes`) rather
    /// than rejected, and an exactly-at-cap body is not flagged
    /// (dropped bytes only) (dsh `readCapped`).
    fn read_capped(
        &self,
        response: &mut fr_net::http::HttpResponse,
    ) -> Result<(Vec<u8>, bool), WebError> {
        if let Some(declared) = response.header("content-length")
            && let Ok(length) = declared.trim().parse::<u64>()
            && length > self.limits.max_response_bytes
        {
            return Err(WebError::new(
                "WEB_FETCH_TOO_LARGE",
                format!(
                    "response exceeds the maximum of {} bytes",
                    self.limits.max_response_bytes
                ),
            ));
        }
        let mut bytes = Vec::new();
        let mut window = vec![0u8; 65_536].into_boxed_slice();
        let mut truncated_by_bytes = false;
        loop {
            let read = response.body.read(&mut window).map_err(|failure| {
                WebError::new("WEB_PROVIDER_ERROR", format!("web fetch failed: {failure}"))
            })?;
            if read == 0 {
                break;
            }
            let remaining = self
                .limits
                .max_response_bytes
                .saturating_sub(bytes.len() as u64);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "remaining is clamped to the u64 cap above; usize fits the collected prefix"
            )]
            let remaining = remaining as usize;
            if read > remaining {
                bytes.extend_from_slice(&window[..remaining]);
                truncated_by_bytes = true;
                break;
            }
            bytes.extend_from_slice(&window[..read]);
        }
        Ok((bytes, truncated_by_bytes))
    }

    fn remaining_ms(&self, started: Instant) -> u64 {
        let elapsed = elapsed_ms(started);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the elapsed check above bounds the difference non-negative; sub-millisecond remainders floor to the read window"
        )]
        let remaining = (self.limits.timeout_ms - elapsed).max(1.0) as u64;
        remaining
    }
}

/// The whole-call deadline arithmetic (dsh `deadline(timeoutMs)` over
/// the wall clock).
fn elapsed_ms(started: Instant) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "millisecond wall clocks stay far below the f64 mantissa; the comparison is a deadline check"
    )]
    let millis = started.elapsed().as_millis() as f64;
    millis
}

/// The dsh `URL.origin` serialization: scheme, host, and the port when
/// non-default.
fn origin(url: &Url) -> String {
    match (url.port(), url.port_or_known_default()) {
        (Some(port), _) => format!("{}://{}:{port}", url.scheme(), host_display(url)),
        (None, _) => format!("{}://{}", url.scheme(), host_display(url)),
    }
}

/// The host as the URL displays it (brackets retained around IPv6).
fn host_display(url: &Url) -> String {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain.to_owned(),
        Some(host) => host.to_string(),
        None => String::new(),
    }
}

/// Map one resolution failure onto the verbatim provider message; the
/// transport-facing `no addresses` family classifies as a provider
/// failure, the public-policy refusals as blocked URLs.
fn pin_error(error: PinError) -> WebError {
    match error {
        PinError::NoAddresses { hostname } => WebError::new(
            "WEB_PROVIDER_ERROR",
            format!("hostname \"{hostname}\" resolved to no addresses"),
        ),
        PinError::NonPublic { hostname } => WebError::new(
            "WEB_BLOCKED_URL",
            format!("URL hostname \"{hostname}\" resolves to a non-public IP address"),
        ),
        PinError::Nat64NonPublic { hostname } => WebError::new(
            "WEB_BLOCKED_URL",
            format!(
                "URL hostname \"{hostname}\" resolves through NAT64 to a non-public IPv4 address"
            ),
        ),
        PinError::Discovery(failure) => {
            WebError::new("WEB_PROVIDER_ERROR", format!("web fetch failed: {failure}"))
        }
    }
}

/// Slice a string to at most `max_len` UTF-16 code units (the dsh
/// `decoded.slice(0, maxBodyChars)` cap, counted like the JavaScript
/// `.length`). A boundary that would split a surrogate pair (the JS
/// slice can leave a lone high surrogate) stops before the character:
/// a Rust `String` cannot represent the lone surrogate.
pub(crate) fn slice_to_utf16_len(text: &str, max_len: usize) -> String {
    let mut utf16_units = 0usize;
    for (index, character) in text.char_indices() {
        if utf16_units + character.len_utf16() > max_len {
            return text[..index].to_owned();
        }
        utf16_units += character.len_utf16();
    }
    text.to_owned()
}

/// The literal-hostname shortcut the policy layer keeps for proxied
/// hops; the direct path always resolves, so fr exposes it for the
/// policy tests and future proxy seam (dsh `isNonPublicIpLiteral`).
#[must_use]
pub fn non_public_ip_literal(hostname: &str) -> bool {
    is_non_public_ip_literal(strip_ipv6_brackets(hostname))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::sync::Arc;

    fn limits() -> HttpFetchLimits {
        HttpFetchLimits {
            max_response_bytes: 1_000_000,
            max_body_chars: 100_000,
            timeout_ms: 30_000.0,
            max_redirects: 5,
            user_agent: default_user_agent(),
        }
    }

    fn provider_with_answers(answers: Vec<std::net::IpAddr>) -> HttpFetchProvider {
        HttpFetchProvider::with_resolver(limits(), Arc::new(move |_| Ok(vec![answers[0]])))
    }

    /// The URL policy: length over the cap, unparseable input, the
    /// non-http(s) schemes (message keeps the colon), embedded
    /// credentials, and the passing forms.
    #[test]
    fn url_policy_pins_the_transport_restrictions() {
        let long = format!("https://a.invalid/{}", "x".repeat(2100));
        assert_eq!(
            validate_fetch_url(&long).unwrap_err().message,
            "URL exceeds the maximum length of 2048"
        );
        assert_eq!(
            validate_fetch_url("not a url").unwrap_err().message,
            "invalid URL: not a url"
        );
        assert_eq!(
            validate_fetch_url("ftp://a.invalid/file")
                .unwrap_err()
                .message,
            "unsupported URL scheme \"ftp:\" (only http and https are allowed)"
        );
        assert_eq!(
            parse_fetch_url("https://user:pass@a.invalid/")
                .unwrap_err()
                .message,
            "credentials in URLs are not allowed"
        );
        assert_eq!(
            parse_fetch_url("https://a.invalid/")
                .expect("parses")
                .scheme(),
            "https"
        );
    }

    /// Content classification and charset extraction follow the dsh
    /// tables; an unknown label refuses loudly instead of mojibake.
    #[test]
    fn content_classification_and_charset_pins() {
        assert_eq!(
            classify_content_type(Some("text/html; charset=utf-8")),
            Some(WebFetchKind::Html)
        );
        assert_eq!(
            classify_content_type(Some("application/xhtml+xml")),
            Some(WebFetchKind::Html)
        );
        assert_eq!(
            classify_content_type(Some("text/plain")),
            Some(WebFetchKind::Text)
        );
        assert_eq!(
            classify_content_type(Some("application/vnd.api+json")),
            Some(WebFetchKind::Text)
        );
        assert_eq!(classify_content_type(Some("image/png")), None);
        assert_eq!(classify_content_type(None), None);

        assert_eq!(
            parse_charset(Some("text/html; charset=UTF-8")),
            Some("utf-8".to_owned())
        );
        assert_eq!(
            parse_charset(Some("text/html; charset=\"iso-8859-1\"")),
            Some("iso-8859-1".to_owned())
        );
        assert_eq!(parse_charset(Some("text/html")), None);
        assert_eq!(
            decoder_for_charset(Some("klingon")).unwrap_err().message,
            "unsupported charset \"klingon\""
        );
        assert_eq!(
            decoder_for_charset(None).expect("default"),
            encoding_rs::UTF_8
        );
    }

    /// Relative Location resolution, same-origin comparisons (the
    /// default port normalizes away), and the origin serialization.
    #[test]
    fn redirect_resolution_and_origin_pins() {
        let base = Url::parse("https://a.invalid/dir/page").expect("base");
        assert_eq!(
            resolve_redirect("next", &base).expect("joins").as_str(),
            "https://a.invalid/dir/next"
        );
        let plain = Url::parse("http://a.invalid/x").expect("plain");
        let explicit = Url::parse("http://a.invalid:80/y").expect("explicit");
        assert!(is_same_origin(&plain, &explicit));
        assert!(!is_same_origin(
            &plain,
            &Url::parse("http://a.invalid:8080/y").expect("port")
        ));
        assert_eq!(origin(&plain), "http://a.invalid");
        assert_eq!(origin(&explicit), "http://a.invalid");
        assert_eq!(
            origin(&Url::parse("https://[2001:db8::1]:8443/").expect("v6")),
            "https://[2001:db8::1]:8443"
        );
    }

    /// The UTF-16 slice never splits a surrogate pair.
    #[test]
    fn utf16_slice_stops_before_surrogates() {
        assert_eq!(slice_to_utf16_len("hello", 3), "hel");
        let astral = "a\u{1F600}b";
        assert_eq!(slice_to_utf16_len(astral, 2), "a");
        assert_eq!(slice_to_utf16_len(astral, 3), "a\u{1F600}");
        assert_eq!(slice_to_utf16_len(astral, 99), astral);
    }

    /// The limits resolution: every default, and each refusal text.
    #[test]
    fn limits_resolution_pins_defaults_and_refusals() {
        let defaults = resolve_limits(&serde_json::json!({})).expect("defaults");
        assert_eq!(defaults.max_response_bytes, 5_000_000);
        assert_eq!(defaults.max_body_chars, 100_000);
        assert_eq!(defaults.timeout_ms, 30_000.0);
        assert_eq!(defaults.max_redirects, 5);
        assert_eq!(defaults.user_agent, default_user_agent());
        assert_eq!(
            resolve_limits(&serde_json::json!({"max-response-bytes": 0})).unwrap_err(),
            "web-fetch-http: max-response-bytes must be a positive finite number"
        );
        assert_eq!(
            resolve_limits(&serde_json::json!({"timeout-ms": 2_147_483_648.0})).unwrap_err(),
            "web-fetch-http: timeoutMs must be no greater than 2147483647"
        );
        assert_eq!(
            resolve_limits(&serde_json::json!({"max-redirects": -1})).unwrap_err(),
            "web-fetch-http: maxRedirects must be a non-negative integer"
        );
    }

    /// The full transport against a loopback origin: plain 200, the
    /// same-origin redirect follow, the cross-origin refusal, the hop
    /// budget, and the unsupported content type.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the loopback scenario table mirrors the dsh provider arms one section per behavior; splitting it would hide the correspondence"
    )]
    fn transport_pins_loopback_semantics() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for request in server.incoming() {
                let mut socket = request.unwrap();
                let mut buffer = [0u8; 4096];
                let read = std::io::Read::read(&mut socket, &mut buffer).unwrap_or(0);
                let text = String::from_utf8_lossy(&buffer[..read]);
                let response = if text.contains("GET /moved") {
                    "HTTP/1.1 301 Moved Permanently\r\nConnection: close\r\nLocation: /final\r\nContent-Length: 0\r\n\r\n"
                } else if text.contains("GET /away") {
                    "HTTP/1.1 302 Found\r\nConnection: close\r\nLocation: http://other.invalid/x\r\nContent-Length: 0\r\n\r\n"
                } else if text.contains("GET /loop") {
                    "HTTP/1.1 302 Found\r\nConnection: close\r\nLocation: /loop\r\nContent-Length: 0\r\n\r\n"
                } else if text.contains("GET /bin") {
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/octet-stream\r\nContent-Length: 2\r\n\r\n\x00\x01"
                } else if text.contains("GET /big") {
                    // Close-delimited (no Content-Length): eight bytes
                    // arrive, the cap keeps four.
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: text/plain\r\n\r\nabcdefgh");
                    continue;
                } else if text.contains("GET /latin") {
                    // The declared charset carries a raw latin-1 byte
                    // (not its UTF-8 spelling), so the response writes
                    // as bytes.
                    let mut latin = Vec::new();
                    latin.extend_from_slice(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: text/plain; charset=iso-8859-1\r\nContent-Length: 4\r\n\r\ncaf");
                    latin.push(0xE9);
                    socket.write_all(&latin).unwrap();
                    continue;
                } else {
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\nok"
                };
                socket.write_all(response.as_bytes()).unwrap();
            }
        });
        let base = format!("http://a.invalid:{port}");
        let provider = provider_with_answers(vec![std::net::IpAddr::from([127, 0, 0, 1])]);

        let plain = provider
            .fetch(&WebFetchRequest {
                url: format!("{base}/ok"),
            })
            .expect("plain fetch");
        assert_eq!(plain.status_code, 200);
        assert_eq!(
            plain.body,
            WebFetchBody::Text {
                content: "ok".to_owned()
            }
        );
        assert!(!plain.truncated);

        let moved = provider
            .fetch(&WebFetchRequest {
                url: format!("{base}/moved"),
            })
            .expect("same-origin redirect follows");
        assert_eq!(moved.url, format!("{base}/final"));

        let away = provider
            .fetch(&WebFetchRequest {
                url: format!("{base}/away"),
            })
            .unwrap_err();
        assert_eq!(away.code, "WEB_REDIRECT_BLOCKED");
        assert_eq!(
            away.message,
            "cross-origin redirect to http://other.invalid is not followed automatically; retry against that URL directly"
        );

        let looped = provider
            .fetch(&WebFetchRequest {
                url: format!("{base}/loop"),
            })
            .unwrap_err();
        assert_eq!(looped.code, "WEB_REDIRECT_BLOCKED");
        assert_eq!(looped.message, "exceeded the maximum of 5 redirects");

        let binary = provider
            .fetch(&WebFetchRequest {
                url: format!("{base}/bin"),
            })
            .unwrap_err();
        assert_eq!(binary.code, "WEB_UNSUPPORTED_CONTENT_TYPE");
        assert_eq!(
            binary.message,
            "unsupported content type \"application/octet-stream\""
        );

        let latin = provider
            .fetch(&WebFetchRequest {
                url: format!("{base}/latin"),
            })
            .expect("declared charset decodes");
        assert_eq!(
            latin.body,
            WebFetchBody::Text {
                content: "café".to_owned()
            }
        );

        let capped = HttpFetchProvider::with_resolver(
            HttpFetchLimits {
                max_response_bytes: 4,
                ..limits()
            },
            Arc::new(move |_| Ok(vec![std::net::IpAddr::from([127, 0, 0, 1])])),
        );
        let over = capped
            .fetch(&WebFetchRequest {
                url: format!("{base}/big"),
            })
            .expect("over-cap stream truncates");
        assert_eq!(
            over.body,
            WebFetchBody::Text {
                content: "abcd".to_owned()
            }
        );
        assert!(over.truncated);
    }

    /// A declared Content-Length over the cap refuses before reading.
    #[test]
    fn declared_length_over_cap_refuses() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            let mut buffer = [0u8; 4096];
            let _ = std::io::Read::read(&mut socket, &mut buffer);
            let response = "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: text/plain\r\nContent-Length: 100\r\n\r\nshort";
            socket.write_all(response.as_bytes()).unwrap();
        });
        let provider = HttpFetchProvider::with_resolver(
            HttpFetchLimits {
                max_response_bytes: 4,
                ..limits()
            },
            Arc::new(move |_| Ok(vec![std::net::IpAddr::from([127, 0, 0, 1])])),
        );
        let error = provider
            .fetch(&WebFetchRequest {
                url: format!("http://a.invalid:{port}/huge"),
            })
            .unwrap_err();
        assert_eq!(error.code, "WEB_FETCH_TOO_LARGE");
        assert_eq!(error.message, "response exceeds the maximum of 4 bytes");
    }
}
