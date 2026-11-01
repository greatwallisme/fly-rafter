//! The fr-net `Fetch` transport contract (dsh `fetch?: typeof fetch` seam
//! shape) and the production ureq backend (Q69-B carrier; 2026-10-27
//! Q77-A plugin home: the workspace's single ureq site).
//!
//! Callers classify outcomes; the seam carries transport failures
//! verbatim from the HTTP layer (dsh `fetch` throws a `TypeError` whose
//! text the adapter never surfaces — the adapter and Files client wrap
//! with their own TRANSPORT messages). Redirect handling mirrors dsh's
//! `redirect: 'error'`: the production backend refuses to follow and the
//! seam turns a 3xx into a transport failure. Abort responsiveness has
//! read-window granularity (the same nature as dsh's undici, where an
//! abort landing on an in-flight body read leaves the error dropped and
//! the deadline settles the demand).

use std::io::Read;
use std::time::Duration;

/// One transport-level failure; classification happens above the seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError {
    message: String,
}

impl TransportError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The failure description (internal; never surfaced verbatim).
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TransportError {}

/// The request methods the provider uses (dsh `fetch` calls: POST
/// /messages, POST /files, GET/DELETE /files/:id).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    /// GET.
    Get,
    /// POST.
    Post,
    /// DELETE.
    Delete,
}

impl HttpMethod {
    /// The wire method spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Delete => "DELETE",
        }
    }
}

/// One outbound request (dsh `RequestInit`: method, headers in insertion
/// order, optional body bytes).
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// Request method.
    pub method: HttpMethod,
    /// Absolute URL.
    pub url: String,
    /// Headers in the caller's insertion order.
    pub headers: Vec<(String, String)>,
    /// Body bytes, when the method carries one.
    pub body: Option<Vec<u8>>,
}

/// One inbound response (dsh `Response`: status, headers, body). The
/// body is always an incremental reader: consumers that want the whole
/// payload read it to end, and the SSE face consumes it chunk by chunk.
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers (lower-cased names as received).
    pub headers: Vec<(String, String)>,
    /// The incremental body reader (bounded by the per-read window).
    pub body: Box<dyn Read + Send>,
}

impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

impl HttpResponse {
    /// The first header value for one lower-cased name (dsh
    /// `headers.get(name)`; the lookup the transport classifier injects).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// The transport seam (dsh `fetch?: typeof fetch`): every provider HTTP
/// call goes through one implementation, and tests inject recorded
/// responses.
pub trait Fetch: Send + Sync {
    /// Execute one request.
    ///
    /// # Errors
    ///
    /// [`TransportError`] for any connection, timeout, or refused-redirect
    /// failure.
    fn fetch(&self, request: HttpRequest) -> Result<HttpResponse, TransportError>;
}

/// Whether one status is a redirect the production backend refuses (dsh
/// `redirect: 'error'`).
#[must_use]
pub fn is_refused_redirect(status: u16) -> bool {
    (300..400).contains(&status)
}

/// The production backend (Q69-B): ureq with rustls, redirects refused,
/// and a per-read window so blocking reads re-check the deadline signal
/// at bounded granularity.
#[derive(Debug)]
pub struct UreqFetch {
    agent: ureq::Agent,
}

impl UreqFetch {
    /// Build the backend: no redirects (dsh `redirect: 'error'`), a 10s
    /// connect timeout (undici's default), and a 200ms read window for
    /// deadline checks. The read window is an internal granularity, not a
    /// wire-visible fact.
    #[must_use]
    pub fn new() -> Self {
        Self::with_read_window(Some(Duration::from_millis(200)))
    }

    /// Build the backend with an explicit read window (the web-fetch
    /// provider passes each hop's remaining deadline as the window, so a
    /// stalled read surfaces at the deadline instead of the default).
    #[must_use]
    pub fn with_read_window(window: Option<Duration>) -> Self {
        let config = Self::config(window);
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }

    /// The shared agent configuration: statuses are data, never errors
    /// (ureq's `http_status_as_error` defaults to `true`, which would
    /// collapse every 4xx/5xx into a transport failure and strip the
    /// status the callers classify on — dsh's `fetch` returns the
    /// `Response` for any status).
    fn config(window: Option<Duration>) -> ureq::config::Config {
        ureq::Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_per_read(window)
            .build()
    }

    /// The default backend behind every [`Fetch`] injection point.
    #[must_use]
    pub fn shared() -> &'static Self {
        static BACKEND: std::sync::OnceLock<UreqFetch> = std::sync::OnceLock::new();
        BACKEND.get_or_init(Self::new)
    }

    /// Build one pinned transport: a per-request agent whose resolver
    /// serves only the validated address set (dsh `requestPinned`; the
    /// agent is this request's own because pinning is how this package
    /// refuses a DNS answer that changes between validation and
    /// connection, and it may not apply process-wide).
    #[must_use]
    pub fn pinned(addresses: Vec<std::net::IpAddr>) -> Self {
        Self::pinned_with_read_window(addresses, Some(Duration::from_millis(200)))
    }

    /// [`Self::pinned`] with an explicit read window (the web-fetch
    /// provider passes each hop's remaining deadline).
    #[must_use]
    pub fn pinned_with_read_window(
        addresses: Vec<std::net::IpAddr>,
        window: Option<Duration>,
    ) -> Self {
        let config = Self::config(window);
        Self {
            agent: ureq::Agent::with_parts(
                config,
                ureq::unversioned::transport::DefaultConnector::default(),
                crate::pinned::PinnedResolver::new(addresses),
            ),
        }
    }

