//! Durable `DeepSeek` attachment-to-file-id index (dsh `upload-index.ts`).
//!
//! The pure parse, validate, and naming surface plus the locked
//! file-rewrite operations of [`DeepSeekUploadIndex`]. The index is
//! owner-private state under the fr home, shared by every session of one
//! installation.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

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

/// One exact remote generation being invalidated (dsh
/// `Pick<DeepSeekUploadRecord, 'variantId' | 'fileId'>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalidation {
    /// Complete request-image transformation identity.
    pub variant_id: String,
    /// The provider file id the request actually used.
    pub file_id: DeepSeekFileId,
}

/// Candidate commit outcome when another process already published a
/// reusable upload (dsh `UploadIndexCommit`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadIndexCommit {
    /// The winning mapping (the candidate, or the already-durable record).
    pub record: DeepSeekUploadRecord,
    /// Whether the candidate entered the index.
    pub accepted: bool,
}

/// Atomic local index shared by every `DeepSeek` session of one installation
/// (dsh `DeepSeekUploadIndex`).
///
/// Every mutation runs under an exclusive
/// cross-process lock beside a crash-atomic rewrite of the JSON document.
///
/// M1 is POSIX-only (PRD 19.2 item 15), so the lock is an advisory
/// `flock` on a `<index>.lock` sidecar held via `File::lock`: the dsh
/// PID-record/takeover protocol exists for Windows lock semantics that
/// cannot arise here, while the observable contract (writers serialized,
/// lock never outlives its holder) holds.
#[derive(Debug, Clone)]
pub struct DeepSeekUploadIndex {
    /// Absolute owner-private JSON index path (dsh `path`).
    pub path: std::path::PathBuf,
}

impl DeepSeekUploadIndex {
    /// Bind the index to an explicit path; tests inject a scratch path,
    /// production uses [`index_path`] under the fr home.
    #[must_use]
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Load the stored index, treating a missing file and an unparsable or
    /// unsupported document as an empty index (dsh `load`: ENOENT and
    /// `InvalidUploadIndexError` both fall back to `{formatVersion: 3,
    /// records: []}`; every other read failure throws).
    fn load(&self) -> std::io::Result<StoredIndex> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(parse_index(&text).unwrap_or(StoredIndex {
                format_version: FORMAT_VERSION,
                records: Vec::new(),
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(StoredIndex {
                format_version: FORMAT_VERSION,
                records: Vec::new(),
            }),
            Err(error) => Err(error),
        }
    }

    /// `mkdir -p` the parent directory with `0700`, mirroring dsh
    /// `writeFileAtomic` `dirMode` for directories this call creates.
    fn ensure_parent(&self) -> std::io::Result<()> {
        let Some(parent) = self.path.parent() else {
            return Ok(());
        };
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
    }

    /// Publish the index document atomically: pretty-printed with a
    /// trailing newline, written to a `0600` staging file beside the
    /// final path, then renamed over it (dsh `writeFileAtomic`,
    /// `JSON.stringify(index, undefined, 2)`).
    fn save(&self, index: &StoredIndex) -> std::io::Result<()> {
        self.ensure_parent()?;
        let mut body = serde_json::to_string_pretty(index)
            .map_err(|_error| std::io::Error::other("upload index serialization failed"))?
            .into_bytes();
        body.push(b'\n');
        let stem = self
            .path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("files-v3.json");
        let staging = self.path.with_file_name(format!(
            "{stem}.{}.tmp",
            fr_attachment::request_image_pipeline::random_hex_suffix()
        ));
        let write = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staging)
            .and_then(|mut file| file.write_all(&body));
        if write.is_ok() && std::fs::rename(&staging, &self.path).is_ok() {
            return Ok(());
        }
        let _ = std::fs::remove_file(&staging);
        Err(write
            .err()
            .unwrap_or_else(|| std::io::Error::other("upload index rename failed")))
    }

