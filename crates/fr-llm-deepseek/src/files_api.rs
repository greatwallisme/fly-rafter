//! `DeepSeek` Files API transport (dsh `files-api.ts`): one direct client
//! over the injected [`Fetch`] seam that retains the configured URL root
//! and refuses redirects before credentials can leave its origin.
//!
//! Every request carries the attribution headers, the resolved
//! authentication headers, `anthropic-version`, and the Files beta header,
//! in that order. Non-2xx responses classify into a [`FilesError`] that
//! retains its HTTP status for the bounded recovery policy.

use crate::codes::{FILES_API_CODE, INVALID_REQUEST_CODE, INVALID_RESPONSE_CODE};
use crate::file_id::DeepSeekFileId;
use crate::messages_api::{MESSAGES_FILES_BETA, messages_api_root};
use fr_llm::adapter::AbortSignal;
use fr_llm::attribution::attribution_headers;
use fr_llm::error::{AUTH_CODE, LlmError, RATE_LIMIT_CODE, SERVER_CODE, TRANSPORT_CODE};
use fr_net::http::{Fetch, HttpMethod, HttpRequest};
use fr_wire::ImageMediaType;
use std::fmt::Write as _;
use std::io::Read as _;

/// Minimum provider-supported file lifetime (dsh
/// `MIN_FILE_EXPIRY_SECONDS`).
pub const MIN_FILE_EXPIRY_SECONDS: u64 = 3_600;
/// Maximum provider-supported file lifetime (dsh
/// `MAX_FILE_EXPIRY_SECONDS`).
pub const MAX_FILE_EXPIRY_SECONDS: u64 = 2_592_000;
/// Maximum Files API upload size (dsh `MAX_FILE_UPLOAD_BYTES`).
pub const MAX_FILE_UPLOAD_BYTES: u64 = 128 * 1024 * 1024;
/// Current per-key file-count quota (dsh `MAX_STORED_FILE_COUNT`).
pub const MAX_STORED_FILE_COUNT: u64 = 10_000;
/// Current per-key storage quota (dsh `MAX_STORED_FILE_BYTES`).
pub const MAX_STORED_FILE_BYTES: u64 = 25 * 1024 * 1024 * 1024;

/// Validated provider file metadata (dsh `DeepSeekFileObject`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepSeekFileObject {
    /// Provider file identifier.
    pub id: DeepSeekFileId,
    /// Exact stored byte length.
    pub bytes: u64,
    /// Upload creation time in epoch seconds.
    pub created_at: u64,
    /// Provider-side file name.
    pub filename: String,
}

/// One successful upload beside its reuse deadline (dsh returns
/// `{...file, expiresAt}`): Messages omits expiry metadata, so the
/// deadline uses upload creation plus the requested lifetime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepSeekFileObjectWithExpiry {
    /// The validated file.
    pub file: DeepSeekFileObject,
    /// Reuse deadline in epoch seconds.
    pub expires_at: u64,
}

/// One page returned by `GET /files` (dsh `DeepSeekFilePage`; null
/// cursors are omitted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepSeekFilePage {
    /// Provider-ordered files of one page.
    pub data: Vec<DeepSeekFileObject>,
    /// Opaque pagination cursor at the page start.
    pub first_id: Option<DeepSeekFileId>,
    /// Opaque pagination cursor at the page end.
    pub last_id: Option<DeepSeekFileId>,
    /// Whether another page follows.
    pub has_more: bool,
}

/// One Files API operation failure beside its classification detail (dsh
/// `DeepSeekFilesError`: an `LlmError` with the status retained plus the
/// joined provider detail, used only for error classification).
#[derive(Debug, Clone)]
pub struct FilesError {
    /// The provider-neutral failure (boxed: the diagnostic payload is
    /// cold-path, so `Result` carriers stay small).
    pub error: Box<LlmError>,
    /// Joined provider detail (code/type/message) for classification.
    detail: String,
}

