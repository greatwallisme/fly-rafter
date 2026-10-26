//! Files upload reuse, invalidation, and quota recovery (dsh
//! `file-store.ts`): the user-scoped durable file-id store keyed by the
//! route namespace and the deterministic request-image variant.

use crate::file_id::{DeepSeekFileId, DeepSeekFileScope};
use crate::files_api::{
    DeepSeekFilesClient, FilesApiOptions, FilesError, UploadInput, is_files_quota_error,
};
use crate::http::Fetch;
use crate::messages_api::messages_api_root;
use crate::request_image::RequestImageAttachment;
use crate::request_image_pipeline::ABORTED_CODE;
use crate::types::FilePolicy;
use crate::upload_index::{
    DeepSeekUploadIndex, DeepSeekUploadRecord, Invalidation, deep_seek_file_scope,
};
use fr_llm::adapter::AbortSignal;
use fr_llm::error::LlmError;
use fr_wire::ImageMediaType;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// The `dsh-` prefix every harness-owned upload carries (dsh
/// `OWNED_FILE_PREFIX`, file-store.ts:13; verbatim per PRD decision 37-5).
pub const OWNED_FILE_PREFIX: &str = "dsh-";

/// Shared Files-store limit for each request image, including file-id
/// references (dsh `MAX_IMAGE_BYTES`).
pub const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// The namespace facts one file operation needs (dsh
/// `DeepSeekFileConnection`).
#[derive(Debug, Clone)]
pub struct FileConnection {
    /// Messages API root (any spelling; normalized by the scope and the
    /// client).
    pub base_url: String,
    /// Provider-resolved authentication headers for this endpoint.
    pub headers: Vec<(String, String)>,
}

impl FileConnection {
    /// The upload namespace identity: the Files resource's parent URL and
    /// the sorted credential header serialization (dsh `fileScope`).
    #[must_use]
    pub fn scope(&self) -> DeepSeekFileScope {
        let mut sorted: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        // dsh sorts entries by key with localeCompare.
        sorted.sort_by(|left, right| left.0.cmp(right.0));
        let serialized = serde_json::to_string(&sorted).unwrap_or_default();
        deep_seek_file_scope(&messages_api_root(&self.base_url), &serialized)
    }
}

/// One in-flight upload shared by concurrent waiters (dsh `SharedUpload`):
/// the transport runs once while every waiter keeps an independent wait.
///
/// Blocking-face mapping of the dsh promise dance: the initiating caller
/// runs the transport inline while joiners block on the condition; a
/// waiter whose own signal aborts releases immediately and, as the last
/// outstanding waiter of an unsettled upload, marks the run abandoned so
/// the runner stops before its next phase (dsh aborts the shared
/// controller at the same point).
struct SharedUpload {
    state: Mutex<SharedState>,
    progress: Condvar,
}

#[derive(Default)]
struct SharedState {
    waiters: usize,
    settled: bool,
    abandoned: bool,
    result: Option<Result<FileReference, FileStoreError>>,
}

impl SharedUpload {
    fn new() -> Self {
        Self {
            state: Mutex::new(SharedState::default()),
            progress: Condvar::new(),
        }
    }

    fn abandoned(&self) -> bool {
        self.state.lock().is_ok_and(|state| state.abandoned)
    }
}

fn store_abort() -> FileStoreError {
    FileStoreError::Llm(
        LlmError::try_new(
            "The request was aborted.",
            ABORTED_CODE,
            None,
            None,
            None,
            None,
        )
        .expect("abort fields are static"),
    )
}

/// Drain one joined shared upload: return as soon as it settles, or
/// immediately with the caller's abort reason, marking the run abandoned
/// when the aborting waiter is the last one outstanding (dsh
/// `waitForUpload`).
fn wait_for_shared(
    shared: &SharedUpload,
    mut state: MutexGuard<'_, SharedState>,
    signal: Option<&AbortSignal>,
) -> Result<FileReference, FileStoreError> {
    loop {
        if state.settled {
            state.waiters -= 1;
            return state
                .result
                .clone()
                .expect("a settled shared upload always carries its result");
        }
        if let Some(signal) = signal
            && signal.is_aborted()
        {
            state.waiters -= 1;
            if state.waiters == 0 {
                state.abandoned = true;
            }
            return Err(store_abort());
        }
        state = shared.progress.wait(state).expect("shared upload state");
    }
}

/// One settled file reference (dsh `DeepSeekFileReference`): the record
/// committed to the index and whether this call published the upload.
#[derive(Debug, Clone)]
pub struct FileReference {
    /// The durable index record.
    pub record: DeepSeekUploadRecord,
    /// Whether this call published the new upload.
    pub uploaded: bool,
}

/// Any file-store failure: a provider-neutral LLM failure, an
/// attachment-pipeline failure, or a local index persistence failure.
///
/// The index variant carries only the rendered message: dsh propagates the
/// raw Node error with no stable code there, so no fabricated code exists
/// to port.
#[derive(Debug, Clone)]
pub enum FileStoreError {
    /// A provider-neutral LLM failure.
    Llm(LlmError),
    /// An attachment-pipeline failure.
    Attachment(crate::request_image_pipeline::AttachmentError),
    /// A local index read/lock/write failure.
    Index {
        /// The rendered failure text.
        message: String,
    },
}

