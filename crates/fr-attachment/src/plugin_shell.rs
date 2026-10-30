//! The C-ABI dylib plugin shell (PRD 4.5): the `fr-attachment` artifact
//! surface over the dougong dynamic channel.
//!
//! Kernel-family edge: this module is the only `dougong-abi` consumer; the
//! rest of the crate stays a plain rlib housing the attachment contract,
//! normalization pipeline, and durable store.
//!
//! The shell provides the `attachments` service (2026-10-27 Q78-A; the
//! Q16b-1A durable-store face): the limits snapshot and the image
//! save the `read_image` tool consumes over the wire, over a store assembled
//! from the deployment defaults (`FR_HOME`, then `~/.fr`). The binary
//! payload rides the shared base64 codec.

use crate::limits::AttachmentConfig;
use crate::local::LocalAttachmentStore;
use crate::store::SaveImageAttachment;
use dougong_abi::{
    ABI_VERSION, HostExports, PluginExports, STATUS_CAPACITY, STATUS_FAILED, STATUS_OK, write_out,
};
use fr_wire::services::attachments as face;

/// The provided service name this plugin offers (Q78-A `provide
/// attachments`).
pub const PROVIDED_SERVICE: &str = "attachments";

/// The dylib artifact's plugin name.
pub const PLUGIN_NAME: &str = "fr-attachment";

/// The descriptor the loader consumes: the `attachments` service provided,
/// nothing injected, no listeners in this slice. The version is sourced
/// from crate metadata so it cannot drift from the artifact.
const DESCRIPTOR: &[u8] = concat!(
    "{\"name\":\"fr-attachment\",\"version\":\"",
    env!("CARGO_PKG_VERSION"),
    "\",\"provide\":[\"attachments\"],\"inject\":[],\"listen\":{}}"
)
.as_bytes();

/// Serves one method invocation against the shell state: a pure
/// (JSON-in, JSON-out) function so the shell surface stays testable.
///
/// # Errors
///
/// Returns the failure text the host surfaces as `STATUS_FAILED`.
pub fn dispatch(state: &ShellState, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
    match method {
        face::METHOD_DESCRIBE => {
            let _ = payload;
            Ok(serde_json::to_vec(&serde_json::json!({
                "name": PLUGIN_NAME,
                "provides": PROVIDED_SERVICE,
                "abiVersion": ABI_VERSION,
            }))
            .unwrap_or_default())
        }
        face::METHOD_IMAGE_LIMITS => {
            let _ = payload;
            let limits = &state.store.image_limits;
            Ok(serde_json::to_vec(&serde_json::json!({
                "maxImageBytes": limits.max_image_bytes,
                "maxImagesPerMessage": limits.max_images_per_message,
                "maxMessageImageBytes": limits.max_message_image_bytes,
                "maxImagePixels": limits.max_image_pixels,
                "maxImageDimension": limits.max_image_dimension,
                "mediaTypes": limits.media_types,
            }))
            .map_err(|error| format!("limits reply encode failed: {error}"))?)
        }
        face::METHOD_SAVE_IMAGE => {
            let request: serde_json::Value = serde_json::from_slice(payload)
                .map_err(|error| format!("attachments payload must be JSON: {error}"))?;
            let data_text = request
                .get("data")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "save-image payload requires data".to_owned())?;
            let data = fr_wire::base64::decode_base64(data_text)
                .map_err(|error| format!("save-image data must be base64: {error:?}"))?;
            let media_type: fr_wire::ImageMediaType = serde_json::from_value(
                request
                    .get("mediaType")
                    .cloned()
                    .ok_or_else(|| "save-image payload requires mediaType".to_owned())?,
            )
            .map_err(|error| format!("save-image mediaType must be a wire media type: {error}"))?;
            let name = request
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let reference = state
                .store
                .save_image(&SaveImageAttachment {
                    data,
                    media_type,
                    name,
                })
                .map_err(|error| {
                    serde_json::json!({
                        "attachmentError": { "message": error.message(), "code": error.code() },
                    })
                    .to_string()
                })?;
            Ok(serde_json::to_vec(&reference)
                .map_err(|error| format!("save-image reply encode failed: {error}"))?)
        }
        _ => Err(format!("unknown fr-attachment method {method}")),
    }
}

