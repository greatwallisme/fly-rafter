// Rust guideline compliant 2026-10-30
//! The model-facing `read_image` tool body (dsh `tool-fs/read-image.ts`,
//! call-for-call twin).
//!
//! A path without a file extension is identified from its file
//! signature, while the attachment service's full decode stays
//! authoritative. The route gate is deliberately stricter than the host
//! upload preflight: an image-reading tool is useful only when the
//! exact calling route can inspect its result, so unknown capability
//! refuses instead of relying on an adapter failure after filesystem
//! and attachment work.

use serde_json::{Value, json};

use crate::fs_domain::FsDomain;
use crate::fs_types::{FsKind, FsObservation};
use crate::observation::ObservedStateGate;
use crate::tools::BodyError;
use crate::tools::Seams;

/// The media types `read_image` accepts by extension (dsh
/// `IMAGE_EXTENSIONS`; magic-byte validation at the attachment service
/// stays authoritative).
fn media_type_for_extension(extension: &str) -> Option<fr_wire::ImageMediaType> {
    match extension {
        ".png" => Some(fr_wire::ImageMediaType::Png),
        ".jpg" | ".jpeg" => Some(fr_wire::ImageMediaType::Jpeg),
        ".webp" => Some(fr_wire::ImageMediaType::Webp),
        ".gif" => Some(fr_wire::ImageMediaType::Gif),
        _ => None,
    }
}

/// Maps a model-supplied path to its declared image media type by
/// extension (dsh `imageMediaTypeForPath`).
///
/// The node `extname` word: the suffix from the last dot of the last
/// segment, lowercased; a dot file has none.
#[must_use]
pub fn image_media_type_for_path(file_path: &str) -> Option<fr_wire::ImageMediaType> {
    let last_segment = file_path.rsplit(['/', '\\']).next().unwrap_or(file_path);
    let extension = match last_segment.rfind('.') {
        // A leading dot with no other dot is a dot file, not an
        // extension (the node `extname` shape).
        Some(0) | None => "",
        Some(index) => &last_segment[index..],
    };
    media_type_for_extension(&extension.to_ascii_lowercase())
}

/// Identifies the media type declared by a supported image file
/// signature (dsh `sniffImageMediaType`).
#[must_use]
pub fn sniff_image_media_type(data: &[u8]) -> Option<fr_wire::ImageMediaType> {
    fn matches_bytes(data: &[u8], offset: usize, expected: &[u8]) -> bool {
        data.len() >= offset + expected.len() && data[offset..offset + expected.len()] == *expected
    }
    fn matches_ascii(data: &[u8], offset: usize, value: &str) -> bool {
        matches_bytes(data, offset, value.as_bytes())
    }
    if matches_bytes(data, 0, &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some(fr_wire::ImageMediaType::Png);
    }
    if matches_bytes(data, 0, &[0xff, 0xd8, 0xff]) {
        return Some(fr_wire::ImageMediaType::Jpeg);
    }
    if matches_ascii(data, 0, "GIF87a") || matches_ascii(data, 0, "GIF89a") {
        return Some(fr_wire::ImageMediaType::Gif);
    }
    if matches_ascii(data, 0, "RIFF") && matches_ascii(data, 8, "WEBP") {
        return Some(fr_wire::ImageMediaType::Webp);
    }
    None
}

/// Enforces the strict image-capability gate for the calling route (dsh
/// `assertImageCapableRoute`): resolves the session's latest routed
/// provider/model (the request-header config; the dsh agent-options
/// fallback arm is structurally unreachable for a model-issued call,
/// which always rides a logged request) and requires the exact resolved
/// route to declare `image` input explicitly.
fn assert_image_capable_route(
    seams: &dyn Seams,
    agent: Option<&str>,
    requested_path: &str,
) -> Result<(), BodyError> {
    let Some(llm) = seams.llm() else {
        return Err(BodyError::plain(format!(
            "cannot read \"{requested_path}\" as an image: the current model route could not be resolved"
        )));
    };
    let Some(agent) = agent else {
        return Err(BodyError::plain(format!(
            "cannot read \"{requested_path}\" as an image: the current model route could not be resolved"
        )));
    };
    let Some(route_header) = seams.route_header() else {
        return Err(BodyError::plain(format!(
            "cannot read \"{requested_path}\" as an image: the current model route could not be resolved"
        )));
    };
    let snapshot = route_header
        .request_header(agent)
        .map_err(BodyError::plain)?;
    let config = snapshot
        .get("header")
        .and_then(|header| header.get("config"))
        .filter(|config| !config.is_null());
    let provider = config
        .and_then(|config| config.get("provider"))
        .and_then(Value::as_str);
    let model = config
        .and_then(|config| config.get("model"))
        .and_then(Value::as_str);
    let (Some(provider), Some(model)) = (provider, model) else {
        return Err(BodyError::plain(format!(
            "cannot read \"{requested_path}\" as an image: the current model route could not be resolved"
        )));
    };
    // The dsh `llm === undefined` arm maps to the route-unresolved text;
    // a resolved service that itself refuses (UNKNOWN_MODEL and the
    // taxonomy family) rides through as its own failure.
    let active = llm
        .resolve_model_info(provider, model)
        .map_err(|error| unmount_or_transport(error, requested_path))?;
    let declares_image = active
        .get("inputModalities")
        .and_then(Value::as_array)
        .is_some_and(|modalities| {
            modalities
                .iter()
                .any(|modality| modality == "image" || modality.as_str() == Some("image"))
        });
    if !declares_image {
        return Err(BodyError::plain(format!(
            "cannot read \"{requested_path}\" as an image: model \"{model}\" does not declare image input; switch to an image-capable model to read images"
        )));
    }
    Ok(())
}

