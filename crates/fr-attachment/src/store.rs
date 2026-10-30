//! Content-addressed, owner-private local attachment storage (the
//! dsh `attachment-local` `store.ts`).
//!
//! The publication protocol is the durability contract: staged write,
//! fsync, hard-link into place, digest-verified dedup on an existing
//! target, read-only mode, and directory entries synced up to the
//! caller-vouched boundary before any durable reference is reported.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::admission;
use crate::error::{
    ATTACHMENT_CORRUPT_CODE, ATTACHMENT_NOT_FOUND_CODE, ATTACHMENT_READ_FAILED_CODE,
    ATTACHMENT_WRITE_FAILED_CODE, IMAGE_TOO_LARGE_CODE, IMAGE_TYPE_MISMATCH_CODE,
    INVALID_ATTACHMENT_REF_CODE, INVALID_IMAGE_CODE,
};
use crate::limits::ImageAttachmentLimits;
use crate::normalize::{self, NormalizationPolicy};
use crate::request_image_pipeline::AttachmentError;
use fr_wire::ImageAttachmentRef;

fn digest(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// The durable reference display name (dsh `displayName`).
///
/// Both separator styles strip by hand: a POSIX host treats a
/// backslash as an ordinary character, so a basename helper would
/// leak a Windows client's full local path into the reference and the
/// session log. Control characters drop; the leaf trims and caps at
/// 255 bytes.
#[must_use]
pub fn display_name(value: &str) -> Option<String> {
    let leaf = value
        .rfind('/')
        .into_iter()
        .chain(value.rfind('\\'))
        .max()
        .map_or(value, |index| &value[index + 1..]);
    let clean: String = leaf
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    let trimmed = clean.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.chars().take(255).collect())
    }
}

/// Validates one durable reference and returns its hex digest (dsh
/// `ensureReference`).
fn ensure_reference(reference: &ImageAttachmentRef) -> Result<String, AttachmentError> {
    let id = reference.attachment_id.as_str();
    let Some(hex) = id.strip_prefix("sha256:") else {
        return Err(AttachmentError::new(
            "Attachment reference is invalid.",
            INVALID_ATTACHMENT_REF_CODE,
        ));
    };
    let valid = hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !valid {
        return Err(AttachmentError::new(
            "Attachment reference is invalid.",
            INVALID_ATTACHMENT_REF_CODE,
        ));
    }
    Ok(hex.to_owned())
}

/// Derives the absolute immutable-object path for one normalized
/// attachment (dsh `normalizedImagePath`): `root/objects/&lt;sh[0..2]&gt;/&lt;sha&gt;`.
///
/// # Errors
///
/// [`AttachmentError`] `INVALID_ATTACHMENT_REF` for a malformed id.
pub fn normalized_image_path(
    root: &Path,
    reference: &ImageAttachmentRef,
) -> Result<PathBuf, AttachmentError> {
    let sha256 = ensure_reference(reference)?;
    Ok(root.join("objects").join(&sha256[..2]).join(&sha256))
}

/// Runs the admission metadata checks (dsh `inspectMetadata`): empty
/// refusal, full decode with limits, and the declared-type match.
fn inspect_metadata(
    data: &[u8],
    declared: fr_wire::ImageMediaType,
    limits: &ImageAttachmentLimits,
) -> Result<admission::SourceFacts, AttachmentError> {
    if data.is_empty() {
        return Err(AttachmentError::new("Image is empty.", INVALID_IMAGE_CODE));
    }
    let facts = admission::detect_facts(
        data,
        Some((limits.max_image_pixels, limits.max_image_dimension)),
    )?;
    if facts.media_type != declared {
        return Err(AttachmentError::new(
            "Declared image type does not match its bytes.",
            IMAGE_TYPE_MISMATCH_CODE,
        ));
    }
    Ok(facts)
}

/// Fully prepared normalized object, verified before any batch member
/// is persisted (dsh `PreparedImageFile`).
#[derive(Debug, Clone)]
pub struct PreparedImageFile {
    /// Deterministic normalized bytes whose digest is the reference id.
    pub data: Vec<u8>,
    /// Durable reference describing the bytes.
    pub reference: ImageAttachmentRef,
}

