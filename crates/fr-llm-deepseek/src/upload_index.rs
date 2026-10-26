//! Durable `DeepSeek` attachment-to-file-id index (dsh `upload-index.ts`).
//!
//! This is the pure parse, validate, and naming surface; the locked
//! file-rewrite operations belong to the Files slice with their
//! atomic-write seam. The index is owner-private state under the fr home,
//! shared by every session of one installation.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::file_id::{DeepSeekFileId, DeepSeekFileScope};

/// The prefix of every file name this provider uploads (dsh `file-store.ts`
/// `OWNED_FILE_PREFIX`).
pub const OWNED_FILE_PREFIX: &str = "dsh-";

/// The index format this build reads and writes.
pub const FORMAT_VERSION: u64 = 3;

/// One durable remote upload mapping; Unix times are milliseconds (dsh
/// `DeepSeekUploadRecord`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepSeekUploadRecord {
    /// Endpoint/API-key namespace this mapping belongs to.
    pub scope: DeepSeekFileScope,
    /// Provider-independent normalized attachment the request version
    /// derives from (dsh `AttachmentId`).
    pub attachment_id: fr_wire::AttachmentId,
    /// Complete request transformation identity, including route budgets
    /// and encoder parameters (dsh `ImageVariantId`).
    pub variant_id: String,
    /// The provider-side id the upload produced.
    pub file_id: DeepSeekFileId,
    /// Encoded request byte length.
    pub bytes: u64,
    /// Upload completion time in Unix milliseconds.
    pub created_at: u64,
    /// Upload expiry in Unix milliseconds.
    pub expires_at: u64,
}

/// The stored index document (dsh `StoredIndex`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredIndex {
    /// The only supported format version.
    #[serde(rename = "formatVersion")]
    pub format_version: u64,
    /// The surviving mappings.
    pub records: Vec<DeepSeekUploadRecord>,
}

/// The absolute owner-private index path beneath one home root (dsh
/// `join(resolveDshHome(), 'llm-deepseek', 'files-v3.json')`).
#[must_use]
pub fn index_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join("llm-deepseek").join("files-v3.json")
}

/// Derive a non-secret stable index namespace without persisting or logging
/// authentication headers (dsh `deepSeekFileScope`).
///
/// The digest is the SHA-256 of the normalized endpoint, a NUL separator,
/// and the serialized credential.
///
/// Q67-B (2026-10-26): the digest uses the `sha2` crate — the one recorded
/// exception to the zero-new-dependency posture, chosen over hand-rolling
/// a cryptographic primitive.
#[must_use]
pub fn deep_seek_file_scope(base_url: &str, credentials: &str) -> DeepSeekFileScope {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(base_url.trim_end_matches('/'));
    hasher.update([0]);
    hasher.update(credentials.as_bytes());
    DeepSeekFileScope::new(format!("{:x}", hasher.finalize()))
}

/// Whether one mapping still has more lifetime than the refresh margin (dsh
/// `reusable`).
#[must_use]
pub fn reusable(record: &DeepSeekUploadRecord, now: u64, refresh_margin_ms: u64) -> bool {
    match (
        i64::try_from(record.expires_at),
        i64::try_from(now),
        i64::try_from(refresh_margin_ms),
    ) {
        (Ok(expires_at), Ok(now), Ok(margin)) => expires_at - now > margin,
        // Parsed records hold safe integers, so the overflow arms are
        // unreachable through `parse_index`; an out-of-range record can
        // never be reused.
        _ => false,
    }
}

