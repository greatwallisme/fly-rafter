//! Deterministic cached request-image versions (dsh attachment-local
//! `request-image.ts` + `encoding.ts` + `image.ts`, the request-image face
//! of the Q71-B ruling).
//!
//! The fr home is the fr-attachment plugin (2026-10-27 Q78-A): the
//! pipeline is the attachment domain's normalization face. The
//! wire-visible facts are verbatim to dsh:
//! the transform version, the descriptor fields, the quality ladder, the
//! alpha-to-WebP / opaque-to-JPEG codec order, the pass-through rule, the
//! verify contract, and the cache layout. Pixel-level encoder output is
//! not byte-identical to sharp/libvips (codec-internal difference, the
//! same nature as a libvips version drift; PRD decision 41).
//!
//! `sharp`-specific facilities that have no blocking-face counterpart are
//! not ported: `compression-limiter.ts` bounds native transforms on the
//! JS event loop, while the fr pipeline runs on the caller's thread.

use crate::request_image::{ImageColorSpace, ImageSampleDepth, RequestImageAttachment};
use fr_llm::adapter::AbortSignal;
use fr_wire::{ImageAttachmentRef, ImageMediaType};
use image::ColorType;
use image::ImageEncoder as _;
use image::ImageReader;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::io::Cursor;
use std::io::Read as _;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

/// Stable attachment failure code: a malformed or unsupported raster.
pub const INVALID_IMAGE_CODE: &str = "INVALID_IMAGE";
/// Stable attachment failure code: a malformed durable reference or target.
pub const INVALID_ATTACHMENT_REF_CODE: &str = "INVALID_ATTACHMENT_REF";
/// Stable attachment failure code: an encoded output failed verification.
pub const ATTACHMENT_WRITE_FAILED_CODE: &str = "ATTACHMENT_WRITE_FAILED";
/// Caller-cancellation marker; the adapter classifies it by inspecting the
/// signal before this error value (dsh rethrows the raw abort reason).
pub const ABORTED_CODE: &str = "ABORTED";

/// Stable attachment failure (dsh `AttachmentError` shape: message plus
/// machine-routing code, no raw bytes or host paths).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentError {
    message: String,
    code: &'static str,
}

impl AttachmentError {
    fn new(message: impl Into<String>, code: &'static str) -> Self {
        Self {
            message: message.into(),
            code,
        }
    }

    /// The human-readable failure description.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The stable machine-routing code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl std::fmt::Display for AttachmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AttachmentError {}

fn invalid_image() -> AttachmentError {
    AttachmentError::new("Unsupported or malformed image data.", INVALID_IMAGE_CODE)
}

fn ensure_live(signal: Option<&AbortSignal>) -> Result<(), AttachmentError> {
    match signal {
        Some(signal) if signal.is_aborted() => Err(AttachmentError::new(
            "The request was aborted.",
            ABORTED_CODE,
        )),
        _ => Ok(()),
    }
}

/// Sharp sample depth reported for the decoded channels (dsh `depth`;
/// only the 8-bit-unsigned versus anything-else distinction is consulted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedDepth {
    /// Eight-bit unsigned samples (dsh `'uchar'`).
    Uchar,
    /// Any other sample depth (16-bit family, float samples).
    Other,
}

/// Sharp colour space reported for the decoded pixels (dsh `space`).
///
/// The image crate performs no colour management: every decode yields the
/// stored channel data, and the only space this pipeline reads or writes
/// is sRGB (admission-normalized inputs; our own encoders). dsh's space
/// check is unreachable defense on those real paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedSpace {
    /// sRGB (dsh `'srgb'`).
    Srgb,
    /// Any other reported space.
    Other,
}

/// Decoded metadata from a supported raster (dsh `DetectedImage`, the
/// request-image subset: media type, dimensions, alpha, depth, space).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedImage {
    /// Media type verified from the bytes.
    pub media_type: ImageMediaType,
    /// Intrinsic width in pixels.
    pub width: u32,
    /// Intrinsic height in pixels.
    pub height: u32,
    /// Whether the decoded pixels carry an alpha channel.
    pub has_alpha: bool,
    /// Reported sample depth.
    pub depth: DetectedDepth,
    /// Reported colour space.
    pub space: DetectedSpace,
}

