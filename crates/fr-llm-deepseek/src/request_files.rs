//! Shared Files resolution, bounded stale-id recovery, and
//! normalized-image diagnostics (dsh `request-files.ts`): the request-scoped
//! state around one Files store, including at most one stale-id retry.

use crate::file_id::DeepSeekFileId;
use crate::file_store::{DeepSeekFileStore, FileConnection, FileStoreError};
use crate::request_image::RequestImageAttachment;
use crate::types::FilePolicy;
use crate::upload_index::Invalidation;
use fr_llm::adapter::AbortSignal;
use fr_llm::error::{LlmError, TIMEOUT_CODE};
use fr_wire::ImageMediaType;
use regex::Regex;
use std::sync::OnceLock;

/// The capability-owned timeout code of one Files resolution (dsh
/// `deadline(..., 'DEEPSEEK_FILES_API_TIMEOUT')`).
pub const DEEPSEEK_FILES_API_TIMEOUT: &str = "DEEPSEEK_FILES_API_TIMEOUT";

/// Position of an image occurrence in the request's conversation messages
/// (dsh `ImageWireLocation`; both counts are one-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageWireLocation {
    /// One-based message position in the request.
    pub message: u64,
    /// One-based image position within the message.
    pub image: u64,
}

/// A file upload failure eligible for request-wide inline fallback (dsh
/// `FileResolutionFailure`).
///
/// The dsh class wraps any cause with a fixed message and rethrows raw
/// when the caller's signal already aborted; the enum keeps both arms so
/// the adapter can classify without inspecting signals again.
#[derive(Debug)]
pub enum FileResolutionFailure {
    /// The outer request signal aborted; the raw store failure propagates
    /// unwrapped (dsh rethrows the original error).
    Abort(FileStoreError),
    /// Any other failure, wrapped as the resolution failure.
    Other(FileStoreError),
}

impl FileResolutionFailure {
    /// The dsh message of the wrapper.
    pub const MESSAGE: &str = "DeepSeek Files API could not resolve a request image.";

    /// The fixed wrapper message.
    #[must_use]
    pub const fn message(&self) -> &'static str {
        Self::MESSAGE
    }

    /// The underlying failure.
    #[must_use]
    pub const fn cause(&self) -> &FileStoreError {
        match self {
            Self::Abort(error) | Self::Other(error) => error,
        }
    }

    /// Whether this failure is caller cancellation.
    #[must_use]
    pub const fn is_abort(&self) -> bool {
        matches!(self, Self::Abort(_))
    }
}

impl std::fmt::Display for FileResolutionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(Self::MESSAGE)
    }
}

impl std::error::Error for FileResolutionFailure {}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static classification regexes compile")
}

fn file_word() -> &'static Regex {
    static FILE: OnceLock<Regex> = OnceLock::new();
    FILE.get_or_init(|| re("(?i)(?-u:\\b)file(?:[_ -]?(?:id|api|not[_ -]?found|deleted|expired))?"))
}

fn missing_word() -> &'static Regex {
    static MISSING: OnceLock<Regex> = OnceLock::new();
    MISSING.get_or_init(|| {
        re("(?i)(?:expired|not[_ -]?found|deleted|do(?:es)? not exist|not created under (?:this|your) account)")
    })
}

fn invalid_id() -> &'static Regex {
    static INVALID: OnceLock<Regex> = OnceLock::new();
    INVALID.get_or_init(|| {
        re("(?i)(?:invalid.{0,20}file[_ -]?(?:id|api)|file[_ -]?(?:id|api).{0,20}invalid)")
    })
}

fn reason_before_image() -> &'static Regex {
    static REASON_FIRST: OnceLock<Regex> = OnceLock::new();
    REASON_FIRST.get_or_init(|| {
        re("(?i)(?:unsupported|invalid|cannot read|failed to (?:decode|process)).{0,40}image")
    })
}

fn image_before_reason() -> &'static Regex {
    static IMAGE_FIRST: OnceLock<Regex> = OnceLock::new();
    IMAGE_FIRST.get_or_init(|| re("(?i)image.{0,40}(?:unsupported|invalid|cannot be decoded)"))
}

fn boundary_character() -> &'static Regex {
    static BOUNDARY: OnceLock<Regex> = OnceLock::new();
    BOUNDARY.get_or_init(|| re("^[\u{5c}p{L}\u{5c}p{N}_-]$"))
}