/// Decodes, normalizes, and verifies one submitted image without
/// touching storage (dsh `prepareImageFile`).
///
/// The admission gate cannot later be refused by the normalized byte
/// cap during publication, because normalization already proved its
/// output fits.
///
/// # Errors
///
/// [`AttachmentError`] the admission family (byte cap, decode,
/// type mismatch) or the normalization conversion family.
pub fn prepare_image_file(
    input: &SaveImageAttachment,
    limits: &ImageAttachmentLimits,
    policy: &NormalizationPolicy,
) -> Result<PreparedImageFile, AttachmentError> {
    if input.data.len() as u64 > limits.max_image_bytes {
        return Err(AttachmentError::new(
            "Image exceeds the configured byte limit.",
            IMAGE_TOO_LARGE_CODE,
        ));
    }
    let facts = inspect_metadata(&input.data, input.media_type, limits)?;
    let normalized = normalize::normalize_image(&input.data, &facts, policy)?;
    let sha256 = digest(&normalized.data);
    let name = input.name.as_deref().and_then(display_name);
    let downscaled = facts.width != normalized.width || facts.height != normalized.height;
    let bytes_len = normalized.data.len();
    Ok(PreparedImageFile {
        data: normalized.data,
        reference: ImageAttachmentRef {
            attachment_id: fr_wire::ids::AttachmentId::new(format!("sha256:{sha256}")),
            media_type: normalized.media_type,
            bytes: bytes_len as u64,
            width: normalized.width,
            height: normalized.height,
            name,
            original_dimensions: downscaled.then_some(fr_wire::content::OriginalDimensions {
                width: facts.width,
                height: facts.height,
            }),
        },
    })
}

/// Runs the full admission policy for one image without touching
/// storage (dsh `validateImageFile`).
///
/// # Errors
///
/// The admission family.
pub fn validate_image_file(
    input: &SaveImageAttachment,
    limits: &ImageAttachmentLimits,
    policy: &NormalizationPolicy,
) -> Result<(), AttachmentError> {
    prepare_image_file(input, limits, policy).map(|_| ())
}

/// One submitted image (dsh `SaveImageAttachment`): encoded bytes, the
/// declared media type, and the optional display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveImageAttachment {
    /// Exact encoded bytes.
    pub data: Vec<u8>,
    /// Declared media type.
    pub media_type: fr_wire::ImageMediaType,
    /// Optional display name (a client-side filename).
    pub name: Option<String>,
}

/// Verified stored bytes beside their durable reference (dsh
/// `StoredImageAttachment`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredImageAttachment {
    /// The durable reference recorded in the session log.
    pub reference: ImageAttachmentRef,
    /// Verified stored bytes.
    pub data: Vec<u8>,
}

/// Makes one directory's entries durable (fsync on a read-only
/// directory handle): a synced file alone does not survive a crash
/// when its directory entry never reached storage (dsh
/// `syncDirectory`; Windows cannot open directory handles and its
/// metadata journaling owns entry durability there).
fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let _ = path;
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let handle = std::fs::File::open(path)?;
        handle.sync_all()
    }
}

/// Creates one private directory tree and persists every ancestor
/// entry up to a caller-vouched durable boundary (dsh
/// `ensureDurableDirectory`): the walk deliberately re-syncs what
/// mkdir reports as existing, because a concurrent first save can
/// create a level this process then merely observes — "already
/// existed" is not "already durable".
fn ensure_durable_directory(path: &Path, boundary: &Path) -> Result<(), AttachmentError> {
    std::fs::create_dir_all(path).map_err(|error| persist_failure(&error))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| persist_failure(&error))?;
    let mut level = path.to_path_buf();
    while level != boundary {
        let Some(parent) = level.parent() else {
            break;
        };
        sync_directory(parent).map_err(|error| persist_failure(&error))?;
        if parent == level {
            break;
        }
        level = parent.to_path_buf();
    }
    Ok(())
}

fn persist_failure(error: &std::io::Error) -> AttachmentError {
    AttachmentError::new(error.to_string(), ATTACHMENT_WRITE_FAILED_CODE)
}