/// `^[0-9a-f]{64}$` (dsh scope/digest grammar, hand-checked to keep the
/// zero-new-dependency posture for the parser).
fn is_hex64(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// `^sha256:[0-9a-f]{64}$` (dsh attachment/variant id grammar).
fn is_sha256_digest(text: &str) -> bool {
    match text.split_once(':') {
        Some(("sha256", digest)) => is_hex64(digest),
        _ => false,
    }
}

/// Parse one stored record with the dsh field grammar (dsh `parseRecord`).
fn parse_record(value: &Value) -> Result<DeepSeekUploadRecord, String> {
    let Some(record) = value.as_object() else {
        return Err("llm-deepseek: upload index contains a non-object record".to_owned());
    };
    let scope = record.get("scope").and_then(Value::as_str);
    let attachment_id = record.get("attachmentId").and_then(Value::as_str);
    let variant_id = record.get("variantId").and_then(Value::as_str);
    let file_id = record.get("fileId").and_then(Value::as_str);
    let bytes = record.get("bytes").and_then(safe_count);
    let created_at = record.get("createdAt").and_then(safe_count);
    let expires_at = record.get("expiresAt").and_then(safe_count);
    if !scope.is_some_and(is_hex64)
        || !attachment_id.is_some_and(is_sha256_digest)
        || !variant_id.is_some_and(is_sha256_digest)
        || file_id.is_none_or(str::is_empty)
        || bytes.is_none()
        || created_at.is_none()
        || expires_at.is_none()
    {
        return Err("llm-deepseek: upload index contains an invalid record".to_owned());
    }
    Ok(DeepSeekUploadRecord {
        scope: DeepSeekFileScope::new(scope.unwrap_or_default()),
        attachment_id: fr_wire::AttachmentId::new(attachment_id.unwrap_or_default()),
        variant_id: variant_id.unwrap_or_default().to_owned(),
        file_id: DeepSeekFileId::new(file_id.unwrap_or_default()),
        bytes: bytes.unwrap_or_default(),
        created_at: created_at.unwrap_or_default(),
        expires_at: expires_at.unwrap_or_default(),
    })
}

/// A non-negative safe integer (dsh `Number.isSafeInteger` with `>= 0`).
fn safe_count(value: &Value) -> Option<u64> {
    let number = value.as_f64()?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
        return None;
    }
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "the bounds above pin the value to 0..=2^53-1"
    )]
    Some(number as u64)
}