/// Whether one provider error classifies as a rejected stale file id (dsh
/// `providerRejectedFileId`): the `file` word plus a missing/invalid
/// reason.
#[must_use]
pub fn provider_rejected_file_id(detail: &str) -> bool {
    file_word().is_match(detail)
        && (missing_word().is_match(detail) || invalid_id().is_match(detail))
}

/// Whether one provider error describes the image bytes being rejected
/// (dsh `providerRejectedNormalizedImage`).
#[must_use]
pub fn provider_rejected_normalized_image(detail: &str) -> bool {
    reason_before_image().is_match(detail) || image_before_reason().is_match(detail)
}

/// Whether the detail names one exact file id as a standalone token (dsh
/// `detailNamesFileId`): every occurrence is checked; both neighbors must
/// sit outside `[\p{L}\p{N}_-]`.
#[must_use]
pub fn detail_names_file_id(detail: &str, file_id: &str) -> bool {
    for (index, _) in detail.match_indices(file_id) {
        let before = detail[..index]
            .chars()
            .next_back()
            .is_some_and(|character| boundary_character().is_match(&character.to_string()));
        let after = detail[index + file_id.len()..]
            .chars()
            .next()
            .is_some_and(|character| boundary_character().is_match(&character.to_string()));
        if !before && !after {
            return true;
        }
    }
    false
}

/// The rendered version facts `normalizedImageFacts` reads (dsh reads them
/// off the stored `RequestImageAttachment`; fr keeps the rendered facts so
/// multi-megabyte request bytes are not cloned per occurrence).
#[derive(Debug, Clone)]
pub struct ImageFacts {
    /// Optional display name of the source attachment.
    pub name: Option<String>,
    /// Durable attachment identity.
    pub attachment_id: fr_wire::AttachmentId,
    /// Encoded media type.
    pub media_type: ImageMediaType,
    /// Whether the request version keeps an alpha channel.
    pub has_alpha: bool,
    /// Request width in pixels.
    pub width: u32,
    /// Request height in pixels.
    pub height: u32,
}

/// One used request file beside the facts its diagnostics need (dsh
/// `UsedRequestFile` holds the whole version object).
#[derive(Debug, Clone)]
struct UsedRequestFile {
    variant_id: String,
    file_id: DeepSeekFileId,
    location: ImageWireLocation,
    facts: ImageFacts,
}

impl UsedRequestFile {
    /// Capture the diagnostic facts of one resolved occurrence.
    fn capture(
        version: &RequestImageAttachment,
        file_id: DeepSeekFileId,
        location: ImageWireLocation,
    ) -> Self {
        Self {
            variant_id: version.variant_id.clone(),
            file_id,
            location,
            facts: ImageFacts {
                name: version.attachment.name.clone(),
                attachment_id: version.attachment.attachment_id.clone(),
                media_type: version.media_type,
                has_alpha: version.has_alpha,
                width: version.width,
                height: version.height,
            },
        }
    }

    /// The dsh dedupe key of one occurrence.
    fn mapping_key(&self) -> String {
        format!("{}\0{}", self.variant_id, self.file_id.as_str())
    }

    /// The dsh candidate dedupe key of one occurrence.
    fn candidate_key(&self) -> String {
        format!(
            "{}\0{}\0{}",
            self.variant_id, self.location.message, self.location.image
        )
    }
}

/// The dsh `staleMappings` walk: dedupe by variant+file, prefer the exact
/// ids the detail names, fall back to every unique mapping.
fn stale_mappings(used: &[UsedRequestFile], detail: &str) -> Vec<UsedRequestFile> {
    let mut unique: Vec<UsedRequestFile> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for file in used {
        let key = file.mapping_key();
        if !keys.contains(&key) {
            keys.push(key);
            unique.push(file.clone());
        }
    }
    let exact: Vec<UsedRequestFile> = unique
        .iter()
        .filter(|file| detail_names_file_id(detail, file.file_id.as_str()))
        .cloned()
        .collect();
    if exact.is_empty() { unique } else { exact }
}

const fn media_type_value(media_type: ImageMediaType) -> &'static str {
    match media_type {
        ImageMediaType::Png => "image/png",
        ImageMediaType::Jpeg => "image/jpeg",
        ImageMediaType::Webp => "image/webp",
        ImageMediaType::Gif => "image/gif",
    }
}