/// Fully decode one supported raster and report its facts (dsh
/// `detectImage`);
///
/// the header-only `probeImage` distinction is a JS perf
/// optimization the request-image face does not observe, so both faces
/// decode here).
///
/// # Errors
///
/// [`AttachmentError`] with [`INVALID_IMAGE_CODE`] for any malformed or
/// unsupported raster.
pub fn detect_image(data: &[u8]) -> Result<DetectedImage, AttachmentError> {
    let reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|_error| invalid_image())?;
    let media_type = match reader.format() {
        Some(image::ImageFormat::Png) => ImageMediaType::Png,
        Some(image::ImageFormat::Jpeg) => ImageMediaType::Jpeg,
        Some(image::ImageFormat::Gif) => ImageMediaType::Gif,
        Some(image::ImageFormat::WebP) => ImageMediaType::Webp,
        _ => return Err(invalid_image()),
    };
    let decoded = reader.decode().map_err(|_error| invalid_image())?;
    let depth = match decoded.color() {
        ColorType::L8 | ColorType::La8 | ColorType::Rgb8 | ColorType::Rgba8 => DetectedDepth::Uchar,
        _ => DetectedDepth::Other,
    };
    Ok(DetectedImage {
        media_type,
        width: decoded.width(),
        height: decoded.height(),
        has_alpha: decoded.color().has_alpha(),
        depth,
        space: DetectedSpace::Srgb,
    })
}

/// Check alpha metadata for bytes produced by this package's encoders
/// (dsh `encodedAlphaIsCompatible`):
///
/// the WebP encoder may omit an all-opaque alpha plane, so only a lost
/// alpha on WebP output stays compatible with an alpha-bearing source.
#[must_use]
pub fn encoded_alpha_is_compatible(
    source_has_alpha: Option<bool>,
    output: (ImageMediaType, bool),
) -> bool {
    source_has_alpha.is_none_or(|source| {
        source == output.1 || (source && !output.1 && output.0 == ImageMediaType::Webp)
    })
}

/// Transform version included in every cache and upload-index identity
/// (dsh `REQUEST_IMAGE_TRANSFORM_VERSION`).
pub const REQUEST_IMAGE_TRANSFORM_VERSION: &str = "request-image-v6";
/// Shared ladder for both encoders: spaced so each step buys a real size
/// reduction (dsh `IMAGE_ENCODING_QUALITIES`).
pub const IMAGE_ENCODING_QUALITIES: [u32; 3] = [85, 75, 60];
/// Fixed lossy-WebP effort; deeper search costs 3-4x encode time for
/// about 5% size (dsh `WEBP_ENCODING_EFFORT`, libwebp `method`).
pub const WEBP_ENCODING_EFFORT: i32 = 0;

/// The exact descriptor string one identity digests (dsh `descriptor`):
/// field order and JSON spelling are load-bearing for `variant_id`.
fn descriptor(attachment: &ImageAttachmentRef, target: (u32, u32, u64)) -> String {
    format!(
        concat!(
            "{{\"transformVersion\":\"{version}\",",
            "\"attachmentId\":\"{id}\",",
            "\"targetWidth\":{w},\"targetHeight\":{h},",
            "\"encodedByteBudget\":{max_bytes},",
            "\"encoding\":{{\"webpQualities\":[{q0},{q1},{q2}],",
            "\"webpEffort\":{effort},",
            "\"jpegQualities\":[{q0},{q1},{q2}],",
            "\"order\":[\"alpha:webp\",\"opaque:jpeg\"],",
            "\"colourspace\":\"srgb\"}}}}"
        ),
        version = REQUEST_IMAGE_TRANSFORM_VERSION,
        id = attachment.attachment_id.as_str(),
        w = target.0,
        h = target.1,
        max_bytes = target.2,
        q0 = IMAGE_ENCODING_QUALITIES[0],
        q1 = IMAGE_ENCODING_QUALITIES[1],
        q2 = IMAGE_ENCODING_QUALITIES[2],
        effort = WEBP_ENCODING_EFFORT,
    )
}

/// Complete deterministic identity for one attachment and route-chosen
/// request target (dsh `requestImageVariantId`): the branded digest over
/// every request transform input, spelled `sha256:<hex>`.
#[must_use]
pub fn request_image_variant_id(
    attachment: &ImageAttachmentRef,
    target: (u32, u32, u64),
) -> String {
    let digest = Sha256::digest(descriptor(attachment, target).as_bytes());
    let mut hex = String::with_capacity(64 + 7);
    hex.push_str("sha256:");
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// One ladder output carrying its complete bytes and exact facts (dsh
/// `EncodedImage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedImage {
    /// Complete encoded bytes.
    pub data: Vec<u8>,
    /// Encoded media type (WebP keeps source alpha, JPEG drops it).
    pub media_type: ImageMediaType,
    /// Encoded width in pixels.
    pub width: u32,
    /// Encoded height in pixels.
    pub height: u32,
}