impl FilesError {
    /// The joined provider detail text.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// The stable machine-routing code of the underlying failure.
    #[must_use]
    pub fn error_code(&self) -> &str {
        self.error.code()
    }

    /// Wrap a non-status failure (transport, abort, precheck) with an
    /// empty classification detail (dsh's non-`DeepSeekFilesError`
    /// throws: quota recovery never applies to them).
    fn from_llm(error: LlmError) -> Self {
        Self {
            error: Box::new(error),
            detail: String::new(),
        }
    }

    fn new(message: &str, status: u16, detail: String) -> Self {
        let code = match status {
            401 | 403 => AUTH_CODE,
            429 => RATE_LIMIT_CODE,
            status if status >= 500 => SERVER_CODE,
            _ => FILES_API_CODE,
        };
        let error = LlmError::try_new(message, code, Some(status), None, None, None)
            .expect("files error fields are static");
        Self {
            error: Box::new(error),
            detail,
        }
    }
}

/// Whether one upload failure reports a provider storage or file-count
/// quota (dsh `isFilesQuotaError`).
///
/// The detail matches
/// `(?:quota|storage|stored files|file count|too many files)` without
/// case sensitivity, so one bounded remote cleanup and upload retry may
/// recover the failure.
#[must_use]
pub fn is_files_quota_error(error: &FilesError) -> bool {
    let detail = error.detail();
    let lower = detail.to_lowercase();
    lower.contains("quota")
        || lower.contains("storage")
        || lower.contains("stored files")
        || lower.contains("file count")
        || lower.contains("too many files")
}

fn invalid_response(operation: &str) -> LlmError {
    LlmError::try_new(
        &format!("DeepSeek Files API returned an invalid {operation} response."),
        INVALID_RESPONSE_CODE,
        None,
        None,
        None,
        None,
    )
    .expect("invalid-response fields are static")
}

fn invalid_json(operation: &str, status: u16) -> LlmError {
    LlmError::try_new(
        &format!("DeepSeek Files API returned invalid JSON for {operation} (HTTP {status})."),
        INVALID_RESPONSE_CODE,
        Some(status),
        None,
        None,
        None,
    )
    .expect("invalid-json fields are static")
}

fn transport_failure(base_url: &str) -> LlmError {
    LlmError::try_new(
        &format!("DeepSeek Files API request to {base_url} failed"),
        TRANSPORT_CODE,
        None,
        None,
        None,
        None,
    )
    .expect("transport fields are static")
}

/// Parse an ISO-8601 timestamp to epoch seconds, flooring sub-second
/// precision (dsh `Math.floor(Date.parse(created_at) / 1_000)`). The
/// provider returns `YYYY-MM-DDTHH:MM:SS(.frac)?(Z|±HH:MM)`; the civil
/// date converts with Hinnant's algorithm, the same basis as the
/// IMF-fixdate parser in the transport classifier.
fn parse_epoch_seconds(value: &str) -> Option<u64> {
    let (core, offset_minutes) = parse_core_and_offset(value)?;
    let (date, time) = core.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second_text = time_parts.next().unwrap_or("0");
    let second: i64 = second_text
        .split_once('.')
        .map_or(second_text, |(whole, _)| whole)
        .parse()
        .ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Hinnant's days_from_civil.
    let shifted_year = if month <= 2 { year - 1 } else { year };
    let era = shifted_year.div_euclid(400);
    let year_of_era = shifted_year.rem_euclid(400);
    let day_of_year = (153 * (month + 9).rem_euclid(12) + 2) / 5 + day - 1;
    // The civil epoch (0000-03-01) sits 719,468 days before the Unix epoch.
    let days = era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100
        + day_of_year
        - 719_468;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_minutes * 60;
    u64::try_from(seconds).ok()
}

