//! The attachment failure-code family (dsh `dsh-attachment/error`).
//!
//! The pipeline module holds the four codes its request-image face
//! raises; this module owns the full protocol family the durable store
//! routes on, plus the membership predicates (the dsh
//! `isAttachmentError`/`isImageAdmissionError` runtime sets).

/// Caller-correctable admission code: too many images in one message.
pub const TOO_MANY_IMAGES_CODE: &str = "TOO_MANY_IMAGES";
/// Caller-correctable admission code: aggregate message bytes over the cap.
pub const IMAGES_TOO_LARGE_CODE: &str = "IMAGES_TOO_LARGE";
/// Caller-correctable admission code: a media type the deployment refuses.
pub const UNSUPPORTED_IMAGE_TYPE_CODE: &str = "UNSUPPORTED_IMAGE_TYPE";
/// Caller-correctable admission code: a malformed base64 image payload.
pub const INVALID_IMAGE_BASE64_CODE: &str = "INVALID_IMAGE_BASE64";
/// Caller-correctable admission code: a malformed or unsupported raster.
pub const INVALID_IMAGE_CODE: &str = "INVALID_IMAGE";
/// Caller-correctable admission code: bytes decode to another format than declared.
pub const IMAGE_TYPE_MISMATCH_CODE: &str = "IMAGE_TYPE_MISMATCH";
/// Caller-correctable admission code: encoded source over the byte cap.
pub const IMAGE_TOO_LARGE_CODE: &str = "IMAGE_TOO_LARGE";
/// Caller-correctable admission code: decoded-pixel count over the cap.
pub const IMAGE_TOO_MANY_PIXELS_CODE: &str = "IMAGE_TOO_MANY_PIXELS";
/// Caller-correctable admission code: a side over the per-side cap.
pub const IMAGE_DIMENSION_TOO_LARGE_CODE: &str = "IMAGE_DIMENSION_TOO_LARGE";
/// A malformed base64 file payload.
pub const INVALID_FILE_BASE64_CODE: &str = "INVALID_FILE_BASE64";
/// A malformed durable reference or target.
pub const INVALID_ATTACHMENT_REF_CODE: &str = "INVALID_ATTACHMENT_REF";
/// An encoded output failed its publication verification.
pub const ATTACHMENT_CORRUPT_CODE: &str = "ATTACHMENT_CORRUPT";
/// A durable write failed.
pub const ATTACHMENT_WRITE_FAILED_CODE: &str = "ATTACHMENT_WRITE_FAILED";
/// A durable object is missing at read time.
pub const ATTACHMENT_NOT_FOUND_CODE: &str = "ATTACHMENT_NOT_FOUND";
/// A durable object read failed.
pub const ATTACHMENT_READ_FAILED_CODE: &str = "ATTACHMENT_READ_FAILED";
/// A projection face the store does not carry.
pub const ATTACHMENT_PROJECTION_UNSUPPORTED_CODE: &str = "ATTACHMENT_PROJECTION_UNSUPPORTED";
/// A files face the store does not carry.
pub const ATTACHMENT_FILES_UNSUPPORTED_CODE: &str = "ATTACHMENT_FILES_UNSUPPORTED";

/// The admission family: the codes a caller can correct by fixing the
/// proposed image content or batch (the dsh `IMAGE_ADMISSION_ERROR_CODES`
/// table order).
pub const IMAGE_ADMISSION_CODES: [&str; 9] = [
    TOO_MANY_IMAGES_CODE,
    IMAGES_TOO_LARGE_CODE,
    UNSUPPORTED_IMAGE_TYPE_CODE,
    INVALID_IMAGE_BASE64_CODE,
    INVALID_IMAGE_CODE,
    IMAGE_TYPE_MISMATCH_CODE,
    IMAGE_TOO_LARGE_CODE,
    IMAGE_TOO_MANY_PIXELS_CODE,
    IMAGE_DIMENSION_TOO_LARGE_CODE,
];

/// The full protocol family (admission codes plus the storage/refusal
/// codes, the dsh `ATTACHMENT_ERROR_CODES` order).
pub const ATTACHMENT_ERROR_CODES: [&str; 17] = [
    TOO_MANY_IMAGES_CODE,
    IMAGES_TOO_LARGE_CODE,
    UNSUPPORTED_IMAGE_TYPE_CODE,
    INVALID_IMAGE_BASE64_CODE,
    INVALID_IMAGE_CODE,
    IMAGE_TYPE_MISMATCH_CODE,
    IMAGE_TOO_LARGE_CODE,
    IMAGE_TOO_MANY_PIXELS_CODE,
    IMAGE_DIMENSION_TOO_LARGE_CODE,
    INVALID_FILE_BASE64_CODE,
    INVALID_ATTACHMENT_REF_CODE,
    ATTACHMENT_CORRUPT_CODE,
    ATTACHMENT_WRITE_FAILED_CODE,
    ATTACHMENT_NOT_FOUND_CODE,
    ATTACHMENT_READ_FAILED_CODE,
    ATTACHMENT_PROJECTION_UNSUPPORTED_CODE,
    ATTACHMENT_FILES_UNSUPPORTED_CODE,
];

/// Whether one code string is in the admission family (the dsh
/// `isImageAdmissionError` runtime membership).
#[must_use]
pub fn is_image_admission_code(code: &str) -> bool {
    IMAGE_ADMISSION_CODES.contains(&code)
}

/// Whether one code string is in the full attachment family (the dsh
/// `isAttachmentError` runtime membership).
#[must_use]
pub fn is_attachment_error_code(code: &str) -> bool {
    ATTACHMENT_ERROR_CODES.contains(&code)
}

// Rust guideline compliant 2026-10-30