/// The shell state: the durable store assembled at activation.
pub struct ShellState {
    store: LocalAttachmentStore,
}

impl std::fmt::Debug for ShellState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellState").finish_non_exhaustive()
    }
}

impl ShellState {
    /// Assembles the state over the deployment defaults (the config
    /// section rides fr-config when the runtime-registry slice wires
    /// the loader document).
    ///
    /// # Errors
    ///
    /// The store configuration validation failure.
    pub fn new() -> Result<Self, String> {
        let config = AttachmentConfig::with_defaults();
        Ok(Self {
            store: LocalAttachmentStore::new(&config)?,
        })
    }

    /// The hosted store (tests drive it directly).
    #[must_use]
    pub const fn store(&self) -> &LocalAttachmentStore {
        &self.store
    }
}

unsafe extern "C" fn plugin_name(out: *mut u8, cap: usize, len: *mut usize) -> i32 {
    // SAFETY: the host guarantees a `cap`-sized out-buffer and a writable
    // `len` slot (export-table contract).
    if unsafe { write_out(PLUGIN_NAME.as_bytes(), out, cap, len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

unsafe extern "C" fn descriptor(out: *mut u8, cap: usize, len: *mut usize) -> i32 {
    // SAFETY: same host buffer contract as `plugin_name`.
    if unsafe { write_out(DESCRIPTOR, out, cap, len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

// ABI v3 hands the outbound `HostExports` vtable here. This shell consumes
// no outbound capability in this slice, so the pointer is dropped
// unstashed: per the export-table contract it is valid for this call only,
// and copying it would extend its lifetime toward the documented UB window
// past `teardown`.
unsafe extern "C" fn setup(_host: *const HostExports) -> *mut std::os::raw::c_void {
    let Ok(state) = ShellState::new() else {
        // Decline activation: a store that cannot validate its
        // configuration must not exist half-wired.
        return std::ptr::null_mut();
    };
    Box::into_raw(Box::new(state)).cast::<std::ffi::c_void>()
}

unsafe extern "C" fn call(
    handle: *mut std::os::raw::c_void,
    method: u32,
    in_data: *const u8,
    in_len: usize,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    if handle.is_null() {
        return STATUS_FAILED;
    }
    // The host contract delivers `in_data` valid for `in_len` readable
    // bytes; an empty payload arrives as null/0 and never dereferences the
    // pointer, which `from_raw_parts` requires non-null even for a
    // zero-length slice.
    let payload: &[u8] = if in_len == 0 || in_data.is_null() {
        &[]
    } else {
        // SAFETY: `in_len` is nonzero and `in_data` non-null here, valid for
        // `in_len` readable bytes per the host contract.
        unsafe { std::slice::from_raw_parts(in_data, in_len) }
    };
    // SAFETY: the handle was produced by `setup` and is valid until the
    // matching `teardown`, which the host never overlaps with a call.
    let state = unsafe { &*(handle.cast::<ShellState>()) };
    let reply = match dispatch(state, method, payload) {
        Ok(reply) => reply,
        Err(message) => {
            let body = serde_json::json!({ "error": message }).to_string();
            // SAFETY: host buffer contract.
            let _ = unsafe { write_out(body.as_bytes(), out, cap, out_len) };
            return STATUS_FAILED;
        }
    };
    // SAFETY: host buffer contract.
    if unsafe { write_out(&reply, out, cap, out_len) } {
        STATUS_OK
    } else {
        STATUS_CAPACITY
    }
}

const unsafe extern "C" fn on_event(
    _handle: *mut std::os::raw::c_void,
    _event: u32,
    _mode: u32,
    _in_data: *const u8,
    _in_len: usize,
    _next_available: u32,
    _out: *mut u8,
    _cap: usize,
    _out_len: *mut usize,
) -> i32 {
    // No listeners are declared; the descriptor's listen map stays empty,
    // so the host never routes events here.
    STATUS_FAILED
}

const unsafe extern "C" fn on_next(
    _handle: *mut std::os::raw::c_void,
    _event: u32,
    _chain_status: i32,
    _data: *const u8,
    _len: usize,
    _out: *mut u8,
    _cap: usize,
    _out_len: *mut usize,
) -> i32 {
    STATUS_FAILED
}

unsafe extern "C" fn teardown(handle: *mut std::os::raw::c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: the handle was produced by `setup` and torn down exactly once.
    drop(unsafe { Box::from_raw(handle.cast::<ShellState>()) });
}

/// Export table for the fr-attachment dylib artifact; the host resolves
/// this symbol by name at load time.
#[unsafe(no_mangle)]
pub static DOUGONG_PLUGIN_EXPORTS: PluginExports = PluginExports {
    abi_version: ABI_VERSION,
    name: plugin_name,
    descriptor,
    setup,
    call,
    on_event,
    on_next,
    teardown,
};

#[cfg(test)]
mod tests {
    use super::{PLUGIN_NAME, ShellState, dispatch};
    use fr_wire::services::attachments as face;

    fn state() -> ShellState {
        ShellState::new().expect("builds")
    }

    #[test]
    fn describe_reports_the_shell_metadata() {
        let reply = dispatch(&state(), face::METHOD_DESCRIBE, b"{}").expect("describe");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply["name"], PLUGIN_NAME);
        assert_eq!(reply["provides"], "attachments");
        assert_eq!(reply["abiVersion"], dougong_abi::ABI_VERSION);
    }

    #[test]
    fn unknown_methods_fail_with_text() {
        assert!(
            dispatch(&state(), 9, b"{}")
                .expect_err("unknown method")
                .contains("unknown fr-attachment method 9")
        );
    }

    #[test]
    fn image_limits_replies_the_deployment_snapshot() {
        let reply = dispatch(&state(), face::METHOD_IMAGE_LIMITS, b"{}").expect("limits");
        let reply: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reply["maxImageBytes"], 20 * 1024 * 1024);
        assert_eq!(reply["maxImagesPerMessage"], 20);
        assert_eq!(reply["maxImageDimension"], 8192);
        assert_eq!(
            reply["mediaTypes"],
            serde_json::json!(["image/png", "image/jpeg", "image/webp", "image/gif"])
        );
    }

    #[test]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the test raster is 4x3, far below one byte per channel"
    )]
    fn save_image_round_trips_a_tiny_png() {
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
        let payload = serde_json::json!({
            "data": fr_wire::base64::encode_base64(&bytes),
            "mediaType": "image/png",
            "name": "shot.png",
        });
        let reply = dispatch(
            &state(),
            face::METHOD_SAVE_IMAGE,
            serde_json::to_vec(&payload).expect("encodes").as_slice(),
        )
        .expect("saves");
        let reference: serde_json::Value = serde_json::from_slice(&reply).expect("json");
        assert_eq!(reference["mediaType"], "image/png");
        assert_eq!(reference["width"], 4);
        assert_eq!(reference["height"], 3);
        assert_eq!(reference["name"], "shot.png");
        assert_eq!(
            reference["attachmentId"]
                .as_str()
                .expect("id")
                .chars()
                .count(),
            71,
            "sha256: plus 64 hex"
        );
    }

    #[test]
    fn save_image_maps_the_attachment_error_to_the_document() {
        let payload = serde_json::json!({
            "data": fr_wire::base64::encode_base64(b"not an image"),
            "mediaType": "image/png",
        });
        let body = dispatch(
            &state(),
            face::METHOD_SAVE_IMAGE,
            serde_json::to_vec(&payload).expect("encodes").as_slice(),
        )
        .expect_err("refuses");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(parsed["attachmentError"]["code"], "INVALID_IMAGE");
    }

    #[test]
    fn descriptor_declares_the_provided_service() {
        // Parses the shipped `DESCRIPTOR` constant itself, so any drift (for
        // example a stale embedded version) fails this test.
        let parsed: serde_json::Value = serde_json::from_slice(super::DESCRIPTOR).expect("json");
        assert_eq!(parsed["name"], PLUGIN_NAME);
        assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(parsed["provide"], serde_json::json!(["attachments"]));
        assert_eq!(parsed["inject"], serde_json::json!([]));
        assert_eq!(parsed["listen"], serde_json::json!({}));
    }
}

// Rust guideline compliant 2026-10-27