/// Split the trailing UTC offset (`+HH:MM` / `-HH:MM` / `Z`) into whole
/// minutes east of UTC.
fn parse_core_and_offset(value: &str) -> Option<(&str, i64)> {
    if let Some(rest) = value.strip_suffix('Z') {
        return Some((rest, 0));
    }
    if let Some(position) = value.rfind(['+', '-']) {
        // Skip the date's hyphens: the offset is the last +/- after 'T'.
        let t_position = value.find('T')?;
        if position > t_position {
            let (core, offset) = (&value[..position], &value[position..]);
            let sign = offset.as_bytes()[0];
            let mut parts = offset[1..].split(':');
            let hours: i64 = parts.next()?.parse().ok()?;
            let minutes: i64 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let signed = if sign == b'+' {
                hours * 60 + minutes
            } else {
                -(hours * 60 + minutes)
            };
            return Some((core, signed));
        }
    }
    Some((value, 0))
}

/// Provider error detail fields joined for classification (dsh
/// `providerErrorDetail` in this file: message plus the code/type/message
/// join of the error envelope).
fn provider_error_detail(value: &serde_json::Value) -> (Option<String>, String) {
    let error = value.get("error").filter(|error| error.is_object());
    let Some(error) = error else {
        return (None, String::new());
    };
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let detail = ["code", "type", "message"]
        .iter()
        .filter_map(|field| error.get(*field).and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    (message, detail)
}

/// Connection facts for one Files client (dsh `FilesApiOptions`).
pub struct FilesApiOptions<'a> {
    /// Messages API root (any spelling; normalized on construction).
    pub base_url: String,
    /// Provider-resolved authentication headers for this endpoint.
    pub headers: Vec<(String, String)>,
    /// The transport seam (dsh `fetch?: typeof fetch`; tests inject).
    pub fetch: &'a dyn Fetch,
}

impl std::fmt::Debug for FilesApiOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesApiOptions")
            .field("base_url", &self.base_url)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

/// Percent-encode one path segment (dsh `encodeURIComponent`).
fn encode_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            );
        if keep {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// The validated parse of one provider file object (dsh
/// `parseFileObject`): every field checked, `created_at` parsed to epoch
/// seconds.
fn parse_file_object(
    value: &serde_json::Value,
    operation: &str,
) -> Result<DeepSeekFileObject, LlmError> {
    let Some(object) = value.as_object() else {
        return Err(invalid_response(operation));
    };
    let created_at = object
        .get("created_at")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_epoch_seconds);
    let id = object.get("id").and_then(serde_json::Value::as_str);
    let size = object.get("size_bytes").and_then(serde_json::Value::as_u64);
    let filename = object.get("filename").and_then(serde_json::Value::as_str);
    let type_ok = object.get("type").and_then(serde_json::Value::as_str) == Some("file");
    let mime_ok = object
        .get("mime_type")
        .is_some_and(serde_json::Value::is_string);
    let Some(id) = id.filter(|id| !id.is_empty()) else {
        return Err(invalid_response(operation));
    };
    let Some(size) = size else {
        return Err(invalid_response(operation));
    };
    let Some(created_at) = created_at else {
        return Err(invalid_response(operation));
    };
    let Some(filename) = filename.filter(|name| !name.is_empty()) else {
        return Err(invalid_response(operation));
    };
    if !type_ok || !mime_ok {
        return Err(invalid_response(operation));
    }
    Ok(DeepSeekFileObject {
        id: DeepSeekFileId::new(id),
        bytes: size,
        created_at,
        filename: filename.to_owned(),
    })
}

/// `DeepSeek` Files API client (dsh `DeepSeekFilesClient`).
pub struct DeepSeekFilesClient<'a> {
    base_url: String,
    auth_headers: Vec<(String, String)>,
    fetch: &'a dyn Fetch,
}

impl std::fmt::Debug for DeepSeekFilesClient<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeepSeekFilesClient")
            .field("base_url", &self.base_url)
            .field("auth_headers", &self.auth_headers)
            .finish_non_exhaustive()
    }
}