/// The rendered facts of one occurrence (dsh `normalizedImageFacts`).
fn normalized_image_facts(file: &UsedRequestFile) -> String {
    let name = file
        .facts
        .name
        .clone()
        .unwrap_or_else(|| file.facts.attachment_id.to_string());
    let colour = if file.facts.has_alpha {
        "sRGBA"
    } else {
        "sRGB"
    };
    format!(
        "\"{name}\" at message {}, image {} ({}, 8-bit {colour}, {}x{})",
        file.location.message,
        file.location.image,
        media_type_value(file.facts.media_type),
        file.facts.width,
        file.facts.height,
    )
}

/// The fixed tail of both normalized-image diagnostics (dsh concatenates
/// it onto either arm).
const DIAGNOSTIC_SUFFIX: &str = "The provider rejected bytes already normalized by the harness; PNG, JPEG, WebP, and GIF remain supported input formats.";

/// Attribute a normalized-image rejection to the actual uploaded
/// occurrences (dsh `normalizedImageDiagnostic`): the exact-named target,
/// else a single candidate, else the full deduplicated candidate list.
fn normalized_image_diagnostic(
    used: &[UsedRequestFile],
    provider_message: &str,
    provider_detail: &str,
) -> String {
    let exact = used
        .iter()
        .find(|file| detail_names_file_id(provider_detail, file.file_id.as_str()));
    let target = exact.or_else(|| if used.len() == 1 { used.first() } else { None });
    if let Some(target) = target {
        return format!(
            "DeepSeek rejected normalized image {}: {provider_message}. {DIAGNOSTIC_SUFFIX}",
            normalized_image_facts(target)
        );
    }
    let mut candidates: Vec<String> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for file in used {
        let key = file.candidate_key();
        if !keys.contains(&key) {
            keys.push(key);
            candidates.push(normalized_image_facts(file));
        }
    }
    format!(
        "DeepSeek rejected a normalized request image: {provider_message}. Candidate images: {}. {DIAGNOSTIC_SUFFIX}",
        candidates.join("; ")
    )
}

/// Files state owned by one model request, including at most one stale-id
/// retry (dsh `RequestFiles`).
pub struct RequestFiles<'a> {
    files: &'a DeepSeekFileStore<'a>,
    connection: &'a FileConnection,
    policy: &'a FilePolicy,
    timeout_ms: f64,
    signal: Option<&'a AbortSignal>,
    activity: &'a (dyn Fn() + Sync),
    used: Vec<UsedRequestFile>,
    retried: bool,
}

impl std::fmt::Debug for RequestFiles<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestFiles")
            .field("used", &self.used.len())
            .field("retried", &self.retried)
            .finish_non_exhaustive()
    }
}

impl<'a> RequestFiles<'a> {
    /// Bind one request's Files state (dsh constructor).
    #[must_use]
    pub fn new(
        files: &'a DeepSeekFileStore<'a>,
        connection: &'a FileConnection,
        policy: &'a FilePolicy,
        timeout_ms: f64,
        signal: Option<&'a AbortSignal>,
        activity: &'a (dyn Fn() + Sync),
    ) -> Self {
        Self {
            files,
            connection,
            policy,
            timeout_ms,
            signal,
            activity,
            used: Vec::new(),
            retried: false,
        }
    }

    /// Reset occurrence tracking before serializing the next HTTP attempt
    /// (dsh `beginAttempt`; the once-only retry gate persists).
    pub fn begin_attempt(&mut self) {
        self.used.clear();
    }