fn homes() -> &'static Mutex<HashSet<PathBuf>> {
    static HOMES: std::sync::OnceLock<Mutex<HashSet<PathBuf>>> = std::sync::OnceLock::new();
    HOMES.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Establishes this process's proof that one harness-home entry and
/// every ancestor below the filesystem root are durable (dsh
/// `ensureDurableHome`): mere existence is insufficient, because a
/// concurrent process may have created the directory but not synced
/// its parent.
fn ensure_durable_home(home: &Path) -> Result<PathBuf, AttachmentError> {
    let known = {
        let seen = homes().lock().map_err(|_poisoned| corrupt_state())?;
        seen.contains(home)
    };
    if !known {
        let root = home.ancestors().last().unwrap_or(home).to_path_buf();
        ensure_durable_directory(home, &root)?;
        let mut seen = homes().lock().map_err(|_poisoned| corrupt_state())?;
        seen.insert(home.to_path_buf());
    }
    Ok(home.to_path_buf())
}

fn corrupt_state() -> AttachmentError {
    AttachmentError::new(
        "Unable to persist attachment.",
        ATTACHMENT_WRITE_FAILED_CODE,
    )
}

/// Streams one immutable object from bounded chunks into a staging
/// file, then publishes it at a digest-derived target (dsh
/// `stageImmutableObject` + `publishStagedObject`): staged write with
/// an exclusive 0600 name, fsync, hard-link into place, digest-
/// verified dedup on an existing target, read-only mode, and the
/// parent entries synced up to (excluding) the root.
pub(crate) fn publish_immutable_object(
    root: &Path,
    target: &Path,
    data: &[u8],
    sha256: &str,
) -> Result<(), AttachmentError> {
    let staged = stage_immutable_object(root, data)?;
    if staged.1 != sha256 {
        remove_temporary(&staged.0)?;
        return Err(AttachmentError::new(
            "Attachment bytes do not match their publication digest.",
            ATTACHMENT_CORRUPT_CODE,
        ));
    }
    publish_staged_object(root, target, &staged.0, sha256)
}

/// Stages one object below `root/tmp` and returns
/// `(temporary path, digest, byte count)`.
fn stage_immutable_object(
    root: &Path,
    data: &[u8],
) -> Result<(PathBuf, String, u64), AttachmentError> {
    let staging = root.join("tmp");
    // Establish the harness home itself against the filesystem root
    // once per process; every process performs that proof
    // independently (dsh stageImmutableObject home boundary).
    let home = root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(corrupt_state)?;
    let boundary = ensure_durable_home(home)?;
    ensure_durable_directory(&staging, &boundary)?;
    let temporary = staging.join(format!(
        "{}.tmp",
        crate::request_image_pipeline::random_hex_suffix()
    ));
    let write = (|| -> std::io::Result<(String, u64)> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let mut hasher = Sha256::new();
        hasher.update(data);
        file.write_all(data)?;
        file.sync_all()?;
        Ok((format!("{:x}", hasher.finalize()), data.len() as u64))
    })();
    let (sha256, bytes) = match write {
        Ok(values) => values,
        Err(error) => {
            remove_temporary(&temporary)?;
            return Err(AttachmentError::new(
                error.to_string(),
                ATTACHMENT_WRITE_FAILED_CODE,
            ));
        }
    };
    Ok((temporary, sha256, bytes))
}

