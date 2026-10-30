//! The persistent content-addressed local attachment store (the dsh
//! `attachment-local` `index.ts` `LocalAttachmentStore`).
//!
//! One store owns the durable image root, the request-image cache
//! root, the admission limits, the normalization policy, and the
//! instance-wide concurrency bound for the native image
//! transformations. The dsh promise-based single-flight for request
//! versions materializes as a condvar-sharing sync face; cancellation
//! follows the same last-waiter-cancels rule.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::attachment_read::AttachmentRead;
use crate::limits::{AttachmentConfig, ImageAttachmentLimits};
use crate::normalize::NormalizationPolicy;
use crate::request_image::RequestImageAttachment;
use crate::request_image_pipeline::{
    AttachmentError, StoredImage, read_request_image_file, request_image_variant_id,
};
use crate::store::{SaveImageAttachment, StoredImageAttachment, read_image_file, save_image_file};
use fr_llm::adapter::{AbortSignal, ImageRequestTarget};
use fr_wire::ImageAttachmentRef;

/// Resolves the harness home (the dsh `resolveDshHome` fr mapping):
/// the explicit value, then `FR_HOME`, then `~/.fr` (the
/// fr-tools-builtin shell-env judgment).
#[must_use]
pub fn resolve_home(configured: Option<&str>) -> PathBuf {
    if let Some(home) = configured {
        return PathBuf::from(home);
    }
    if let Ok(home) = std::env::var("FR_HOME")
        && !home.trim().is_empty()
    {
        return PathBuf::from(home);
    }
    let mut home = std::env::var("HOME").map_or_else(|_| PathBuf::new(), PathBuf::from);
    home.push(".fr");
    home
}

/// FIFO limiter for the native image transformations (the dsh
/// `CompressionLimiter` sync materialization).
#[derive(Debug)]
pub(crate) struct CompressionLimiter {
    concurrency: usize,
    state: Mutex<LimiterState>,
}

#[derive(Debug, Default)]
struct LimiterState {
    active: usize,
    queue: VecDeque<Arc<(Mutex<bool>, Condvar)>>,
}

/// Locks one mutex, recovering a poisoned guard (a panic in another
/// thread leaves the shared queue consistent: plain counters).
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl CompressionLimiter {
    fn new(concurrency: usize) -> Self {
        Self {
            concurrency,
            state: Mutex::new(LimiterState::default()),
        }
    }

    /// Runs one task after an instance slot becomes available.
    fn run<T>(
        &self,
        task: impl FnOnce() -> Result<T, AttachmentError>,
    ) -> Result<T, AttachmentError> {
        let mut state = lock(&self.state);
        if self.concurrency == 0 {
            return task();
        }
        if state.active >= self.concurrency {
            let slot = Arc::new((Mutex::new(false), Condvar::new()));
            state.queue.push_back(Arc::clone(&slot));
            wait_turn(&slot.0, &slot.1);
            state = lock(&self.state);
        }
        state.active += 1;
        drop(state);
        let outcome = task();
        release_next(&self.state);
        outcome
    }
}

/// Blocks until the queued slot receives its turn.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the condvar-sharing protocol holds the guard across the wait by design; the tightening heuristic cannot see the wait hand-off"
)]
fn wait_turn(flag: &Mutex<bool>, signal: &Condvar) {
    let mut released = lock(flag);
    while !*released {
        released = signal
            .wait(released)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

/// Wakes one queued slot (the ownership hand-off keeps the Arc drop
/// inside this scope, clear of the queue lock).
#[expect(
    clippy::significant_drop_tightening,
    reason = "the condvar-sharing protocol holds the guard across the wait by design; the tightening heuristic cannot see the wait hand-off"
)]
fn wake(slot: Arc<(Mutex<bool>, Condvar)>) {
    let pair = Arc::clone(&slot);
    drop(slot);
    let mut released = lock(&pair.0);
    *released = true;
    pair.1.notify_one();
}

