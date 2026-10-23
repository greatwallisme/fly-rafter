//! Canonical generation filenames and artifact path assembly (dsh
//! session-format `filename.ts` + format.ts path builders).

use std::path::{Path, PathBuf};

use super::{JsonlCompression, PersistError, SESSION_FORMAT_VERSION};
use fr_wire::event::SessionHeader;

/// The canonical raw log basename shape: `session.jsonl` (version zero) or
/// `session.vN.jsonl` with a lowercase numeric component. Uppercase,
/// leading-zero, temporary, and compression-suffixed names are not
/// canonical.
fn parse_session_format_log_filename(filename: &str) -> Option<u64> {
    let rest = filename.strip_prefix("session")?;
    // `session.jsonl` — the original suffix-only generation zero name.
    if rest.strip_prefix(".jsonl") == Some("") {
        return Some(0);
    }
    let generation = rest.strip_prefix(".v")?;
    let digits = generation.strip_suffix(".jsonl")?;
    if digits.is_empty()
        || digits.starts_with('0')
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    digits.parse::<u64>().ok()
}

/// The raw JSONL log basename for one format generation (dsh
/// `sessionFormatLogFilename`).
#[must_use]
pub fn session_format_log_filename(version: u64) -> String {
    if version == 0 {
        "session.jsonl".to_owned()
    } else {
        format!("session.v{version}.jsonl")
    }
}

/// The artifact suffix for one physical encoding.
#[must_use]
pub const fn log_suffix(compression: JsonlCompression) -> &'static str {
    match compression {
        JsonlCompression::Zstd => ".jsonl.zstd",
        JsonlCompression::None => ".jsonl",
    }
}

const fn compression_suffix(compression: JsonlCompression) -> &'static str {
    match compression {
        JsonlCompression::Zstd => ".zstd",
        JsonlCompression::None => "",
    }
}

/// The canonical filename for one immutable generation.
#[must_use]
pub fn generation_log_filename(version: u64, compression: JsonlCompression) -> String {
    format!(
        "{}{}",
        session_format_log_filename(version),
        compression_suffix(compression)
    )
}

/// Parses one canonical generation filename for the selected physical
/// encoding; noncanonical names do not identify committed generations.
#[must_use]
pub fn parse_generation_log_filename(filename: &str, compression: JsonlCompression) -> Option<u64> {
    let suffix = compression_suffix(compression);
    let stem = filename.strip_suffix(suffix)?;
    parse_session_format_log_filename(stem)
}

/// The human-navigable project directory under `root`; `None` selects the
/// `_no-cwd` grouping.
///
/// # Errors
///
/// Returns `Corruption` for an empty `cwd`.
pub fn project_dir(root: &Path, cwd: Option<&str>) -> Result<PathBuf, PersistError> {
    match cwd {
        None => Ok(root.join("_no-cwd")),
        Some(cwd) => Ok(root.join(super::paths::project_key(cwd)?)),
    }
}

/// The directory owned by one session, available for future session-local
/// artifacts.
///
/// # Errors
///
/// Returns `Corruption` for an empty `cwd`.
pub fn session_dir(
    root: &Path,
    cwd: Option<&str>,
    id: &fr_wire::ids::SessionId,
) -> Result<PathBuf, PersistError> {
    Ok(project_dir(root, cwd)?.join(super::paths::encode_segment(id.as_str())?))
}

/// One immutable generation's configured artifact path.
///
/// # Errors
///
/// Returns `Corruption` for an empty `cwd`.
pub fn generation_log_path(
    root: &Path,
    cwd: Option<&str>,
    id: &fr_wire::ids::SessionId,
    version: u64,
    compression: JsonlCompression,
) -> Result<PathBuf, PersistError> {
    Ok(session_dir(root, cwd, id)?.join(generation_log_filename(version, compression)))
}

/// The current generation's append target path for a session.
///
/// # Errors
///
/// Returns `Corruption` for an empty `cwd`.
pub fn log_path(
    root: &Path,
    cwd: Option<&str>,
    id: &fr_wire::ids::SessionId,
    compression: JsonlCompression,
) -> Result<PathBuf, PersistError> {
    generation_log_path(root, cwd, id, SESSION_FORMAT_VERSION, compression)
}

/// The logical header projected from one stored physical header (dsh
/// `SessionStorageMetadata.meta` shape); the exact inherited cut lives on
/// the last tagged end-seed event, never on the header line.
#[must_use]
pub const fn stored_header_is_current(header: &SessionHeader) -> bool {
    header.version == SESSION_FORMAT_VERSION
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        generation_log_filename, generation_log_path, log_suffix, parse_generation_log_filename,
        parse_session_format_log_filename, project_dir, session_dir,
    };
    use crate::persist::JsonlCompression;
    use fr_wire::ids::SessionId;

    #[test]
    fn canonical_filenames_parse_by_generation() {
        assert_eq!(parse_session_format_log_filename("session.jsonl"), Some(0));
        assert_eq!(
            parse_session_format_log_filename("session.v4.jsonl"),
            Some(4)
        );
        assert_eq!(parse_session_format_log_filename("Session.v4.jsonl"), None);
        assert_eq!(parse_session_format_log_filename("session.v04.jsonl"), None);
        assert_eq!(parse_session_format_log_filename("session.v.jsonl"), None);
        assert_eq!(
            parse_session_format_log_filename("session.v4.jsonl.tmp"),
            None
        );
        assert_eq!(
            parse_session_format_log_filename("session.v4.jsonl.zstd"),
            None
        );
        assert_eq!(parse_session_format_log_filename("other.jsonl"), None);
    }

    #[test]
    fn generation_parsing_strips_the_compression_suffix() {
        assert_eq!(
            parse_generation_log_filename("session.v4.jsonl.zstd", JsonlCompression::Zstd),
            Some(4)
        );
        assert_eq!(
            parse_generation_log_filename("session.v4.jsonl.zstd", JsonlCompression::None),
            None
        );
        assert_eq!(
            generation_log_filename(4, JsonlCompression::Zstd),
            "session.v4.jsonl.zstd"
        );
        assert_eq!(
            generation_log_filename(0, JsonlCompression::None),
            "session.jsonl"
        );
        assert_eq!(log_suffix(JsonlCompression::Zstd), ".jsonl.zstd");
        assert_eq!(log_suffix(JsonlCompression::None), ".jsonl");
    }

    #[test]
    fn artifact_paths_assemble_with_encoded_segments() {
        let id = SessionId::new("sess/1");
        let root = Path::new("/sessions");
        let dir = session_dir(root, Some("/repo"), &id).expect("dir");
        assert_eq!(dir, Path::new("/sessions/--repo--/sess~002F1"));
        let no_cwd = project_dir(root, None).expect("no cwd");
        assert_eq!(no_cwd, Path::new("/sessions/_no-cwd"));
        let path =
            generation_log_path(root, Some("/repo"), &id, 4, JsonlCompression::Zstd).expect("path");
        assert_eq!(
            path,
            Path::new("/sessions/--repo--/sess~002F1/session.v4.jsonl.zstd")
        );
    }
}
