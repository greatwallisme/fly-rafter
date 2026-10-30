//! Verbatim content-addressed local file storage (the dsh
//! `attachment-local` `file-store.ts`).
//!
//! The digest names a directory so the sanitized display name stays
//! the stored leaf name, giving models and users a path that ends in
//! the real filename. The dsh async-iterable stream faces materialize
//! as chunk slices here: the storage semantics — staged publication,
//! digest-verified alias dedup, and terminal integrity verification —
//! are unchanged.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{
    ATTACHMENT_CORRUPT_CODE, ATTACHMENT_NOT_FOUND_CODE, ATTACHMENT_READ_FAILED_CODE,
    INVALID_ATTACHMENT_REF_CODE,
};
use crate::request_image_pipeline::AttachmentError;
use crate::store::{publish_immutable_alias, publish_immutable_object};
use fr_wire::FileAttachmentRef;

const WINDOWS_DEVICE_NAME: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

fn is_windows_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default();
    let stem = stem.trim_end_matches(['.', ' ']);
    WINDOWS_DEVICE_NAME
        .iter()
        .any(|device| device.eq_ignore_ascii_case(stem))
}

/// Sanitizes one caller display name into a safe stored leaf name
/// (dsh `fileLeafName`).
///
/// Both separator styles strip by hand; control and Windows-refused
/// characters replace; trailing dots and spaces trim; device names
/// prefix; and the result caps at 255 UTF-8 bytes.
#[must_use]
pub fn file_leaf_name(value: Option<&str>) -> String {
    let Some(value) = value else {
        return "file".to_owned();
    };
    let leaf = value
        .rfind('/')
        .into_iter()
        .chain(value.rfind('\\'))
        .max()
        .map_or(value, |index| &value[index + 1..]);
    let clean: String = leaf
        .chars()
        .filter(|character| !character.is_control())
        .map(|character| {
            if matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                character
            }
        })
        .collect();
    let cleaned = clean.trim();
    let mut without_tail = cleaned.trim_end_matches(['.', ' ']).to_owned();
    if is_windows_device_name(&without_tail) {
        without_tail = format!("_{without_tail}");
    }
    without_tail = {
        let prefixed = utf8_prefix(&without_tail, 255);
        prefixed.trim_end_matches(['.', ' ']).to_owned()
    };
    if without_tail.is_empty() || without_tail == "." || without_tail == ".." {
        "file".to_owned()
    } else {
        without_tail
    }
}

/// Truncates to at most `max_bytes` UTF-8 bytes without splitting a
/// character (the dsh `utf8Prefix`).
fn utf8_prefix(value: &str, max_bytes: usize) -> String {
    let mut bytes = 0_usize;
    let mut prefix = String::new();
    for character in value.chars() {
        let character_bytes = character.len_utf8();
        if bytes + character_bytes > max_bytes {
            break;
        }
        prefix.push(character);
        bytes += character_bytes;
    }
    prefix
}

/// Validates one durable file reference and returns its hex digest
/// (dsh `ensureFileReference`): the name must be the stored leaf
/// spelling of itself.
fn ensure_file_reference(reference: &FileAttachmentRef) -> Result<String, AttachmentError> {
    let hex = reference
        .attachment_id
        .as_str()
        .strip_prefix("sha256:")
        .unwrap_or_default();
    let valid = hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !valid || reference.name != file_leaf_name(Some(&reference.name)) {
        return Err(AttachmentError::new(
            "File attachment reference is invalid.",
            INVALID_ATTACHMENT_REF_CODE,
        ));
    }
    Ok(hex.to_owned())
}

/// Derives the absolute immutable-object path for one stored file (dsh
/// `storedFilePath`): `root/files/<sh[0..2]>/<sha>/<name>`.
///
/// # Errors
///
/// [`AttachmentError`] `INVALID_ATTACHMENT_REF` for a malformed
/// reference.
pub fn stored_file_path(
    root: &Path,
    reference: &FileAttachmentRef,
) -> Result<PathBuf, AttachmentError> {
    let sha256 = ensure_file_reference(reference)?;
    Ok(root
        .join("files")
        .join(&sha256[..2])
        .join(&sha256)
        .join(&reference.name))
}

