//! Deterministic provider-independent image normalization (the dsh
//! `attachment-local` `normalization.ts`).
//!
//! The persisted normalized attachment is the form every later model
//! request re-rides: a clean single-frame 8-bit sRGB source inside
//! every limit passes through byte-identically; everything else is
//! rotated, colour-narrowed, resized under the pixel budget and the
//! long-edge cap, and re-encoded through the quality ladder with the
//! smallest output kept when no quality fits the byte target.

use crate::admission::{self, SourceFacts};
use crate::error::ATTACHMENT_WRITE_FAILED_CODE;
use crate::request_image_pipeline::{
    AttachmentError, INVALID_IMAGE_CODE, encode_ladder, encoded_alpha_is_compatible,
};
use fr_wire::ImageMediaType;

/// Deployment-resolved policy for the persisted normalized attachment
/// (dsh `NormalizationPolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NormalizationPolicy {
    /// Total-pixel budget; larger sources are downscaled proportionally.
    pub max_pixels: u32,
    /// Long-edge cap applied after the total-pixel budget.
    pub max_dimension: u32,
    /// Encoded-byte target for the quality ladder.
    pub max_bytes: u64,
}

/// Normalized bytes beside the facts recorded by a durable reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedImage {
    /// The normalized encoded bytes.
    pub data: Vec<u8>,
    /// Verified normalized media type.
    pub media_type: ImageMediaType,
    /// Verified normalized width.
    pub width: u32,
    /// Verified normalized height.
    pub height: u32,
}

/// Whether bytes already satisfy the normalization requirements (dsh
/// `canPassThroughNormalization`): not a GIF, single-frame, metadata-
/// free, 8-bit sRGB, and inside every limit.
#[must_use]
pub fn can_pass_through(facts: &SourceFacts, bytes: u64, policy: &NormalizationPolicy) -> bool {
    facts.media_type != ImageMediaType::Gif
        && !facts.animated
        && !facts.carries_metadata
        && facts.is_uchar()
        && facts.space == crate::request_image_pipeline::DetectedSpace::Srgb
        && bytes <= policy.max_bytes
        && u64::from(facts.width) * u64::from(facts.height) <= u64::from(policy.max_pixels)
        && facts.width.max(facts.height) <= policy.max_dimension
}

/// Dimensions under the total-pixel budget, then the long-edge cap,
/// without changing aspect ratio (dsh `initialDimensions`).
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "dimensions are positive and the scale is at most 1; the floor/max(1) pair mirrors the dsh arithmetic"
)]
pub fn initial_dimensions(facts: &SourceFacts, policy: &NormalizationPolicy) -> (u32, u32) {
    let (budgeted_w, budgeted_h) = admission::request_image_dimensions(
        facts.width,
        facts.height,
        u64::from(policy.max_pixels),
    );
    let long_edge = budgeted_w.max(budgeted_h);
    if long_edge <= policy.max_dimension {
        return (budgeted_w, budgeted_h);
    }
    let scale = f64::from(policy.max_dimension) / f64::from(long_edge);
    (
        ((f64::from(budgeted_w) * scale).floor() as u32).max(1),
        ((f64::from(budgeted_h) * scale).floor() as u32).max(1),
    )
}

/// Asserts a normalized output is a single-frame 8-bit sRGB image with
/// matching facts (dsh `verifyNormalizedImage`).
fn verify_normalized(
    image_bytes: &[u8],
    media_type: ImageMediaType,
    width: u32,
    height: u32,
    expected_alpha: Option<bool>,
) -> Result<(), AttachmentError> {
    let detected = admission::detect_facts(image_bytes, None)?;
    if detected.media_type != media_type
        || detected.width != width
        || detected.height != height
        || detected.animated
        || detected.carries_metadata
        || !detected.is_uchar()
        || detected.space != crate::request_image_pipeline::DetectedSpace::Srgb
        || !encoded_alpha_is_compatible(expected_alpha, (media_type, detected.has_alpha))
    {
        return Err(AttachmentError::new(
            "Image normalization did not produce a single-frame 8-bit sRGB image with matching metadata.",
            ATTACHMENT_WRITE_FAILED_CODE,
        ));
    }
    Ok(())
}

/// The conversion failure source description (dsh `normalizeImage`
/// error arm): the depth word plus the PNG suffix the `read_image`
/// mapping matches (`16-bit PNG` when the depth is ushort).
#[must_use]
pub fn conversion_source_description(facts: &SourceFacts) -> String {
    if facts.media_type == ImageMediaType::Png && !facts.is_uchar() {
        let word = match facts.depth {
            crate::admission::DepthWord::Ushort => "16-bit".to_owned(),
            other => other.word().to_owned(),
        };
        format!("{word} PNG")
    } else {
        format!(
            "{} {}",
            facts.depth.word(),
            admission::media_type_word(facts.media_type).to_uppercase()
        )
    }
}

