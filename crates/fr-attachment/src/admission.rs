//! Admission-side raster inspection (the dsh `attachment-local`
//! `image.ts` + `request-projection.ts` admission faces).
//!
//! The request-image pipeline module holds its own request-image
//! subset detection; the admission face needs the full sharp fact
//! table: the perceived (orientation-applied) axes, the animation
//! and retained-metadata container facts, and the sharp depth word
//! the normalization failure texts quote. The image crate reports
//! the EXIF orientation through the decoder trait (PNG eXIf, JPEG
//! APP1, WebP EXIF chunk); the animation and metadata facts walk the
//! containers directly.

use std::io::Cursor;

use image::ImageDecoder as _;

use crate::error::{IMAGE_DIMENSION_TOO_LARGE_CODE, IMAGE_TOO_MANY_PIXELS_CODE};
use crate::request_image_pipeline::{AttachmentError, DetectedSpace, INVALID_IMAGE_CODE};
use fr_wire::ImageMediaType;

/// The sharp depth word (dsh `metadata.depth`): the normalization
/// failure texts interpolate it (`ushort` renders the `16-bit PNG`
/// source description the `read_image` error mapping matches).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthWord {
    /// Eight-bit unsigned samples.
    Uchar,
    /// Sixteen-bit unsigned samples.
    Ushort,
    /// Thirty-two-bit float samples.
    Float,
}

impl DepthWord {
    /// The sharp word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Uchar => "uchar",
            Self::Ushort => "ushort",
            Self::Float => "float",
        }
    }
}

/// The admission fact table (dsh `DetectedImage`, the full face).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFacts {
    /// Media type verified from the bytes.
    pub media_type: ImageMediaType,
    /// Perceived width (orientation applied), pixels.
    pub width: u32,
    /// Perceived height (orientation applied), pixels.
    pub height: u32,
    /// Whether the decoded pixels carry an alpha channel.
    pub has_alpha: bool,
    /// Reported sample depth word.
    pub depth: DepthWord,
    /// Reported colour space (the sRGB check is unreachable defense
    /// on real paths; see the pipeline judgment).
    pub space: DetectedSpace,
    /// Whether the container carries more than one frame.
    pub animated: bool,
    /// Whether the bytes carry descriptive metadata, a color profile,
    /// or an orientation.
    pub carries_metadata: bool,
    /// The EXIF orientation the decoders report.
    pub orientation: image::metadata::Orientation,
}

impl SourceFacts {
    /// The perceived sample depth (the dsh `depth === 'uchar'` face).
    #[must_use]
    pub const fn is_uchar(&self) -> bool {
        matches!(self.depth, DepthWord::Uchar)
    }
}

/// Aspect-preserving integer dimensions within a hard total-pixel
/// budget (dsh `requestImageDimensions`; the shared request-projection
/// geometry, inward-rounded, no enlargement).
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    clippy::float_cmp,
    reason = "the dsh geometry is float-native; the port mirrors its exact arithmetic including the scale == 1.0 identity arm"
)]
pub fn request_image_dimensions(width: u32, height: u32, max_pixels: u64) -> (u32, u32) {
    let area = u64::from(width) * u64::from(height);
    let scale = if area == 0 {
        1.0
    } else {
        let ratio = max_pixels as f64 / area as f64;
        (1.0_f64).min(ratio.sqrt())
    };
    if scale == 1.0 {
        return (width, height);
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "dimensions are positive and the scale is at most 1; the casts mirror the dsh float arithmetic"
    )]
    if width >= height {
        let mut projected_width = ((f64::from(width) * scale).floor() as u32).max(1);
        let mut projected_height = (((u64::from(projected_width) * u64::from(height)) as f64
            / f64::from(width))
        .round() as u32)
            .max(1);
        while u64::from(projected_width) * u64::from(projected_height) > max_pixels
            && projected_width > 1
        {
            projected_width -= 1;
            projected_height = (((u64::from(projected_width) * u64::from(height)) as f64
                / f64::from(width))
            .round() as u32)
                .max(1);
        }
        (projected_width, projected_height)
    } else {
        let mut projected_height = ((f64::from(height) * scale).floor() as u32).max(1);
        let mut projected_width = (((u64::from(projected_height) * u64::from(width)) as f64
            / f64::from(height))
        .round() as u32)
            .max(1);
        while u64::from(projected_width) * u64::from(projected_height) > max_pixels
            && projected_height > 1
        {
            projected_height -= 1;
            projected_width = (((u64::from(projected_height) * u64::from(width)) as f64
                / f64::from(height))
            .round() as u32)
                .max(1);
        }
        (projected_width, projected_height)
    }
}