/// Publishes one staged object at its target: durable parent tree,
/// hard-link with digest-verified dedup, read-only mode, and the
/// parent walk synced up to (excluding) the root.
fn publish_staged_object(
    root: &Path,
    target: &Path,
    staged: &Path,
    sha256: &str,
) -> Result<(), AttachmentError> {
    let Some(parent) = target.parent() else {
        return Err(corrupt_state());
    };
    let outcome = (|| -> Result<(), AttachmentError> {
        ensure_durable_directory(parent, root)?;
        if let Err(error) = std::fs::hard_link(staged, target) {
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(AttachmentError::new(
                    error.to_string(),
                    ATTACHMENT_WRITE_FAILED_CODE,
                ));
            }
            if digest_file(target).map_err(|error| persist_failure(&error))? != sha256 {
                return Err(AttachmentError::new(
                    "Stored attachment failed integrity verification.",
                    ATTACHMENT_CORRUPT_CODE,
                ));
            }
        }
        // Windows shares the read-only attribute across hard links and
        // refuses to unlink either name once set, so discard the
        // staging name before read-only mode lands (the dsh order).
        remove_temporary(staged)?;
        std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o400))
            .map_err(|error| persist_failure(&error))?;
        let mut level = parent.to_path_buf();
        while level != root {
            sync_directory(&level).map_err(|error| persist_failure(&error))?;
            match level.parent() {
                Some(parent) => level = parent.to_path_buf(),
                None => break,
            }
        }
        Ok(())
    })();
    match outcome {
        Ok(()) => Ok(()),
        Err(error) => {
            remove_temporary(staged)?;
            if error.code() == ATTACHMENT_CORRUPT_CODE
                || error.code() == ATTACHMENT_WRITE_FAILED_CODE
            {
                Err(error)
            } else {
                Err(AttachmentError::new(
                    error.message().to_owned(),
                    ATTACHMENT_WRITE_FAILED_CODE,
                ))
            }
        }
    }
}

/// Recomputes one stored file's sha256 (dsh `digestFile`).
fn digest_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        hasher.update(&chunk[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Removes one staging name; any cleanup failure except an absent
/// file replaces the in-flight failure exactly as the dsh rethrow
/// does (dsh `removeTemporary`).
fn remove_temporary(path: &Path) -> Result<(), AttachmentError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AttachmentError::new(
            error.to_string(),
            ATTACHMENT_WRITE_FAILED_CODE,
        )),
    }
}

/// Publishes another durable hard-link name for an existing immutable
/// object (dsh `publishImmutableAlias`): the file-store display-name
/// alias; dedup verifies the existing target's digest.
pub(crate) fn publish_immutable_alias(
    root: &Path,
    source: &Path,
    target: &Path,
    sha256: &str,
) -> Result<(), AttachmentError> {
    let Some(parent) = target.parent() else {
        return Err(corrupt_state());
    };
    let outcome = (|| -> Result<(), AttachmentError> {
        let home = root
            .parent()
            .and_then(Path::parent)
            .ok_or_else(corrupt_state)?;
        let boundary = ensure_durable_home(home)?;
        ensure_durable_directory(parent, &boundary)?;
        if let Err(error) = std::fs::hard_link(source, target) {
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(AttachmentError::new(
                    error.to_string(),
                    ATTACHMENT_WRITE_FAILED_CODE,
                ));
            }
            if digest_file(target).map_err(|error| persist_failure(&error))? != sha256 {
                return Err(AttachmentError::new(
                    "Stored attachment failed integrity verification.",
                    ATTACHMENT_CORRUPT_CODE,
                ));
            }
        }
        std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o400))
            .map_err(|error| persist_failure(&error))?;
        let mut level = parent.to_path_buf();
        while level != root {
            sync_directory(&level).map_err(|error| persist_failure(&error))?;
            match level.parent() {
                Some(parent) => level = parent.to_path_buf(),
                None => break,
            }
        }
        Ok(())
    })();
    match outcome {
        Ok(()) => Ok(()),
        Err(error) if error.code() == ATTACHMENT_WRITE_FAILED_CODE => Err(error),
        Err(error) if error.code() == ATTACHMENT_CORRUPT_CODE => Err(error),
        Err(error) => Err(AttachmentError::new(
            error.message().to_owned(),
            ATTACHMENT_WRITE_FAILED_CODE,
        )),
    }
}

