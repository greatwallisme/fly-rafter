//! One request image version (dsh `RequestImageAttachment` from
//! `dsh-attachment`).
//!
//! The canonical fr home is the future attachment crate; until that slice
//! lands, the request surface of the provider consumes this verbatim shape
//! from here.

use fr_wire::{ImageAttachmentRef, ImageMediaType};

/// Provider-compatible sample depth (dsh `depth: 'uchar'`; the only value
/// proven after request encoding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageSampleDepth {
    /// Eight-bit unsigned samples.
    Uchar,
}

/// Provider-compatible color space (dsh `space: 'srgb'`; the only value
/// proven after request encoding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageColorSpace {
    /// sRGB.
    Srgb,
}

/// Cached request version derived from one provider-independent normalized
/// attachment (dsh `RequestImageAttachment`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestImageAttachment {
    /// Cache and upload-index key over the attachment id, policy, and fixed
    /// encoder parameters (dsh `ImageVariantId`).
    pub variant_id: String,
    /// Durable normalized attachment this request version derives from.
    pub attachment: ImageAttachmentRef,
    /// Encoded request bytes.
    pub data: Vec<u8>,
    /// Encoded media type.
    pub media_type: ImageMediaType,
    /// Exact encoded byte length (`data.len()`).
    pub bytes: u64,
    /// Encoded width in pixels.
    pub width: u32,
    /// Encoded height in pixels.
    pub height: u32,
    /// Proven sample depth.
    pub depth: ImageSampleDepth,
    /// Proven color space.
    pub space: ImageColorSpace,
    /// Whether the encoded request version retains an alpha channel.
    pub has_alpha: bool,
}

// Rust guideline compliant 2026-10-26
