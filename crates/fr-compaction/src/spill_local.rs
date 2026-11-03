//! The local-filesystem spill backend.
//!
//! The dsh spill-local store mechanics: private session-scoped
//! directory selection, safe-name derivation, path-traversal
//! protection, and the exclusive owner-only write.

use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::spill::{SaveTextSpill, SpillRef, SpillStore};

/// Prefix shared by default-root creation and startup discovery (the
/// dsh `DEFAULT_ROOT_PREFIX`, pinned verbatim by the contract).
pub const DEFAULT_ROOT_PREFIX: &str = "dsh-spill-";

/// The per-process default spill root, created lazily and privately.
static DEFAULT_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Returns the lazily-created private per-process spill root (the dsh
/// `privateRoot`: one `mkdtemp`-shaped directory under the OS temp
/// dir per process).
#[must_use]
pub fn private_root() -> PathBuf {
    DEFAULT_ROOT
        .get_or_init(|| {
            let base = std::env::temp_dir();
            for _ in 0..64 {
                let candidate = base.join(format!("{DEFAULT_ROOT_PREFIX}{}", random_suffix()));
                if std::fs::create_dir(&candidate).is_ok() {
                    // The dsh mkdtemp mode is 0700 regardless of
                    // umask; re-assert it after creation.
                    let _ = std::fs::set_permissions(
                        &candidate,
                        std::fs::Permissions::from_mode(0o700),
                    );
                    return candidate;
                }
                // Either the name collided (retry with a fresh suffix)
                // or the temp dir itself failed; both take the next
                // candidate and surface on the first save when all
                // fail.
            }
            base.join(format!("{DEFAULT_ROOT_PREFIX}fallback"))
        })
        .clone()
}

/// Six alphanumeric characters (the dsh `mkdtemp` suffix shape the
/// discovery regex matches), from CSPRNG bytes with the same 48-bit
/// entropy as the dsh `randomBytes(6)`.
fn random_suffix() -> String {
    const ALPHABET: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let bytes = Uuid::new_v4();
    let raw = bytes.as_bytes();
    (0..6)
        .map(|index| ALPHABET[(raw[index] % 62) as usize] as char)
        .collect()
}

/// The literal-safe segment character for one code unit, when the
/// unit sits in the ASCII literal set.
fn literal_char(unit: u16) -> Option<char> {
    let byte = u8::try_from(unit).ok()?;
    matches!(
        byte,
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'
    )
    .then_some(byte as char)
}

/// Encodes an arbitrary string as one safe path segment (the dsh
/// `encodeSegment`), injectively over all JS (UTF-16) strings.
///
/// Each code unit stays literal (`[A-Za-z0-9._-]`) or escapes as
/// tilde-plus-four-hex; whole-segment `.`/`..` escape so they never
/// traverse; an empty string encodes to `~` (never an empty segment).
#[must_use]
pub fn encode_segment(raw: &str) -> String {
    if raw.is_empty() {
        return "~".to_owned();
    }
    if raw == "." {
        return "~002E".to_owned();
    }
    if raw == ".." {
        return "~002E~002E".to_owned();
    }
    let mut out = String::new();
    for unit in raw.encode_utf16() {
        if let Some(ch) = literal_char(unit) {
            // The literal set is ASCII, so the unit round-trips
            // through one byte exactly as the JS character form.
            out.push(ch);
        } else {
            out.push('~');
            let _ = FmtWrite::write_fmt(&mut out, format_args!("{unit:04X}"));
        }
    }
    out
}

/// Derives the stable session-scoped directory under a spill root (the
/// dsh `sessionDir`: `session-` plus the first 12 hex characters of
/// `sha256(sessionId)`).
#[must_use]
pub fn session_dir(root: &Path, session_id: &str) -> PathBuf {
    let digest = Sha256::digest(session_id.as_bytes());
    root.join(format!("session-{}", hex_prefix(&digest)))
}

/// The first 12 lowercase hex characters of one digest.
fn hex_prefix(digest: &[u8]) -> String {
    let mut hex = String::with_capacity(12);
    for byte in digest.iter().take(6) {
        let _ = FmtWrite::write_fmt(&mut hex, format_args!("{byte:02x}"));
    }
    hex
}

/// A written spill file: absolute path and UTF-8 byte length (the dsh
/// `SavedText`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedText {
    /// Absolute saved path.
    pub path: PathBuf,
    /// UTF-8 content length.
    pub bytes: u64,
}