impl From<LlmError> for FileStoreError {
    fn from(error: LlmError) -> Self {
        Self::Llm(error)
    }
}

impl From<crate::request_image_pipeline::AttachmentError> for FileStoreError {
    fn from(error: crate::request_image_pipeline::AttachmentError) -> Self {
        Self::Attachment(error)
    }
}

impl From<FilesError> for FileStoreError {
    fn from(error: FilesError) -> Self {
        Self::Llm(*error.error)
    }
}

impl From<std::io::Error> for FileStoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Index {
            message: error.to_string(),
        }
    }
}

/// The user-facing view of a store failure (the LLM failure itself, or the
/// attachment failure's message and code carried in an LLM-shaped error;
/// dsh throws either the raw `LlmError` or the `AttachmentError` which the
/// adapter wraps as `FileResolutionFailure`).
impl FileStoreError {
    /// The user-readable failure text.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Llm(error) => error.message(),
            Self::Attachment(error) => error.message(),
            Self::Index { message } => message,
        }
    }

    /// The stable machine-routing code.
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::Llm(error) => error.code(),
            Self::Attachment(error) => error.code(),
            Self::Index { .. } => "",
        }
    }

    /// Whether the failure is caller cancellation.
    #[must_use]
    pub fn is_abort(&self) -> bool {
        match self {
            Self::Llm(error) => error.code() == ABORTED_CODE,
            Self::Attachment(error) => error.code() == ABORTED_CODE,
            Self::Index { .. } => false,
        }
    }
}

/// One upload attempt failure beside its quota-recovery eligibility (dsh:
/// quota recovery applies only to `DeepSeekFilesError` instances — a
/// plain `LlmError` such as the bytes-echo mismatch is never recoverable).
enum UploadAttemptError {
    /// A status-classified Files API failure.
    Files(FilesError),
    /// The upload succeeded but the echoed size did not match.
    EchoMismatch(LlmError),
}

impl From<FilesError> for UploadAttemptError {
    fn from(error: FilesError) -> Self {
        Self::Files(error)
    }
}

impl UploadAttemptError {
    fn into_store(self) -> FileStoreError {
        match self {
            Self::Files(error) => error.into(),
            Self::EchoMismatch(error) => error.into(),
        }
    }
}

const fn extension(media_type: ImageMediaType) -> &'static str {
    match media_type {
        ImageMediaType::Png => "png",
        ImageMediaType::Jpeg => "jpeg",
        ImageMediaType::Webp => "webp",
        ImageMediaType::Gif => "gif",
    }
}

/// The provider-side name of one upload (dsh `filename`, file-store.ts
/// :114-118): the `dsh-` prefix, the first 16 hex characters after the
/// attachment id's `sha256:` prefix, the first 8 of the variant id, and
/// the media-type extension.
pub(crate) fn filename(version: &RequestImageAttachment) -> String {
    let attachment: String = version
        .attachment
        .attachment_id
        .as_str()
        .chars()
        .skip("sha256:".len())
        .take(16)
        .collect();
    let variant: String = version
        .variant_id
        .chars()
        .skip("sha256:".len())
        .take(8)
        .collect();
    format!(
        "{OWNED_FILE_PREFIX}{attachment}-{variant}.{}",
        extension(version.media_type)
    )
}

/// Construction boundaries of the file store (dsh `FileStoreOptions`).
pub struct FileStoreOptions<'a> {
    /// Durable upload index.
    pub index: DeepSeekUploadIndex,
    /// Current Unix time in milliseconds (dsh `options.now`).
    pub now: &'a (dyn Fn() -> u64 + Sync + 'a),
    /// The transport seam (dsh `options.fetch`; tests inject).
    pub fetch: &'a dyn Fetch,
}

/// User-scoped durable file-id reuse for the `DeepSeek` route (dsh
/// `DeepSeekFileStore`).
pub struct DeepSeekFileStore<'a> {
    index: DeepSeekUploadIndex,
    now: &'a (dyn Fn() -> u64 + Sync + 'a),
    fetch: &'a dyn Fetch,
    inflight: Mutex<HashMap<String, Arc<SharedUpload>>>,
}

impl std::fmt::Debug for FileStoreOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStoreOptions").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for DeepSeekFileStore<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeepSeekFileStore")
            .field("index", &self.index)
            .finish_non_exhaustive()
    }
}

impl<'a> DeepSeekFileStore<'a> {
    /// Bind the index, clock, and transport boundaries.
    #[must_use]
    pub fn new(options: FileStoreOptions<'a>) -> Self {
        Self {
            index: options.index,
            now: options.now,
            fetch: options.fetch,
            inflight: Mutex::new(HashMap::new()),
        }
    }

