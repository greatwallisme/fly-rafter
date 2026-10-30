//! The attachment admission limits (dsh `ImageAttachmentLimits`) and the
//! local store configuration (the dsh `attachment-local` `Config`).
//!
//! The fr home mapping follows the established brand map: `DSH_HOME` to
//! `FR_HOME`, `~/.dsh` to `~/.fr` (the fr-tools-builtin shell-env
//! judgment).

use fr_wire::ImageMediaType;

/// Default maximum encoded bytes for one submitted image; oversized
/// sources are refused, not shrunk (20 MiB).
pub const DEFAULT_MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
/// Default maximum images in one prompt (20).
pub const DEFAULT_MAX_IMAGES_PER_MESSAGE: u64 = 20;
/// Default maximum aggregate image bytes in one prompt (200 MiB).
pub const DEFAULT_MAX_MESSAGE_IMAGE_BYTES: u64 = 200 * 1024 * 1024;
/// Default maximum intrinsic pixels for one submitted image (64,000,000).
pub const DEFAULT_MAX_IMAGE_PIXELS: u64 = 64_000_000;
/// Default per-side pixel cap for one submitted image (8192).
pub const DEFAULT_MAX_IMAGE_DIMENSION: u32 = 8192;
/// Default total-pixel budget of the stored normalized image (2048*2048).
pub const DEFAULT_NORMALIZED_IMAGE_MAX_PIXELS: u32 = 2048 * 2048;
/// Default long-edge cap of the stored normalized image (8192).
pub const DEFAULT_NORMALIZED_IMAGE_MAX_DIMENSION: u32 = 8192;
/// Default encoded-byte target for one stored normalized image (4 MiB).
pub const DEFAULT_NORMALIZED_IMAGE_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Conservative default number of simultaneous native image
/// transformations per store (2).
pub const DEFAULT_IMAGE_COMPRESSION_CONCURRENCY: usize = 2;
/// Maximum configurable native image transformations per store (8).
pub const MAX_IMAGE_COMPRESSION_CONCURRENCY: usize = 8;

/// The admission limits snapshot (dsh `ImageAttachmentLimits`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageAttachmentLimits {
    /// Maximum encoded bytes accepted for one submitted image.
    pub max_image_bytes: u64,
    /// Maximum image count accepted in one submitted message.
    pub max_images_per_message: u64,
    /// Maximum aggregate encoded image bytes accepted in one message.
    pub max_message_image_bytes: u64,
    /// Maximum intrinsic width times height for one submitted image.
    pub max_image_pixels: u64,
    /// Maximum intrinsic width and height for one submitted image.
    pub max_image_dimension: u32,
    /// The media types this deployment accepts, fixed order.
    pub media_types: [ImageMediaType; 4],
}

impl ImageAttachmentLimits {
    /// The snapshot over one resolved configuration.
    #[must_use]
    pub const fn new(
        max_image_bytes: u64,
        max_images_per_message: u64,
        max_message_image_bytes: u64,
        max_image_pixels: u64,
        max_image_dimension: u32,
    ) -> Self {
        Self {
            max_image_bytes,
            max_images_per_message,
            max_message_image_bytes,
            max_image_pixels,
            max_image_dimension,
            media_types: [
                ImageMediaType::Png,
                ImageMediaType::Jpeg,
                ImageMediaType::Webp,
                ImageMediaType::Gif,
            ],
        }
    }

    /// Whether this deployment accepts one media type (the dsh
    /// `imageLimits.mediaTypes.includes` face).
    #[must_use]
    pub fn accepts(&self, media_type: ImageMediaType) -> bool {
        let table = self.media_types;
        table[0] == media_type
            || table[1] == media_type
            || table[2] == media_type
            || table[3] == media_type
    }
}

/// The local store configuration (the dsh `attachment-local` `Config`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentConfig {
    /// Explicit harness home; omitted follows `FR_HOME`, then `~/.fr`.
    pub home: Option<String>,
    /// Maximum encoded bytes accepted for one submitted image.
    pub max_image_bytes: u64,
    /// Maximum image count accepted in one submitted message.
    pub max_images_per_message: u64,
    /// Maximum aggregate encoded image bytes per submitted message.
    pub max_message_image_bytes: u64,
    /// Maximum intrinsic width times height for one submitted image.
    pub max_image_pixels: u64,
    /// Maximum intrinsic width and height for one submitted image.
    pub max_image_dimension: u32,
    /// Total-pixel budget of the stored normalized image.
    pub normalized_image_max_pixels: u32,
    /// Long-edge cap of the stored normalized image.
    pub normalized_image_max_dimension: u32,
    /// Encoded-byte target of the stored normalized image.
    pub normalized_image_max_bytes: u64,
    /// Maximum simultaneous image transformations in this instance.
    pub image_compression_concurrency: usize,
}