/// The canonical object path shared by every display name for one
/// digest (dsh `storedFileObjectPath`).
fn stored_file_object_path(root: &Path, sha256: &str) -> PathBuf {
    root.join("file-objects").join(&sha256[..2]).join(sha256)
}

/// Commits one file byte-for-byte below a versioned attachment root
/// (dsh `saveFileVerbatim`).
///
/// # Errors
///
/// The publication family.
pub fn save_file_verbatim(
    root: &Path,
    data: &[u8],
    name: Option<&str>,
) -> Result<FileAttachmentRef, AttachmentError> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let sha256 = format!("{:x}", hasher.finalize());
    let reference = FileAttachmentRef {
        attachment_id: fr_wire::ids::AttachmentId::new(format!("sha256:{sha256}")),
        name: file_leaf_name(name),
        bytes: data.len() as u64,
    };
    let object_path = stored_file_object_path(root, &sha256);
    publish_immutable_object(root, &object_path, data, &sha256)?;
    let alias = stored_file_path(root, &reference)?;
    publish_immutable_alias(root, &object_path, &alias, &sha256)?;
    Ok(reference)
}

/// Commits one file byte-for-byte from ordered bounded chunks (the
/// dsh `saveFileStreamVerbatim` sync materialization).
///
/// The digest accumulates across chunks and the object publishes
/// before the display-name alias.
///
/// # Errors
///
/// The publication family.
pub fn save_file_stream_verbatim(
    root: &Path,
    chunks: &[&[u8]],
    name: Option<&str>,
) -> Result<FileAttachmentRef, AttachmentError> {
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    for chunk in chunks {
        hasher.update(chunk);
        bytes += chunk.len() as u64;
    }
    let sha256 = format!("{:x}", hasher.finalize());
    let leaf = file_leaf_name(name);
    let object_path = stored_file_object_path(root, &sha256);
    // The dsh streams chunks into the staging file directly; the sync
    // face materializes the joined bytes for the staged write.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the chunk batch is an in-memory slice pair, so the total is far below usize::MAX on every supported target"
    )]
    let mut data = Vec::with_capacity(bytes as usize);
    for chunk in chunks {
        data.extend_from_slice(chunk);
    }
    publish_immutable_object(root, &object_path, &data, &sha256)?;
    let reference = FileAttachmentRef {
        attachment_id: fr_wire::ids::AttachmentId::new(format!("sha256:{sha256}")),
        name: leaf,
        bytes,
    };
    let alias = stored_file_path(root, &reference)?;
    publish_immutable_alias(root, &object_path, &alias, &sha256)?;
    Ok(reference)
}

/// Reads one stored file and verifies its byte count and digest (the
/// dsh `readFileStreamVerbatim` terminal verification; the bounded
/// chunk face is the async iterable's sync materialization).
///
/// # Errors
///
/// [`AttachmentError`] `ATTACHMENT_NOT_FOUND` for a missing object,
/// `ATTACHMENT_READ_FAILED` for a failed read, `ATTACHMENT_CORRUPT`
/// when the bytes fail their verification.
pub fn read_file_verbatim(
    root: &Path,
    reference: &FileAttachmentRef,
) -> Result<Vec<u8>, AttachmentError> {
    let sha256 = ensure_file_reference(reference)?;
    let data = match std::fs::read(stored_file_path(root, reference)?) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AttachmentError::new(
                "File attachment object is missing.",
                ATTACHMENT_NOT_FOUND_CODE,
            ));
        }
        Err(error) => {
            return Err(AttachmentError::new(
                error.to_string(),
                ATTACHMENT_READ_FAILED_CODE,
            ));
        }
    };
    if data.len() as u64 != reference.bytes {
        return Err(AttachmentError::new(
            "Stored file attachment failed integrity verification.",
            ATTACHMENT_CORRUPT_CODE,
        ));
    }
    let mut hasher = Sha256::new();
    hasher.update(&data);
    if format!("{:x}", hasher.finalize()) != sha256 {
        return Err(AttachmentError::new(
            "Stored file attachment failed integrity verification.",
            ATTACHMENT_CORRUPT_CODE,
        ));
    }
    Ok(data)
}