    fn send(
        &self,
        request: HttpRequest,
    ) -> Result<ureq::http::Response<ureq::Body>, TransportError> {
        // ureq 3 types the call by body presence: only POST carries a
        // body in this provider (dsh sends bodies on POST alone too).
        let response = match (request.method, request.body) {
            (HttpMethod::Post, Some(body)) => {
                let mut call = self.agent.post(&request.url);
                for (name, value) in &request.headers {
                    call = call.header(name, value);
                }
                call.send(&*body)
            }
            (HttpMethod::Post, None) => {
                let mut call = self.agent.post(&request.url);
                for (name, value) in &request.headers {
                    call = call.header(name, value);
                }
                call.send(&[][..])
            }
            (HttpMethod::Get | HttpMethod::Delete, None) => {
                let mut call = match request.method {
                    HttpMethod::Get => self.agent.get(&request.url),
                    _ => self.agent.delete(&request.url),
                };
                for (name, value) in &request.headers {
                    call = call.header(name, value);
                }
                call.call()
            }
            (HttpMethod::Get | HttpMethod::Delete, Some(_)) => {
                return Err(TransportError::new("HTTP request failed"));
            }
        }
        .map_err(|_error| TransportError::new("HTTP request failed"))?;
        Ok(response)
    }
}

impl Default for UreqFetch {
    fn default() -> Self {
        Self::new()
    }
}

impl UreqFetch {
    /// Execute one request without the redirect-status refusal: a 3xx
    /// response returns as a response (the web-fetch provider's manual
    /// same-origin redirect loop reads the `Location` header itself;
    /// the band split mirrors dsh `redirect: 'manual'` vs `'error'`).
    ///
    /// # Errors
    ///
    /// [`TransportError`] for connection failures.
    pub fn fetch_raw(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let response = self.send(request)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or("").to_owned(),
                )
            })
            .collect();
        Ok(HttpResponse {
            status,
            headers,
            body: Box::new(response.into_body().into_reader()),
        })
    }
}

impl Fetch for UreqFetch {
    fn fetch(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let response = self.fetch_raw(request)?;
        if is_refused_redirect(response.status) {
            return Err(TransportError::new("HTTP redirect refused"));
        }
        Ok(response)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// The refused band is exactly 3xx; 2xx and 4xx stay responses.
    #[test]
    fn refused_redirect_band_pins_redirect_error() {
        assert!(!is_refused_redirect(200));
        assert!(!is_refused_redirect(299));
        assert!(is_refused_redirect(300));
        assert!(is_refused_redirect(399));
        assert!(!is_refused_redirect(400));
    }

    /// Header lookup is case-insensitive with first-match semantics.
    #[test]
    fn header_lookup_pins_case_insensitivity() {
        let response = HttpResponse {
            status: 200,
            headers: vec![
                ("retry-after".to_owned(), "3".to_owned()),
                ("Request-Id".to_owned(), "req_1".to_owned()),
            ],
            body: Box::new(std::io::empty()),
        };
        assert_eq!(response.header("retry-after"), Some("3"));
        assert_eq!(response.header("Retry-After"), Some("3"));
        assert_eq!(response.header("request-id"), Some("req_1"));
        assert_eq!(response.header("x-absent"), None);
    }

    /// The production backend walks the full ureq path against a local
    /// loopback server: status, headers, body bytes, and the refused
    /// redirect mapping all hold on real sockets.
    #[test]
    fn ureq_backend_pins_loopback_roundtrip_and_redirect_refusal() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for request in server.incoming() {
                let mut socket = request.unwrap();
                let mut buf = [0u8; 1024];
                let read = std::io::Read::read(&mut socket, &mut buf).unwrap_or(0);
                let text = String::from_utf8_lossy(&buf[..read]);
                let response = if text.contains("GET /moved") {
                    "HTTP/1.1 302 Found\r\nLocation: /there\r\nContent-Length: 0\r\n\r\n"
                } else if text.contains("GET /missing") {
                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\nnot found!!"
                } else {
                    "HTTP/1.1 200 OK\r\nRetry-After: 3\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nhello"
                };
                socket.write_all(response.as_bytes()).unwrap();
            }
        });

        let fetch = UreqFetch::new();
        let mut ok = fetch
            .fetch(HttpRequest {
                method: HttpMethod::Get,
                url: format!("http://127.0.0.1:{port}/hello"),
                headers: Vec::new(),
                body: None,
            })
            .unwrap();
        assert_eq!(ok.status, 200);
        assert_eq!(ok.header("retry-after"), Some("3"));
        let mut body = String::new();
        ok.body.read_to_string(&mut body).unwrap();
        assert_eq!(body, "hello");

        // A 4xx is a response, not a transport failure: callers
        // classify statuses (dsh fetch returns the Response for any
        // status; ureq's status-as-error default stays off).
        let mut missing = fetch
            .fetch(HttpRequest {
                method: HttpMethod::Get,
                url: format!("http://127.0.0.1:{port}/missing"),
                headers: Vec::new(),
                body: None,
            })
            .unwrap();
        assert_eq!(missing.status, 404);
        let mut body = String::new();
        missing.body.read_to_string(&mut body).unwrap();
        assert_eq!(body, "not found!!");

        let redirect = fetch
            .fetch(HttpRequest {
                method: HttpMethod::Get,
                url: format!("http://127.0.1:{port}/moved"),
                headers: Vec::new(),
                body: None,
            })
            .unwrap_err();
        assert_eq!(redirect.message(), "HTTP redirect refused");
    }
}