impl AttachmentConfig {
    /// The deployment defaults (the dsh `Config` schemastery defaults).
    #[must_use]
    pub const fn with_defaults() -> Self {
        Self {
            home: None,
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
            max_images_per_message: DEFAULT_MAX_IMAGES_PER_MESSAGE,
            max_message_image_bytes: DEFAULT_MAX_MESSAGE_IMAGE_BYTES,
            max_image_pixels: DEFAULT_MAX_IMAGE_PIXELS,
            max_image_dimension: DEFAULT_MAX_IMAGE_DIMENSION,
            normalized_image_max_pixels: DEFAULT_NORMALIZED_IMAGE_MAX_PIXELS,
            normalized_image_max_dimension: DEFAULT_NORMALIZED_IMAGE_MAX_DIMENSION,
            normalized_image_max_bytes: DEFAULT_NORMALIZED_IMAGE_MAX_BYTES,
            image_compression_concurrency: DEFAULT_IMAGE_COMPRESSION_CONCURRENCY,
        }
    }

    /// Validates the integer constraints (the dsh schemastery
    /// `step(1).min(1)` shapes and the concurrency cap; the texts adapt
    /// the brand per the Q76-B judgment).
    ///
    /// # Errors
    ///
    /// The first violated constraint (the refusal family, brand-mapped).
    pub fn validate(&self) -> Result<(), String> {
        fn check(value: u64, name: &str) -> Result<(), String> {
            if value < 1 {
                return Err(format!("fr-attachment: {name} must be an integer from 1"));
            }
            Ok(())
        }
        check(self.max_image_bytes, "max-image-bytes")?;
        check(self.max_images_per_message, "max-images-per-message")?;
        check(self.max_message_image_bytes, "max-message-image-bytes")?;
        check(self.max_image_pixels, "max-image-pixels")?;
        check(u64::from(self.max_image_dimension), "max-image-dimension")?;
        check(
            u64::from(self.normalized_image_max_pixels),
            "normalized-image-max-pixels",
        )?;
        check(
            u64::from(self.normalized_image_max_dimension),
            "normalized-image-max-dimension",
        )?;
        check(
            self.normalized_image_max_bytes,
            "normalized-image-max-bytes",
        )?;
        if self.image_compression_concurrency == 0
            || self.image_compression_concurrency > MAX_IMAGE_COMPRESSION_CONCURRENCY
        {
            return Err(format!(
                "fr-attachment: image-compression-concurrency must be an integer from 1 through {MAX_IMAGE_COMPRESSION_CONCURRENCY}"
            ));
        }
        Ok(())
    }
}

// Rust guideline compliant 2026-10-30

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate_and_limits_accept_the_four_types() {
        let config = AttachmentConfig::with_defaults();
        config.validate().expect("defaults validate");
        let limits = ImageAttachmentLimits::new(
            config.max_image_bytes,
            config.max_images_per_message,
            config.max_message_image_bytes,
            config.max_image_pixels,
            config.max_image_dimension,
        );
        for media in [
            ImageMediaType::Png,
            ImageMediaType::Jpeg,
            ImageMediaType::Webp,
            ImageMediaType::Gif,
        ] {
            assert!(limits.accepts(media));
        }
    }

    #[test]
    fn validation_refuses_zero_and_over_cap_concurrency() {
        let mut config = AttachmentConfig::with_defaults();
        config.max_image_bytes = 0;
        let text = config.validate().expect_err("zero refuses");
        assert_eq!(
            text,
            "fr-attachment: max-image-bytes must be an integer from 1"
        );
        config.max_image_bytes = DEFAULT_MAX_IMAGE_BYTES;
        config.image_compression_concurrency = 9;
        let text = config.validate().expect_err("over-cap concurrency refuses");
        assert_eq!(
            text,
            "fr-attachment: image-compression-concurrency must be an integer from 1 through 8"
        );
    }
}