/// Parse one stored index document with the full dsh validation chain (dsh
/// `parseIndex`).
///
/// # Errors
/// A plain diagnostic `String` (dsh throws plain `Error`) for invalid JSON,
/// a non-object document, an unsupported format version, an invalid record,
/// or duplicate `scope`/`variantId` mappings.
pub fn parse_index(text: &str) -> Result<StoredIndex, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|_invalid_json| "llm-deepseek: upload index is not valid JSON".to_owned())?;
    let index = value
        .as_object()
        .ok_or_else(|| "llm-deepseek: upload index is not an object".to_owned())?;
    if index.get("formatVersion").and_then(Value::as_f64) != Some(3.0)
        || !index
            .get("records")
            .is_some_and(serde_json::Value::is_array)
    {
        return Err("llm-deepseek: unsupported upload index format".to_owned());
    }
    let mut records = Vec::new();
    let raw_records: &[Value] = index
        .get("records")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    for raw in raw_records {
        records.push(parse_record(raw)?);
    }
    let mut seen = std::collections::HashSet::new();
    for record in &records {
        let key = format!("{}\0{}", record.scope.as_str(), record.variant_id);
        if !seen.insert(key) {
            return Err("llm-deepseek: upload index contains duplicate mappings".to_owned());
        }
    }
    Ok(StoredIndex {
        format_version: FORMAT_VERSION,
        records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(scope: &str, variant: &str) -> DeepSeekUploadRecord {
        DeepSeekUploadRecord {
            scope: DeepSeekFileScope::new(scope),
            attachment_id: fr_wire::AttachmentId::new(format!("sha256:{variant}")),
            variant_id: format!("sha256:{variant}"),
            file_id: DeepSeekFileId::new("file-123"),
            bytes: 4,
            created_at: 1_000,
            expires_at: 2_000,
        }
    }

    /// The files scenario: the index lives at `<FR_HOME>/llm-deepseek/
    /// files-v3.json` with `formatVersion` 3, uploads carry the `dsh-`
    /// name prefix, and the parser rejects every malformed shape with the
    /// dsh diagnostics.
    #[test]
    fn parse_index_validates_and_names() {
        let hex = "a".repeat(64);
        let index = StoredIndex {
            format_version: FORMAT_VERSION,
            records: vec![record(&hex, &"b".repeat(64))],
        };
        let text = serde_json::to_string(&index).expect("serialize");
        assert_eq!(parse_index(&text).expect("round trip"), index);
        assert_eq!(
            index_path(std::path::Path::new("/home/u/.fr")),
            std::path::Path::new("/home/u/.fr/llm-deepseek/files-v3.json")
        );
        assert_eq!(OWNED_FILE_PREFIX, "dsh-");

        let rejects: [(&str, &str); 5] = [
            ("not json", "llm-deepseek: upload index is not valid JSON"),
            ("[1]", "llm-deepseek: upload index is not an object"),
            (
                "{\"formatVersion\": 2, \"records\": []}",
                "llm-deepseek: unsupported upload index format",
            ),
            (
                "{\"formatVersion\": 3, \"records\": [{}]}",
                "llm-deepseek: upload index contains an invalid record",
            ),
            (
                "{\"formatVersion\": 3, \"records\": [{\"scope\": 1}]}",
                "llm-deepseek: upload index contains an invalid record",
            ),
        ];
        for (text, message) in rejects {
            assert_eq!(parse_index(text).expect_err(text), message, "{text}");
        }

        // Duplicate scope/variant mappings reject, and a non-object record
        // is named as such.
        let duplicate = json!({
            "formatVersion": 3,
            "records": [
                {"scope": hex, "attachmentId": format!("sha256:{hex}"),
                 "variantId": format!("sha256:{hex}"), "fileId": "f1",
                 "bytes": 1, "createdAt": 1, "expiresAt": 2},
                {"scope": hex, "attachmentId": format!("sha256:{hex}"),
                 "variantId": format!("sha256:{hex}"), "fileId": "f2",
                 "bytes": 1, "createdAt": 1, "expiresAt": 2},
            ],
        });
        assert_eq!(
            parse_index(&duplicate.to_string()).expect_err("dup"),
            "llm-deepseek: upload index contains duplicate mappings"
        );
        let non_object = json!({"formatVersion": 3, "records": [7]});
        assert_eq!(
            parse_index(&non_object.to_string()).expect_err("non-object"),
            "llm-deepseek: upload index contains a non-object record"
        );
    }

    #[test]
    fn reusable_needs_lifetime_beyond_margin() {
        let record = record(&"a".repeat(64), &"b".repeat(64));
        // More remaining lifetime than the margin: reusable.
        assert!(reusable(&record, 500, 1_000));
        // Exactly the margin remaining: not reusable (strict >).
        assert!(!reusable(&record, 500, 1_500));
        // Already expired: never reusable.
        assert!(!reusable(&record, 2_500, 0));
    }

    /// Q67-B: the namespace digest over the normalized endpoint, a NUL, and
    /// the credential; trailing slashes do not change the namespace.
    #[test]
    fn deep_seek_file_scope_pins_sha256() {
        assert_eq!(
            deep_seek_file_scope("https://api.deepseek.com/anthropic", "sk-abc123").as_str(),
            "d27abffcb4bd4d4e30713dec80ffc8521275b7f26b25cc7ef053bdd0559010e0"
        );
        assert_eq!(
            deep_seek_file_scope("https://api.deepseek.com/anthropic/", "sk-abc123").as_str(),
            "d27abffcb4bd4d4e30713dec80ffc8521275b7f26b25cc7ef053bdd0559010e0"
        );
    }
}

// Rust guideline compliant 2026-10-26