/// Releases one finished slot and hands the turn to the next queued
/// waiter, if any.
fn release_next(state: &Mutex<LimiterState>) {
    let mut state = lock(state);
    state.active -= 1;
    if let Some(next) = state.queue.pop_front() {
        wake(next);
    }
}

/// One in-flight request-version computation shared across waiters
/// (the dsh `SharedRequest` sync materialization): the first caller
/// runs the work; concurrent same-variant callers wait for the slot;
/// the last cancelled waiter aborts the shared signal.
struct SharedRequest {
    signal: AbortSignal,
    slot: Mutex<Option<Result<RequestImageAttachment, AttachmentError>>>,
    done: Condvar,
    waiters: AtomicUsize,
    settled: AtomicBool,
}

impl SharedRequest {
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the condvar-sharing protocol holds the guard across the wait by design; the tightening heuristic cannot see the wait hand-off"
    )]
    fn register(map: &Mutex<HashMap<String, Arc<Self>>>, key: &str) -> (Arc<Self>, bool) {
        let mut entries = lock(map);
        if let Some(existing) = entries.get(key)
            && existing.signal.is_aborted()
        {
            entries.remove(key);
        }
        if let Some(existing) = entries.get(key) {
            return (Arc::clone(existing), false);
        }
        let shared = Arc::new(Self {
            signal: AbortSignal::new(),
            slot: Mutex::new(None),
            done: Condvar::new(),
            waiters: AtomicUsize::new(0),
            settled: AtomicBool::new(false),
        });
        let handle = Arc::clone(&shared);
        entries.insert(key.to_owned(), shared);
        (handle, true)
    }

    fn retire(map: &Mutex<HashMap<String, Arc<Self>>>, key: &str, shared: &Arc<Self>) {
        let mut entries = lock(map);
        if let Some(current) = entries.get(key)
            && Arc::ptr_eq(current, shared)
        {
            entries.remove(key);
        }
    }

    /// Runs the computation once and releases every waiter (the
    /// runner's path).
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the condvar-sharing protocol holds the guard across the wait by design; the tightening heuristic cannot see the wait hand-off"
    )]
    fn run(
        &self,
        work: impl FnOnce(&AbortSignal) -> Result<RequestImageAttachment, AttachmentError>,
    ) {
        let outcome = work(&self.signal);
        let mut slot = lock(&self.slot);
        *slot = Some(outcome);
        self.settled.store(true, Ordering::SeqCst);
        self.done.notify_all();
    }

    /// Waits for the slot, honoring the caller's signal (the joiner's
    /// path): an aborted caller releases, and the last cancelled
    /// waiter aborts the shared computation.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the condvar-sharing protocol holds the guard across the wait by design; the tightening heuristic cannot see the wait hand-off"
    )]
    fn wait(
        &self,
        signal: Option<&AbortSignal>,
    ) -> Result<RequestImageAttachment, AttachmentError> {
        self.waiters.fetch_add(1, Ordering::SeqCst);
        let outcome = loop {
            if signal.is_some_and(AbortSignal::is_aborted) {
                break Err(abort_error(signal));
            }
            let guard = lock(&self.slot);
            if let Some(value) = guard.as_ref() {
                break value.clone();
            }
            let (guard, _timeout) = self
                .done
                .wait_timeout(guard, std::time::Duration::from_millis(50))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(value) = guard.as_ref() {
                break value.clone();
            }
        };
        self.waiters.fetch_sub(1, Ordering::SeqCst);
        if outcome.is_err() && signal.is_some_and(AbortSignal::is_aborted) {
            let remaining = self.waiters.load(Ordering::SeqCst);
            if remaining == 0 && !self.settled.load(Ordering::SeqCst) {
                self.signal.abort();
            }
        }
        outcome
    }
}

fn abort_error(signal: Option<&AbortSignal>) -> AttachmentError {
    let _ = signal;
    AttachmentError::new(
        "The request was aborted.",
        crate::request_image_pipeline::ABORTED_CODE,
    )
}