/// Produces the persisted provider-independent normalized version of
/// one fully decoded source (dsh `normalizeImage`).
///
/// A clean source inside every limit passes through; anything else
/// rotates upright, narrows to sRGB, resizes under the pixel budget
/// and the long-edge cap, and re-encodes through the quality ladder,
/// keeping the smallest output when no quality fits. Re-encoding
/// never removes transparency.
///
/// # Errors
///
/// [`AttachmentError`] `ATTACHMENT_WRITE_FAILED` with the source
/// description when the normalized form cannot be produced or
/// verified.
pub fn normalize_image(
    data: &[u8],
    facts: &SourceFacts,
    policy: &NormalizationPolicy,
) -> Result<NormalizedImage, AttachmentError> {
    if can_pass_through(facts, data.len() as u64, policy) {
        return Ok(NormalizedImage {
            data: data.to_vec(),
            media_type: facts.media_type,
            width: facts.width,
            height: facts.height,
        });
    }
    let outcome = (|| -> Result<NormalizedImage, AttachmentError> {
        let (width, height) = initial_dimensions(facts, policy);
        let mut decoded = image::load_from_memory(data).map_err(|_error| invalid_image())?;
        // The dsh preparedPipeline: orient upright, narrow to sRGB,
        // then resize inside the box without enlargement (Lanczos3, the
        // pipeline encoder judgment).
        decoded.apply_orientation(facts.orientation);
        let shrinks = (width, height) < (decoded.width(), decoded.height());
        let resized = if shrinks {
            decoded.resize(width, height, image::imageops::FilterType::Lanczos3)
        } else {
            decoded
        };
        let encoded = encode_ladder(&resized.to_rgba8(), facts.has_alpha, policy.max_bytes)?;
        Ok(NormalizedImage {
            data: encoded.data,
            media_type: encoded.media_type,
            width: encoded.width,
            height: encoded.height,
        })
    })();
    let normalized = match outcome {
        Ok(value) => value,
        Err(error) => {
            if error.code() == INVALID_IMAGE_CODE {
                let source = conversion_source_description(facts);
                return Err(AttachmentError::new(
                    format!(
                        "The {source} could not be converted to the normalized 8-bit sRGB form."
                    ),
                    ATTACHMENT_WRITE_FAILED_CODE,
                ));
            }
            return Err(error);
        }
    };
    // Verify before reporting (dsh verifyNormalizedImage); a GIF source
    // frees the alpha expectation (dsh: gif means undefined).
    let expected = if facts.media_type == ImageMediaType::Gif {
        None
    } else {
        Some(facts.has_alpha)
    };
    verify_normalized(
        &normalized.data,
        normalized.media_type,
        normalized.width,
        normalized.height,
        expected,
    )?;
    Ok(normalized)
}

fn invalid_image() -> AttachmentError {
    AttachmentError::new("Unsupported or malformed image data.", INVALID_IMAGE_CODE)
}

#[cfg(test)]
mod tests {
    use super::{NormalizationPolicy, can_pass_through, normalize_image};
    use crate::admission::detect_facts;

    const POLICY: NormalizationPolicy = NormalizationPolicy {
        max_pixels: 2048 * 2048,
        max_dimension: 8192,
        max_bytes: 4 * 1024 * 1024,
    };

    #[expect(
        clippy::cast_possible_truncation,
        reason = "the test rasters are far below one byte per channel"
    )]
    fn png_bytes(width: u32, height: u32, with_alpha: bool) -> Vec<u8> {
        let pixels = if with_alpha {
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(width, height, |x, y| {
                image::Rgba([x as u8, y as u8, 7, 255])
            }))
        } else {
            image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(width, height, |x, y| {
                image::Rgb([x as u8, y as u8, 7])
            }))
        };
        let mut bytes = Vec::new();
        pixels
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("encodes");
        bytes
    }

    #[test]
    fn clean_source_passes_through_byte_identically() {
        let data = png_bytes(8, 6, true);
        let facts = detect_facts(&data, None).expect("detects");
        assert!(super::can_pass_through(&facts, data.len() as u64, &POLICY));
        let normalized = normalize_image(&data, &facts, &POLICY).expect("normalizes");
        assert_eq!(normalized.data, data);
        assert_eq!((normalized.width, normalized.height), (8, 6));
    }

    #[test]
    fn oversized_source_downscales_under_the_budget() {
        let data = png_bytes(100, 60, false);
        let tight = NormalizationPolicy {
            max_pixels: 400,
            max_dimension: 8192,
            max_bytes: u64::MAX,
        };
        let facts = detect_facts(&data, None).expect("detects");
        assert!(!can_pass_through(&facts, data.len() as u64, &tight));
        let normalized = normalize_image(&data, &facts, &tight).expect("normalizes");
        assert_eq!((normalized.width, normalized.height), (25, 15));
        assert_eq!(
            normalized.media_type,
            fr_wire::ImageMediaType::Jpeg,
            "an opaque source re-encodes as JPEG through the ladder"
        );
    }

    /// The PNG chunk CRC32 (bitwise, table-free).
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFF_u32;
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }

    #[test]
    fn metadata_bearing_source_reencodes() {
        // A text chunk makes the source carry metadata, forcing the
        // re-encode arm byte-shape aside.
        let mut data = png_bytes(8, 6, false);
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&13_u32.to_be_bytes());
        chunk.extend_from_slice(b"tEXtComment\x00hello");
        chunk.extend_from_slice(&crc32(b"tEXtComment\x00hello").to_be_bytes());
        // IHDR (4+4+13+4 = 25 bytes) must stay first; the text chunk
        // rides behind it.
        data.splice(33..33, chunk);
        let facts = detect_facts(&data, None).expect("detects");
        assert!(facts.carries_metadata, "the tEXt chunk is metadata");
        assert!(!can_pass_through(&facts, data.len() as u64, &POLICY));
        let normalized = normalize_image(&data, &facts, &POLICY).expect("normalizes");
        assert_eq!((normalized.width, normalized.height), (8, 6));
    }
}