    fn client(&self, connection: &FileConnection) -> DeepSeekFilesClient<'_> {
        DeepSeekFilesClient::new(FilesApiOptions {
            base_url: connection.base_url.clone(),
            headers: connection.headers.clone(),
            fetch: self.fetch,
        })
    }

    /// Resolve or upload one deterministic request image (dsh
    /// `ensureUploaded`). Concurrent calls share one transport run while
    /// retaining independent waits.
    ///
    /// # Errors
    /// The 32 MiB precheck, any Files upload failure, or a local index
    /// persistence failure.
    ///
    /// # Panics
    /// Only if an internal shared-upload mutex is poisoned by a panic in
    /// another waiter thread.
    pub fn ensure_uploaded(
        &self,
        version: &RequestImageAttachment,
        connection: &FileConnection,
        policy: &FilePolicy,
        signal: Option<&AbortSignal>,
    ) -> Result<FileReference, FileStoreError> {
        if let Some(signal) = signal
            && signal.is_aborted()
        {
            return Err(store_abort());
        }
        let scope = connection.scope();
        let key = format!("{}\0{}", scope.as_str(), version.variant_id);
        let shared = {
            let mut inflight = self.inflight.lock().expect("file store inflight lock");
            if let Some(active) = inflight.get(&key).cloned() {
                if active.abandoned() {
                    inflight.remove(&key);
                } else {
                    let mut state = active.state.lock().expect("shared upload state");
                    state.waiters += 1;
                    drop(inflight);
                    return wait_for_shared(&active, state, signal);
                }
            }
            let shared = Arc::new(SharedUpload::new());
            {
                // The initiating caller registers itself as the first
                // waiter of its own transport run (dsh: the initiator also
                // waits through waitForUpload).
                let mut state = shared.state.lock().expect("shared upload state");
                state.waiters = 1;
            }
            inflight.insert(key.clone(), Arc::clone(&shared));
            shared
        };
        let result =
            self.ensure_uploaded_once(version, connection, &scope, policy, signal, &shared);
        {
            let mut state = shared.state.lock().expect("shared upload state");
            state.settled = true;
            state.result = Some(result.clone());
            state.waiters -= 1;
        }
        shared.progress.notify_all();
        let mut inflight = self.inflight.lock().expect("file store inflight lock");
        if let Some(registered) = inflight.get(&key)
            && Arc::ptr_eq(registered, &shared)
        {
            inflight.remove(&key);
        }
        result
    }

    /// The single transport run behind one shared upload (dsh
    /// `ensureUploadedOnce`): index cache, upload, bytes-echo validation,
    /// one quota-recovery round, and the index commit with its silent
    /// duplicate cleanup.
    fn ensure_uploaded_once(
        &self,
        version: &RequestImageAttachment,
        connection: &FileConnection,
        scope: &DeepSeekFileScope,
        policy: &FilePolicy,
        signal: Option<&AbortSignal>,
        shared: &SharedUpload,
    ) -> Result<FileReference, FileStoreError> {
        if version.bytes > MAX_IMAGE_BYTES {
            return Err(LlmError::try_new(
                "DeepSeek image exceeds the 32 MiB per-image limit.",
                crate::codes::INVALID_REQUEST_CODE,
                None,
                None,
                None,
                None,
            )
            .expect("precheck fields are static")
            .into());
        }
        let now = (self.now)();
        let margin_ms = policy.refresh_margin_seconds * 1_000;
        if let Some(cached) = self.index.get(scope, &version.variant_id, now, margin_ms)? {
            return Ok(FileReference {
                record: cached,
                uploaded: false,
            });
        }
        let client = self.client(connection);
        let upload =
            |signal: Option<&AbortSignal>| -> Result<DeepSeekUploadRecord, UploadAttemptError> {
                let remote = client.upload(
                    &UploadInput {
                        data: version.data.clone(),
                        media_type: version.media_type,
                        filename: filename(version),
                        expires_after_seconds: policy.expires_after_seconds,
                    },
                    signal,
                )?;
                if remote.file.bytes != version.bytes {
                    return Err(UploadAttemptError::EchoMismatch(
                        LlmError::try_new(
                            "DeepSeek Files API upload response does not match the submitted image.",
                            crate::codes::INVALID_RESPONSE_CODE,
                            None,
                            None,
                            None,
                            None,
                        )
                        .expect("bytes-echo fields are static"),
                    ));
                }
                Ok(DeepSeekUploadRecord {
                    scope: scope.clone(),
                    attachment_id: version.attachment.attachment_id.clone(),
                    variant_id: version.variant_id.clone(),
                    file_id: remote.file.id,
                    bytes: remote.file.bytes,
                    created_at: remote.file.created_at * 1_000,
                    expires_at: remote.expires_at * 1_000,
                })
            };
        let candidate = match upload(signal) {
            Ok(record) => record,
            Err(UploadAttemptError::EchoMismatch(error)) => return Err(error.into()),
            Err(UploadAttemptError::Files(error)) => {
                // dsh: quota recovery applies only to DeepSeekFilesError
                // instances; every other failure propagates untouched.
                if !is_files_quota_error(&error) {
                    return Err(error.into());
                }
                if shared.abandoned() {
                    return Err(error.into());
                }
                let deleted =
                    self.reclaim_oldest_owned(connection, policy.quota_cleanup_batch, signal)?;
                if deleted == 0 {
                    return Err(error.into());
                }
                upload(signal).map_err(UploadAttemptError::into_store)?
            }
        };
        let candidate_file_id = candidate.file_id.clone();
        let committed = self.index.commit(candidate, (self.now)(), margin_ms)?;
        if !committed.accepted {
            // The winning mapping is durable. A failed duplicate cleanup
            // affects quota only and is retried by recovery.
            let _ = client.delete(&candidate_file_id, signal);
        }
        Ok(FileReference {
            record: committed.record,
            uploaded: committed.accepted,
        })
    }

    /// Invalidate exact local mappings in one index update after a model
    /// request rejects their remote ids (dsh `invalidate`).
    ///
    /// # Errors
    /// A local index persistence failure.
    pub fn invalidate(
        &self,
        generations: &[Invalidation],
        connection: &FileConnection,
    ) -> Result<(), FileStoreError> {
        self.index.remove(&connection.scope(), generations)?;
        Ok(())
    }

    /// Delete the indexed remote file for one attachment and remove its
    /// local mapping (dsh `release`); `Ok(false)` when no reusable mapping
    /// exists.
    ///
    /// # Errors
    /// A Files delete failure or a local index persistence failure.
    pub fn release(
        &self,
        version: &RequestImageAttachment,
        connection: &FileConnection,
        policy: &FilePolicy,
        signal: Option<&AbortSignal>,
    ) -> Result<bool, FileStoreError> {
        let scope = connection.scope();
        let record = self.index.get(
            &scope,
            &version.variant_id,
            (self.now)(),
            policy.refresh_margin_seconds * 1_000,
        )?;
        let Some(record) = record else {
            return Ok(false);
        };
        self.client(connection).delete(&record.file_id, signal)?;
        self.index.remove(
            &scope,
            &[Invalidation {
                variant_id: version.variant_id.clone(),
                file_id: record.file_id,
            }],
        )?;
        Ok(true)
    }

    /// Delete the oldest provider files whose names identify harness
    /// ownership (dsh `reclaimOldestOwned`); returns the number of
    /// successfully deleted files.
    ///
    /// # Errors
    /// A Files list or delete failure.
    pub fn reclaim_oldest_owned(
        &self,
        connection: &FileConnection,
        count: u64,
        signal: Option<&AbortSignal>,
    ) -> Result<u64, FileStoreError> {
        let client = self.client(connection);
        let keep = usize::try_from(count).unwrap_or(usize::MAX);
        let mut after: Option<DeepSeekFileId> = None;
        let mut owned: Vec<(DeepSeekFileId, u64)> = Vec::new();
        loop {
            let page = client.list(after.as_ref(), Some(1_000), signal)?;
            for file in page.data {
                if !file.filename.starts_with(OWNED_FILE_PREFIX) {
                    continue;
                }
                owned.push((file.id, file.created_at));
            }
            // The API offers no ascending-order query; retain the oldest
            // candidates across every page.
            owned.sort_by_key(|(_, created_at)| *created_at);
            owned.truncate(keep);
            if !page.has_more || page.last_id.is_none() || page.last_id == after {
                break;
            }
            after = page.last_id;
        }
        let mut deleted = 0u64;
        for (file_id, _) in &owned {
            client.delete(file_id, signal)?;
            deleted += 1;
        }
        Ok(deleted)
    }

    /// Delete every remote harness-owned file in the active API-key
    /// namespace and clear its index (dsh `releaseAll`); returns the number
    /// of deleted files.
    ///
    /// # Errors
    /// A Files list or delete failure, or a local index persistence
    /// failure.
    pub fn release_all(
        &self,
        connection: &FileConnection,
        signal: Option<&AbortSignal>,
    ) -> Result<u64, FileStoreError> {
        let mut total = 0u64;
        loop {
            let deleted = self.reclaim_oldest_owned(connection, 1_000, signal)?;
            total += deleted;
            if deleted < 1_000 {
                break;
            }
        }
        self.index.clear(&connection.scope())?;
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{HttpMethod, HttpRequest, TransportError};
    use fr_wire::ids::AttachmentId;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    use std::sync::mpsc;

    fn hex_string(n: usize, byte: u8) -> String {
        let pattern = format!("{byte:02x}");
        pattern.repeat(n.div_ceil(2)).chars().take(n).collect()
    }

    /// A full 64-hex attachment digest; the upload-index parser rejects
    /// shorter digests, so fixtures that round-trip the index need this.
    const SAMPLE_ATTACHMENT_ID: &str =
        "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn version(variant: &str, bytes: usize) -> RequestImageAttachment {
        let wide = u64::try_from(bytes).expect("test byte counts fit u64");
        RequestImageAttachment {
            variant_id: format!("sha256:{variant}"),
            attachment: fr_wire::ImageAttachmentRef {
                attachment_id: AttachmentId::new(SAMPLE_ATTACHMENT_ID),
                media_type: ImageMediaType::Png,
                bytes: wide,
                width: 16,
                height: 16,
                name: None,
                original_dimensions: None,
            },
            data: vec![7u8; bytes],
            media_type: ImageMediaType::Png,
            bytes: wide,
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

    #[derive(Debug, Clone)]
    struct StoredFile {
        id: String,
        filename: String,
        created_at: u64,
        bytes: u64,
    }

    struct RouterState {
        next_id: u64,
        quota_remaining: u32,
        files: Vec<StoredFile>,
        wrong_echo: bool,
    }

    /// Stateful Files API double: uploads extend the store, list paginates
    /// in insertion order, delete removes the receipt-named id. An
    /// optional one-shot gate blocks the first upload until released, and
    /// every call pings the recorder so tests can observe entry timing.
    struct RouterFetch {
        state: Mutex<RouterState>,
        gate: Mutex<Option<mpsc::Receiver<()>>>,
        entered: mpsc::Sender<()>,
        upload_posts: AtomicU64,
        deletes: Mutex<Vec<String>>,
    }

    impl RouterFetch {
        fn new(
            quota_remaining: u32,
            gate: Option<mpsc::Receiver<()>>,
            entered: mpsc::Sender<()>,
        ) -> Self {
            Self {
                state: Mutex::new(RouterState {
                    next_id: 0,
                    quota_remaining,
                    files: Vec::new(),
                    wrong_echo: false,
                }),
                gate: Mutex::new(gate),
                entered,
                upload_posts: AtomicU64::new(0),
                deletes: Mutex::new(Vec::new()),
            }
        }

        fn seed(&self, filename: &str, created_at: u64, bytes: u64) -> String {
            let mut state = self.state.lock().expect("router state");
            state.next_id += 1;
            let id = format!("file-{}", state.next_id);
            state.files.push(StoredFile {
                id: id.clone(),
                filename: filename.to_owned(),
                created_at,
                bytes,
            });
            id
        }

        fn file_json(file: &StoredFile) -> String {
            // Seeded created_at values are small epoch seconds; render them
            // as the ISO instant parse_file_object expects.
            let seconds = file.created_at;
            let iso = format!(
                "1970-01-01T{:02}:{:02}:{:02}Z",
                seconds / 3_600,
                (seconds % 3_600) / 60,
                seconds % 60
            );
            format!(
                "{{\"id\":\"{}\",\"type\":\"file\",\"mime_type\":\"image/png\",\
                 \"size_bytes\":{},\"created_at\":\"{iso}\",\
                 \"filename\":\"{}\"}}",
                file.id, file.bytes, file.filename
            )
        }

        fn requests(&self) -> Vec<String> {
            self.deletes.lock().expect("deletes").clone()
        }

        /// Serve the `GET /v1/files?` listing from the stored fixtures
        /// (cursor `after_id`, page `limit`, null-cursor omission).
        fn list_page(&self, url: &str) -> crate::http::HttpResponse {
            let query = url.split('?').nth(1).unwrap_or_default();
            let limit: usize = query
                .split('&')
                .find_map(|pair| pair.strip_prefix("limit="))
                .and_then(|value| value.parse().ok())
                .unwrap_or(1_000);
            let after = query
                .split('&')
                .find_map(|pair| pair.strip_prefix("after_id="))
                .unwrap_or_default();
            let state = self.state.lock().expect("router state");
            let skip = state
                .files
                .iter()
                .position(|file| file.id == after)
                .map_or(0, |position| position + 1);
            let page: Vec<StoredFile> =
                state.files.iter().skip(skip).take(limit).cloned().collect();
            let has_more = state.files.len() > skip + page.len();
            drop(state);
            let rows = page
                .iter()
                .map(Self::file_json)
                .collect::<Vec<_>>()
                .join(",");
            let last = page
                .last()
                .map(|file| format!("\"last_id\":\"{}\"", file.id));
            let body = format!(
                "{{\"data\":[{rows}],\"first_id\":{},{}\"has_more\":{has_more}}}",
                page.first()
                    .map_or_else(|| "null".to_owned(), |file| format!("\"{}\"", file.id)),
                last.map_or(String::new(), |value| format!("{value},"))
            );
            crate::http::HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: Box::new(std::io::Cursor::new(body.into_bytes())),
            }
        }
    }

    impl Fetch for RouterFetch {
        fn fetch(&self, request: HttpRequest) -> Result<crate::http::HttpResponse, TransportError> {
            let url = request.url.clone();
            let send = |status: u16, body: String| {
                Ok(crate::http::HttpResponse {
                    status,
                    headers: Vec::new(),
                    body: Box::new(std::io::Cursor::new(body.into_bytes())),
                })
            };
            if url.ends_with("/v1/files") && matches!(request.method, HttpMethod::Post) {
                // Count the upload before gating so an in-flight gated run
                // is observable to the test.
                self.upload_posts.fetch_add(1, AtomicOrdering::SeqCst);
                let _ = self.entered.send(());
                {
                    let mut gate = self.gate.lock().expect("gate");
                    if let Some(release) = gate.take() {
                        let _ = release.recv();
                    }
                }
                {
                    let mut state = self.state.lock().expect("router state");
                    if state.quota_remaining > 0 {
                        state.quota_remaining -= 1;
                        return send(
                            400,
                            "{\"error\":{\"message\":\"too many files stored\"}}".to_owned(),
                        );
                    }
                    drop(state);
                }
                let body = request.body.expect("upload body");
                // Echo the submitted file-part byte length.
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
                let submitted = (data_end - data_start) as u64;
                let mut state = self.state.lock().expect("router state");
                state.next_id += 1;
                let echoed = if state.wrong_echo {
                    submitted + 1
                } else {
                    submitted
                };
                let file = StoredFile {
                    id: format!("file-{}", state.next_id),
                    filename: "dsh-uploaded.png".to_owned(),
                    created_at: 1_777_636_800,
                    bytes: echoed,
                };
                state.files.push(file.clone());
                drop(state);
                return send(200, Self::file_json(&file));
            }
            if url.contains("/v1/files?") && matches!(request.method, HttpMethod::Get) {
                return Ok(self.list_page(&url));
            }
            if matches!(request.method, HttpMethod::Delete) {
                let id = url.rsplit('/').next().unwrap_or_default();
                self.state
                    .lock()
                    .expect("router state")
                    .files
                    .retain(|file| file.id != id);
                self.deletes.lock().expect("deletes").push(id.to_owned());
                return send(
                    200,
                    format!("{{\"id\":\"{id}\",\"type\":\"file_deleted\"}}"),
                );
            }
            send(404, "{}".to_owned())
        }
    }

    fn make_store<'a>(
        index_path: std::path::PathBuf,
        fetch: &'a RouterFetch,
        now: &'a AtomicU64,
        on_second_tick: Option<&'a (dyn Fn() + Sync)>,
    ) -> DeepSeekFileStore<'a> {
        let tick: Box<dyn Fn() -> u64 + Sync> = match on_second_tick {
            None => Box::new(move || now.load(AtomicOrdering::SeqCst)),
            Some(hook) => {
                let calls = std::sync::atomic::AtomicUsize::new(0);
                Box::new(move || {
                    let current = now.fetch_add(1, AtomicOrdering::SeqCst);
                    // The hook fires on the second clock read (the commit
                    // call), whatever the clock's starting value is.
                    if calls.fetch_add(1, AtomicOrdering::SeqCst) == 1 {
                        hook();
                    }
                    current
                })
            }
        };
        let leaked: &'a (dyn Fn() -> u64 + Sync) = Box::leak(tick);
        DeepSeekFileStore::new(FileStoreOptions {
            index: DeepSeekUploadIndex::new(index_path),
            now: leaked,
            fetch,
        })
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "fr-file-store-{tag}-{}-{}",
            std::process::id(),
            crate::request_image_pipeline::random_hex_suffix()
        ));
        std::fs::create_dir_all(&directory).expect("scratch dir");
        directory
    }

    /// The provider-side name pins file-store.ts:114-118: `dsh-` + 16
    /// attachment-digest characters + `-` + 8 variant-digest characters +
    /// the media-type extension (jpeg keeps its four-letter dsh spelling).
    #[test]
    fn filename_pins_dsh_shape() {
        let mut jpeg = version("f".repeat(64).as_str(), 8);
        jpeg.media_type = ImageMediaType::Jpeg;
        assert_eq!(filename(&jpeg), "dsh-0123456789abcdef-ffffffff.jpeg");
        let mut webp = version(&hex_string(64, 0xab), 8);
        webp.media_type = ImageMediaType::Webp;
        assert_eq!(filename(&webp), "dsh-0123456789abcdef-abababab.webp");
        let mut gif = version("f".repeat(64).as_str(), 8);
        gif.media_type = ImageMediaType::Gif;
        assert_eq!(filename(&gif), "dsh-0123456789abcdef-ffffffff.gif");
        assert_eq!(
            filename(&version("f".repeat(64).as_str(), 8)),
            "dsh-0123456789abcdef-ffffffff.png"
        );
    }

    /// First resolution uploads and commits (uploaded=true); the second
    /// resolution reuses the committed mapping over the refresh margin
    /// without any further transport.
    #[test]
    fn ensure_uploaded_uploads_then_reuses_index() {
        let root = scratch("reuse");
        let fetch = RouterFetch::new(0, None, mpsc::channel().0);
        let now = AtomicU64::new(1_000);
        let store = make_store(root.join("files-v3.json"), &fetch, &now, None);
        let first = store
            .ensure_uploaded(
                &version(&hex_string(64, 1), 64),
                &connection(),
                &policy(),
                None,
            )
            .unwrap();
        assert!(first.uploaded);
        assert_eq!(first.record.file_id.as_str(), "file-1");
        assert_eq!(first.record.created_at, 1_777_636_800 * 1_000);
        assert_eq!(first.record.expires_at, 1_777_636_800 * 1_000 + 3_600_000);
        let second = store
            .ensure_uploaded(
                &version(&hex_string(64, 1), 64),
                &connection(),
                &policy(),
                None,
            )
            .unwrap();
        assert!(!second.uploaded);
        assert_eq!(second.record.file_id.as_str(), "file-1");
        assert_eq!(fetch.upload_posts.load(AtomicOrdering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The 32 MiB precheck rejects before any I/O with the dsh text and
    /// code; a wrong echoed size fails with the bytes-echo text.
    #[test]
    fn precheck_and_bytes_echo_pin_messages() {
        let root = scratch("precheck");
        let fetch = RouterFetch::new(0, None, mpsc::channel().0);
        let now = AtomicU64::new(1_000);
        let store = make_store(root.join("files-v3.json"), &fetch, &now, None);
        let mut oversized = version(&hex_string(64, 2), 8);
        oversized.bytes = MAX_IMAGE_BYTES + 1;
        let error = store
            .ensure_uploaded(&oversized, &connection(), &policy(), None)
            .unwrap_err();
        assert_eq!(
            error.message(),
            "DeepSeek image exceeds the 32 MiB per-image limit."
        );
        assert_eq!(error.code(), crate::codes::INVALID_REQUEST_CODE);
        assert_eq!(fetch.upload_posts.load(AtomicOrdering::SeqCst), 0);

        let echo_root = scratch("echo");
        let echo_fetch = RouterFetch::new(0, None, mpsc::channel().0);
        echo_fetch.state.lock().expect("state").wrong_echo = true;
        let echo_store = make_store(echo_root.join("files-v3.json"), &echo_fetch, &now, None);
        let error = echo_store
            .ensure_uploaded(
                &version(&hex_string(64, 3), 64),
                &connection(),
                &policy(),
                None,
            )
            .unwrap_err();
        assert_eq!(
            error.message(),
            "DeepSeek Files API upload response does not match the submitted image."
        );
        assert_eq!(error.code(), crate::codes::INVALID_RESPONSE_CODE);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&echo_root);
    }

    /// A quota failure recovers exactly once: reclaim deletes the oldest
    /// owned file, the retry upload succeeds, and the mapping commits. The
    /// echo mismatch is never quota-recoverable, so a recovering router
    /// still surfaces it (dsh `isFilesQuotaError` instanceof gate).
    #[test]
    fn quota_recovers_once_via_reclaim_then_retry() {
        let root = scratch("quota");
        let fetch = RouterFetch::new(1, None, mpsc::channel().0);
        let now = AtomicU64::new(1_000);
        let store = make_store(root.join("files-v3.json"), &fetch, &now, None);
        // Seed one previously uploaded owned file.
        let seed_id = fetch.seed("dsh-old.png", 500, 32);
        let result = store
            .ensure_uploaded(
                &version(&hex_string(64, 4), 64),
                &connection(),
                &policy(),
                None,
            )
            .unwrap();
        assert!(result.uploaded);
        assert_eq!(fetch.requests(), vec![seed_id]);
        // The failed candidate's delete was never issued; the recovered
        // upload is the committed one.
        assert_eq!(fetch.upload_posts.load(AtomicOrdering::SeqCst), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// When recovery deletes nothing (no owned files exist), the original
    /// quota failure propagates.
    #[test]
    fn quota_without_reclaim_progress_propagates() {
        let root = scratch("quota-empty");
        let fetch = RouterFetch::new(5, None, mpsc::channel().0);
        fetch.seed("vendor-logo.png", 100, 32);
        let now = AtomicU64::new(1_000);
        let store = make_store(root.join("files-v3.json"), &fetch, &now, None);
        let error = store
            .ensure_uploaded(
                &version(&hex_string(64, 5), 64),
                &connection(),
                &policy(),
                None,
            )
            .unwrap_err();
        assert_eq!(error.message(), "too many files stored");
        assert_eq!(error.code(), crate::codes::FILES_API_CODE);
        assert_eq!(fetch.requests().len(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A commit lost to a concurrently published reusable mapping deletes
    /// the candidate silently and returns the winning record (dsh
    /// `!committed.accepted` arm); the race is simulated through the clock
    /// seam, which writes the rival mapping between cache-read and commit.
    #[test]
    fn commit_loser_deletes_candidate_silently() {
        let root = scratch("loser");
        let index_path = root.join("files-v3.json");
        let fetch = RouterFetch::new(0, None, mpsc::channel().0);
        let now = AtomicU64::new(1_000);
        let rival_variant = hex_string(64, 6);
        let hook_path = index_path.clone();
        let hook_variant = rival_variant.clone();
        let hook = move || {
            let rival = DeepSeekUploadIndex::new(&hook_path);
            let scope = connection().scope();
            rival
                .commit(
                    DeepSeekUploadRecord {
                        scope,
                        attachment_id: AttachmentId::new(SAMPLE_ATTACHMENT_ID),
                        variant_id: format!("sha256:{hook_variant}"),
                        file_id: DeepSeekFileId::new("file-rival"),
                        bytes: 64,
                        created_at: 900,
                        expires_at: 999_000,
                    },
                    1_000,
                    1_000,
                )
                .expect("rival commit");
        };
        let store = make_store(index_path, &fetch, &now, Some(&hook));
        let result = store
            .ensure_uploaded(&version(&rival_variant, 64), &connection(), &policy(), None)
            .unwrap();
        assert!(!result.uploaded);
        assert_eq!(result.record.file_id.as_str(), "file-rival");
        assert_eq!(fetch.requests(), vec!["file-1".to_owned()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Reclaim pages the provider at limit 1000, filters to the `dsh-`
    /// prefix, keeps the oldest candidates across every page, and deletes
    /// only those.
    #[test]
    fn reclaim_keeps_oldest_owned_across_pages() {
        let root = scratch("reclaim");
        let fetch = RouterFetch::new(0, None, mpsc::channel().0);
        // 1001 owned files whose oldest sits on the second page (insertion
        // order is the provider order), plus one foreign file.
        for position in 0..1001u64 {
            fetch.seed(&format!("dsh-bulk-{position}.png"), 10_000 - position, 8);
        }
        fetch.seed("vendor.png", 1, 8);
        let now = AtomicU64::new(1_000);
        let index_root = scratch("reclaim-idx");
        let store = make_store(index_root.join("files-v3.json"), &fetch, &now, None);
        let deleted = store.reclaim_oldest_owned(&connection(), 1, None).unwrap();
        assert_eq!(deleted, 1);
        // The oldest owned file is bulk-1000 (created_at 9000) on page 2;
        // the foreign file with the smaller created_at is never touched.
        assert_eq!(fetch.requests(), vec!["file-1001".to_owned()]);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(scratch("reclaim-idx"));
    }

    /// release deletes the mapped remote file, unmaps it, and reports
    /// absence afterwards; `release_all` deletes every owned file across
    /// batches, keeps foreign files, and clears the namespace index.
    #[test]
    fn release_and_release_all_pin_deletion_semantics() {
        let root = scratch("release");
        let fetch = RouterFetch::new(0, None, mpsc::channel().0);
        let now = AtomicU64::new(1_000);
        let store = make_store(root.join("files-v3.json"), &fetch, &now, None);
        store
            .ensure_uploaded(
                &version(&hex_string(64, 7), 64),
                &connection(),
                &policy(),
                None,
            )
            .unwrap();
        let released = store
            .release(
                &version(&hex_string(64, 7), 64),
                &connection(),
                &policy(),
                None,
            )
            .unwrap();
        assert!(released);
        assert_eq!(fetch.requests(), vec!["file-1".to_owned()]);
        let absent = store
            .release(
                &version(&hex_string(64, 7), 64),
                &connection(),
                &policy(),
                None,
            )
            .unwrap();
        assert!(!absent);

        // release_all: two owned files seeded, one foreign.
        fetch.seed("dsh-a.png", 10, 8);
        fetch.seed("dsh-b.png", 20, 8);
        fetch.seed("keep.png", 30, 8);
        let total = store.release_all(&connection(), None).unwrap();
        assert_eq!(total, 2);
        let remaining = fetch.state.lock().expect("state");
        assert_eq!(remaining.files.len(), 1);
        assert_eq!(remaining.files[0].filename, "keep.png");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// invalidate drops exactly the named generation, so the next
    /// resolution of the same variant uploads afresh.
    #[test]
    fn invalidate_removes_exact_generation() {
        let root = scratch("invalidate");
        let fetch = RouterFetch::new(0, None, mpsc::channel().0);
        let now = AtomicU64::new(1_000);
        let store = make_store(root.join("files-v3.json"), &fetch, &now, None);
        let first = store
            .ensure_uploaded(
                &version(&hex_string(64, 8), 64),
                &connection(),
                &policy(),
                None,
            )
            .unwrap();
        store
            .invalidate(
                &[Invalidation {
                    variant_id: first.record.variant_id.clone(),
                    file_id: first.record.file_id,
                }],
                &connection(),
            )
            .unwrap();
        let second = store
            .ensure_uploaded(
                &version(&hex_string(64, 8), 64),
                &connection(),
                &policy(),
                None,
            )
            .unwrap();
        assert!(second.uploaded);
        assert_eq!(fetch.upload_posts.load(AtomicOrdering::SeqCst), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Releases the gated transport even if an assertion panics, so the
    /// scoped join never deadlocks on an abandoned in-flight upload.
    struct ReleaseOnDrop(Option<mpsc::Sender<()>>);

    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    /// Two concurrent callers of one scope+variant share a single transport
    /// run and receive the same file reference; the second upload only
    /// starts if sharing broke (it does not).
    #[test]
    fn concurrent_waiters_share_one_transport_run() {
        let root = scratch("share");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut release = ReleaseOnDrop(Some(release_tx));
        let fetch = RouterFetch::new(0, Some(release_rx), entered_tx);
        let now = AtomicU64::new(1_000);
        let store = make_store(root.join("files-v3.json"), &fetch, &now, None);
        let variant = hex_string(64, 9);
        std::thread::scope(|scope| {
            let initiator = scope.spawn(|| {
                store
                    .ensure_uploaded(&version(&variant, 64), &connection(), &policy(), None)
                    .unwrap()
            });
            // Wait until the transport run is actually in flight, then let
            // the joiner register on the shared upload.
            entered_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("initiator entered the transport");
            let joiner = scope.spawn(|| {
                store
                    .ensure_uploaded(&version(&variant, 64), &connection(), &policy(), None)
                    .unwrap()
            });
            // Generous window: a broken dedup would start a second upload
            // (and a second entered ping) long before this elapses.
            std::thread::sleep(std::time::Duration::from_millis(200));
            assert_eq!(
                fetch.upload_posts.load(AtomicOrdering::SeqCst),
                1,
                "the joiner must share the in-flight upload"
            );
            release
                .0
                .take()
                .expect("single release")
                .send(())
                .expect("release the transport");
            let first = initiator.join().expect("initiator thread");
            let second = joiner.join().expect("joiner thread");
            // Both waiters share the single transport result.
            assert!(first.uploaded);
            assert!(second.uploaded);
            assert_eq!(first.record.file_id, second.record.file_id);
            assert_eq!(first.record.variant_id, second.record.variant_id);
            assert_eq!(
                fetch.upload_posts.load(AtomicOrdering::SeqCst),
                1,
                "exactly one transport run behind both waiters"
            );
        });
        let _ = std::fs::remove_dir_all(&root);
    }
}

// Rust guideline compliant 2026-10-26