/// The persistent content-addressed local attachment store (dsh
/// `LocalAttachmentStore`).
///
/// One store owns the durable image root, the request-image cache
/// root, the admission limits, the normalization policy, and the
/// instance-wide compression bound.
pub struct LocalAttachmentStore {
    /// Absolute versioned storage root: `<home>/attachments/v1`.
    pub root: PathBuf,
    /// The request-image cache root: `<home>/cache/attachments`.
    cache_root: PathBuf,
    /// The admission limits snapshot.
    pub image_limits: ImageAttachmentLimits,
    /// The provider-independent normalization policy.
    pub normalization_policy: NormalizationPolicy,
    compression: CompressionLimiter,
    request_inflight: Mutex<HashMap<String, Arc<SharedRequest>>>,
}

impl std::fmt::Debug for LocalAttachmentStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalAttachmentStore")
            .field("root", &self.root)
            .field("cache_root", &self.cache_root)
            .field("image_limits", &self.image_limits)
            .field("normalization_policy", &self.normalization_policy)
            .finish_non_exhaustive()
    }
}

impl LocalAttachmentStore {
    /// Assembles the store over one validated configuration (dsh
    /// `constructor`): the paths derive from the home, the limits and
    /// policy snapshot the config, and the concurrency bound validates
    /// up front.
    ///
    /// # Errors
    ///
    /// The configuration validation failure (the brand-mapped family).
    pub fn new(config: &AttachmentConfig) -> Result<Self, String> {
        config.validate()?;
        let home = resolve_home(config.home.as_deref());
        Ok(Self {
            root: home.join("attachments").join("v1"),
            cache_root: home.join("cache").join("attachments"),
            image_limits: ImageAttachmentLimits::new(
                config.max_image_bytes,
                config.max_images_per_message,
                config.max_message_image_bytes,
                config.max_image_pixels,
                config.max_image_dimension,
            ),
            normalization_policy: NormalizationPolicy {
                max_pixels: config.normalized_image_max_pixels,
                max_dimension: config.normalized_image_max_dimension,
                max_bytes: config.normalized_image_max_bytes,
            },
            compression: CompressionLimiter::new(config.image_compression_concurrency),
            request_inflight: Mutex::new(HashMap::new()),
        })
    }

    /// Runs the full admission policy for one image without touching
    /// storage (dsh `validateImage`).
    ///
    /// # Errors
    ///
    /// The admission family.
    pub fn validate_image(&self, input: &SaveImageAttachment) -> Result<(), AttachmentError> {
        self.compression.run(|| {
            crate::store::validate_image_file(input, &self.image_limits, &self.normalization_policy)
        })
    }

    /// Saves one image: admission, normalization, and publication
    /// under the instance compression bound (dsh `saveImage`).
    ///
    /// # Errors
    /// The admission and publication families.
    pub fn save_image(
        &self,
        input: &SaveImageAttachment,
    ) -> Result<ImageAttachmentRef, AttachmentError> {
        let root = self.root.clone();
        let limits = self.image_limits.clone();
        let policy = self.normalization_policy;
        let input = input.clone();
        self.compression
            .run(move || save_image_file(&root, &input, &limits, &policy))
    }

    /// Saves a batch: every member validates first, every member
    /// prepares under the bound, then the members publish in order (dsh
    /// `saveImages`).
    ///
    /// # Errors
    /// The admission and publication families.
    pub fn save_images(
        &self,
        inputs: &[SaveImageAttachment],
    ) -> Result<Vec<ImageAttachmentRef>, AttachmentError> {
        for input in inputs {
            self.validate_image(input)?;
        }
        let mut prepared = Vec::with_capacity(inputs.len());
        for input in inputs {
            let limits = self.image_limits.clone();
            let policy = self.normalization_policy;
            prepared.push(
                self.compression
                    .run(|| crate::store::prepare_image_file(input, &limits, &policy))?,
            );
        }
        let mut references = Vec::with_capacity(prepared.len());
        for item in &prepared {
            references.push(crate::store::commit_prepared_image_file(&self.root, item)?);
        }
        Ok(references)
    }

    /// Reads and verifies one stored image (dsh `readImage`).
    ///
    /// # Errors
    /// The read family.
    pub fn read_image(
        &self,
        reference: &ImageAttachmentRef,
    ) -> Result<StoredImageAttachment, AttachmentError> {
        read_image_file(&self.root, reference)
    }