fn encode_jpeg(
    rgb: &[u8],
    width: u32,
    height: u32,
    quality: u32,
) -> Result<EncodedImage, AttachmentError> {
    let mut data = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
        &mut data,
        u8::try_from(quality).unwrap_or(u8::MAX),
    );
    encoder
        .write_image(rgb, width, height, image::ExtendedColorType::Rgb8)
        .map_err(|_error| {
            AttachmentError::new(
                "Image compression task rejected with a non-Error value.",
                ATTACHMENT_WRITE_FAILED_CODE,
            )
        })?;
    Ok(EncodedImage {
        data,
        media_type: ImageMediaType::Jpeg,
        width,
        height,
    })
}

/// Encode one RGBA raster with the lossy WebP advanced API: the quality
/// ladder value and `method = WEBP_ENCODING_EFFORT` both reach libwebp,
/// which the thin `webp` wrapper cannot express.
///
/// # Errors
///
/// [`AttachmentError`] with [`ATTACHMENT_WRITE_FAILED_CODE`] when the
/// native encoder rejects the raster or the encode fails.
fn encode_webp(
    rgba: &[u8],
    width: u32,
    height: u32,
    quality: f32,
) -> Result<EncodedImage, AttachmentError> {
    let failed = || {
        AttachmentError::new(
            "Image compression task rejected with a non-Error value.",
            ATTACHMENT_WRITE_FAILED_CODE,
        )
    };
    let mut config = libwebp_sys::WebPConfig::new_with_preset(
        libwebp_sys::WebPPreset::WEBP_PRESET_DEFAULT,
        quality,
    )
    .map_err(|()| failed())?;
    config.method = WEBP_ENCODING_EFFORT;
    let picture_width = i32::try_from(width).map_err(|_error| failed())?;
    let picture_height = i32::try_from(height).map_err(|_error| failed())?;
    let stride = picture_width.checked_mul(4).ok_or_else(failed)?;
    let mut picture = libwebp_sys::WebPPicture::new().map_err(|()| failed())?;
    picture.width = picture_width;
    picture.height = picture_height;
    let mut writer = std::mem::MaybeUninit::<libwebp_sys::WebPMemoryWriter>::uninit();
    // SAFETY: `writer` points to one `WebPMemoryWriter` worth of storage.
    unsafe { libwebp_sys::WebPMemoryWriterInit(writer.as_mut_ptr()) };
    // SAFETY: `WebPMemoryWriterInit` fully initialized the value.
    let mut writer = unsafe { writer.assume_init() };
    picture.writer = Some(libwebp_sys::WebPMemoryWrite);
    picture.custom_ptr = std::ptr::from_mut(&mut writer).cast::<std::ffi::c_void>();

    // SAFETY: `rgba` holds `stride * height` initialized bytes for the
    // declared extent; the picture is freed on every path below before
    // the writer contents are adopted or cleared.
    let imported = unsafe {
        libwebp_sys::WebPPictureImportRGBA(std::ptr::from_mut(&mut picture), rgba.as_ptr(), stride)
    };
    // SAFETY: `config` and `picture` were initialized by the calls above.
    let encoded = imported != 0
        && unsafe {
            libwebp_sys::WebPEncode(
                std::ptr::from_ref(&config),
                std::ptr::from_mut(&mut picture),
            )
        } != 0;
    // SAFETY: the picture was initialized once above and freed once here.
    unsafe { libwebp_sys::WebPPictureFree(std::ptr::from_mut(&mut picture)) };
    if !encoded {
        // SAFETY: the writer was initialized once above and is cleared
        // exactly once here; ownership of its buffer is never adopted on
        // this path.
        unsafe { libwebp_sys::WebPMemoryWriterClear(std::ptr::from_mut(&mut writer)) };
        return Err(failed());
    }
    // SAFETY: a successful encode leaves `writer.mem` pointing at
    // `writer.size` initialized bytes owned by the writer.
    let data = unsafe { std::slice::from_raw_parts(writer.mem, writer.size) }.to_vec();
    // SAFETY: the writer was initialized once above and is cleared exactly
    // once here, after its buffer bytes were copied out.
    unsafe { libwebp_sys::WebPMemoryWriterClear(std::ptr::from_mut(&mut writer)) };
    Ok(EncodedImage {
        data,
        media_type: ImageMediaType::Webp,
        width,
        height,
    })
}