    /// Resolve a retained image under its own upload deadline (dsh
    /// `resolve`).
    ///
    /// # Errors
    /// [`FileResolutionFailure`] for any store failure; the abort arm
    /// carries the raw cause when the outer signal already aborted.
    ///
    /// # Panics
    /// Only if the files-API timeout fails deadline validation, which the
    /// resolved connection bounds.
    pub fn resolve(
        &mut self,
        version: &RequestImageAttachment,
        location: ImageWireLocation,
    ) -> Result<DeepSeekFileId, FileResolutionFailure> {
        // Blocking-face mapping of dsh `deadline(...)`: the fused deadline
        // is classified around the (bounded, single-round-trip) store
        // call; the outer abort propagates through the store's own signal
        // checks.
        let limit = fr_timeout::deadline(None, self.timeout_ms, DEEPSEEK_FILES_API_TIMEOUT)
            .expect("files API timeout is validated at resolve time");
        let result = self
            .files
            .ensure_uploaded(version, self.connection, self.policy, self.signal);
        match result {
            Ok(reference) => {
                (self.activity)();
                self.used.push(UsedRequestFile::capture(
                    version,
                    reference.record.file_id.clone(),
                    location,
                ));
                Ok(reference.record.file_id)
            }
            Err(error) => {
                if let Some(signal) = self.signal
                    && signal.is_aborted()
                {
                    return Err(FileResolutionFailure::Abort(error));
                }
                // The blocking deadline fires only when observed.
                let _ = limit.check();
                if let Some(reason) = fr_timeout::timeout_of(
                    limit.signal().reason(),
                    Some(DEEPSEEK_FILES_API_TIMEOUT),
                ) {
                    // The deadline elapsed during the failed call; dsh's
                    // fused signal aborts the transport and the timeout
                    // reason becomes the cause chain head.
                    return Err(FileResolutionFailure::Other(FileStoreError::Llm(
                        LlmError::try_new(
                            &reason.to_string(),
                            TIMEOUT_CODE,
                            None,
                            None,
                            None,
                            None,
                        )
                        .expect("timeout fields are static"),
                    )));
                }
                Err(FileResolutionFailure::Other(error))
            }
        }
    }

    /// Invalidate every rejected mapping in one index update; only the
    /// first stale-id response permits another request (dsh `retry`).
    ///
    /// # Errors
    /// A local index persistence failure during invalidation.
    pub fn retry(&mut self, detail: &str) -> Result<bool, FileStoreError> {
        if self.used.is_empty() || !provider_rejected_file_id(detail) {
            return Ok(false);
        }
        let generations: Vec<Invalidation> = stale_mappings(&self.used, detail)
            .into_iter()
            .map(|file| Invalidation {
                variant_id: file.variant_id,
                file_id: file.file_id,
            })
            .collect();
        self.files.invalidate(&generations, self.connection)?;
        if self.retried {
            return Ok(false);
        }
        self.retried = true;
        Ok(true)
    }