impl<'a> DeepSeekFilesClient<'a> {
    /// Retain the configured URL root (normalized through
    /// [`messages_api_root`]) and the resolved authentication headers.
    #[must_use]
    pub fn new(options: FilesApiOptions<'a>) -> Self {
        Self {
            base_url: messages_api_root(&options.base_url),
            auth_headers: options.headers,
            fetch: options.fetch,
        }
    }

    /// The normalized root this client retains.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn base_headers(&self) -> Vec<(String, String)> {
        let mut headers: Vec<(String, String)> = attribution_headers()
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect();
        headers.extend(self.auth_headers.iter().cloned());
        headers.push(("anthropic-version".to_owned(), "2023-06-01".to_owned()));
        headers.push(("anthropic-beta".to_owned(), MESSAGES_FILES_BETA.to_owned()));
        headers
    }

    /// Execute one Files call: dsh's header order (attribution, auth,
    /// `anthropic-version`, `anthropic-beta`), `redirect: 'error'` from
    /// the backend, and the abort check at every read window boundary.
    fn request(
        &self,
        method: HttpMethod,
        path: &str,
        body: Option<(Vec<u8>, &str)>,
        operation: &str,
        signal: Option<&AbortSignal>,
    ) -> Result<(u16, serde_json::Value), FilesError> {
        if let Some(signal) = signal
            && signal.is_aborted()
        {
            return Err(FilesError {
                error: Box::new(abort_error()),
                detail: String::new(),
            });
        }
        let mut headers = self.base_headers();
        if let Some((_, content_type)) = &body {
            headers.push(("content-type".to_owned(), (*content_type).to_owned()));
        }
        let request = HttpRequest {
            method,
            url: format!("{}{}", self.base_url, path),
            headers,
            body: body.map(|(bytes, _)| bytes),
        };
        let mut response = self.fetch.fetch(request).map_err(|_transport| FilesError {
            error: Box::new(transport_failure(&self.base_url)),
            detail: String::new(),
        })?;
        let status = response.status;
        let mut bytes = Vec::new();
        let read = response.body.read_to_end(&mut bytes);
        if let Err(_error) = read {
            return Err(FilesError {
                error: Box::new(transport_failure(&self.base_url)),
                detail: String::new(),
            });
        }
        if !(200..300).contains(&status) {
            let parsed: serde_json::Value =
                serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
            let (message, detail) = provider_error_detail(&parsed);
            let fallback = format!("DeepSeek Files API error (HTTP {status})");
            return Err(FilesError::new(
                message.as_deref().unwrap_or(&fallback),
                status,
                detail,
            ));
        }
        let json = serde_json::from_slice(&bytes).map_err(|_parse| FilesError {
            error: Box::new(invalid_json(operation, status)),
            detail: String::new(),
        })?;
        Ok((status, json))
    }
}

fn abort_error() -> LlmError {
    LlmError::try_new(
        "DeepSeek Files API request aborted",
        fr_attachment::request_image_pipeline::ABORTED_CODE,
        None,
        None,
        None,
        None,
    )
    .expect("abort fields are static")
}

/// One upload input (dsh `upload`): deterministic request-version bytes,
/// media type, provider filename, and the requested lifetime.
#[derive(Debug)]
pub struct UploadInput {
    /// Encoded image bytes.
    pub data: Vec<u8>,
    /// Declared media type of the bytes.
    pub media_type: ImageMediaType,
    /// Provider-side file name (dsh-prefixed per decision 37-5).
    pub filename: String,
    /// Requested file lifetime in seconds.
    pub expires_after_seconds: u64,
}