/// Result of exhausting candidates at one raster size (dsh
/// `ExhaustedEncoding`): the smallest completed fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LadderOutcome {
    /// The first candidate within the byte target.
    Fit(EncodedImage),
    /// Every candidate exceeded the target; the smallest wins.
    Smallest(EncodedImage),
}

/// Execute encoding candidates in preference order and stop after the
/// first fitting output (dsh `encodeFirstWithinLimit`).
///
/// # Errors
///
/// Propagates the first candidate error; encoder failures fail the
/// request like any other production fault.
pub fn encode_first_within_limit(
    attempts: Vec<Box<dyn FnOnce() -> Result<EncodedImage, AttachmentError>>>,
    max_bytes: u64,
) -> Result<LadderOutcome, AttachmentError> {
    let mut attempts = attempts.into_iter();
    let Some(first) = attempts.next() else {
        // Internal invariant: the ladder always builds one candidate per
        // quality entry, and the quality table is non-empty.
        unreachable!(
            "image encoding requires at least one candidate; the quality ladder constant is non-empty by construction"
        )
    };
    let mut smallest = first()?;
    if smallest.data.len() as u64 <= max_bytes {
        return Ok(LadderOutcome::Fit(smallest));
    }
    for attempt in attempts {
        let candidate = attempt()?;
        if candidate.data.len() as u64 <= max_bytes {
            return Ok(LadderOutcome::Fit(candidate));
        }
        if candidate.data.len() < smallest.data.len() {
            smallest = candidate;
        }
    }
    Ok(LadderOutcome::Smallest(smallest))
}

/// Verified request bytes beside the alpha fact (dsh
/// `VerifiedRequestImage`).
#[derive(Debug, Clone)]
struct VerifiedRequestImage {
    data: Vec<u8>,
    media_type: ImageMediaType,
    width: u32,
    height: u32,
    has_alpha: bool,
}

/// One normalized attachment beside its verified bytes (dsh
/// `StoredImageAttachment`).
#[derive(Debug, Clone)]
pub struct StoredImage {
    /// Durable normalized attachment reference.
    pub reference: ImageAttachmentRef,
    /// Verified stored bytes.
    pub data: Vec<u8>,
}

/// Validate one route-chosen target (dsh `validateTarget`): every field
/// positive, in declaration order.
fn validate_target(target: (u32, u32, u64)) -> Result<(), AttachmentError> {
    let positive = |value: u64, name: &str| {
        if value == 0 {
            return Err(AttachmentError::new(
                format!("{name} must be a positive integer."),
                INVALID_ATTACHMENT_REF_CODE,
            ));
        }
        Ok(())
    };
    positive(u64::from(target.0), "Image request width")?;
    positive(u64::from(target.1), "Image request height")?;
    positive(target.2, "Image request maxBytes")
}

/// Resize by the source long edge only, so the encoder derives the short
/// edge as the route predicts (dsh `pipeline`): a single-edge constraint
/// with no enlargement.
fn resized_long_edge(source: &image::DynamicImage, target: (u32, u32)) -> image::DynamicImage {
    let (src_w, src_h) = (source.width(), source.height());
    let by_width = src_w >= src_h;
    let desired = if by_width {
        target.0.min(src_w)
    } else {
        target.1.min(src_h)
    };
    let shrinks = if by_width {
        desired < src_w
    } else {
        desired < src_h
    };
    if !shrinks {
        return source.clone();
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "std has no fallible float-to-integer conversion; pixel dimensions are non-negative and far below 2^32, and the saturating cast mirrors the sharp long-edge resize"
    )]
    let (new_w, new_h) = if by_width {
        let height =
            ((f64::from(src_h) * f64::from(desired) / f64::from(src_w)).round() as u32).max(1);
        (desired, height)
    } else {
        let width =
            ((f64::from(src_w) * f64::from(desired) / f64::from(src_h)).round() as u32).max(1);
        (width, desired)
    };
    image::DynamicImage::ImageRgba8(image::imageops::resize(
        source,
        new_w,
        new_h,
        image::imageops::FilterType::Lanczos3,
    ))
}