/// Classifies a seam failure: the dsh `undefined` service arm (the
/// unmounted store over the wire fails closed with the host
/// `STATUS_INACTIVE` status) versus a service-side refusal, which rides
/// through verbatim.
fn unmount_or_transport(error: String, requested_path: &str) -> BodyError {
    if error.contains("host status") {
        return BodyError::plain(format!(
            "cannot read \"{requested_path}\" as an image: the current model route could not be resolved"
        ));
    }
    BodyError::plain(error)
}

/// Refuses a media type outside the deployment's accepted set (dsh
/// `assertDeploymentAccepts`).
fn assert_deployment_accepts(
    limits: &Value,
    media_type: &str,
    display_path: &str,
) -> Result<(), BodyError> {
    let accepted = limits
        .get("mediaTypes")
        .and_then(Value::as_array)
        .is_some_and(|types| types.iter().any(|entry| entry.as_str() == Some(media_type)));
    if !accepted {
        return Err(BodyError::plain(format!(
            "cannot read \"{display_path}\": {media_type} images are not accepted by this deployment"
        )));
    }
    Ok(())
}

/// Formats an image read as the model-facing envelope beside its
/// image block (dsh `formatImageReadOutput`).
///
/// A downscaled read names the on-disk dimensions and the multiplier
/// that maps coordinates measured on the attached image back onto the
/// original file.
#[must_use]
pub fn format_image_read_output(display_path: &str, image: &Value) -> String {
    let scaled = image.get("originalDimensions").map_or_else(
        String::new,
        |originals| {
            let original_width = originals.get("width").and_then(Value::as_f64);
            let original_height = originals.get("height").and_then(Value::as_f64);
            let width = image.get("width").and_then(Value::as_f64);
            let height = image.get("height").and_then(Value::as_f64);
            match (original_width, original_height, width, height) {
                (Some(ow), Some(oh), Some(w), Some(h)) if w > 0.0 && h > 0.0 => {
                    // Integer rounding can give the two axes slightly
                    // different ratios, so the advice names one
                    // multiplier only when both round to the same value.
                    let x = format!("{:.2}", ow / w);
                    let y = format!("{:.2}", oh / h);
                    let advice = if x == y {
                        format!("multiply coordinates by {x}")
                    } else {
                        format!("multiply x coordinates by {x} and y coordinates by {y}")
                    };
                    format!(
                        " (downscaled from {}x{} px; {advice} to locate features in the original file)",
                        crate::service::js_number(ow),
                        crate::service::js_number(oh),
                    )
                }
                _ => String::new(),
            }
        },
    );
    let media_type = image.get("mediaType").and_then(Value::as_str).unwrap_or("");
    let width =
        crate::service::js_number(image.get("width").and_then(Value::as_f64).unwrap_or(0.0));
    let height =
        crate::service::js_number(image.get("height").and_then(Value::as_f64).unwrap_or(0.0));
    let bytes =
        crate::service::js_number(image.get("bytes").and_then(Value::as_f64).unwrap_or(0.0));
    format!(
        "<path>{display_path}</path>\n<type>image</type>\n<content>\n{media_type} image, {width}x{height} px, {bytes} bytes{scaled}\n</content>"
    )
}

/// The `read_image` execution frame: the domains plus the composed
/// seams the image body runs through.
pub struct ImageToolDeps<'a> {
    /// The fs domain (the single sandboxed local backend).
    pub fs: &'a FsDomain,
    /// The observed-state gate (the in-crate observation policy).
    pub gate: &'a ObservedStateGate,
    /// The composed cross-plugin seam (attachment store, model
    /// metadata, routing header).
    pub seams: &'a dyn Seams,
}

impl std::fmt::Debug for ImageToolDeps<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageToolDeps").finish_non_exhaustive()
    }
}