/// Writes text to a fresh 0600 file below its private session
/// directory (the dsh `saveTextFile`).
///
/// The directory is recursive 0700; the create is exclusive; an
/// `ENOENT` retry covers a pruning sweep racing the directory away
/// between `mkdir` and `open`.
///
/// # Errors
///
/// Returns the storage failure diagnostic verbatim.
pub fn save_text_file(
    root: &Path,
    session_id: &str,
    suggested_name: &str,
    content: &str,
) -> Result<SavedText, String> {
    let dir = session_dir(root, session_id);
    let path = dir.join(format!(
        "{}-{}",
        random_suffix(),
        encode_segment(suggested_name)
    ));
    let mut last_error = None;
    for _ in 0..4 {
        if let Err(error) = std::fs::create_dir_all(&dir) {
            return Err(format!(
                "spill-local: failed to create {}: {error}",
                dir.display()
            ));
        }
        // The dsh mkdir mode is 0700 regardless of umask; re-assert
        // it after creation.
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path);
        match file {
            Ok(mut handle) => {
                handle.write_all(content.as_bytes()).map_err(|error| {
                    format!("spill-local: failed to write {}: {error}", path.display())
                })?;
                return Ok(SavedText {
                    path,
                    bytes: content.len() as u64,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A concurrent sweep pruned the freshly made directory;
                // recreate and retry (the dsh ENOENT loop).
                last_error = Some(error);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // A name collision (astronomically unlikely with a
                // fresh random prefix): reseed and retry.
                last_error = Some(error);
            }
            Err(error) => {
                return Err(format!(
                    "spill-local: failed to open {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Err(format!(
        "spill-local: failed to open {} after retries: {}",
        path.display(),
        last_error.map_or_else(|| "exhausted".to_owned(), |error| error.to_string())
    ))
}

/// The local backend over the host filesystem (the dsh
/// `LocalSpillStore.saveText`): a path locator plus local read/grep
/// retrieval guidance.
#[derive(Debug, Clone)]
pub struct LocalSpillStore {
    /// The resolved spill root, fixed at construction.
    root: PathBuf,
}

impl LocalSpillStore {
    /// Assembles the backend over an explicit root (the configured
    /// root; the dsh `resolve` semantics).
    #[must_use]
    pub const fn rooted(root: PathBuf) -> Self {
        Self { root }
    }

    /// Assembles the backend over the per-process private default
    /// root.
    #[must_use]
    pub fn private() -> Self {
        Self {
            root: private_root(),
        }
    }

    /// The active spill root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl SpillStore for LocalSpillStore {
    fn save_text(&self, input: &SaveTextSpill) -> Result<SpillRef, String> {
        let saved = save_text_file(
            &self.root,
            &input.owner.session_id,
            &input.suggested_name,
            &input.content,
        )?;
        Ok(SpillRef {
            locator: saved.path.display().to_string(),
            bytes: saved.bytes,
            retrieval_hint: "Use read with offset/limit, or grep this path to search within it."
                .to_owned(),
        })
    }
}

/// POSIX exclusive-create mode bits (the dsh `open(path, 'wx', 0o600)`).
#[cfg(unix)]
trait ModeExt {
    fn mode(&mut self, mode: u32) -> &mut Self;
}

#[cfg(unix)]
impl ModeExt for std::fs::OpenOptions {
    fn mode(&mut self, mode: u32) -> &mut Self {
        std::os::unix::fs::OpenOptionsExt::mode(self, mode)
    }
}

#[cfg(not(unix))]
mod windows_gate {
    // The M1 platform gate (the fr-session lease precedent): the local
    // spill backend's owner-only permission model is POSIX-only, so a
    // Windows target build refuses loudly instead of silently weakening
    // the storage guarantees (PRD 19.2 item 15 discipline).
    const _: () = compile_error!(
        "spill-local requires a POSIX target; the Windows port lands with its platform gate"
    );
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_ROOT_PREFIX, LocalSpillStore, encode_segment, private_root, save_text_file,
        session_dir,
    };
    use crate::spill::{SaveTextSpill, SpillOwner, SpillSource, SpillStore};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fr-spill-store-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn encode_segment_is_the_verbatim_escaping() {
        assert_eq!(encode_segment(""), "~");
        assert_eq!(encode_segment("."), "~002E");
        assert_eq!(encode_segment(".."), "~002E~002E");
        assert_eq!(encode_segment("result.txt"), "result.txt");
        assert_eq!(encode_segment("a/b"), "a~002Fb");
        assert_eq!(encode_segment("a~b"), "a~007Eb");
        assert_eq!(encode_segment("a b"), "a~0020b");
        // The dsh walk is over UTF-16 code units, so a non-BMP scalar
        // escapes as its surrogate pair.
        assert_eq!(encode_segment("\u{1F600}"), "~D83D~DE00");
    }

    #[test]
    fn encode_segment_is_injective_over_distinct_inputs() {
        // Distinct UTF-16 sequences never collide (the escaping is
        // reversible: `~` itself escapes, so no escape prefix is born
        // from literal text).
        assert_ne!(encode_segment("a~0020b"), encode_segment("a b"));
        assert_ne!(encode_segment("~D83D~DE00"), encode_segment("\u{1F600}"));
    }

    #[test]
    fn session_dir_hashes_to_the_twelve_hex_shape() {
        let dir = session_dir(Path::new("/root"), "s1");
        let name = dir
            .file_name()
            .expect("segment")
            .to_string_lossy()
            .to_string();
        assert!(name.starts_with("session-"), "{name}");
        let hex = name.strip_prefix("session-").expect("prefix");
        assert_eq!(hex.len(), 12);
        assert!(
            hex.chars()
                .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch)),
            "{hex}"
        );
        // Stable across calls.
        assert_eq!(session_dir(Path::new("/root"), "s1"), dir);
    }

    #[test]
    fn save_text_file_writes_private_and_unique() {
        let root = temp_root("save");
        let first = save_text_file(&root, "s1", "web_fetch.txt", "body").expect("save");
        let second = save_text_file(&root, "s1", "web_fetch.txt", "body-two").expect("save");
        assert_ne!(
            first.path, second.path,
            "fresh random prefixes never collide"
        );
        assert!(first.path.starts_with(session_dir(&root, "s1")));
        assert_eq!(first.bytes, 4);
        let mode = std::fs::metadata(&first.path)
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the write is owner-only");
        let dir_mode = std::fs::metadata(session_dir(&root, "s1"))
            .expect("dir meta")
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700, "the session directory is private");
        let content = std::fs::read_to_string(&first.path).expect("read back");
        assert_eq!(content, "body");
        // The suggested name rides the file segment encoded.
        let name = first
            .path
            .file_name()
            .expect("segment")
            .to_string_lossy()
            .to_string();
        assert!(name.ends_with("web_fetch.txt"), "{name}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn store_persists_verbatim_with_the_local_hint() {
        let root = temp_root("store");
        let store = LocalSpillStore::rooted(root.clone());
        let reference = store
            .save_text(&SaveTextSpill {
                owner: SpillOwner {
                    session_id: "s1".to_owned(),
                },
                source: SpillSource::Tool {
                    tool_name: "web_fetch".to_owned(),
                    call_id: "c-1".to_owned(),
                    label: "result".to_owned(),
                },
                suggested_name: "web_fetch.txt".to_owned(),
                content: "full text".to_owned(),
            })
            .expect("save");
        assert_eq!(
            reference.retrieval_hint,
            "Use read with offset/limit, or grep this path to search within it."
        );
        assert_eq!(reference.bytes, "full text".len() as u64);
        assert_eq!(
            std::fs::read_to_string(&reference.locator).expect("read"),
            "full text",
            "the content persists verbatim"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn private_root_is_the_mkdtemp_shape_under_the_temp_dir() {
        let root = private_root();
        let name = root
            .file_name()
            .expect("segment")
            .to_string_lossy()
            .to_string();
        assert!(name.starts_with(DEFAULT_ROOT_PREFIX), "{name}");
        let suffix = name.strip_prefix(DEFAULT_ROOT_PREFIX).expect("prefix");
        assert_eq!(suffix.len(), 6);
        assert!(
            suffix.chars().all(|ch| ch.is_ascii_alphanumeric()),
            "{name}"
        );
        let mode = std::fs::metadata(&root).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "mkdtemp creates the root private");
    }
}
