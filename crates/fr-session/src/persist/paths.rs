//! Path sanitization for unvalidated session ids and project directories
//! (dsh format.ts `encodeSegment` / `projectKey`).
//!
//! A session id is an unvalidated branded string, so it MUST be encoded
//! before any filesystem use: no traversal, no collision.

use std::fmt::Write as _;

/// The safe segment characters that stay literal.
const fn is_safe_segment_byte(unit: u16) -> bool {
    matches!(
        unit,
        0x61..=0x7A | 0x41..=0x5A | 0x30..=0x39 | 0x2E | 0x5F | 0x2D
    )
}

/// Encodes an arbitrary string as a single safe path segment, injectively
/// over all JS (UTF-16) strings — including lone surrogates.
///
/// Safe code units remain literal; every other unit, including `~`, becomes
/// `~XXXX`.
///
/// Special-casing `.` and `..` prevents traversal by an otherwise safe whole
/// segment.
///
/// # Errors
///
/// Returns [`PersistError`](crate::persist::PersistError) `Corruption` for
/// an empty segment (dsh throws `cannot encode an empty path segment`).
pub fn encode_segment(raw: &str) -> Result<String, crate::persist::PersistError> {
    if raw.is_empty() {
        return Err(crate::persist::PersistError::Corruption(
            "cannot encode an empty path segment".to_owned(),
        ));
    }
    if raw == "." {
        return Ok("~002E".to_owned());
    }
    if raw == ".." {
        return Ok("~002E~002E".to_owned());
    }
    let mut out = String::with_capacity(raw.len());
    for unit in raw.encode_utf16() {
        if unit != u16::from(b'~') && is_safe_segment_byte(unit) {
            // Safe ASCII code units round-trip through u16 unchanged.
            out.push(char::from(u8::try_from(unit).unwrap_or(b'?')));
        } else {
            out.push('~');
            let _ = write!(out, "{unit:04X}");
        }
    }
    Ok(out)
}

/// Builds the readable directory key for a project path: filesystem and
/// drive separators collapse to `-`, unsafe code units use the same `~XXXX`
///
/// escape as session ids, and the key is bounded for filesystem component
/// limits. Separator replacement and truncation are intentionally lossy.
///
/// # Errors
///
/// Returns `Corruption` for an empty project path.
pub fn project_key(cwd: &str) -> Result<String, crate::persist::PersistError> {
    if cwd.is_empty() {
        return Err(crate::persist::PersistError::Corruption(
            "cannot encode an empty project path".to_owned(),
        ));
    }
    let mut readable = String::with_capacity(cwd.len());
    let mut separator_run = false;
    for unit in cwd.encode_utf16() {
        if matches!(unit, 0x2F | 0x5C | 0x3A) {
            if !separator_run {
                readable.push('-');
            }
            separator_run = true;
        } else if unit != u16::from(b'~') && is_safe_segment_byte(unit) {
            readable.push(char::from(u8::try_from(unit).unwrap_or(b'?')));
            separator_run = false;
        } else {
            readable.push('~');
            let _ = write!(readable, "{unit:04X}");
            separator_run = false;
        }
    }
    let slug = readable.trim_start_matches('-');
    let slug = if slug.is_empty() { "root" } else { slug };
    let end = slug
        .char_indices()
        .map(|(index, _)| index)
        .find(|&index| index >= 251)
        .unwrap_or(slug.len());
    Ok(format!("--{}--", &slug[..end]))
}

#[cfg(test)]
mod tests {
    use super::{encode_segment, project_key};
    use crate::persist::PersistError;

    #[test]
    fn encode_segment_keeps_safe_and_escapes_the_rest() {
        assert_eq!(encode_segment("abc-123_x.y").expect("safe"), "abc-123_x.y");
        assert_eq!(encode_segment("a/b").expect("slash"), "a~002Fb");
        assert_eq!(encode_segment("a~b").expect("tilde"), "a~007Eb");
        assert_eq!(encode_segment(".").expect("dot"), "~002E");
        assert_eq!(encode_segment("..").expect("dotdot"), "~002E~002E");
        // Astral characters escape per UTF-16 code unit, byte-identical to
        // the dsh encoding over its UTF-16 strings.
        assert_eq!(encode_segment("\u{1F600}").expect("emoji"), "~D83D~DE00");
        let error = encode_segment("").expect_err("empty");
        assert_eq!(error.message(), "cannot encode an empty path segment");
    }

    #[test]
    fn project_key_collapses_separators_and_bounds_the_slug() {
        assert_eq!(
            project_key("/home/user/project").expect("key"),
            "--home-user-project--"
        );
        assert_eq!(project_key("C:\\repo\\x").expect("win"), "--C-repo-x--");
        // Trailing separators do not extend the run.
        assert_eq!(project_key("/a//b/").expect("runs"), "--a-b---");
        assert_eq!(
            project_key("").expect_err("empty").message(),
            "cannot encode an empty project path"
        );
        // A separators-only path collapses to the `root` fallback.
        assert_eq!(project_key("///").expect("root"), "--root--");
    }

    #[test]
    fn project_key_slugs_are_bounded_at_251() {
        let long = "x".repeat(400);
        let key = project_key(&long).expect("key");
        assert!(key.starts_with("--"));
        assert!(key.ends_with("--"));
        assert_eq!(key.len(), 2 + 251 + 2);
        let _ = PersistError::Corruption(String::new());
    }
}

// Rust guideline compliant 2026-10-23