    /// Attribute a normalized-image rejection to the actual uploaded image
    /// occurrences (dsh `errorMessage`); any other shape returns the
    /// provider message unchanged.
    #[must_use]
    pub fn error_message(&self, status: u16, message: &str, detail: &str) -> String {
        if status == 400 && !self.used.is_empty() && provider_rejected_normalized_image(detail) {
            normalized_image_diagnostic(&self.used, message, detail)
        } else {
            message.to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_store::FileStoreOptions;
    use crate::http::HttpRequest;
    use crate::upload_index::DeepSeekUploadIndex;
    use fr_llm::adapter::AbortSignal as AdapterSignal;
    use fr_wire::ids::AttachmentId;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    fn hex_string(n: usize, byte: u8) -> String {
        let pattern = format!("{byte:02x}");
        pattern.repeat(n.div_ceil(2)).chars().take(n).collect()
    }

    fn version(variant_byte: u8) -> RequestImageAttachment {
        let variant = hex_string(64, variant_byte);
        RequestImageAttachment {
            variant_id: format!("sha256:{variant}"),
            attachment: fr_wire::ImageAttachmentRef {
                attachment_id: AttachmentId::new(format!("sha256:{}", hex_string(64, 0x42))),
                media_type: ImageMediaType::Png,
                bytes: 64,
                width: 16,
                height: 16,
                name: None,
                original_dimensions: None,
            },
            data: vec![9u8; 64],
            media_type: ImageMediaType::Png,
            bytes: 64,
            width: 16,
            height: 16,
            depth: crate::request_image::ImageSampleDepth::Uchar,
            space: crate::request_image::ImageColorSpace::Srgb,
            has_alpha: false,
        }
    }

    fn connection() -> FileConnection {
        FileConnection {
            base_url: "https://api.deepseek.com/anthropic".to_owned(),
            headers: vec![("x-api-key".to_owned(), "k".to_owned())],
        }
    }

    fn policy() -> FilePolicy {
        FilePolicy {
            expires_after_seconds: 3_600,
            refresh_margin_seconds: 60,
            quota_cleanup_batch: 10,
        }
    }

    /// Files test double: uploads echo the submitted byte length; the first
    /// `fail_uploads` uploads answer `failure_status`; every call sleeps
    /// `sleep_ms` first.
    struct RecordingFetch {
        fail_uploads: AtomicUsize,
        failure_status: u16,
        sleep_ms: u64,
        recorded: Mutex<Vec<String>>,
    }

    impl RecordingFetch {
        fn new(fail_uploads: usize, failure_status: u16, sleep_ms: u64) -> Self {
            Self {
                fail_uploads: AtomicUsize::new(fail_uploads),
                failure_status,
                sleep_ms,
                recorded: Mutex::new(Vec::new()),
            }
        }

        /// Decrement only when positive, so the counter never wraps.
        fn take_failure(counter: &AtomicUsize) -> bool {
            loop {
                let current = counter.load(AtomicOrdering::SeqCst);
                if current == 0 {
                    return false;
                }
                if counter
                    .compare_exchange(
                        current,
                        current - 1,
                        AtomicOrdering::SeqCst,
                        AtomicOrdering::SeqCst,
                    )
                    .is_ok()
                {
                    return true;
                }
            }
        }

        fn deletes(&self) -> Vec<String> {
            self.recorded
                .lock()
                .expect("recorded")
                .iter()
                .filter(|entry| entry.starts_with("DELETE"))
                .cloned()
                .collect()
        }
    }

    impl crate::http::Fetch for RecordingFetch {
        fn fetch(
            &self,
            request: HttpRequest,
        ) -> Result<crate::http::HttpResponse, crate::http::TransportError> {
            if self.sleep_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(self.sleep_ms));
            }
            let send = |status: u16, body: String| {
                Ok(crate::http::HttpResponse {
                    status,
                    headers: Vec::new(),
                    body: Box::new(std::io::Cursor::new(body.into_bytes())),
                })
            };
            if request.url.ends_with("/v1/files")
                && matches!(request.method, crate::http::HttpMethod::Post)
            {
                self.recorded
                    .lock()
                    .expect("recorded")
                    .push(format!("POST {}", request.url));
                if Self::take_failure(&self.fail_uploads) {
                    return send(self.failure_status, "{}".to_owned());
                }
                let body = request.body.expect("upload body");
                let data_start = body
                    .windows(4)
                    .rposition(|window| window == b"\r\n\r\n")
                    .expect("multipart file part")
                    + 4;
                let data_end = body[data_start..]
                    .windows(4)
                    .position(|window| window == b"\r\n--")
                    .expect("multipart terminator")
                    + data_start;
                return send(
                    200,
                    format!(
                        "{{\"id\":\"file-up\",\"type\":\"file\",\"mime_type\":\"image/png\",\
                         \"size_bytes\":{},\"created_at\":\"2026-05-01T12:00:00Z\",\
                         \"filename\":\"dsh-up.png\"}}",
                        data_end - data_start
                    ),
                );
            }
            if matches!(request.method, crate::http::HttpMethod::Delete) {
                let id = request.url.rsplit('/').next().unwrap_or_default();
                self.recorded
                    .lock()
                    .expect("recorded")
                    .push(format!("DELETE {id}"));
                return send(
                    200,
                    format!("{{\"id\":\"{id}\",\"type\":\"file_deleted\"}}"),
                );
            }
            send(200, "{\"data\":[],\"has_more\":false}".to_owned())
        }
    }