/// Publishes one already verified normalized image below a versioned
/// attachment root (dsh `commitPreparedImageFile`): the prepared bytes
/// must still match their reference before anything lands.
///
/// # Errors
///
/// [`AttachmentError`] `ATTACHMENT_CORRUPT` on the prepared-bytes
/// verification, or the publication family.
pub fn commit_prepared_image_file(
    root: &Path,
    prepared: &PreparedImageFile,
) -> Result<ImageAttachmentRef, AttachmentError> {
    let sha256 = ensure_reference(&prepared.reference)?;
    if digest(&prepared.data) != sha256 || prepared.data.len() as u64 != prepared.reference.bytes {
        return Err(AttachmentError::new(
            "Prepared attachment bytes do not match their reference.",
            ATTACHMENT_CORRUPT_CODE,
        ));
    }
    let path = normalized_image_path(root, &prepared.reference)?;
    publish_immutable_object(root, &path, &prepared.data, &sha256)?;
    Ok(prepared.reference.clone())
}

/// Decodes and normalizes one image once, then publishes the prepared
/// object (dsh `saveImageFile`).
///
/// # Errors
///
/// The admission family, then the publication family.
pub fn save_image_file(
    root: &Path,
    input: &SaveImageAttachment,
    limits: &ImageAttachmentLimits,
    policy: &NormalizationPolicy,
) -> Result<ImageAttachmentRef, AttachmentError> {
    let prepared = prepare_image_file(input, limits, policy)?;
    commit_prepared_image_file(root, &prepared)
}