/// Executes one `read_image` call (dsh `applyReadImageTool.execute`).
///
/// Every pre-read gate runs before any filesystem I/O so a refusal
/// never leaks partial reads or attachment writes; an extension-less
/// path declares no format, so only its format and deployment
/// media-type checks wait for the bytes.
///
/// # Errors
///
/// The dsh refusal texts verbatim (the pre-read gate family, the
/// route-gate family, the fs family, and the six save-error arms).
#[expect(
    clippy::too_many_lines,
    reason = "the body mirrors the dsh execute one arm per refusal family; splitting it would hide the gate order"
)]
pub fn read_image_execute(
    deps: &ImageToolDeps<'_>,
    arguments: &Value,
    agent: Option<&str>,
) -> Result<Value, BodyError> {
    let file_path = arguments
        .get("file_path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if file_path.trim().is_empty() {
        return Err(BodyError::plain("file_path must be a non-empty string"));
    }
    let extension = last_extension_lower(file_path);
    let declared = image_media_type_for_path(file_path);
    if declared.is_none() && !extension.is_empty() {
        return Err(BodyError::plain(format!(
            "cannot read \"{file_path}\": the {extension} extension does not declare a supported image format; read_image accepts PNG/JPEG/WebP/GIF files, including extension-less files in those formats"
        )));
    }
    // The defensive re-check (dsh: execution re-reads ctx.get('attachments')
    // for direct callers and after provider withdrawal).
    let Some(attachments) = deps.seams.attachments() else {
        return Err(BodyError::plain(format!(
            "cannot read \"{file_path}\" as an image: no attachment service is mounted"
        )));
    };
    let limits = attachments.image_limits().map_err(|error| {
        if error.contains("host status") {
            BodyError::plain(format!(
                "cannot read \"{file_path}\" as an image: no attachment service is mounted"
            ))
        } else {
            BodyError::plain(error)
        }
    })?;
    if let Some(declared) = declared {
        assert_deployment_accepts(&limits, declared_type_word(declared), file_path)?;
    }
    assert_image_capable_route(deps.seams, agent, file_path)?;

    let cwd = crate::fs_tools::session_cwd(deps.seams, agent)?;
    let target = deps
        .fs
        .resolve(file_path, cwd.as_deref())
        .map_err(fs_failure)?;
    let info = deps.fs.stat(&target).map_err(fs_failure)?;
    let Some(info) = info else {
        deps.gate.observe(&target, FsObservation::Absent, agent);
        return Err(BodyError::plain(format!(
            "cannot read \"{}\": not found",
            target.display_path
        )));
    };
    if info.kind != FsKind::File {
        return Err(BodyError::plain(format!(
            "cannot read \"{}\": not a regular file",
            target.display_path
        )));
    }
    // The tool result is one message carrying one image, so the
    // per-message aggregate bound applies beside the per-image bound.
    let byte_cap = limits
        .get("maxImageBytes")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX)
        .min(
            limits
                .get("maxMessageImageBytes")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX),
        );
    let data = deps.fs.read_bytes(&target, byte_cap).map_err(fs_failure)?;
    let media_type = declared.or_else(|| sniff_image_media_type(&data));
    let Some(media_type) = media_type else {
        return Err(BodyError::plain(format!(
            "cannot read \"{}\": the file content is not a supported image format; read_image accepts PNG/JPEG/WebP/GIF",
            target.display_path
        )));
    };
    if declared.is_none() {
        assert_deployment_accepts(
            &limits,
            declared_type_word(media_type),
            &target.display_path,
        )?;
    }
    // Persist before returning: the image block must reference a durably
    // committed object by the time the tool/result event is appended.
    let save_payload = json!({
        "data": crate::fs_domain::encode_base64(&data),
        "mediaType": declared_type_word(media_type),
        "name": base_leaf(&target.display_path),
    });
    let reference = match attachments.save_image(&save_payload) {
        Ok(reference) => reference,
        Err(error) => {
            return Err(save_failure(
                &error,
                declared.is_none(),
                &extension,
                media_type,
                &limits,
                &target.display_path,
            ));
        }
    };
    deps.gate.observe(
        &target,
        FsObservation::Present {
            version: info.version,
        },
        agent,
    );
    Ok(json!({
        "path": FsDomain::process_path(&target),
        "image": reference,
    }))
}

/// Maps an fs-domain error onto the body failure (the dsh `FsError`
/// extends `HarnessError`: the code stays routable, the class rides as
/// `FsError`).
fn fs_failure(error: crate::fs_types::FsError) -> BodyError {
    BodyError {
        code: Some(error.code.as_str().to_owned()),
        name: "FsError".to_owned(),
        message: error.message,
    }
}

/// The lowercased extension of the path's last segment (the node
/// `extname` word, including the dot; empty for extension-less names).
fn last_extension_lower(file_path: &str) -> String {
    let last_segment = file_path.rsplit(['/', '\\']).next().unwrap_or(file_path);
    match last_segment.rfind('.') {
        Some(0) | None => String::new(),
        Some(index) => last_segment[index..].to_ascii_lowercase(),
    }
}

/// The wire word for one media type (the fr-wire serde spelling).
const fn declared_type_word(media_type: fr_wire::ImageMediaType) -> &'static str {
    match media_type {
        fr_wire::ImageMediaType::Png => "image/png",
        fr_wire::ImageMediaType::Jpeg => "image/jpeg",
        fr_wire::ImageMediaType::Webp => "image/webp",
        fr_wire::ImageMediaType::Gif => "image/gif",
    }
}

/// The display-name leaf of the resolved path (the dsh
/// `basename(target.displayPath)` save name).
fn base_leaf(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned()
}