    /// The provider-local host path of one stored image (dsh
    /// `imageHostPath`).
    ///
    /// # Errors
    /// The invalid-reference refusal.
    pub fn image_host_path(
        &self,
        reference: &ImageAttachmentRef,
    ) -> Result<PathBuf, AttachmentError> {
        crate::store::normalized_image_path(&self.root, reference)
    }

    /// Commits one file byte-for-byte (dsh `saveFile`).
    ///
    /// # Errors
    /// The publication family.
    pub fn save_file(
        &self,
        data: &[u8],
        name: Option<&str>,
    ) -> Result<fr_wire::FileAttachmentRef, AttachmentError> {
        crate::file_store::save_file_verbatim(&self.root, data, name)
    }

    /// Commits one file byte-for-byte from ordered chunks (dsh
    /// `saveFileStream`).
    ///
    /// # Errors
    /// The publication family.
    pub fn save_file_stream(
        &self,
        chunks: &[&[u8]],
        name: Option<&str>,
    ) -> Result<fr_wire::FileAttachmentRef, AttachmentError> {
        crate::file_store::save_file_stream_verbatim(&self.root, chunks, name)
    }

    /// Reads one stored file with its terminal verification (dsh
    /// `readFileStream` terminal semantics).
    ///
    /// # Errors
    /// The read family.
    pub fn read_file(
        &self,
        reference: &fr_wire::FileAttachmentRef,
    ) -> Result<Vec<u8>, AttachmentError> {
        crate::file_store::read_file_verbatim(&self.root, reference)
    }

    /// The stored file's host path (dsh `fileHostPath`).
    ///
    /// # Errors
    /// The invalid-reference refusal.
    pub fn file_host_path(
        &self,
        reference: &fr_wire::FileAttachmentRef,
    ) -> Result<PathBuf, AttachmentError> {
        crate::file_store::stored_file_path(&self.root, reference)
    }

    /// Generates or reuses one request image under the single-flight
    /// map (dsh `readImageRequest` → `requestVersion`).
    ///
    /// # Panics
    /// Never on a well-formed store: the slot expectation reads the
    /// runner's own settled value.
    ///
    /// # Errors
    /// The read and request-image families.
    pub fn read_image_request(
        &self,
        reference: &ImageAttachmentRef,
        target: ImageRequestTarget,
        signal: Option<&AbortSignal>,
    ) -> Result<RequestImageAttachment, AttachmentError> {
        if signal.is_some_and(AbortSignal::is_aborted) {
            return Err(abort_error(signal));
        }
        let variant =
            request_image_variant_id(reference, (target.width, target.height, target.max_bytes));
        let key = variant;
        let (shared, is_runner) = SharedRequest::register(&self.request_inflight, &key);
        if is_runner {
            let root = self.root.clone();
            let cache_root = self.cache_root.clone();
            let reference = reference.clone();
            let shared_for_retire = Arc::clone(&shared);
            self.compression.run(|| {
                shared.run(|shared_signal| {
                    let stored = read_image_file(&root, &reference)?;
                    read_request_image_file(
                        &cache_root,
                        &StoredImage {
                            reference: stored.reference,
                            data: stored.data,
                        },
                        (target.width, target.height, target.max_bytes),
                        Some(shared_signal),
                    )
                });
                Ok(())
            })?;
            SharedRequest::retire(&self.request_inflight, &key, &shared_for_retire);
            // The runner reads its own just-settled slot.
            let slot = lock(&shared.slot);
            return slot
                .clone()
                .expect("the runner settled the slot before returning");
        }
        shared.wait(signal)
    }
}