    /// Leak-bound construction keeps the self-referential borrows of one
    /// request scope alive for the test's lifetime.
    fn request_files<'a>(
        root: &std::path::Path,
        fetch: &'a RecordingFetch,
        timeout_ms: f64,
        signal: Option<&'a AdapterSignal>,
        activity: &'a (dyn Fn() + Sync),
    ) -> RequestFiles<'a> {
        let store: &'a DeepSeekFileStore<'a> =
            Box::leak(Box::new(DeepSeekFileStore::new(FileStoreOptions {
                index: DeepSeekUploadIndex::new(root.join("files-v3.json")),
                now: &|| 1_000,
                fetch,
            })));
        let connection: &'a FileConnection = Box::leak(Box::new(connection()));
        let policy: &'a FilePolicy = Box::leak(Box::new(policy()));
        RequestFiles::new(store, connection, policy, timeout_ms, signal, activity)
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "fr-request-files-{tag}-{}-{}",
            std::process::id(),
            crate::request_image_pipeline::random_hex_suffix()
        ));
        std::fs::create_dir_all(&directory).expect("scratch dir");
        directory
    }

    /// The three regex families pin the dsh arms: file word plus
    /// missing/invalid reason, the normalized-image reason ordering, and
    /// the standalone-token boundary check over every occurrence.
    #[test]
    fn classification_regexes_pin_dsh_families() {
        assert!(provider_rejected_file_id("File file-123 not found"));
        assert!(provider_rejected_file_id("the file_id has expired"));
        assert!(provider_rejected_file_id("invalid file id provided"));
        assert!(provider_rejected_file_id("file-api is invalid here"));
        assert!(provider_rejected_file_id("file does not exist"));
        assert!(provider_rejected_file_id(
            "the file was not created under your account"
        ));
        // A word boundary keeps `profile` out of the file family.
        assert!(!provider_rejected_file_id("profile updated"));
        assert!(!provider_rejected_file_id("the file was uploaded"));
        assert!(!provider_rejected_file_id("expired session token"));

        assert!(provider_rejected_normalized_image(
            "failed to decode image bytes"
        ));
        assert!(provider_rejected_normalized_image(
            "image content is unsupported by this route"
        ));
        assert!(provider_rejected_normalized_image(
            "cannot read the image payload"
        ));
        assert!(!provider_rejected_normalized_image(
            "image saved successfully"
        ));

        assert!(detail_names_file_id(
            "cannot read file-abc: bad request",
            "file-abc"
        ));
        assert!(!detail_names_file_id(
            "cannot read file-abcdef: bad",
            "file-abc"
        ));
        assert!(!detail_names_file_id("file-abc1 is bad", "file-abc"));
        // The first occurrence is embedded, the second standalone; a
        // hyphen neighbor counts as attached (it is in the class).
        assert!(detail_names_file_id("file-abc1 file-abc,", "file-abc"));
        assert!(!detail_names_file_id("file-abc-and-file-abc", "file-abc"));
        assert!(!detail_names_file_id("", "file-abc"));
    }

    /// Resolve records successful occurrences and the activity pulse; the
    /// exact-target diagnostic arm names the resolved occurrence, and
    /// beginAttempt resets the tracking.
    #[test]
    fn resolve_pins_success_and_exact_diagnostic() {
        let root = scratch("resolve");
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&calls);
        let activity = move || {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
        };
        let ok_fetch = RecordingFetch::new(0, 0, 0);
        let mut files = request_files(&root, &ok_fetch, 5_000.0, None, &activity);
        let file_id = files
            .resolve(
                &version(1),
                ImageWireLocation {
                    message: 1,
                    image: 1,
                },
            )
            .unwrap();
        assert_eq!(file_id.as_str(), "file-up");
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

        let diagnostic =
            files.error_message(400, "provider said no", "cannot read file-up: bad image");
        assert_eq!(
            diagnostic,
            "DeepSeek rejected normalized image \"sha256:4242424242424242424242424242424242424242424242424242424242424242\" at message 1, image 1 (image/png, 8-bit sRGB, 16x16): provider said no. The provider rejected bytes already normalized by the harness; PNG, JPEG, WebP, and GIF remain supported input formats."
        );
        files.begin_attempt();
        assert_eq!(
            files.error_message(400, "provider said no", "cannot read file-up"),
            "provider said no"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The failure arms: a plain store failure wraps with the dsh message,
    /// an aborted caller rethrows raw, and an elapsed deadline replaces
    /// the cause with the dsh timeout reason.
    #[test]
    fn resolve_failure_arms_pin_wrapper_and_deadline() {
        let root = scratch("resolve-fail");
        let activity = || {};
        let failing = RecordingFetch::new(usize::MAX, 503, 0);
        let mut files = request_files(&root, &failing, 5_000.0, None, &activity);
        let failure = files
            .resolve(
                &version(2),
                ImageWireLocation {
                    message: 1,
                    image: 1,
                },
            )
            .unwrap_err();
        assert_eq!(failure.message(), FileResolutionFailure::MESSAGE);
        assert!(!failure.is_abort());
        assert_eq!(failure.cause().code(), fr_llm::error::SERVER_CODE);

        // Outer abort: the raw cause rethrows as the abort arm.
        let signal = AdapterSignal::new();
        signal.abort();
        let mut aborted = request_files(&root, &failing, 5_000.0, Some(&signal), &activity);
        let failure = aborted
            .resolve(
                &version(3),
                ImageWireLocation {
                    message: 1,
                    image: 1,
                },
            )
            .unwrap_err();
        assert!(failure.is_abort());

        // Deadline elapsed during a failed call: timeout cause.
        let slow = RecordingFetch::new(usize::MAX, 503, 30);
        let mut timed_out = request_files(&root, &slow, 1.0, None, &activity);
        let failure = timed_out
            .resolve(
                &version(4),
                ImageWireLocation {
                    message: 1,
                    image: 1,
                },
            )
            .unwrap_err();
        assert!(!failure.is_abort());
        assert_eq!(failure.cause().code(), fr_llm::error::TIMEOUT_CODE);
        assert_eq!(
            failure.cause().message(),
            "DEEPSEEK_FILES_API_TIMEOUT after 1ms"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// retry classifies stale-id details, invalidates the used mappings in
    /// the index, and permits only one follow-up request per `RequestFiles`
    /// lifetime.
    #[test]
    fn retry_pins_stale_invalidation_and_once_gate() {
        let root = scratch("retry");
        let fetch = RecordingFetch::new(0, 0, 0);
        let activity = || {};
        let mut files = request_files(&root, &fetch, 5_000.0, None, &activity);
        // No usage yet: retry refuses.
        assert!(!files.retry("File file-up not found").unwrap());
        files
            .resolve(
                &version(5),
                ImageWireLocation {
                    message: 1,
                    image: 1,
                },
            )
            .unwrap();

        // A detail the regex does not classify: no invalidation.
        assert!(!files.retry("server exploded").unwrap());
        // A stale-id detail: invalidates and grants the one retry.
        assert!(files.retry("File file-up not found").unwrap());
        assert_eq!(fetch.deletes(), Vec::<String>::new());
        // The index no longer resolves the mapping...
        let scope = connection().scope();
        let index = DeepSeekUploadIndex::new(root.join("files-v3.json"));
        assert!(
            index
                .get(&scope, &version(5).variant_id, 1_000, 0)
                .unwrap()
                .is_none()
        );
        // ...and the second stale response is refused.
        assert!(!files.retry("File file-up not found").unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// errorMessage pins both diagnostic arms and every passthrough arm.
    #[test]
    fn error_message_pins_diagnostic_and_passthrough_arms() {
        let root = scratch("message");
        let activity = || {};
        let fetch = RecordingFetch::new(0, 0, 0);
        let mut files = request_files(&root, &fetch, 5_000.0, None, &activity);
        // Single candidate arm: 400 + used + normalized-image detail, but
        // the detail names no file id.
        files
            .resolve(
                &version(6),
                ImageWireLocation {
                    message: 2,
                    image: 3,
                },
            )
            .unwrap();
        let message = files.error_message(400, "bad image", "the image is unsupported");
        assert_eq!(
            message,
            "DeepSeek rejected normalized image \"sha256:4242424242424242424242424242424242424242424242424242424242424242\" at message 2, image 3 (image/png, 8-bit sRGB, 16x16): bad image. The provider rejected bytes already normalized by the harness; PNG, JPEG, WebP, and GIF remain supported input formats."
        );
        // Two distinct candidates: the candidates arm.
        let second = scratch("message-second");
        let mut files_two = request_files(&second, &fetch, 5_000.0, None, &activity);
        files_two
            .resolve(
                &version(7),
                ImageWireLocation {
                    message: 1,
                    image: 1,
                },
            )
            .unwrap();
        files_two
            .resolve(
                &version(8),
                ImageWireLocation {
                    message: 1,
                    image: 2,
                },
            )
            .unwrap();
        let message = files_two.error_message(400, "bad image", "the image is unsupported");
        assert!(message.starts_with(
            "DeepSeek rejected a normalized request image: bad image. Candidate images: "
        ));
        assert!(message.contains("\"sha256:4242"));
        assert!(message.contains("at message 1, image 1"));
        assert!(message.contains("at message 1, image 2"));
        assert!(message.ends_with(
            "The provider rejected bytes already normalized by the harness; PNG, JPEG, WebP, and GIF remain supported input formats."
        ));

        // Passthrough arms: non-400, or no normalized-image wording.
        assert_eq!(
            files.error_message(500, "boom", "image unsupported"),
            "boom"
        );
        assert_eq!(files.error_message(400, "boom", "quota exceeded"), "boom");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&second);
    }
}

// Rust guideline compliant 2026-10-26