/// The lowercase format word (the dsh `mediaType.slice('image/'.length)`
/// face).
#[must_use]
pub const fn media_type_word(media_type: ImageMediaType) -> &'static str {
    match media_type {
        ImageMediaType::Png => "png",
        ImageMediaType::Jpeg => "jpeg",
        ImageMediaType::Webp => "webp",
        ImageMediaType::Gif => "gif",
    }
}

/// Walks PNG chunks after the fixed signature; invokes the callback
/// with each four-byte chunk type.
fn scan_png(data: &[u8], mut hit: impl FnMut([u8; 4])) -> bool {
    const SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    if data.len() < 8 || data[..8] != SIGNATURE {
        return false;
    }
    let mut offset = 8_usize;
    while offset + 8 <= data.len() {
        let length = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        let mut kind = [0_u8; 4];
        kind.copy_from_slice(&data[offset + 4..offset + 8]);
        hit(kind);
        let Some(next) = offset.checked_add(12 + length) else {
            return true;
        };
        if next > data.len() {
            return true;
        }
        offset = next;
    }
    true
}

/// PNG metadata chunk types (the sharp word table: eXIf, iCCP, and the
/// text triple; the color/physics ancillaries are NOT metadata).
const fn png_metadata_chunk(kind: [u8; 4]) -> bool {
    matches!(&kind, b"eXIf" | b"iCCP" | b"tEXt" | b"iTXt" | b"zTXt")
}

/// Walks JPEG segments after the SOI; invokes the callback with each
/// marker byte.
fn scan_jpeg(data: &[u8], mut hit: impl FnMut(u8, &[u8])) -> bool {
    if data.len() < 2 || data[0] != 0xff || data[1] != 0xd8 {
        return false;
    }
    let mut offset = 2_usize;
    while offset + 4 <= data.len() {
        if data[offset] != 0xff {
            return true;
        }
        let marker = data[offset + 1];
        if marker == 0xd9 || marker == 0xda {
            return true;
        }
        let length = u16::from_be_bytes([data[offset + 2], data[offset + 3]]) as usize;
        if offset + 2 + length > data.len() {
            return true;
        }
        hit(
            marker,
            &data[offset + 4..(offset + 2 + length).min(data.len())],
        );
        offset += 2 + length;
    }
    true
}

/// Walks the WebP RIFF sub-chunks (after the 12-byte RIFF header);
/// invokes the callback with each four-byte chunk id.
fn scan_webp(data: &[u8], mut hit: impl FnMut([u8; 4])) -> bool {
    const RIFF: &[u8; 4] = b"RIFF";
    const WEBP: &[u8; 4] = b"WEBP";
    if data.len() < 12 || &data[0..4] != RIFF || &data[8..12] != WEBP {
        return false;
    }
    let mut offset = 12_usize;
    while offset + 8 <= data.len() {
        let length = u32::from_le_bytes([
            data[offset + 4],
            data[offset + 5],
            data[offset + 6],
            data[offset + 7],
        ]) as usize;
        let mut id = [0_u8; 4];
        id.copy_from_slice(&data[offset..offset + 4]);
        hit(id);
        let Some(next) = offset.checked_add(8 + length + (length & 1)) else {
            return true;
        };
        if next > data.len() {
            return true;
        }
        offset = next;
    }
    true
}

/// Gathers the container facts (animation and retained metadata) for
/// one verified media type (the sharp `carriesMetadata`/`animated`
/// word table, container-walked; GIF never reaches the pass-through
/// arm, so its comment extension stays unread).
fn container_facts(media_type: ImageMediaType, data: &[u8]) -> (bool, bool) {
    let mut animated = false;
    let mut carries_metadata = false;
    match media_type {
        ImageMediaType::Png => {
            scan_png(data, |kind| {
                if &kind == b"acTL" {
                    animated = true;
                }
                if png_metadata_chunk(kind) {
                    carries_metadata = true;
                }
            });
        }
        ImageMediaType::Jpeg => {
            scan_jpeg(data, |marker, _payload| {
                // APP1 Exif or XMP, APP2 ICC profile, APP13 Photoshop
                // (the dsh `tifftagPhotoshop` word), COM comments.
                if marker == 0xe1 || marker == 0xe2 || marker == 0xed || marker == 0xfe {
                    carries_metadata = true;
                }
            });
        }
        ImageMediaType::Webp => {
            scan_webp(data, |id| {
                if &id == b"ANIM" || &id == b"ANMF" {
                    animated = true;
                }
                if &id == b"EXIF" || &id == b"XMP " || &id == b"ICCP" {
                    carries_metadata = true;
                }
            });
        }
        ImageMediaType::Gif => {}
    }
    (animated, carries_metadata)
}