/// Reads and verifies one content-addressed image (dsh
/// `readImageFile`).
///
/// The digest proves these are the exact bytes admission fully
/// decoded, so the read path only re-derives the header fields (no
/// raster decode, no per-request pixel amplification on history
/// replay).
///
/// # Errors
///
/// [`AttachmentError`] `ATTACHMENT_NOT_FOUND` for a missing object,
/// `ATTACHMENT_READ_FAILED` for a failed read, `ATTACHMENT_CORRUPT`
/// for any verification failure.
pub fn read_image_file(
    root: &Path,
    reference: &ImageAttachmentRef,
) -> Result<StoredImageAttachment, AttachmentError> {
    let sha256 = ensure_reference(reference)?;
    let data = match std::fs::read(normalized_image_path(root, reference)?) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AttachmentError::new(
                "Attachment object is missing.",
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
    if digest(&data) != sha256 {
        return Err(AttachmentError::new(
            "Stored attachment failed integrity verification.",
            ATTACHMENT_CORRUPT_CODE,
        ));
    }
    let metadata = admission::detect_facts(&data, None)?;
    if metadata.media_type != reference.media_type
        || data.len() as u64 != reference.bytes
        || metadata.width != reference.width
        || metadata.height != reference.height
    {
        return Err(AttachmentError::new(
            "Stored attachment metadata does not match its reference.",
            ATTACHMENT_CORRUPT_CODE,
        ));
    }
    Ok(StoredImageAttachment {
        reference: reference.clone(),
        data,
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use super::{commit_prepared_image_file, prepare_image_file, read_image_file, save_image_file};
    use crate::limits::ImageAttachmentLimits;
    use crate::normalize::NormalizationPolicy;

    const LIMITS: ImageAttachmentLimits =
        ImageAttachmentLimits::new(20 * 1024 * 1024, 20, 200 * 1024 * 1024, 64_000_000, 8192);
    const POLICY: NormalizationPolicy = NormalizationPolicy {
        max_pixels: 2048 * 2048,
        max_dimension: 8192,
        max_bytes: 4 * 1024 * 1024,
    };

    /// One unique scratch directory per test (cleaned on drop).
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fr-attachment-store-{tag}-{}-{}",
                std::process::id(),
                crate::request_image_pipeline::random_hex_suffix()
            ));
            std::fs::create_dir_all(&dir).expect("scratch dir creates");
            Self(dir)
        }

        /// The store root: `<scratch>/attachments/v1` (the shape
        /// `LocalAttachmentStore` derives from the home).
        fn root(&self) -> PathBuf {
            self.0.join("attachments").join("v1")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "the test rasters are far below one byte per channel"
    )]
    fn opaque_png(width: u32, height: u32) -> Vec<u8> {
        let pixels =
            image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(width, height, |x, y| {
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

    #[test]
    fn save_then_read_round_trips_and_dedups() {
        let scratch = Scratch::new("roundtrip");
        let input = super::SaveImageAttachment {
            data: opaque_png(6, 4),
            media_type: fr_wire::ImageMediaType::Png,
            name: Some("photo.png".to_owned()),
        };
        let first = save_image_file(&scratch.root(), &input, &LIMITS, &POLICY).expect("saves");
        assert_eq!(first.name.as_deref(), Some("photo.png"));
        assert!(
            first.original_dimensions.is_none(),
            "a clean source is not downscaled"
        );
        // The object path exists below the root and is read-only.
        let path = super::normalized_image_path(&scratch.root(), &first).expect("path");
        assert!(path.exists());
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o222, 0, "the published object is read-only");
        // A second save of identical bytes lands on the same digest.
        let second =
            save_image_file(&scratch.root(), &input, &LIMITS, &POLICY).expect("saves again");
        assert_eq!(first, second);
        // The read verifies and returns the exact bytes.
        let stored = read_image_file(&scratch.root(), &first).expect("reads");
        assert_eq!(stored.data, input.data);
    }

    #[test]
    fn oversized_and_mismatched_inputs_refuse() {
        let scratch = Scratch::new("refusals");
        let tight = ImageAttachmentLimits::new(8, 20, 200 * 1024 * 1024, 64_000_000, 8192);
        let big = super::SaveImageAttachment {
            data: opaque_png(6, 4),
            media_type: fr_wire::ImageMediaType::Png,
            name: None,
        };
        let error = save_image_file(&scratch.root(), &big, &tight, &POLICY).expect_err("byte cap");
        assert_eq!(error.code(), crate::error::IMAGE_TOO_LARGE_CODE);
        let mut mismatched = big;
        mismatched.media_type = fr_wire::ImageMediaType::Jpeg;
        let error = save_image_file(&scratch.root(), &mismatched, &LIMITS, &POLICY)
            .expect_err("type mismatch");
        assert_eq!(error.code(), crate::error::IMAGE_TYPE_MISMATCH_CODE);
        assert_eq!(
            error.message(),
            "Declared image type does not match its bytes."
        );
    }

    #[test]
    fn downscaled_save_records_original_dimensions() {
        let scratch = Scratch::new("downscaled");
        let input = super::SaveImageAttachment {
            data: opaque_png(100, 60),
            media_type: fr_wire::ImageMediaType::Png,
            name: None,
        };
        let policy = NormalizationPolicy {
            max_pixels: 400,
            max_dimension: 8192,
            max_bytes: 4 * 1024 * 1024,
        };
        let reference = save_image_file(&scratch.root(), &input, &LIMITS, &policy).expect("saves");
        let original = reference.original_dimensions.expect("records originals");
        assert_eq!((original.width, original.height), (100, 60));
        assert_eq!((reference.width, reference.height), (25, 15));
    }

    #[test]
    fn corrupt_object_refuses_the_read() {
        let scratch = Scratch::new("corrupt");
        let input = super::SaveImageAttachment {
            data: opaque_png(6, 4),
            media_type: fr_wire::ImageMediaType::Png,
            name: None,
        };
        let reference = save_image_file(&scratch.root(), &input, &LIMITS, &POLICY).expect("saves");
        let path = super::normalized_image_path(&scratch.root(), &reference).expect("path");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("unlocks");
        std::fs::write(&path, b"corrupted bytes").expect("corrupts");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).expect("relocks");
        let error = read_image_file(&scratch.root(), &reference).expect_err("digest mismatch");
        assert_eq!(error.code(), crate::error::ATTACHMENT_CORRUPT_CODE);
    }

    #[test]
    fn prepared_commit_verifies_the_reference() {
        let scratch = Scratch::new("prepared");
        let input = super::SaveImageAttachment {
            data: opaque_png(6, 4),
            media_type: fr_wire::ImageMediaType::Png,
            name: None,
        };
        let mut prepared = prepare_image_file(&input, &LIMITS, &POLICY).expect("prepares");
        prepared.data.push(0);
        let error = commit_prepared_image_file(&scratch.root(), &prepared).expect_err("corrupt");
        assert_eq!(
            error.message(),
            "Prepared attachment bytes do not match their reference."
        );
    }
}