/// The save-error mapping (dsh `saveImage` catch chain): dimension
/// refusals stay recoverable tool errors — an oversized image must
/// never enter durable history, where it would ride every later model
/// request past provider-side dimension rejections.
fn save_failure(
    error: &str,
    extension_less: bool,
    extension: &str,
    media_type: fr_wire::ImageMediaType,
    limits: &Value,
    display_path: &str,
) -> BodyError {
    // The wire `attachmentError` document (the service's structured
    // refusal) versus a transport failure, which rides through. The
    // require transport prefixes the body (`failed with host status N:
    // <body>`); strip it before the document parse.
    let body_text = error
        .split_once("failed with host status ")
        .and_then(|(_, rest)| rest.split_once(": "))
        .map_or(error, |(_, body)| body);
    let Ok(parsed) = serde_json::from_str::<Value>(body_text) else {
        return BodyError::plain(error.to_owned());
    };
    let Some(document) = parsed.get("attachmentError") else {
        return BodyError::plain(error.to_owned());
    };
    let code = document.get("code").and_then(Value::as_str).unwrap_or("");
    let message = document
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let max_dimension = limits
        .get("maxImageDimension")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let max_pixels = limits
        .get("maxImagePixels")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if code == "IMAGE_DIMENSION_TOO_LARGE" {
        return BodyError::plain(format!(
            "cannot read \"{display_path}\": at least one image side exceeds the {max_dimension}px limit; downscale the image and read the smaller copy"
        ));
    }
    if code == "IMAGE_TOO_MANY_PIXELS" {
        return BodyError::plain(format!(
            "cannot read \"{display_path}\": the image exceeds the {max_pixels}-pixel decoded-size limit; downscale the image and read the smaller copy"
        ));
    }
    if code == "IMAGE_TOO_LARGE" {
        return BodyError::plain(format!(
            "cannot read \"{display_path}\": the image cannot be stored within the deployment's byte limits; downscale the image and read the smaller copy"
        ));
    }
    if code == "ATTACHMENT_WRITE_FAILED" && message.to_lowercase().contains("16-bit png") {
        return BodyError::plain(format!(
            "cannot read \"{display_path}\": the 16-bit PNG could not be converted to the normalized 8-bit sRGB form; convert it to an 8-bit PNG/JPEG/WebP and retry"
        ));
    }
    if code == "INVALID_IMAGE" && extension_less {
        return BodyError::plain(format!(
            "cannot read \"{display_path}\": the bytes do not decode as a supported PNG/JPEG/WebP/GIF image; the file may be truncated or corrupt"
        ));
    }
    if code != "IMAGE_TYPE_MISMATCH" {
        return BodyError::plain(error.to_owned());
    }
    if extension_less {
        return BodyError::plain(format!(
            "cannot read \"{display_path}\": the file signature claims {}, but the bytes decode as a different image format; the file may be corrupt",
            declared_type_word(media_type)
        ));
    }
    BodyError::plain(format!(
        "cannot read \"{display_path}\": the {extension} extension declares {}, but the bytes use a different image format; rename the file to match its actual format if it is PNG/JPEG/WebP/GIF, or convert it to one of those formats",
        declared_type_word(media_type)
    ))
}

/// Projects one structured image read into its model-facing envelope
/// and image (dsh `imageReadContent` + `output.render`): the two
/// content blocks used by native and nested dispatches.
///
/// # Errors
///
/// The wire decode refusal when the settled value is not a shape the
/// image block can carry.
pub fn render_image_read(args: &Value, value: &Value) -> Result<Vec<Value>, String> {
    let _ = args;
    let path = value
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "image-read value requires path".to_owned())?;
    let image = value
        .get("image")
        .ok_or_else(|| "image-read value requires image".to_owned())?;
    let reference: fr_wire::ImageAttachmentRef = serde_json::from_value(image.clone())
        .map_err(|error| format!("image-read value carries a malformed reference: {error}"))?;
    let attachment = serde_json::to_value(&reference)
        .map_err(|error| format!("image reference encode failed: {error}"))?;
    Ok(vec![
        json!({ "type": "text", "text": format_image_read_output(path, image) }),
        json!({ "type": "image", "attachment": attachment }),
    ])
}

/// Pure display: a generic card in the read family with a follow-along
/// location on the image file (dsh `presentCall`).
#[must_use]
pub fn present_call_image(arguments: &Value) -> Value {
    let file_path = arguments
        .get("file_path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    json!({
        "card": "generic",
        "title": format!("Read image {file_path}"),
        "kind": "read",
        "locations": [{ "path": file_path }],
    })
}

/// Persists the resolved path only (dsh `output.presentationMeta`).
///
/// The attachment reference is NOT copied here: the settled content
/// already carries the image block with the complete reference, so a
/// second copy would keep two records of one fact — and a
/// tools/post-execute hook that legitimately replaces the content
/// would leave the stale copy behind, showing an image the result no
/// longer returns. The path needs its own structured record because
/// the content carries it only as model-facing envelope text, which
/// the client does not parse.
#[must_use]
pub fn presentation_meta_image(_args: &Value, value: &Value) -> Value {
    json!({ "path": value.get("path").cloned().unwrap_or(Value::Null) })
}