/// Decodes one raster and reports the full fact table (dsh
/// `detectImage(data, limits)`).
///
/// The pixel limit check precedes the per-side check; both run over
/// the perceived axes.
///
/// # Errors
///
/// [`AttachmentError`] `INVALID_IMAGE` for a malformed or unsupported
/// raster; `IMAGE_TOO_MANY_PIXELS` then `IMAGE_DIMENSION_TOO_LARGE`
/// for the limit refusals, in that order.
pub fn detect_facts(
    data: &[u8],
    limits: Option<(u64, u32)>,
) -> Result<SourceFacts, AttachmentError> {
    if data.is_empty() {
        return Err(empty_image());
    }
    let reader = image::ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|_error| invalid_image())?;
    let media_type = match reader.format() {
        Some(image::ImageFormat::Png) => ImageMediaType::Png,
        Some(image::ImageFormat::Jpeg) => ImageMediaType::Jpeg,
        Some(image::ImageFormat::Gif) => ImageMediaType::Gif,
        Some(image::ImageFormat::WebP) => ImageMediaType::Webp,
        _ => return Err(invalid_image()),
    };
    let mut decoder = reader.into_decoder().map_err(|_error| invalid_image())?;
    let orientation = decoder.orientation().map_err(|_error| invalid_image())?;
    let decoded_raster =
        image::DynamicImage::from_decoder(decoder).map_err(|_error| invalid_image())?;
    // The perceived axes transpose for orientations 5-8 (the dsh
    // `imageMetadata` word: the viewer's width and height).
    let transposed = matches!(
        orientation,
        image::metadata::Orientation::Rotate90FlipH | image::metadata::Orientation::Rotate270FlipH
    );
    let (width, height) = if transposed {
        (decoded_raster.height(), decoded_raster.width())
    } else {
        (decoded_raster.width(), decoded_raster.height())
    };
    if let Some((max_pixels, max_dimension)) = limits {
        if u64::from(width) * u64::from(height) > max_pixels {
            return Err(AttachmentError::new(
                "Image exceeds the configured decoded-pixel limit.",
                IMAGE_TOO_MANY_PIXELS_CODE,
            ));
        }
        if width.max(height) > max_dimension {
            return Err(AttachmentError::new(
                "Image exceeds the configured per-side pixel limit.",
                IMAGE_DIMENSION_TOO_LARGE_CODE,
            ));
        }
    }
    let depth = match decoded_raster.color() {
        image::ColorType::L16
        | image::ColorType::La16
        | image::ColorType::Rgb16
        | image::ColorType::Rgba16 => DepthWord::Ushort,
        image::ColorType::Rgb32F | image::ColorType::Rgba32F => DepthWord::Float,
        _ => DepthWord::Uchar,
    };
    let (animated, carries_metadata) = container_facts(media_type, data);
    Ok(SourceFacts {
        media_type,
        width,
        height,
        has_alpha: decoded_raster.color().has_alpha(),
        depth,
        space: DetectedSpace::Srgb,
        animated,
        carries_metadata,
        orientation,
    })
}

fn invalid_image() -> AttachmentError {
    AttachmentError::new("Unsupported or malformed image data.", INVALID_IMAGE_CODE)
}

fn empty_image() -> AttachmentError {
    AttachmentError::new("Image is empty.", INVALID_IMAGE_CODE)
}

#[cfg(test)]
mod tests {
    use super::{detect_facts, request_image_dimensions};

    #[test]
    fn geometry_pins_inward_rounding_and_shrink_loop() {
        assert_eq!(request_image_dimensions(100, 100, 10_000), (100, 100));
        assert_eq!(request_image_dimensions(2000, 1000, 1_000_000), (1414, 707));
        assert_eq!(request_image_dimensions(1000, 2000, 1_000_000), (707, 1414));
        assert_eq!(request_image_dimensions(3, 3, 4), (2, 2));
        assert_eq!(request_image_dimensions(1, 1, 1), (1, 1));
    }

    #[test]
    fn empty_bytes_refuse_as_invalid_image() {
        let error = detect_facts(&[], None).expect_err("empty refuses");
        assert_eq!(error.code(), crate::error::INVALID_IMAGE_CODE);
        assert_eq!(error.message(), "Image is empty.");
    }

    #[test]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the test rasters are far below one byte per channel"
    )]
    fn clean_png_reports_no_metadata_and_upright_facts() {
        let pixels = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(4, 3, |x, y| {
            image::Rgba([x as u8, y as u8, 7, 255])
        }));
        let mut bytes = Vec::new();
        pixels
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("encodes");
        let facts = detect_facts(&bytes, None).expect("detects");
        assert_eq!(facts.media_type, fr_wire::ImageMediaType::Png);
        assert_eq!((facts.width, facts.height), (4, 3));
        assert!(facts.has_alpha);
        assert!(!facts.animated);
        // The image crate's own encoder emits no metadata chunks.
        assert!(!facts.carries_metadata);
        assert!(facts.is_uchar());
    }
}
