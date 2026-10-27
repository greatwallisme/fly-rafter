// Rust guideline compliant 2026-10-28
//! Content identity for workspace instruction duplicate suppression (dsh
//! agent-instructions/src/digest.ts, verbatim semantics).

use sha1::{Digest, Sha1};

/// The content identity used across instruction loading and session
/// state: a lowercase hexadecimal SHA-1 of the exact UTF-8 text.
#[must_use]
pub fn instruction_content_sha1(content: &str) -> String {
    let digest = Sha1::digest(content.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// The whitespace-insensitive identity for duplicate suppression.
///
/// Leading and trailing whitespace trims before hashing, so a symlinked
/// or byte-copied sibling that differs only by surrounding whitespace
/// still collapses to a single rendered file.
#[must_use]
pub fn trimmed_instruction_digest(content: &str) -> String {
    instruction_content_sha1(content.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_matches_the_reference_vector() {
        assert_eq!(
            instruction_content_sha1("abc"),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
    }

    #[test]
    fn trimmed_digest_ignores_surrounding_whitespace() {
        assert_eq!(
            trimmed_instruction_digest("  same  \n"),
            trimmed_instruction_digest("same")
        );
        assert_ne!(
            trimmed_instruction_digest("same"),
            trimmed_instruction_digest("different")
        );
    }
}