/// Serves one read-image method invocation (the fourth export-table
/// band: the 50-53 method space).
///
/// # Errors
///
/// The payload decode refusal, an unknown method, or the tool body's
/// own failure (the wire body-error shape).
pub fn serve(deps: &ImageToolDeps<'_>, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
    let request: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(payload)
            .map_err(|error| format!("builtin-tools payload must be JSON: {error}"))?
    };
    match method {
        crate::service::METHOD_READ_IMAGE_EXECUTE => {
            let call = request
                .get("call")
                .ok_or_else(|| "builtin-tools execute payload requires call".to_string())?;
            let agent = call.get("agent").and_then(Value::as_str);
            let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
            match read_image_execute(deps, &arguments, agent) {
                Ok(value) => serde_json::to_vec(&json!({ "value": value }))
                    .map_err(|error| format!("body reply encode failed: {error}")),
                Err(error) => {
                    let body = match error.code {
                        Some(code) => json!({
                            "error": { "code": code, "message": error.message, "name": error.name }
                        }),
                        None => {
                            json!({ "error": { "message": error.message, "name": error.name } })
                        }
                    };
                    serde_json::to_vec(&body)
                        .map_err(|error| format!("body error encode failed: {error}"))
                }
            }
        }
        crate::service::METHOD_READ_IMAGE_RENDER => {
            let rendered = render_image_read(&request["args"], &request["value"])?;
            serde_json::to_vec(&rendered)
                .map_err(|error| format!("builtin-tools reply encode failed: {error}"))
        }
        crate::service::METHOD_READ_IMAGE_PRESENT_CALL => {
            serde_json::to_vec(&present_call_image(&request["args"]))
                .map_err(|error| format!("builtin-tools reply encode failed: {error}"))
        }
        crate::service::METHOD_READ_IMAGE_PRESENTATION_META => serde_json::to_vec(
            &presentation_meta_image(&request["args"], &request["value"]),
        )
        .map_err(|error| format!("builtin-tools reply encode failed: {error}")),
        other => Err(format!("unknown fr-tools-builtin image method {other}")),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        ImageToolDeps, format_image_read_output, image_media_type_for_path, present_call_image,
        presentation_meta_image, read_image_execute, render_image_read, sniff_image_media_type,
    };
    use crate::fs_domain::FsDomain;
    use crate::observation::ObservedStateGate;
    use crate::tools::{AttachmentSeam, LlmSeam, RouteHeaderSeam, Seams};

    /// One unique scratch directory per test (cleaned on drop).
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fr-read-image-{tag}-{}-{}",
                std::process::id(),
                fr_tools_uuid()
            ));
            std::fs::create_dir_all(&dir).expect("scratch dir creates");
            Self(dir)
        }

        fn path(&self, name: &str) -> String {
            self.0.join(name).to_string_lossy().into_owned()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fr_tools_uuid() -> String {
        // The test-scratch uniqueness word (the pipeline judgment).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        format!("{nanos:x}")
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "the test raster is 4x3, far below one byte per channel"
    )]
    fn tiny_png() -> Vec<u8> {
        let pixels = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(4, 3, |x, y| {
            image::Rgb([x as u8, y as u8, 7])
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

    /// The scripted cross-plugin seam set: the attachment store records
    /// saves and replies canned references, the model metadata answers
    /// one modality table, and the route header answers one config.
    struct ImageSeams {
        mounted: bool,
        limits: Option<Result<Value, String>>,
        saved: std::sync::Mutex<Vec<Value>>,
        save_reply: Option<Result<Value, String>>,
        modalities: Option<Vec<&'static str>>,
        resolve_error: Option<String>,
        header: Option<Value>,
    }

    impl Default for ImageSeams {
        fn default() -> Self {
            Self {
                mounted: true,
                limits: Some(Ok(json!({
                    "maxImageBytes": 20 * 1024 * 1024,
                    "maxMessageImageBytes": 200 * 1024 * 1024,
                    "maxImagePixels": 64_000_000,
                    "maxImageDimension": 8192,
                    "mediaTypes": ["image/png", "image/jpeg", "image/webp", "image/gif"],
                }))),
                saved: std::sync::Mutex::new(Vec::new()),
                save_reply: Some(Ok(json!({
                    "attachmentId": "sha256:aa",
                    "mediaType": "image/png",
                    "bytes": 12,
                    "width": 4,
                    "height": 3,
                    "name": "photo.png",
                }))),
                modalities: Some(vec!["text", "image"]),
                resolve_error: None,
                header: Some(json!({
                    "header": { "config": { "provider": "p", "model": "vl" } }
                })),
            }
        }
    }

    impl AttachmentSeam for ImageSeams {
        fn image_limits(&self) -> Result<Value, String> {
            self.limits.clone().expect("limits scripted")
        }
        fn save_image(&self, input: &Value) -> Result<Value, String> {
            self.saved.lock().expect("saved").push(input.clone());
            self.save_reply.clone().expect("save reply scripted")
        }
    }

    impl LlmSeam for ImageSeams {
        fn resolve_model_info(&self, _provider: &str, _model: &str) -> Result<Value, String> {
            if let Some(error) = &self.resolve_error {
                return Err(error.clone());
            }
            Ok(json!({
                "inputModalities": self.modalities,
            }))
        }
    }

    impl RouteHeaderSeam for ImageSeams {
        fn request_header(&self, _session: &str) -> Result<Value, String> {
            Ok(self.header.clone().unwrap_or(Value::Null))
        }
    }

    impl Seams for ImageSeams {
        fn policy(&self) -> &dyn crate::tools::PolicySeam {
            unreachable!("the image body never reads the policy seam")
        }
        fn approval(&self) -> &dyn crate::tools::ApprovalSeam {
            unreachable!("the image body never reads the approval seam")
        }
        fn attachments(&self) -> Option<&dyn AttachmentSeam> {
            self.mounted.then_some(self)
        }
        fn llm(&self) -> Option<&dyn LlmSeam> {
            Some(self)
        }
        fn route_header(&self) -> Option<&dyn RouteHeaderSeam> {
            Some(self)
        }
        fn shell_resolve(&self, _shell: &str, _request: &Value) -> Result<Value, String> {
            unreachable!()
        }
        fn shell_start(
            &self,
            _: &str,
            _: &Value,
            _: &str,
            _: Option<&str>,
            _: &str,
            _: &str,
            _: &[&str],
        ) -> Result<(String, String), String> {
            unreachable!()
        }
        fn shell_result(&self, _job_id: &str) -> Result<Value, String> {
            unreachable!()
        }
        fn jobs_wait(
            &self,
            _id: &str,
            _timeout_ms: f64,
            _caller: Option<&str>,
        ) -> Result<Value, String> {
            unreachable!()
        }
        fn jobs_read(&self, _id: &str, _caller: Option<&str>) -> Result<Value, String> {
            unreachable!()
        }
        fn jobs_kill(&self, _: &str, _: Option<&str>, _: Option<&str>) -> Result<String, String> {
            unreachable!()
        }
        fn jobs_remove(&self, _id: &str, _caller: Option<&str>) -> Result<(), String> {
            unreachable!()
        }
        fn working_directory_ensure(&self, _agent: &str) -> Result<String, String> {
            // The image tests pass absolute paths, so the cwd value is
            // inert; the temp dir keeps it real.
            Ok(std::env::temp_dir().to_string_lossy().into_owned())
        }
        fn shell_env_collect(
            &self,
            _agent: Option<&str>,
        ) -> Result<crate::shell_env::FrozenEnv, String> {
            unreachable!()
        }
        fn subprocess_spawn(&self, _spec: &Value) -> Result<String, String> {
            unreachable!()
        }
        fn subprocess_read(&self, _: &str, _: &str, _: u64) -> Result<Value, String> {
            unreachable!()
        }
        fn subprocess_wait(&self, _handle: &str, _timeout_ms: f64) -> Result<Value, String> {
            unreachable!()
        }
        fn subprocess_resolve_executable(&self, _command: &str) -> Result<String, String> {
            unreachable!()
        }
        fn spill_store_save_text(&self, _save: &Value) -> Result<Option<Value>, String> {
            unreachable!()
        }
    }

    struct Rig {
        scratch: Scratch,
        deps_fs: FsDomain,
        gate: ObservedStateGate,
        seams: ImageSeams,
    }

    impl Rig {
        fn new(tag: &str) -> Self {
            let scratch = Scratch::new(tag);
            let config = crate::fs_domain::FsDomainConfig::new().expect("config");
            let deps_fs = FsDomain::new(config, Box::new(NullPolicy), None);
            Self {
                scratch,
                deps_fs,
                gate: ObservedStateGate::new(),
                seams: ImageSeams::default(),
            }
        }

        fn deps(&self) -> ImageToolDeps<'_> {
            ImageToolDeps {
                fs: &self.deps_fs,
                gate: &self.gate,
                seams: &self.seams,
            }
        }
    }

    struct NullPolicy;

    impl crate::fs_domain::StandingPolicy for NullPolicy {
        fn resolve(&self, _session: Option<&str>) -> Result<Value, String> {
            Ok(Value::Null)
        }
    }

    fn write_png(rig: &Rig, name: &str) -> String {
        let path = rig.scratch.path(name);
        std::fs::write(&path, tiny_png()).expect("writes");
        path
    }

    #[test]
    fn extension_table_pins_the_dsh_words() {
        assert_eq!(
            image_media_type_for_path("a.png"),
            Some(fr_wire::ImageMediaType::Png)
        );
        assert_eq!(
            image_media_type_for_path("a.jpg"),
            Some(fr_wire::ImageMediaType::Jpeg)
        );
        assert_eq!(
            image_media_type_for_path("a.JPEG"),
            Some(fr_wire::ImageMediaType::Jpeg)
        );
        assert_eq!(
            image_media_type_for_path("a.webp"),
            Some(fr_wire::ImageMediaType::Webp)
        );
        assert_eq!(
            image_media_type_for_path("a.gif"),
            Some(fr_wire::ImageMediaType::Gif)
        );
        assert_eq!(image_media_type_for_path("a.txt"), None);
        assert_eq!(image_media_type_for_path(".hidden"), None);
        assert_eq!(image_media_type_for_path("/dir/noext"), None);
    }

    #[test]
    fn sniff_pins_the_four_signatures() {
        assert_eq!(
            sniff_image_media_type(b"\x89PNG\r\n\x1a\n"),
            Some(fr_wire::ImageMediaType::Png)
        );
        assert_eq!(
            sniff_image_media_type(b"\xff\xd8\xff\xe0"),
            Some(fr_wire::ImageMediaType::Jpeg)
        );
        assert_eq!(
            sniff_image_media_type(b"GIF87a"),
            Some(fr_wire::ImageMediaType::Gif)
        );
        assert_eq!(
            sniff_image_media_type(b"GIF89a"),
            Some(fr_wire::ImageMediaType::Gif)
        );
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0; 4]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(
            sniff_image_media_type(&webp),
            Some(fr_wire::ImageMediaType::Webp)
        );
        assert_eq!(sniff_image_media_type(b"not an image"), None);
    }

    #[test]
    fn envelope_pins_the_ratio_advice() {
        let plain = format_image_read_output(
            "/p/a.png",
            &json!({ "mediaType": "image/png", "bytes": 12, "width": 4, "height": 3 }),
        );
        assert_eq!(
            plain,
            "<path>/p/a.png</path>\n<type>image</type>\n<content>\nimage/png image, 4x3 px, 12 bytes\n</content>"
        );
        let downscaled = format_image_read_output(
            "/p/a.png",
            &json!({
                "mediaType": "image/png", "bytes": 12, "width": 4, "height": 3,
                "originalDimensions": { "width": 8, "height": 6 },
            }),
        );
        assert!(
            downscaled.contains("(downscaled from 8x6 px; multiply coordinates by 2.00 to locate features in the original file)"),
            "{downscaled}"
        );
    }

    #[test]
    fn blank_and_unsupported_paths_refuse_before_any_io() {
        let rig = Rig::new("gates");
        let deps = rig.deps();
        let error =
            read_image_execute(&deps, &json!({ "file_path": "   " }), None).expect_err("blank");
        assert_eq!(error.message, "file_path must be a non-empty string");
        let error =
            read_image_execute(&deps, &json!({ "file_path": "a.txt" }), None).expect_err("type");
        assert!(error.message.contains(
            "the .txt extension does not declare a supported image format; read_image accepts PNG/JPEG/WebP/GIF files, including extension-less files in those formats"
        ));
    }

    #[test]
    fn unmounted_store_refuses_with_the_mount_text() {
        let mut rig = Rig::new("unmounted");
        rig.seams.mounted = false;
        let error = read_image_execute(&rig.deps(), &json!({ "file_path": "a.png" }), None)
            .expect_err("unmounted");
        assert_eq!(
            error.message,
            "cannot read \"a.png\" as an image: no attachment service is mounted"
        );
    }

    #[test]
    fn route_gate_refuses_the_unresolvable_and_imageless_models() {
        let mut rig = Rig::new("route");
        let path = write_png(&rig, "a.png");
        // No session header yet: the route could not be resolved.
        rig.seams.header = Some(Value::Null);
        let error = read_image_execute(
            &rig.deps(),
            &json!({ "file_path": path }),
            Some("session-1"),
        )
        .expect_err("no header");
        assert!(
            error
                .message
                .ends_with(" as an image: the current model route could not be resolved"),
            "{}",
            error.message
        );
        // A text-only model refuses with the switch advice.
        rig.seams.header = Some(json!({
            "header": { "config": { "provider": "p", "model": "text-only" } }
        }));
        rig.seams.modalities = Some(vec!["text"]);
        let error = read_image_execute(
            &rig.deps(),
            &json!({ "file_path": path }),
            Some("session-1"),
        )
        .expect_err("text only");
        assert!(
            error.message.ends_with(
                " does not declare image input; switch to an image-capable model to read images",
            ),
            "{}",
            error.message
        );
    }

    #[test]
    fn happy_path_saves_observes_and_returns_the_reference() {
        let rig = Rig::new("happy");
        let path = write_png(&rig, "photo.png");
        let deps = rig.deps();
        let value = read_image_execute(&deps, &json!({ "file_path": path }), Some("session-1"))
            .expect("reads");
        assert_eq!(value["image"]["mediaType"], "image/png");
        assert_eq!(value["image"]["width"], 4);
        assert_eq!(value["image"]["name"], "photo.png");
        assert!(value["path"].as_str().expect("path").contains("photo.png"));
        // The save carried the base64 bytes, the declared type, and the
        // leaf name.
        let saved = rig.seams.saved.lock().expect("saved");
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0]["mediaType"], "image/png");
        assert_eq!(saved[0]["name"], "photo.png");
        assert_ne!(saved[0]["data"].as_str().expect("base64"), "");
    }

    #[test]
    fn extension_less_sniff_feeds_the_deployment_check() {
        let rig = Rig::new("sniffed");
        let path = write_png(&rig, "noext");
        let deps = rig.deps();
        let value = read_image_execute(&deps, &json!({ "file_path": path }), Some("session-1"))
            .expect("reads");
        assert_eq!(value["image"]["mediaType"], "image/png");
    }

    #[test]
    fn save_failures_map_the_six_recovery_arms() {
        let mut rig = Rig::new("save-map");
        let path = write_png(&rig, "a.png");
        let document = |code: &str, message: &str| {
            json!({ "attachmentError": { "code": code, "message": message } }).to_string()
        };
        rig.seams.save_reply = Some(Err(document("IMAGE_DIMENSION_TOO_LARGE", "too tall")));
        let error = read_image_execute(&rig.deps(), &json!({ "file_path": path }), Some("s"))
            .expect_err("dimension");
        assert!(
            error.message.contains("at least one image side exceeds the 8192px limit; downscale the image and read the smaller copy"),
            "{}",
            error.message
        );

        rig.seams.save_reply = Some(Err(document("IMAGE_TOO_MANY_PIXELS", "many")));
        let error = read_image_execute(&rig.deps(), &json!({ "file_path": path }), Some("s"))
            .expect_err("pixels");
        assert!(
            error
                .message
                .contains("the image exceeds the 64000000-pixel decoded-size limit"),
            "{}",
            error.message
        );

        rig.seams.save_reply = Some(Err(document("IMAGE_TOO_LARGE", "big")));
        let error = read_image_execute(&rig.deps(), &json!({ "file_path": path }), Some("s"))
            .expect_err("bytes");
        assert!(
            error
                .message
                .contains("the image cannot be stored within the deployment's byte limits"),
            "{}",
            error.message
        );

        rig.seams.save_reply = Some(Err(document(
            "ATTACHMENT_WRITE_FAILED",
            "The 16-bit PNG could not be converted to the normalized 8-bit sRGB form.",
        )));
        let error = read_image_execute(&rig.deps(), &json!({ "file_path": path }), Some("s"))
            .expect_err("16-bit");
        assert!(
            error.message.contains(
                "the 16-bit PNG could not be converted to the normalized 8-bit sRGB form; convert it to an 8-bit PNG/JPEG/WebP and retry"
            ),
            "{}",
            error.message
        );

        rig.seams.save_reply = Some(Err(document("INVALID_IMAGE", "junk")));
        let error = read_image_execute(&rig.deps(), &json!({ "file_path": path }), Some("s"))
            .expect_err("declared invalid");
        // A declared path does not take the truncation arm: the raw
        // document rides through.
        assert!(
            error.message.contains("attachmentError"),
            "{}",
            error.message
        );
    }

    #[test]
    fn extension_less_invalid_bytes_take_the_truncation_arm() {
        let mut rig = Rig::new("truncated");
        let path = rig.scratch.path("noext");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\njunk").expect("writes");
        rig.seams.save_reply = Some(Err(json!({
            "attachmentError": { "code": "INVALID_IMAGE", "message": "junk" }
        })
        .to_string()));
        let error = read_image_execute(&rig.deps(), &json!({ "file_path": path }), Some("s"))
            .expect_err("invalid");
        assert!(
            error.message.contains(
                "the bytes do not decode as a supported PNG/JPEG/WebP/GIF image; the file may be truncated or corrupt"
            ),
            "{}",
            error.message
        );
    }

    #[test]
    fn type_mismatch_splits_the_declared_and_sniffed_arms() {
        let mut rig = Rig::new("mismatch");
        let path = write_png(&rig, "a.png");
        let document = json!({
            "attachmentError": { "code": "IMAGE_TYPE_MISMATCH", "message": "no" }
        })
        .to_string();
        // Declared: the rename advice names the extension.
        rig.seams.save_reply = Some(Err(document));
        let error = read_image_execute(&rig.deps(), &json!({ "file_path": path }), Some("s"))
            .expect_err("declared mismatch");
        assert!(
            error.message.contains(
                "the .png extension declares image/png, but the bytes use a different image format"
            ),
            "{}",
            error.message
        );
        // Extension-less: the signature-claims arm.
        let sniffed = rig.scratch.path("noext");
        std::fs::write(&sniffed, tiny_png()).expect("writes");
        let error = read_image_execute(&rig.deps(), &json!({ "file_path": sniffed }), Some("s"))
            .expect_err("sniffed mismatch");
        assert!(
            error.message.contains(
                "the file signature claims image/png, but the bytes decode as a different image format"
            ),
            "{}",
            error.message
        );
    }

    #[test]
    fn presentation_faces_pin_the_read_family_card_and_path_meta() {
        let card = present_call_image(&json!({ "file_path": "/p/a.png" }));
        assert_eq!(card["card"], "generic");
        assert_eq!(card["title"], "Read image /p/a.png");
        assert_eq!(card["kind"], "read");
        assert_eq!(card["locations"], json!([{ "path": "/p/a.png" }]));
        let meta =
            presentation_meta_image(&Value::Null, &json!({ "path": "/p/a.png", "image": {} }));
        assert_eq!(meta, json!({ "path": "/p/a.png" }));
    }

    #[test]
    fn render_projects_the_envelope_and_the_image_block() {
        let blocks = render_image_read(
            &Value::Null,
            &json!({
                "path": "/p/a.png",
                "image": {
                    "attachmentId": "sha256:aa",
                    "mediaType": "image/png",
                    "bytes": 12,
                    "width": 4,
                    "height": 3,
                },
            }),
        )
        .expect("renders");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert!(
            blocks[0]["text"]
                .as_str()
                .expect("text")
                .starts_with("<path>/p/a.png</path>")
        );
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["attachment"]["attachmentId"], "sha256:aa");
    }
}