impl AttachmentRead for LocalAttachmentStore {
    fn read_image_request(
        &self,
        reference: &ImageAttachmentRef,
        target: ImageRequestTarget,
        signal: Option<&AbortSignal>,
    ) -> Result<RequestImageAttachment, AttachmentError> {
        Self::read_image_request(self, reference, target, signal)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{AttachmentConfig, LocalAttachmentStore, resolve_home};
    use crate::store::SaveImageAttachment;

    fn config_in(home: &std::path::Path) -> AttachmentConfig {
        AttachmentConfig {
            home: Some(home.to_string_lossy().into_owned()),
            ..AttachmentConfig::with_defaults()
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "the test rasters are far below one byte per channel"
    )]
    fn opaque_png(width: u32, height: u32) -> Vec<u8> {
        let pixels =
            image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(width, height, |x, y| {
                image::Rgb([x as u8, y as u8, 9])
            }));
        let mut bytes = Vec::new();
        pixels
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("encodes");
        bytes
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fr-attachment-local-{tag}-{}-{}",
                std::process::id(),
                crate::request_image_pipeline::random_hex_suffix()
            ));
            std::fs::create_dir_all(&dir).expect("scratch dir creates");
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn home_resolution_prefers_config_then_env_shape() {
        assert_eq!(
            resolve_home(Some("/data/fr")),
            PathBuf::from("/data/fr"),
            "the explicit home wins"
        );
        // The env and home fallbacks are environment-dependent; the
        // explicit arm above is the pin.
    }

    #[test]
    fn store_paths_derive_from_the_home() {
        let scratch = Scratch::new("paths");
        let store = LocalAttachmentStore::new(&config_in(&scratch.0)).expect("builds");
        assert_eq!(store.root, scratch.0.join("attachments").join("v1"));
        assert_eq!(
            store.cache_root,
            scratch.0.join("cache").join("attachments")
        );
        assert_eq!(store.image_limits.max_image_bytes, 20 * 1024 * 1024);
        assert_eq!(
            store.normalization_policy.max_pixels,
            2048 * 2048,
            "the normalization budget default"
        );
    }

    #[test]
    fn save_read_file_round_trip() {
        let scratch = Scratch::new("file");
        let store = LocalAttachmentStore::new(&config_in(&scratch.0)).expect("builds");
        let reference = store
            .save_file(b"report.pdf", Some("/client/dir/Report.PDF"))
            .expect("saves");
        assert_eq!(reference.name, "Report.PDF", "the sanitized leaf name");
        assert_eq!(reference.bytes, 10);
        let stored = store.read_file(&reference).expect("reads");
        assert_eq!(stored, b"report.pdf");
        let path = store.file_host_path(&reference).expect("path");
        assert!(path.to_string_lossy().contains("/files/"));
    }

    #[test]
    fn save_image_and_request_version_share_the_store() {
        let scratch = Scratch::new("image");
        let store = LocalAttachmentStore::new(&config_in(&scratch.0)).expect("builds");
        let input = SaveImageAttachment {
            data: opaque_png(6, 4),
            media_type: fr_wire::ImageMediaType::Png,
            name: Some("shot.png".to_owned()),
        };
        let reference = store.save_image(&input).expect("saves");
        let stored = store.read_image(&reference).expect("reads");
        assert_eq!(stored.data, input.data);
        // The request-version face resolves through the single-flight
        // runner and returns the pipeline's verified attachment.
        let target = fr_llm::adapter::ImageRequestTarget {
            width: 6,
            height: 4,
            max_bytes: 1024 * 1024,
        };
        let version = store
            .read_image_request(&reference, target, None)
            .expect("request version");
        assert_eq!((version.width, version.height), (6, 4));
        assert_eq!(version.attachment, reference);
        // The batch face saves in order and validates first.
        let references = store
            .save_images(&[input.clone(), input])
            .expect("batch saves");
        assert_eq!(references.len(), 2);
        assert_eq!(references[0], references[1], "content-addressed dedup");
    }

    #[test]
    fn over_cap_concurrency_refuses_the_build() {
        let scratch = Scratch::new("cap");
        let mut config = config_in(&scratch.0);
        config.image_compression_concurrency = 9;
        let error = LocalAttachmentStore::new(&config).expect_err("refuses");
        assert_eq!(
            error,
            "fr-attachment: image-compression-concurrency must be an integer from 1 through 8"
        );
    }
}