    /// Run one locked read-modify-write cycle (dsh `withFileLock`).
    fn with_file_lock<T>(
        &self,
        operation: impl FnOnce() -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        self.ensure_parent()?;
        let stem = self
            .path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("files-v3.json");
        let lock_path = self.path.with_file_name(format!("{stem}.lock"));
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)?;
        handle.lock()?;
        operation()
    }

    /// Read one reusable mapping (dsh `get`).
    ///
    /// # Errors
    /// [`std::io::Error`] for read failures other than a missing file.
    pub fn get(
        &self,
        scope: &DeepSeekFileScope,
        variant_id: &str,
        now: u64,
        refresh_margin_ms: u64,
    ) -> std::io::Result<Option<DeepSeekUploadRecord>> {
        let index = self.load()?;
        Ok(index
            .records
            .into_iter()
            .find(|candidate| candidate.scope == *scope && candidate.variant_id == variant_id)
            .filter(|record| reusable(record, now, refresh_margin_ms)))
    }

    /// Publish a completed upload unless another process already published
    /// a reusable mapping; stale records for the same key are dropped (dsh
    /// `commit`).
    ///
    /// # Errors
    /// [`std::io::Error`] for read/lock/write failures.
    pub fn commit(
        &self,
        candidate: DeepSeekUploadRecord,
        now: u64,
        refresh_margin_ms: u64,
    ) -> std::io::Result<UploadIndexCommit> {
        self.with_file_lock(|| {
            let index = self.load()?;
            let existing = index
                .records
                .iter()
                .find(|record| {
                    record.scope == candidate.scope
                        && record.variant_id == candidate.variant_id
                        && reusable(record, now, refresh_margin_ms)
                })
                .cloned();
            if let Some(record) = existing {
                return Ok(UploadIndexCommit {
                    record,
                    accepted: false,
                });
            }
            let mut records: Vec<DeepSeekUploadRecord> = index
                .records
                .into_iter()
                .filter(|record| {
                    reusable(record, now, refresh_margin_ms)
                        && !(record.scope == candidate.scope
                            && record.variant_id == candidate.variant_id)
                })
                .collect();
            records.push(candidate.clone());
            self.save(&StoredIndex {
                format_version: FORMAT_VERSION,
                records,
            })?;
            Ok(UploadIndexCommit {
                record: candidate,
                accepted: true,
            })
        })
    }

    /// Remove exact mappings in one locked rewrite without deleting
    /// concurrently installed successors (dsh `remove`); pairs absent from
    /// the index are ignored and unchanged indexes are not rewritten.
    ///
    /// # Errors
    /// [`std::io::Error`] for read/lock/write failures.
    pub fn remove(
        &self,
        scope: &DeepSeekFileScope,
        generations: &[Invalidation],
    ) -> std::io::Result<()> {
        let invalidated: std::collections::HashSet<(&str, &str)> = generations
            .iter()
            .map(|generation| (generation.variant_id.as_str(), generation.file_id.as_str()))
            .collect();
        self.with_file_lock(|| {
            let index = self.load()?;
            let before = index.records.len();
            let records: Vec<DeepSeekUploadRecord> = index
                .records
                .into_iter()
                .filter(|record| {
                    !(record.scope == *scope
                        && invalidated
                            .contains(&(record.variant_id.as_str(), record.file_id.as_str())))
                })
                .collect();
            if records.len() != before {
                self.save(&StoredIndex {
                    format_version: FORMAT_VERSION,
                    records,
                })?;
            }
            Ok(())
        })
    }

    /// Remove every local mapping for one remote namespace (dsh `clear`);
    /// unchanged indexes are not rewritten.
    ///
    /// # Errors
    /// [`std::io::Error`] for read/lock/write failures.
    pub fn clear(&self, scope: &DeepSeekFileScope) -> std::io::Result<()> {
        self.with_file_lock(|| {
            let index = self.load()?;
            let before = index.records.len();
            let records: Vec<DeepSeekUploadRecord> = index
                .records
                .into_iter()
                .filter(|record| record.scope != *scope)
                .collect();
            if records.len() != before {
                self.save(&StoredIndex {
                    format_version: FORMAT_VERSION,
                    records,
                })?;
            }
            Ok(())
        })
    }
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

    fn hex(n: usize) -> String {
        "a".repeat(n)
    }

    fn committed_record(scope: &str, variant: &str, created_at: u64) -> DeepSeekUploadRecord {
        DeepSeekUploadRecord {
            scope: DeepSeekFileScope::new(scope),
            attachment_id: fr_wire::AttachmentId::new(format!("sha256:{variant}")),
            variant_id: format!("sha256:{variant}"),
            file_id: DeepSeekFileId::new(format!("file-{variant}")),
            bytes: 8,
            created_at,
            expires_at: created_at + 10_000,
        }
    }

    fn scratch_index(tag: &str) -> DeepSeekUploadIndex {
        let directory = std::env::temp_dir().join(format!(
            "fr-upload-index-{tag}-{}-{}",
            std::process::id(),
            fr_attachment::request_image_pipeline::random_hex_suffix()
        ));
        DeepSeekUploadIndex::new(directory.join("llm-deepseek").join("files-v3.json"))
    }

    /// get/commit walk: the first commit accepts and persists, a reusable
    /// duplicate loses, an expired one is replaced, and get honours the
    /// refresh margin.

    #[test]
    fn commit_and_get_pin_reuse_semantics() {
        let index = scratch_index("commit");
        let scope = DeepSeekFileScope::new(hex(64));
        let fresh = committed_record(scope.as_str(), &"b".repeat(64), 1_000);
        let winner = index.commit(fresh.clone(), 2_000, 500).unwrap();
        assert!(winner.accepted);
        assert_eq!(winner.record, fresh);

        // Same key while still reusable: candidate loses, durable record wins.
        let challenger = committed_record(scope.as_str(), &"b".repeat(64), 3_000);
        let loser = index.commit(challenger, 2_000, 500).unwrap();
        assert!(!loser.accepted);
        assert_eq!(loser.record, fresh);

        // Same key whose stored record expired: the challenger replaces it.
        let challenger = committed_record(scope.as_str(), &"b".repeat(64), 3_000);
        let replaced = index.commit(challenger, 11_000, 500).unwrap();
        assert!(replaced.accepted);
        assert_eq!(replaced.record.created_at, 3_000);

        // get returns the mapping only above the margin.
        let cached = index.get(&scope, &fresh.variant_id, 12_000, 500).unwrap();
        assert!(cached.is_some());
        let stale = index.get(&scope, &fresh.variant_id, 13_000, 500).unwrap();
        assert!(stale.is_none());
        let missing = index.get(&scope, &"c".repeat(64), 12_000, 500).unwrap();
        assert!(missing.is_none());
        let _ = std::fs::remove_dir_all(index.path.parent().unwrap());
    }

    /// remove drops only exact scope+variant+file pairs and skips the
    /// rewrite when nothing matched; clear drops a whole namespace.
    #[test]
    fn remove_and_clear_pin_scope_filtering() {
        let index = scratch_index("remove");
        let scope = DeepSeekFileScope::new(hex(64));
        let other_scope = DeepSeekFileScope::new("b".repeat(64));
        let first = committed_record(scope.as_str(), &"b".repeat(64), 1_000);
        let second = committed_record(scope.as_str(), &"c".repeat(64), 1_000);
        let foreign = committed_record(other_scope.as_str(), &"b".repeat(64), 1_000);
        index.commit(first.clone(), 2_000, 0).unwrap();
        index.commit(second.clone(), 2_000, 0).unwrap();
        index.commit(foreign, 2_000, 0).unwrap();

        // Wrong file id for the same variant: nothing removed, no rewrite.
        index
            .remove(
                &scope,
                &[Invalidation {
                    variant_id: second.variant_id.clone(),
                    file_id: DeepSeekFileId::new("file-other"),
                }],
            )
            .unwrap();
        assert!(
            index
                .get(&scope, &second.variant_id, 2_000, 0)
                .unwrap()
                .is_some()
        );

        // Exact pair: removed without touching same-key successors or the
        // other namespace.
        index
            .remove(
                &scope,
                &[
                    Invalidation {
                        variant_id: first.variant_id.clone(),
                        file_id: first.file_id.clone(),
                    },
                    Invalidation {
                        variant_id: second.variant_id.clone(),
                        file_id: DeepSeekFileId::new("absent"),
                    },
                ],
            )
            .unwrap();
        assert!(
            index
                .get(&scope, &first.variant_id, 2_000, 0)
                .unwrap()
                .is_none()
        );
        assert!(
            index
                .get(&scope, &second.variant_id, 2_000, 0)
                .unwrap()
                .is_some()
        );
        assert!(
            index
                .get(&other_scope, &first.variant_id, 2_000, 0)
                .unwrap()
                .is_some()
        );

        // clear drops only the named namespace.
        index.clear(&scope).unwrap();
        assert!(
            index
                .get(&scope, &second.variant_id, 2_000, 0)
                .unwrap()
                .is_none()
        );
        assert!(
            index
                .get(&other_scope, &first.variant_id, 2_000, 0)
                .unwrap()
                .is_some()
        );
        let _ = std::fs::remove_dir_all(index.path.parent().unwrap().parent().unwrap());
    }

    /// The stored document is pretty-printed with a trailing newline, the
    /// index file is `0600`, its parent directory `0700`, and a missing or
    /// corrupt document loads as an empty index.
    #[test]
    fn persistence_pins_layout_and_recovery() {
        use std::os::unix::fs::PermissionsExt as _;
        let index = scratch_index("layout");
        let scope = DeepSeekFileScope::new(hex(64));
        index
            .commit(
                committed_record(scope.as_str(), &"b".repeat(64), 1_000),
                2_000,
                0,
            )
            .unwrap();
        let text = std::fs::read_to_string(&index.path).unwrap();
        assert!(text.starts_with("{\n  \"formatVersion\": 3,"));
        assert!(text.ends_with("}\n"));
        let mode = std::fs::metadata(&index.path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let dir_mode = index
            .path
            .parent()
            .unwrap()
            .metadata()
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700);
        assert!(index.path.with_file_name("files-v3.json.lock").exists());

        // A fresh index over the same document reads the stored mapping.
        let reopened = DeepSeekUploadIndex::new(&index.path);
        assert!(
            reopened
                .get(&scope, &format!("sha256:{}", "b".repeat(64)), 2_000, 0)
                .unwrap()
                .is_some()
        );

        // Missing file: empty. Corrupt document: empty (dsh ENOENT and
        // InvalidUploadIndexError both fall back to an empty index).
        let empty = scratch_index("empty");
        assert_eq!(empty.load().unwrap().records.len(), 0);
        std::fs::create_dir_all(empty.path.parent().unwrap()).unwrap();
        std::fs::write(&empty.path, "not json").unwrap();
        assert_eq!(empty.load().unwrap().records.len(), 0);
        let _ = std::fs::remove_dir_all(index.path.parent().unwrap());
        let _ = std::fs::remove_dir_all(empty.path.parent().unwrap());
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
