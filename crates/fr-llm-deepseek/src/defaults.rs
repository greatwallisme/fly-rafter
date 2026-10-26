//! Shared provider limits (dsh `defaults.ts`, values verbatim).

/// Default maximum idle interval while an adapter stream read is outstanding.
pub const DEFAULT_STREAM_IDLE_TIMEOUT_MS: u64 = 300_000;
/// Default combined request/response context capacity.
pub const DEFAULT_CONTEXT_WINDOW: u64 = 1_000_000;
/// Default per-request output-token cap.
pub const DEFAULT_MAX_TOKENS: u64 = 256_000;
/// Default bound on accumulated base64 image payload after Files API fallback.
pub const DEFAULT_MAX_INLINE_REQUEST_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
/// Deterministic raw-byte removal step.
pub const DEFAULT_IMAGE_OFFLOAD_BYTE_QUANTUM: u64 = 64 * 1024 * 1024;
/// Deterministic base64-byte removal step after Files API fallback.
pub const DEFAULT_INLINE_IMAGE_OFFLOAD_BYTE_QUANTUM: u64 = 10 * 1024 * 1024;
/// Deterministic image-count removal step.
pub const DEFAULT_IMAGE_OFFLOAD_COUNT_QUANTUM: u64 = 20;
/// Default explicit lifetime for uploaded images.
pub const DEFAULT_FILE_EXPIRY_SECONDS: u64 = 7 * 24 * 60 * 60;
/// Default proactive refresh window for indexed file ids.
pub const DEFAULT_FILE_REFRESH_MARGIN_SECONDS: u64 = 60 * 60;
/// Default number of oldest harness-owned files removed on quota recovery.
pub const DEFAULT_FILE_QUOTA_CLEANUP_BATCH: u64 = 100;
/// Default deadline for resolving one request image through the Files API.
pub const DEFAULT_FILES_API_TIMEOUT_MS: u64 = 60_000;

/// Maximum accumulated file-referenced image bytes per chat request (dsh
/// `request-pricing.ts`).
pub const DEFAULT_MAX_REQUEST_FILES_BYTES: u64 = 128 * 1024 * 1024;
/// Maximum number of represented images per chat request (dsh
/// `request-pricing.ts`).
pub const DEFAULT_MAX_IMAGES_PER_REQUEST: u64 = 600;
/// Encoded-byte target for one deterministic request preview when a catalog
/// entry declares image input without an explicit bound (dsh
/// `request-pricing.ts`).
pub const DEFAULT_REQUEST_IMAGE_MAX_BYTES: u64 = 2 * 1024 * 1024;