/// Encode one prepared raster through the codec ladder: WebP keeps a
/// source alpha channel, everything else is JPEG (dsh `encodingLadder`),
/// each candidate built lazily from highest to lowest quality.
fn encode_ladder(
    pixels: &image::RgbaImage,
    has_alpha: bool,
    max_bytes: u64,
) -> Result<EncodedImage, AttachmentError> {
    let (width, height) = pixels.dimensions();
    let rgba = pixels.as_raw().clone();
    let media_type = if has_alpha {
        ImageMediaType::Webp
    } else {
        ImageMediaType::Jpeg
    };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "std has no fallible conversion to f32; ladder qualities are small exact integers far below the 24-bit f32 mantissa"
    )]
    let attempts: Vec<Box<dyn FnOnce() -> Result<EncodedImage, AttachmentError>>> =
        IMAGE_ENCODING_QUALITIES
            .iter()
            .copied()
            .map(|quality| {
                let rgba = rgba.clone();
                Box::new(move || {
                    if media_type == ImageMediaType::Webp {
                        encode_webp(&rgba, width, height, f64::from(quality) as f32)
                    } else {
                        let image =
                            image::RgbaImage::from_raw(width, height, rgba).ok_or_else(|| {
                                AttachmentError::new(
                                    "Image compression task rejected with a non-Error value.",
                                    ATTACHMENT_WRITE_FAILED_CODE,
                                )
                            })?;
                        let rgb = image::DynamicImage::ImageRgba8(image).to_rgb8();
                        encode_jpeg(rgb.as_raw(), width, height, quality)
                    }
                }) as Box<dyn FnOnce() -> Result<EncodedImage, AttachmentError>>
            })
            .collect();
    match encode_first_within_limit(attempts, max_bytes)? {
        LadderOutcome::Fit(encoded) | LadderOutcome::Smallest(encoded) => Ok(encoded),
    }
}

/// Generate or reuse one request image below the local attachment cache
/// root (dsh `readRequestImageFile`).
///
/// # Errors
///
/// [`AttachmentError`] for a malformed target or raster, a failed encode
/// or verify, or caller cancellation.
pub fn read_request_image_file(
    root: &Path,
    attachment: &StoredImage,
    target: (u32, u32, u64),
    signal: Option<&AbortSignal>,
) -> Result<RequestImageAttachment, AttachmentError> {
    ensure_live(signal)?;
    validate_target(target)?;
    let source = detect_image(&attachment.data)?;
    let variant_id = request_image_variant_id(&attachment.reference, target);
    let hash = variant_id
        .strip_prefix("sha256:")
        .map_or_else(|| variant_id.clone(), str::to_owned);
    let path = cache_path(root, &hash);
    let cached = read_cached(&path, target, source.has_alpha, signal)?;
    let from_cache = cached.is_some();
    let version = if let Some(version) = cached {
        version
    } else {
        let created = create_request_image(attachment, target, source.has_alpha)?;
        if created.data == attachment.data {
            VerifiedRequestImage {
                has_alpha: source.has_alpha,
                ..created
            }
        } else {
            verify_request_image(created, source.has_alpha)?
        }
    };
    ensure_live(signal)?;
    if !from_cache && version.data != attachment.data {
        write_cached(&path, &version.data, signal)?;
    }
    Ok(RequestImageAttachment {
        variant_id,
        attachment: attachment.reference.clone(),
        bytes: version.data.len() as u64,
        data: version.data,
        media_type: version.media_type,
        width: version.width,
        height: version.height,
        depth: ImageSampleDepth::Uchar,
        space: ImageColorSpace::Srgb,
        has_alpha: version.has_alpha,
    })
}

/// Pass through the source when the target already covers it and the
/// bytes fit; otherwise resize and run the quality ladder, keeping the
/// smallest output when every quality exceeds the byte target (dsh
/// `createRequestImage`).
fn create_request_image(
    attachment: &StoredImage,
    target: (u32, u32, u64),
    has_alpha: bool,
) -> Result<VerifiedRequestImage, AttachmentError> {
    if u64::from(target.0) >= u64::from(attachment.reference.width)
        && u64::from(target.1) >= u64::from(attachment.reference.height)
        && attachment.data.len() as u64 <= target.2
    {
        return Ok(VerifiedRequestImage {
            data: attachment.data.clone(),
            media_type: attachment.reference.media_type,
            width: attachment.reference.width,
            height: attachment.reference.height,
            // Overwritten by the caller with the source probe result (dsh
            // spreads `hasAlpha: source.hasAlpha` over this arm).
            has_alpha: false,
        });
    }
    let source = image::ImageReader::new(Cursor::new(&attachment.data))
        .with_guessed_format()
        .map_err(|_error| invalid_image())?
        .decode()
        .map_err(|_error| invalid_image())?;
    let resized = resized_long_edge(&source, (target.0, target.1));
    let encoded = encode_ladder(&resized.to_rgba8(), has_alpha, target.2)?;
    Ok(VerifiedRequestImage {
        data: encoded.data,
        media_type: encoded.media_type,
        width: encoded.width,
        height: encoded.height,
        has_alpha,
    })
}