/// The `content-type` and body of one multipart/form-data upload (dsh
/// `FormData` serialization: two scalar fields then the file part).
fn multipart(input: &UploadInput) -> (String, Vec<u8>) {
    let boundary = format!(
        "----fr-formdata-{}",
        fr_attachment::request_image_pipeline::random_hex_suffix()
    );
    let media = match input.media_type {
        ImageMediaType::Png => "image/png",
        ImageMediaType::Jpeg => "image/jpeg",
        ImageMediaType::Webp => "image/webp",
        ImageMediaType::Gif => "image/gif",
    };
    let mut body = Vec::new();
    let mut part = |field: &str, value: &str| {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    };
    part("expires_after[anchor]", "created_at");
    part(
        "expires_after[seconds]",
        &input.expires_after_seconds.to_string(),
    );
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\nContent-Type: {media}\r\n\r\n",
            input.filename
        )
        .as_bytes(),
    );
    body.extend_from_slice(&input.data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

impl DeepSeekFilesClient<'_> {
    /// Upload one image with an explicit expiry (dsh `upload`): the
    /// prechecks run before any I/O, the reuse deadline is upload
    /// creation plus the requested lifetime.
    ///
    /// # Errors
    ///
    /// Returns [`FilesError`] when a precheck rejects the input, the
    /// transport fails, the abort signal fires, or the provider answers
    /// with a non-2xx status or an unparseable payload.
    ///
    /// # Panics
    ///
    /// Panics only if the constant precheck error fields fail
    /// [`LlmError::try_new`] validation (a programmer error that cannot
    /// occur for these static texts).
    pub fn upload(
        &self,
        input: &UploadInput,
        signal: Option<&AbortSignal>,
    ) -> Result<DeepSeekFileObjectWithExpiry, FilesError> {
        if input.data.len() as u64 > MAX_FILE_UPLOAD_BYTES {
            return Err(FilesError {
                error: Box::new(
                    LlmError::try_new(
                        "DeepSeek Files API upload exceeds 128 MiB.",
                        INVALID_REQUEST_CODE,
                        None,
                        None,
                        None,
                        None,
                    )
                    .expect("upload precheck fields are static"),
                ),
                detail: String::new(),
            });
        }
        if input.expires_after_seconds < MIN_FILE_EXPIRY_SECONDS
            || input.expires_after_seconds > MAX_FILE_EXPIRY_SECONDS
        {
            return Err(FilesError {
                error: Box::new(
                    LlmError::try_new(
                        "DeepSeek file expiry must be between 3600 and 2592000 seconds.",
                        INVALID_REQUEST_CODE,
                        None,
                        None,
                        None,
                        None,
                    )
                    .expect("expiry precheck fields are static"),
                ),
                detail: String::new(),
            });
        }
        let (content_type, body) = multipart(input);
        let (_, json) = self.request(
            HttpMethod::Post,
            "/files",
            Some((body, &content_type)),
            "upload",
            signal,
        )?;
        let file = parse_file_object(&json, "upload").map_err(FilesError::from_llm)?;
        Ok(DeepSeekFileObjectWithExpiry {
            expires_at: file.created_at + input.expires_after_seconds,
            file,
        })
    }

    /// List one provider-ordered page of files (dsh `list`): pagination
    /// parameters and the null-cursor omission.
    ///
    /// # Errors
    ///
    /// Returns [`FilesError`] when the transport fails, the abort signal
    /// fires, the provider answers with a non-2xx status, or the page
    /// payload fails validation.
    pub fn list(
        &self,
        after: Option<&DeepSeekFileId>,
        limit: Option<u64>,
        signal: Option<&AbortSignal>,
    ) -> Result<DeepSeekFilePage, FilesError> {
        let mut path = String::from("/files?");
        if let Some(after) = after {
            let _ = write!(path, "after_id={}", encode_segment(after.as_str()));
        }
        if let Some(limit) = limit {
            if after.is_some() {
                path.push('&');
            }
            let _ = write!(path, "limit={limit}");
        }
        let (_, json) = self.request(HttpMethod::Get, &path, None, "list", signal)?;
        let page = json.as_object().ok_or_else(|| FilesError {
            error: Box::new(invalid_response("list")),
            detail: String::new(),
        })?;
        let rows = page.get("data").and_then(serde_json::Value::as_array);
        let Some(rows) = rows else {
            return Err(FilesError {
                error: Box::new(invalid_response("list")),
                detail: String::new(),
            });
        };
        let first_id = page.get("first_id").and_then(serde_json::Value::as_str);
        let last_id = page.get("last_id").and_then(serde_json::Value::as_str);
        let Some(has_more) = page.get("has_more").and_then(serde_json::Value::as_bool) else {
            return Err(FilesError {
                error: Box::new(invalid_response("list")),
                detail: String::new(),
            });
        };
        let mut data = Vec::with_capacity(rows.len());
        for row in rows {
            data.push(parse_file_object(row, "list").map_err(FilesError::from_llm)?);
        }
        Ok(DeepSeekFilePage {
            data,
            first_id: first_id.map(DeepSeekFileId::new),
            last_id: last_id.map(DeepSeekFileId::new),
            has_more,
        })
    }

    /// Retrieve one file object (dsh `retrieve`).
    ///
    /// # Errors
    ///
    /// Returns [`FilesError`] when the transport fails, the abort signal
    /// fires, the provider answers with a non-2xx status, or the payload
    /// fails validation.
    pub fn retrieve(
        &self,
        file_id: &DeepSeekFileId,
        signal: Option<&AbortSignal>,
    ) -> Result<DeepSeekFileObject, FilesError> {
        let (_, json) = self.request(
            HttpMethod::Get,
            &format!("/files/{}", encode_segment(file_id.as_str())),
            None,
            "retrieve",
            signal,
        )?;
        parse_file_object(&json, "retrieve").map_err(FilesError::from_llm)
    }

    /// Delete one provider file and validate the deletion receipt (dsh
    /// `delete`: the response must name the id and the `file_deleted`
    /// type).
    ///
    /// # Errors
    ///
    /// Returns [`FilesError`] when the transport fails, the abort signal
    /// fires, the provider answers with a non-2xx status, or the deletion
    /// receipt fails validation.
    pub fn delete(
        &self,
        file_id: &DeepSeekFileId,
        signal: Option<&AbortSignal>,
    ) -> Result<(), FilesError> {
        let (_, json) = self.request(
            HttpMethod::Delete,
            &format!("/files/{}", encode_segment(file_id.as_str())),
            None,
            "delete",
            signal,
        )?;
        let id_ok = json.get("id").and_then(serde_json::Value::as_str) == Some(file_id.as_str());
        let type_ok = json.get("type").and_then(serde_json::Value::as_str) == Some("file_deleted");
        if !id_ok || !type_ok {
            return Err(FilesError {
                error: Box::new(invalid_response("delete")),
                detail: String::new(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_net::http::TransportError;

    /// The provider timestamp shape parses to the epoch second dsh's
    /// `Date.parse` yields, including fractional seconds and offsets.
    #[test]
    fn parse_epoch_seconds_pins_provider_timestamps() {
        assert_eq!(
            parse_epoch_seconds("2026-05-01T12:00:00.123Z"),
            Some(1_777_636_800)
        );
        assert_eq!(
            parse_epoch_seconds("2026-05-01T12:00:00Z"),
            Some(1_777_636_800)
        );
        assert_eq!(
            parse_epoch_seconds("2026-05-01T14:30:00+02:30"),
            Some(1_777_636_800)
        );
        assert_eq!(
            parse_epoch_seconds("2026-05-01T09:30:00-02:30"),
            Some(1_777_636_800)
        );
        assert_eq!(parse_epoch_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_epoch_seconds("malformed"), None);
        assert_eq!(parse_epoch_seconds("2026-13-01T00:00:00Z"), None);
    }

    // -- behavior tests over an injected transport --

    struct RecordedFetch {
        status: u16,
        body: String,
        recorded: std::sync::Mutex<Vec<HttpRequest>>,
    }

    impl RecordedFetch {
        fn new(status: u16, body: &str) -> Self {
            Self {
                status,
                body: body.to_owned(),
                recorded: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<HttpRequest> {
            self.recorded.lock().expect("recorded lock").clone()
        }
    }

    impl Fetch for RecordedFetch {
        fn fetch(
            &self,
            request: HttpRequest,
        ) -> Result<fr_net::http::HttpResponse, TransportError> {
            self.recorded.lock().expect("recorded lock").push(request);
            Ok(fr_net::http::HttpResponse {
                status: self.status,
                headers: Vec::new(),
                body: Box::new(std::io::Cursor::new(self.body.clone().into_bytes())),
            })
        }
    }

    fn sample_file_json() -> String {
        concat!(
            "{\"id\":\"file_abc\",\"type\":\"file\",\"mime_type\":\"image/png\",",
            "\"size_bytes\":64,\"created_at\":\"2026-05-01T12:00:00.123Z\",",
            "\"filename\":\"dsh-x.png\"}"
        )
        .to_owned()
    }

    fn client(fetch: &RecordedFetch) -> DeepSeekFilesClient<'_> {
        DeepSeekFilesClient::new(FilesApiOptions {
            base_url: "https://api.deepseek.com/anthropic".to_owned(),
            headers: vec![("x-api-key".to_owned(), "k".to_owned())],
            fetch,
        })
    }

    fn upload_input() -> UploadInput {
        UploadInput {
            data: vec![1, 2, 3],
            media_type: ImageMediaType::Png,
            filename: "dsh-a.png".to_owned(),
            expires_after_seconds: 3_600,
        }
    }

    /// The upload walk pins the URL normalization, the dsh header order,
    /// the multipart field order, and the reuse-deadline arithmetic.
    #[test]
    fn upload_pins_request_shape_and_deadline() {
        let fetch = RecordedFetch::new(200, &sample_file_json());
        let client = client(&fetch);
        let uploaded = client.upload(&upload_input(), None).unwrap();
        assert_eq!(uploaded.file.id.as_str(), "file_abc");
        assert_eq!(uploaded.file.bytes, 64);
        assert_eq!(uploaded.file.created_at, 1_777_636_800);
        assert_eq!(uploaded.expires_at, 1_777_636_800 + 3_600);
        let request = &fetch.requests()[0];
        assert_eq!(request.method, HttpMethod::Post);
        assert_eq!(request.url, "https://api.deepseek.com/anthropic/v1/files");
        let names: Vec<&str> = request
            .headers
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "user-agent",
                "x-api-key",
                "anthropic-version",
                "anthropic-beta",
                "content-type",
            ]
        );
        assert_eq!(request.headers[1].1, "k");
        assert_eq!(request.headers[3].1, MESSAGES_FILES_BETA);
        let body = String::from_utf8(request.body.clone().expect("upload body")).expect("utf8");
        assert!(body.contains("name=\"expires_after[anchor]\"\r\n\r\ncreated_at\r\n"));
        assert!(body.contains("name=\"expires_after[seconds]\"\r\n\r\n3600\r\n"));
        assert!(
            body.contains("name=\"file\"; filename=\"dsh-a.png\"\r\nContent-Type: image/png\r\n")
        );
        assert!(body.ends_with("--\r\n"));
    }

    /// The upload prechecks reject before any I/O with the dsh texts.
    #[test]
    fn upload_prechecks_pin_dsh_texts() {
        let fetch = RecordedFetch::new(200, &sample_file_json());
        let client = client(&fetch);
        let oversized = UploadInput {
            data: vec![0; 129 * 1024 * 1024],
            ..upload_input()
        };
        assert_eq!(
            client.upload(&oversized, None).unwrap_err().error.message(),
            "DeepSeek Files API upload exceeds 128 MiB."
        );
        let short = UploadInput {
            expires_after_seconds: 3_599,
            ..upload_input()
        };
        assert_eq!(
            client.upload(&short, None).unwrap_err().error.message(),
            "DeepSeek file expiry must be between 3600 and 2592000 seconds."
        );
        assert!(fetch.requests().is_empty(), "prechecks run before I/O");
    }

    /// Non-2xx responses classify by status, keep the provider message,
    /// and quota wording enables the bounded recovery predicate.
    #[test]
    fn error_classification_pins_statuses_and_quota() {
        let fetch = RecordedFetch::new(
            401,
            "{\"error\":{\"type\":\"authentication_error\",\"message\":\"bad key\"}}",
        );
        let failure = client(&fetch).upload(&upload_input(), None).unwrap_err();
        assert_eq!(failure.error.message(), "bad key");
        assert_eq!(failure.error_code(), AUTH_CODE);

        let quota = RecordedFetch::new(400, "{\"error\":{\"message\":\"too many files stored\"}}");
        let failure = client(&quota).upload(&upload_input(), None).unwrap_err();
        assert_eq!(failure.error_code(), FILES_API_CODE);
        assert_eq!(failure.detail(), "too many files stored");
        assert!(is_files_quota_error(&failure));

        let rate = RecordedFetch::new(429, "{}");
        let failure = client(&rate).upload(&upload_input(), None).unwrap_err();
        assert_eq!(failure.error_code(), RATE_LIMIT_CODE);
        assert_eq!(
            failure.error.message(),
            "DeepSeek Files API error (HTTP 429)"
        );

        let server = RecordedFetch::new(503, "not json at all");
        let failure = client(&server).upload(&upload_input(), None).unwrap_err();
        assert_eq!(failure.error_code(), SERVER_CODE);
    }

    /// The list parse validates the page shape and every member; the
    /// delete receipt must name the id and the deletion type.
    #[test]
    fn list_and_delete_pin_receipt_validation() {
        let good = RecordedFetch::new(
            200,
            concat!(
                "{\"data\":[{\"id\":\"file_1\",\"type\":\"file\",\"mime_type\":\"image/png\",",
                "\"size_bytes\":1,\"created_at\":\"2026-05-01T12:00:00Z\",\"filename\":\"f.png\"}],",
                "\"first_id\":\"file_1\",\"has_more\":false}"
            ),
        );
        let page = client(&good)
            .list(Some(&DeepSeekFileId::new("cursor")), Some(100), None)
            .unwrap();
        assert_eq!(page.data.len(), 1);
        assert_eq!(
            page.first_id.as_ref().map(DeepSeekFileId::as_str),
            Some("file_1")
        );
        assert!(!page.has_more);
        assert!(
            good.requests()[0]
                .url
                .ends_with("/v1/files?after_id=cursor&limit=100")
        );

        let broken = RecordedFetch::new(200, "{\"data\":[],\"has_more\":\"yes\"}");
        let failure = client(&broken).list(None, None, None).unwrap_err();
        assert_eq!(
            failure.error.message(),
            "DeepSeek Files API returned an invalid list response."
        );

        let receipt = RecordedFetch::new(200, "{\"id\":\"file_1\",\"type\":\"file_deleted\"}");
        client(&receipt)
            .delete(&DeepSeekFileId::new("file_1"), None)
            .unwrap();
        assert!(receipt.requests()[0].url.ends_with("/v1/files/file_1"));
        let wrong = RecordedFetch::new(200, "{\"id\":\"other\",\"type\":\"file_deleted\"}");
        let failure = client(&wrong)
            .delete(&DeepSeekFileId::new("file_1"), None)
            .unwrap_err();
        assert_eq!(
            failure.error.message(),
            "DeepSeek Files API returned an invalid delete response."
        );
    }
}