/// The cache path of one variant hash (dsh `cachePath`): the
/// `request-images` child of the root, sharded by the first hex byte.
#[must_use]
pub fn cache_path(root: &Path, hash: &str) -> PathBuf {
    root.join("request-images")
        .join(hash.get(0..2).unwrap_or(hash))
        .join(hash)
}

/// Read one cached variant and re-derive its facts; any read, decode, or
/// validation problem counts as a miss and the variant regenerates (dsh
/// `readCached`: ENOENT and foreign read errors both return undefined,
/// abort rethrows).
fn read_cached(
    path: &Path,
    target: (u32, u32, u64),
    expected_alpha: bool,
    signal: Option<&AbortSignal>,
) -> Result<Option<VerifiedRequestImage>, AttachmentError> {
    let read = std::fs::read(path);
    // Any read failure is a cache miss; an abort mid-read rethrows
    // (dsh: ENOENT -> undefined, `signal?.throwIfAborted()` first).
    let Ok(data) = read else {
        ensure_live(signal)?;
        return Ok(None);
    };
    let Ok(detected) = detect_image(&data) else {
        ensure_live(signal)?;
        return Ok(None);
    };
    if detected.depth != DetectedDepth::Uchar
        || detected.space != DetectedSpace::Srgb
        || detected.width > target.0
        || detected.height > target.1
        || !encoded_alpha_is_compatible(
            Some(expected_alpha),
            (detected.media_type, detected.has_alpha),
        )
    {
        return Ok(None);
    }
    Ok(Some(VerifiedRequestImage {
        data,
        media_type: detected.media_type,
        width: detected.width,
        height: detected.height,
        has_alpha: detected.has_alpha,
    }))
}

/// Assert that an encoded output is an 8-bit sRGB single-frame image with
/// matching facts (dsh `verifyRequestImage`).
fn verify_request_image(
    image: VerifiedRequestImage,
    expected_alpha: bool,
) -> Result<VerifiedRequestImage, AttachmentError> {
    let detected = detect_image(&image.data)?;
    let matches = detected.media_type == image.media_type
        && detected.width == image.width
        && detected.height == image.height
        && detected.depth == DetectedDepth::Uchar
        && detected.space == DetectedSpace::Srgb
        && encoded_alpha_is_compatible(
            Some(expected_alpha),
            (detected.media_type, detected.has_alpha),
        );
    if !matches {
        return Err(AttachmentError::new(
            "Encoded model-request image does not match its verified 8-bit sRGB metadata.",
            ATTACHMENT_WRITE_FAILED_CODE,
        ));
    }
    Ok(VerifiedRequestImage {
        has_alpha: detected.has_alpha,
        ..image
    })
}

/// Shared random hex suffix source for scratch or boundary names (the
/// upload multipart boundary and the cache scratch suffix).
///
/// The bytes come from `/dev/urandom` (dsh `randomUUID`; M1 is POSIX-only
/// per PRD 19.2 item 15, so `/dev/urandom` is the single source, mirroring
/// the fr-session decision).
#[must_use]
pub fn random_hex_suffix() -> String {
    random_tmp_suffix()
}

fn random_tmp_suffix() -> String {
    let mut bytes = [0u8; 16];
    // Read failures degrade to a counter-stable suffix; the name only has
    // to be unique among concurrent writers of this process.
    let source =
        std::fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes));
    if source.is_err() {
        static FALLBACK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = FALLBACK.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        bytes[..8].copy_from_slice(&seq.to_be_bytes());
    }
    let mut hex = String::with_capacity(32);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Publish one variant atomically: a `0600` scratch file plus rename
/// under a `0700` directory (dsh `writeCached`; the `finally` cleanup of
/// the scratch name maps to the best-effort removal below).
fn write_cached(
    path: &Path,
    data: &[u8],
    signal: Option<&AbortSignal>,
) -> Result<(), AttachmentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_error| {
            AttachmentError::new(
                "Image normalization did not produce a single-frame 8-bit sRGB image with matching metadata.",
                ATTACHMENT_WRITE_FAILED_CODE,
            )
        })?;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let scratch = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("variant"),
        random_tmp_suffix()
    ));
    let write = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&scratch)
        .and_then(|mut file| file.write_all(data));
    if write.is_ok() && std::fs::rename(&scratch, path).is_ok() {
        return Ok(());
    }
    let _ = std::fs::remove_file(&scratch);
    ensure_live(signal)?;
    Err(AttachmentError::new(
        "Image normalization did not produce a single-frame 8-bit sRGB image with matching metadata.",
        ATTACHMENT_WRITE_FAILED_CODE,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_wire::ids::AttachmentId;

    fn reference(id: &str, width: u32, height: u32, bytes: u64) -> ImageAttachmentRef {
        ImageAttachmentRef {
            attachment_id: AttachmentId::new(id),
            media_type: ImageMediaType::Png,
            bytes,
            width,
            height,
            name: None,
            original_dimensions: None,
        }
    }

    fn png_bytes(width: u32, height: u32, with_alpha: bool) -> Vec<u8> {
        let image = if with_alpha {
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(width, height, |x, _| {
                if x == 0 {
                    image::Rgba([0, 0, 0, 0])
                } else {
                    image::Rgba([200, 10, 10, 255])
                }
            }))
        } else {
            image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(width, height, |_, _| {
                image::Rgb([200, 10, 10])
            }))
        };
        let mut data = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut data), image::ImageFormat::Png)
            .unwrap();
        data
    }

    /// The descriptor spelling and the digest over it are load-bearing for
    /// the cache and upload-index identity; the expected digest was
    /// computed independently (python hashlib) over the exact string.
    #[test]
    fn descriptor_and_variant_id_pin_dsh_identity() {
        let attachment = reference("sha256:abc", 64, 48, 100);
        let variant = request_image_variant_id(&attachment, (32, 32, 512));
        assert_eq!(
            variant,
            "sha256:be2e08caf897b12548ee81391d2357b87ffd33e680b13409ff5a71bdfb1c55b3"
        );
    }

    /// Only a lost alpha plane on WebP output stays compatible with an
    /// alpha-bearing source; every other addition or removal fails.
    #[test]
    fn encoded_alpha_compatibility_pins_dsh_arms() {
        assert!(encoded_alpha_is_compatible(
            None,
            (ImageMediaType::Jpeg, false)
        ));
        assert!(encoded_alpha_is_compatible(
            Some(true),
            (ImageMediaType::Webp, false)
        ));
        assert!(encoded_alpha_is_compatible(
            Some(true),
            (ImageMediaType::Webp, true)
        ));
        assert!(!encoded_alpha_is_compatible(
            Some(true),
            (ImageMediaType::Jpeg, false)
        ));
        assert!(encoded_alpha_is_compatible(
            Some(false),
            (ImageMediaType::Jpeg, false)
        ));
        assert!(!encoded_alpha_is_compatible(
            Some(false),
            (ImageMediaType::Webp, true)
        ));
    }

    /// Full decode reports the request-image facts; malformed bytes and
    /// unsupported formats reject with the dsh `INVALID_IMAGE` message.
    #[test]
    fn detect_image_pins_facts_and_rejects() {
        let opaque = detect_image(&png_bytes(8, 6, false)).unwrap();
        assert_eq!(opaque.media_type, ImageMediaType::Png);
        assert_eq!((opaque.width, opaque.height), (8, 6));
        assert!(!opaque.has_alpha);
        assert_eq!(opaque.depth, DetectedDepth::Uchar);
        assert_eq!(opaque.space, DetectedSpace::Srgb);

        let transparent = detect_image(&png_bytes(4, 4, true)).unwrap();
        assert!(transparent.has_alpha);

        // A 16-bit PNG decodes but is not 8-bit-unsigned.
        let wide = image::DynamicImage::ImageRgb16(image::ImageBuffer::from_fn(2, 2, |_, _| {
            image::Rgb([65535u16, 0, 0])
        }));
        let mut data = Vec::new();
        wide.write_to(&mut Cursor::new(&mut data), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(detect_image(&data).unwrap().depth, DetectedDepth::Other);

        let malformed = detect_image(b"not an image").unwrap_err();
        assert_eq!(malformed.code(), INVALID_IMAGE_CODE);
        assert_eq!(malformed.message(), "Unsupported or malformed image data.");
    }

    /// Target validation names the first zero field with the dsh text.
    #[test]
    fn validate_target_pins_positive_messages() {
        assert_eq!(
            validate_target((0, 32, 512)).unwrap_err().message(),
            "Image request width must be a positive integer."
        );
        assert_eq!(
            validate_target((32, 0, 512)).unwrap_err().message(),
            "Image request height must be a positive integer."
        );
        assert_eq!(
            validate_target((32, 32, 0)).unwrap_err().message(),
            "Image request maxBytes must be a positive integer."
        );
        assert!(matches!(validate_target((32, 32, 512)), Ok(())));
    }

    /// The ladder stops at the first fitting candidate and keeps the
    /// smallest completed fallback when every candidate exceeds the
    /// target.
    #[test]
    fn ladder_picks_first_fit_else_smallest() {
        let candidate = |size: usize| {
            Box::new(move || {
                Ok(EncodedImage {
                    data: vec![0u8; size],
                    media_type: ImageMediaType::Jpeg,
                    width: 1,
                    height: 1,
                })
            }) as Box<dyn FnOnce() -> Result<EncodedImage, AttachmentError>>
        };
        let fit =
            encode_first_within_limit(vec![candidate(100), candidate(80), candidate(60)], 120)
                .unwrap();
        assert_eq!(
            fit,
            LadderOutcome::Fit(EncodedImage {
                data: vec![0u8; 100],
                media_type: ImageMediaType::Jpeg,
                width: 1,
                height: 1,
            })
        );
        let smallest =
            encode_first_within_limit(vec![candidate(100), candidate(80), candidate(60)], 50)
                .unwrap();
        assert!(matches!(smallest, LadderOutcome::Smallest(ref image) if image.data.len() == 60));
    }

    /// A covering target with fitting bytes passes the source through
    /// byte-identically and writes no cache artifact.
    #[test]
    fn passthrough_pins_byte_identity_and_no_cache_write() {
        let root = std::env::temp_dir().join(format!("fr-rip-passthrough-{}", std::process::id()));
        let data = png_bytes(8, 8, false);
        let attachment = StoredImage {
            reference: reference("sha256:pass", 8, 8, data.len() as u64),
            data: data.clone(),
        };
        let version = read_request_image_file(
            &root,
            &attachment,
            (16, 16, u64::try_from(data.len()).unwrap()),
            None,
        )
        .unwrap();
        assert_eq!(version.data, data);
        assert_eq!(version.media_type, ImageMediaType::Png);
        assert!(!version.has_alpha);
        assert_eq!(version.bytes, data.len() as u64);
        assert!(!cache_path(&root, &version.variant_id[7..]).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A tiny byte target forces the opaque ladder to JPEG; the output
    /// verifies against its source alpha and the variant is published to
    /// the cache for the next call.
    #[test]
    fn ladder_encodes_verifies_and_caches() {
        let root = std::env::temp_dir().join(format!("fr-rip-ladder-{}", std::process::id()));
        let data = png_bytes(8, 8, false);
        let attachment = StoredImage {
            reference: reference("sha256:ladder", 8, 8, data.len() as u64),
            data: data.clone(),
        };
        let target = (8, 8, 16);
        let version = read_request_image_file(&root, &attachment, target, None).unwrap();
        assert_eq!(version.media_type, ImageMediaType::Jpeg);
        assert_ne!(version.data, data);
        assert!(!version.has_alpha);
        assert_eq!((version.width, version.height), (8, 8));
        assert_eq!(version.depth, ImageSampleDepth::Uchar);
        assert_eq!(version.space, ImageColorSpace::Srgb);
        let cached_path = cache_path(&root, &version.variant_id[7..]);
        assert!(cached_path.exists());
        // The second call reads the same variant from the cache.
        let again = read_request_image_file(&root, &attachment, target, None).unwrap();
        assert_eq!(again.data, version.data);
        assert_eq!(again.variant_id, version.variant_id);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An alpha-bearing source runs the WebP ladder and the encoded
    /// output keeps its transparency through verification.
    #[test]
    fn alpha_source_encodes_webp() {
        let root = std::env::temp_dir().join(format!("fr-rip-alpha-{}", std::process::id()));
        let data = png_bytes(8, 8, true);
        let attachment = StoredImage {
            reference: reference("sha256:alpha", 8, 8, data.len() as u64),
            data,
        };
        let version = read_request_image_file(&root, &attachment, (4, 4, 1_048_576), None).unwrap();
        assert_eq!(version.media_type, ImageMediaType::Webp);
        assert!(version.has_alpha);
        assert_eq!((version.width, version.height), (4, 4));
        let _ = std::fs::remove_dir_all(&root);
    }
}
