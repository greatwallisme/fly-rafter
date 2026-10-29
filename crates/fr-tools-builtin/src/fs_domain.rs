// Rust guideline compliant 2026-10-29
//! The sandbox-enforcing local filesystem backend (dsh `fs-local` +
//! `fs-sandbox` composed, the single-arm bundle truth).
//!
//! dsh loads `fs-sandbox` INSTEAD OF `fs-local` in its bundle (a
//! double `ctx.fs` registration fails the load), so the fr port lands
//! one composed backend: the local storage mechanics with the per-call
//! policy fence on the two mutations. Reads pass through untouched:
//! every mode permits reading.
//!
//! The per-target lock chain serializes mutating ops so the
//! read→guard→write window cannot interleave; concurrent writes/edits
//! are deterministically ordered (one wins, the rest see the new
//! version and reject as stale).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use serde_json::Value;
use serde_json::json;

use crate::fence;
use crate::fs_types::FsDirEntry;
use crate::fs_types::FsEditOutcome;
use crate::fs_types::FsEditRequest;
use crate::fs_types::FsError;
use crate::fs_types::FsErrorCode;
use crate::fs_types::FsInfo;
use crate::fs_types::FsKind;
use crate::fs_types::FsPathInfo;
use crate::fs_types::FsTarget;
use crate::fs_types::FsWriteIntent;
use crate::fs_types::FsWriteOutcome;
use crate::fs_types::WriteOperation;
use crate::fsio;

/// The backend configuration (the dsh `Config` with kebab-case keys).
#[derive(Debug, Clone)]
pub struct FsDomainConfig {
    /// Base directory for relative paths (defaults to the process cwd).
    pub cwd: String,
    /// Exclusive UTF-8 byte limit on each overwrite-diff side.
    pub diff_basis_max_bytes: u64,
}

impl FsDomainConfig {
    /// The default configuration over the process cwd.
    ///
    /// # Errors
    ///
    /// The process-cwd read failure.
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            cwd: std::env::current_dir()
                .map_err(|error| format!("fs-local: the process cwd is unreadable: {error}"))?
                .to_string_lossy()
                .into_owned(),
            diff_basis_max_bytes: fsio::DEFAULT_DIFF_BASIS_MAX_BYTES,
        })
    }
}

/// The standing-policy resolution seam (the dsh
/// `ctx.sandboxPolicy.resolve()` consumption; the wire implementation
/// rides the plugin's vtable, tests inject fakes).
pub trait StandingPolicy: Send + Sync {
    /// Resolves the deployment's standing policy for one session.
    ///
    /// # Errors
    ///
    /// The resolution refusal.
    fn resolve(&self, session: Option<&str>) -> Result<Value, String>;
}

/// The local filesystem domain (the dsh `LocalFileSystem` + fence
/// composition).
pub struct FsDomain {
    config: FsDomainConfig,
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    policy: Box<dyn StandingPolicy>,
    default_mode: Option<String>,
}

impl std::fmt::Debug for FsDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsDomain")
            .field("config", &self.config)
            .field("default_mode", &self.default_mode)
            .finish_non_exhaustive()
    }
}

impl FsDomain {
    /// Assembles the backend over its config and the standing-policy
    /// seam.
    #[must_use]
    pub fn new(
        config: FsDomainConfig,
        policy: Box<dyn StandingPolicy>,
        default_mode: Option<String>,
    ) -> Self {
        Self {
            config,
            locks: Mutex::new(HashMap::new()),
            policy,
            default_mode,
        }
    }

    /// The deployment default mode — the capability fact the tool layer
    /// reads to advertise escalation (the dsh `sandboxMode` getter).
    #[must_use]
    pub fn sandbox_mode(&self) -> Option<&str> {
        self.default_mode.as_deref()
    }

    /// Run `op` with exclusive access to `target_key` (FIFO per key;
    /// entries persist for the process lifetime per the dylib
    /// teardown-only judgment).
    fn with_lock<T>(&self, target_key: &str, op: impl FnOnce() -> T) -> T {
        let key_lock = {
            let mut table = self
                .locks
                .lock()
                .expect("the per-target lock table cannot be poisoned by a panicking op");
            Arc::clone(table.entry(target_key.to_owned()).or_default())
        };
        let _guard = key_lock
            .lock()
            .expect("a panicking mutation must not poison the per-target lock");
        op()
    }

    /// Resolve a model/plugin-supplied path into a stable target
    /// (the dsh `resolve`; relative paths resolve against `opts.cwd`).
    ///
    /// # Errors
    ///
    /// The `resolveLocalTarget` refusal family.
    pub fn resolve(&self, path: &str, cwd: Option<&str>) -> Result<FsTarget, FsError> {
        let local = fsio::resolve_local_target(cwd.unwrap_or(&self.config.cwd), path)?;
        Ok(FsTarget {
            target_key: local.target_key,
            display_path: local.display_path,
        })
    }

    /// The canonical absolute path a subprocess in this execution world
    /// can open (the dsh `processPath` — the opaque key spelled out).
    #[must_use]
    pub fn process_path(target: &FsTarget) -> String {
        target.target_key.clone()
    }

    /// Map an absolute harness-host path into this execution world (the
    /// dsh `processPathFromHostPath`; the local backend resolves
    /// absolute spellings, else `None`).
    #[must_use]
    pub fn process_path_from_host_path(host_path: &str) -> Option<String> {
        std::path::Path::new(host_path)
            .is_absolute()
            .then(|| fsio::lexical_resolve("/", host_path))
    }

    /// The canonical `file:` URI for a target (the dsh `fileUrl`; the
    /// percent-encoding set matches `pathToFileURL`).
    #[must_use]
    pub fn file_url(target: &FsTarget) -> String {
        let path = Self::process_path(target);
        let mut out = String::from("file://");
        for byte in path.as_bytes() {
            match byte {
                b'A'..=b'Z'
                | b'a'..=b'z'
                | b'0'..=b'9'
                | b'-'
                | b'_'
                | b'.'
                | b'~'
                | b'/'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'@' => out.push(char::from(*byte)),
                other => {
                    use std::fmt::Write as _;
                    let _ = write!(out, "%{other:02X}");
                }
            }
        }
        out
    }

    /// The containment prefix for one canonical directory (the slash
    /// suffix form).
    #[must_use]
    fn prefix_of(dir: &str) -> String {
        let mut prefix = String::with_capacity(dir.len() + 1);
        prefix.push_str(dir);
        prefix.push('/');
        prefix
    }

    /// Test canonical containment without exposing target keys (the
    /// dsh `contains`).
    #[must_use]
    pub fn contains(parent: &FsTarget, child: &FsTarget) -> bool {
        let parent = Self::process_path(parent);
        let child = Self::process_path(child);
        child == parent || child.starts_with(&Self::prefix_of(&parent))
    }

    /// Return target metadata, or `None` when the target does not exist
    /// (the dsh `stat`).
    ///
    /// # Errors
    ///
    /// A non-missing metadata fault.
    pub fn stat(&self, target: &FsTarget) -> Result<Option<FsInfo>, FsError> {
        let info = fsio::probe(&target.target_key)?;
        Ok(info.map(|info| FsInfo {
            version: info.version,
            kind: info.kind,
            size: Some(info.size),
        }))
    }

    /// Return path metadata without following the final path component
    /// when it is a symbolic link (the dsh `lstat`).
    ///
    /// # Errors
    ///
    /// An empty path or a non-missing metadata fault.
    pub fn lstat(&self, path: &str, cwd: Option<&str>) -> Result<Option<FsPathInfo>, FsError> {
        if path.trim().is_empty() {
            return Err(FsError::new(
                FsErrorCode::NotFound,
                "file_path must be a non-empty string",
            ));
        }
        let spelling = fsio::local_display_path(cwd.unwrap_or(&self.config.cwd), path);
        let info = fsio::probe_no_follow(&spelling)?;
        Ok(info.map(|info| FsPathInfo {
            version: info.version,
            kind: info.kind,
            size: Some(info.size),
        }))
    }

    /// Read the whole regular text file as a single decoded string (the
    /// dsh `readText`).
    ///
    /// # Errors
    ///
    /// The read-family refusals.
    pub fn read_text(&self, target: &FsTarget) -> Result<String, FsError> {
        let local = fsio::LocalTarget {
            display_path: target.display_path.clone(),
            target_key: target.target_key.clone(),
        };
        fsio::read_whole_text(&local, &|| false)
    }

    /// Stream the whole regular text file as decoded text chunks (the
    /// dsh `streamText`; the caller drives the pull iterator).
    ///
    /// # Errors
    ///
    /// The open-preflight refusal.
    pub fn stream_text(
        &self,
        target: &FsTarget,
        signal: Box<dyn Fn() -> bool>,
    ) -> Result<fsio::StreamWholeText, FsError> {
        let local = fsio::LocalTarget {
            display_path: target.display_path.clone(),
            target_key: target.target_key.clone(),
        };
        fsio::StreamWholeText::open(&local, signal)
    }

    /// Read the whole regular file as raw bytes with an inclusive byte
    /// cap (the dsh `readBytes`).
    ///
    /// # Errors
    ///
    /// The cap or preflight refusals.
    pub fn read_bytes(&self, target: &FsTarget, max_bytes: u64) -> Result<Vec<u8>, FsError> {
        let local = fsio::LocalTarget {
            display_path: target.display_path.clone(),
            target_key: target.target_key.clone(),
        };
        fsio::read_whole_bytes(&local, &|| false, max_bytes)
    }

    /// Read one byte window of the regular file (the dsh
    /// `readByteRange`).
    ///
    /// # Errors
    ///
    /// The preflight refusal.
    pub fn read_byte_range(
        &self,
        target: &FsTarget,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>, FsError> {
        let local = fsio::LocalTarget {
            display_path: target.display_path.clone(),
            target_key: target.target_key.clone(),
        };
        fsio::read_byte_window(&local, offset, length, &|| false)
    }

    /// List direct children of a directory in stable name order (the
    /// dsh `listDir`).
    ///
    /// # Errors
    ///
    /// The listing refusal family.
    pub fn list_dir(&self, target: &FsTarget) -> Result<Vec<FsDirEntry>, FsError> {
        let local = fsio::LocalTarget {
            display_path: target.display_path.clone(),
            target_key: target.target_key.clone(),
        };
        let entries = fsio::list_directory(&local, &|| false)?;
        Ok(entries
            .into_iter()
            .map(|entry| FsDirEntry {
                name: entry.name,
                kind: entry.kind,
                target: FsTarget {
                    target_key: entry.target.target_key,
                    display_path: entry.target.display_path,
                },
                version: entry.version,
                size: entry.size,
            })
            .collect())
    }

    /// Atomically create or replace UTF-8 text behind the per-call
    /// policy fence (the dsh `SandboxedFileSystem.writeText` over the
    /// local mechanics).
    ///
    /// # Errors
    ///
    /// The fence refusal, the guarded-mutation refusals, or the write
    /// fault.
    pub fn write_text(
        &self,
        target: &FsTarget,
        content: &str,
        expected: Option<&FsWriteIntent>,
        policy: Option<&Value>,
    ) -> Result<FsWriteOutcome, FsError> {
        let policy = self.effective_policy(policy)?;
        let fresh = fence::checked_target(target, Some(&policy))?;
        let signal = || false;
        self.with_lock(&fresh.target_key, || {
            let existing = fsio::probe(&fresh.target_key)?;
            if existing
                .as_ref()
                .is_some_and(|info| info.kind != FsKind::File)
            {
                return Err(FsError::new(
                    FsErrorCode::NotRegularFile,
                    format!(
                        "cannot write \"{}\": not a regular file",
                        fresh.display_path
                    ),
                ));
            }
            match expected {
                Some(FsWriteIntent::ReplaceIfVersion { version }) => {
                    let Some(existing) = &existing else {
                        return Err(FsError::new(
                            FsErrorCode::StaleVersion,
                            format!(
                                "cannot write \"{}\": file no longer exists",
                                fresh.display_path
                            ),
                        ));
                    };
                    if existing.version != *version {
                        return Err(FsError::new(
                            FsErrorCode::StaleVersion,
                            format!(
                                "cannot write \"{}\": file changed since it was read",
                                fresh.display_path
                            ),
                        ));
                    }
                }
                Some(FsWriteIntent::CreateIfAbsent) if existing.is_some() => {
                    return Err(FsError::new(
                        FsErrorCode::NotObserved,
                        format!(
                            "cannot overwrite existing \"{}\" without reading it first",
                            fresh.display_path
                        ),
                    ));
                }
                // No expectation means an unconditional but still
                // atomic write.
                _ => {}
            }
            // Capture an optional contextual-diff basis before the
            // write; either side at/above the limit yields `None` and
            // consumers retain their whole-file fallback.
            let diffable =
                existing.is_some() && (content.len() as u64) < self.config.diff_basis_max_bytes;
            let before = if diffable {
                fsio::read_text_for_diff(
                    &fresh.target_key,
                    self.config.diff_basis_max_bytes,
                    &signal,
                )?
            } else {
                None
            };
            let create_guard = match expected {
                Some(FsWriteIntent::CreateIfAbsent) => Some(fresh.display_path.as_str()),
                _ => None,
            };
            fsio::write_file_atomic(
                &fresh.target_key,
                content,
                existing.as_ref().map(|info| info.mode),
                &signal,
                create_guard,
            )?;
            let after = fsio::probe(&fresh.target_key)?;
            Ok(FsWriteOutcome {
                operation: if existing.is_some() {
                    WriteOperation::Update
                } else {
                    WriteOperation::Create
                },
                version: version_after_write(after.as_ref(), &fresh),
                before,
                after: fsio::normalize_line_endings(content),
            })
        })
    }

    /// Atomically edit literal text behind the per-call policy fence
    /// (the dsh `SandboxedFileSystem.editText` over the local
    /// mechanics; the version guard is checked before matching inside
    /// the per-target lock).
    ///
    /// # Errors
    ///
    /// The fence refusal, the stale/regular-file refusals, the literal
    /// match refusals, or the write fault.
    pub fn edit_text(
        &self,
        target: &FsTarget,
        edit: &FsEditRequest,
        expected: Option<&str>,
        policy: Option<&Value>,
    ) -> Result<FsEditOutcome, FsError> {
        let policy = self.effective_policy(policy)?;
        let fresh = fence::checked_target(target, Some(&policy))?;
        let signal = || false;
        self.with_lock(&fresh.target_key, || {
            let existing = fsio::probe(&fresh.target_key)?;
            // Stale guard before literal matching: an edit based on an
            // old read reports stale, not a match refusal against newer
            // content. Missing targets use the same stale code on
            // guarded and unconditional edit paths.
            let Some(existing) = existing else {
                return Err(FsError::new(
                    FsErrorCode::StaleVersion,
                    format!(
                        "cannot edit \"{}\": file changed since it was read",
                        fresh.display_path
                    ),
                ));
            };
            if existing.kind != FsKind::File {
                return Err(FsError::new(
                    FsErrorCode::NotRegularFile,
                    format!("cannot edit \"{}\": not a regular file", fresh.display_path),
                ));
            }
            if expected.is_some_and(|version| existing.version != version) {
                return Err(FsError::new(
                    FsErrorCode::StaleVersion,
                    format!(
                        "cannot edit \"{}\": file changed since it was read",
                        fresh.display_path
                    ),
                ));
            }
            let (original, endings) =
                fsio::read_for_edit(&fresh.target_key, &fresh.display_path, &signal)?;
            let (edited, _replacements) = fsio::apply_literal_edit(
                &original,
                &edit.old_string,
                &edit.new_string,
                edit.replace_all,
                &fresh.display_path,
            )?;
            let content = fsio::restore_line_endings(&edited, endings);
            fsio::write_file_atomic(
                &fresh.target_key,
                &content,
                Some(existing.mode),
                &signal,
                None,
            )?;
            let after = fsio::probe(&fresh.target_key)?;
            Ok(FsEditOutcome {
                version: version_after_write(after.as_ref(), &fresh),
                before: original,
                after: edited,
            })
        })
    }

    /// The effective policy: the per-call stamp, else the standing
    /// resolution (the dsh `sandboxPolicy ?? this.ctx.sandboxPolicy.resolve()`).
    fn effective_policy(&self, policy: Option<&Value>) -> Result<Value, FsError> {
        policy.map_or_else(
            || {
                self.policy.resolve(None).map_err(|error| {
                    FsError::new(
                        FsErrorCode::IoError,
                        format!("the standing sandbox policy failed to resolve: {error}"),
                    )
                })
            },
            |policy| Ok(policy.clone()),
        )
    }
}

/// The version token after a write (the dsh `versionAfterWrite`; a
/// concurrent unlink between rename and stat falls back to the
/// sentinel spelling).
fn version_after_write(after: Option<&fsio::PathInfo>, target: &FsTarget) -> String {
    after.map_or_else(
        || format!("missing:{}", target.target_key),
        |info| info.version.clone(),
    )
}

/// The `'fs'` wire face served over the domain (the Q16b-3B full-table
/// seam; method selectors are the fr-wire `services::fs` constants).
impl FsDomain {
    /// Serves one `'fs'` method invocation.
    ///
    /// # Errors
    ///
    /// The payload decode refusal, an unknown method, or the domain
    /// operation's own failure (the typed body-error shape).
    pub fn serve(&self, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        let request: Value = if payload.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(payload)
                .map_err(|error| format!("fs payload must be JSON: {error}"))?
        };
        // The body protocol: a domain failure rides back as a
        // successful wire reply carrying the `{error}` body (the
        // 16a `{value}|{error}` judgment); only a decode refusal or an
        // unknown method fails the wire call itself.
        if !(fr_wire::services::fs::METHOD_DESCRIBE..=fr_wire::services::fs::METHOD_WATCH)
            .contains(&method)
        {
            return Err(format!("unknown fs method {method}"));
        }
        let reply = match self.serve_op(method, &request) {
            Ok(value) => value,
            Err(error) => json!({ "error": fs_body_error_value(&error) }),
        };
        serde_json::to_vec(&reply).map_err(|error| format!("fs reply encode failed: {error}"))
    }

    /// Serves one `'fs'` operation to its reply value.
    fn serve_op(&self, method: u32, request: &Value) -> Result<Value, FsError> {
        use fr_wire::services::fs as face;
        Ok(match method {
            face::METHOD_DESCRIBE => json!({
                "plugin": crate::service::PLUGIN_NAME,
                "service": face::SERVICE_NAME,
                "methods": describe_methods(),
            }),
            face::METHOD_RESOLVE => {
                let path = require_str(request, "path")?;
                let cwd = request.get("cwd").and_then(Value::as_str);
                let target = self.resolve(path, cwd)?;
                json!({ "target": target })
            }
            face::METHOD_PROCESS_PATH => {
                let target: FsTarget = parse_target(request)?;
                json!({ "path": Self::process_path(&target) })
            }
            face::METHOD_PROCESS_PATH_FROM_HOST_PATH => {
                let path = require_str(request, "host_path")?;
                json!({ "path": Self::process_path_from_host_path(path) })
            }
            face::METHOD_FILE_URL => {
                let target: FsTarget = parse_target(request)?;
                json!({ "url": Self::file_url(&target) })
            }
            face::METHOD_CONTAINS => {
                let parent: FsTarget = serde_json::from_value(
                    request
                        .get("parent")
                        .cloned()
                        .ok_or_else(|| "parent target missing".to_owned())?,
                )
                .map_err(|error| format!("parent target decode failed: {error}"))?;
                let child: FsTarget = serde_json::from_value(
                    request
                        .get("child")
                        .cloned()
                        .ok_or_else(|| "child target missing".to_owned())?,
                )
                .map_err(|error| format!("child target decode failed: {error}"))?;
                json!({ "contained": Self::contains(&parent, &child) })
            }
            face::METHOD_STAT => {
                let target: FsTarget = parse_target(request)?;
                let info = self.stat(&target)?;
                json!({ "info": info })
            }
            face::METHOD_LSTAT => {
                let info = self.lstat(
                    require_str(request, "path")?,
                    request.get("cwd").and_then(Value::as_str),
                )?;
                json!({ "info": info })
            }
            face::METHOD_READ_TEXT => {
                let target: FsTarget = parse_target(request)?;
                json!({ "text": self.read_text(&target)? })
            }
            face::METHOD_STREAM_TEXT => {
                let target: FsTarget = parse_target(request)?;
                let stream = self.stream_text(&target, Box::new(|| false))?;
                let mut chunks: Vec<String> = Vec::new();
                for chunk in stream {
                    chunks.push(chunk?);
                }
                json!({ "chunks": chunks })
            }
            face::METHOD_READ_BYTES => {
                let target: FsTarget = parse_target(request)?;
                let max_bytes = request
                    .get("max_bytes")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "max_bytes must be a non-negative integer".to_owned())?;
                let bytes = self.read_bytes(&target, max_bytes)?;
                json!({ "bytes": encode_base64(&bytes) })
            }
            face::METHOD_READ_BYTE_RANGE => {
                let target: FsTarget = parse_target(request)?;
                let offset = request
                    .get("offset")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "offset must be a non-negative integer".to_owned())?;
                let length = request
                    .get("length")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "length must be a non-negative integer".to_owned())?;
                let bytes = self.read_byte_range(&target, offset, length)?;
                json!({ "bytes": encode_base64(&bytes) })
            }
            face::METHOD_LIST_DIR => {
                let target: FsTarget = parse_target(request)?;
                json!({ "entries": self.list_dir(&target)? })
            }
            face::METHOD_WRITE_TEXT | face::METHOD_EDIT_TEXT | face::METHOD_WATCH => {
                return self.serve_write_op(method, request);
            }
            // The band pre-check in `serve` rejects everything else.
            _ => unreachable!("the band pre-check rejected the unknown method"),
        })
    }

    /// Serves one write-family `'fs'` operation.
    fn serve_write_op(&self, method: u32, request: &Value) -> Result<Value, FsError> {
        use fr_wire::services::fs as face;
        let reply = match method {
            face::METHOD_WRITE_TEXT => {
                let target: FsTarget = parse_target(request)?;
                let content = require_str(request, "content")?;
                let expected: Option<FsWriteIntent> = request
                    .get("expected")
                    .filter(|value| !value.is_null())
                    .map(|value| {
                        serde_json::from_value(value.clone())
                            .map_err(|error| format!("write intent decode failed: {error}"))
                    })
                    .transpose()?;
                let outcome =
                    self.write_text(&target, content, expected.as_ref(), request.get("policy"))?;
                json!({ "outcome": outcome })
            }
            face::METHOD_EDIT_TEXT => {
                let target: FsTarget = parse_target(request)?;
                let edit: FsEditRequest = serde_json::from_value(
                    request
                        .get("edit")
                        .cloned()
                        .ok_or_else(|| "edit request missing".to_owned())?,
                )
                .map_err(|error| format!("edit request decode failed: {error}"))?;
                let expected = request
                    .get("expected")
                    .filter(|value| !value.is_null())
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let outcome =
                    self.edit_text(&target, &edit, expected.as_deref(), request.get("policy"))?;
                json!({ "outcome": outcome })
            }
            face::METHOD_WATCH => {
                return Err(FsError::new(
                    FsErrorCode::IoError,
                    "Filesystem watching is not supported by this provider.",
                ));
            }
            // The band pre-check above makes this arm unreachable; it
            // stays for match exhaustion.
            _ => unreachable!("the band pre-check rejected the unknown method"),
        };
        Ok(reply)
    }
}

/// The typed failure mapped onto the fr body-error wire shape.
fn fs_body_error_value(error: &FsError) -> Value {
    json!({
        "code": error.code,
        "message": error.message,
        "name": "FsError",
    })
}

impl From<String> for FsError {
    fn from(message: String) -> Self {
        // The serve-face payload-shape refusals ride the typed channel
        // and surface as the generic I/O arm.
        Self::new(FsErrorCode::IoError, message)
    }
}

/// Reads one required string field off the request.
fn require_str<'a>(request: &'a Value, field: &str) -> Result<&'a str, String> {
    request
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{field} must be a string"))
}

/// Parses one target off the request.
fn parse_target(request: &Value) -> Result<FsTarget, String> {
    serde_json::from_value(
        request
            .get("target")
            .cloned()
            .ok_or_else(|| "target missing".to_owned())?,
    )
    .map_err(|error| format!("target decode failed: {error}"))
}

/// The face metadata echo (the describe method table).
fn describe_methods() -> Value {
    use fr_wire::services::fs as face;
    json!({
        "describe": face::METHOD_DESCRIBE,
        "resolve": face::METHOD_RESOLVE,
        "process_path": face::METHOD_PROCESS_PATH,
        "process_path_from_host_path": face::METHOD_PROCESS_PATH_FROM_HOST_PATH,
        "file_url": face::METHOD_FILE_URL,
        "contains": face::METHOD_CONTAINS,
        "stat": face::METHOD_STAT,
        "lstat": face::METHOD_LSTAT,
        "read_text": face::METHOD_READ_TEXT,
        "stream_text": face::METHOD_STREAM_TEXT,
        "read_bytes": face::METHOD_READ_BYTES,
        "read_byte_range": face::METHOD_READ_BYTE_RANGE,
        "list_dir": face::METHOD_LIST_DIR,
        "write_text": face::METHOD_WRITE_TEXT,
        "edit_text": face::METHOD_EDIT_TEXT,
        "watch": face::METHOD_WATCH,
    })
}

/// The URL-safe base64 encoding (RFC 4648, unpadded) for byte payloads
/// (the fr byte-transport spelling, matching the fr-shell spill face).
#[must_use]
pub fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).map_or(0, |byte| u32::from(*byte));
        let b2 = chunk.get(2).map_or(0, |byte| u32::from(*byte));
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((triple >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One unique scratch directory per test (cleaned by the caller).
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fr-fs-{tag}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
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

    /// The standing policy over one fixed value (the test seam).
    struct FixedPolicy(Value);

    impl StandingPolicy for FixedPolicy {
        fn resolve(&self, _session: Option<&str>) -> Result<Value, String> {
            Ok(self.0.clone())
        }
    }

    fn make(root: &str, mode: &str) -> FsDomain {
        let config = FsDomainConfig {
            cwd: root.to_owned(),
            diff_basis_max_bytes: fsio::DEFAULT_DIFF_BASIS_MAX_BYTES,
        };
        FsDomain::new(
            config,
            Box::new(FixedPolicy(json!({ "mode": mode, "workspace_root": root }))),
            Some(mode.to_owned()),
        )
    }

    fn target_of(domain: &FsDomain, path: &str) -> FsTarget {
        domain.resolve(path, None).expect("target resolves")
    }

    #[test]
    fn resolve_realpaths_existing_files() {
        let scratch = Scratch::new("resolve");
        std::fs::write(scratch.path("a.txt"), "x").expect("seed");
        let domain = make(&scratch.path(""), "danger-full-access");
        let target = domain.resolve("a.txt", None).expect("resolves");
        let expected = std::fs::canonicalize(scratch.path("a.txt"))
            .expect("canonicalize")
            .to_string_lossy()
            .into_owned();
        assert_eq!(target.target_key, expected);
        assert_eq!(target.display_path, scratch.path("a.txt"));
    }

    #[test]
    fn resolve_missing_file_uses_ancestor_suffix_key() {
        let scratch = Scratch::new("resolve-missing");
        let domain = make(&scratch.path(""), "danger-full-access");
        let target = domain
            .resolve("missing/deep/file.txt", None)
            .expect("missing path resolves");
        assert!(
            target.target_key.ends_with("missing/deep/file.txt"),
            "the key appends the missing suffix: {}",
            target.target_key
        );
    }

    #[test]
    fn resolve_file_parent_segment_refuses() {
        let scratch = Scratch::new("resolve-notdir");
        std::fs::write(scratch.path("afile"), "x").expect("seed");
        let domain = make(&scratch.path(""), "danger-full-access");
        let error = domain
            .resolve("afile/child.txt", None)
            .expect_err("the file parent refuses");
        assert_eq!(error.code, FsErrorCode::NotFound);
        assert!(
            error
                .message
                .ends_with("a parent path segment is not a directory")
        );
    }

    #[test]
    fn read_rejects_binary_and_invalid_utf8() {
        let scratch = Scratch::new("read-text");
        let domain = make(&scratch.path(""), "danger-full-access");
        std::fs::write(scratch.path("bin.bin"), [0_u8, 1, 2]).expect("seed binary");
        std::fs::write(scratch.path("bad.txt"), [0xff, 0xfe]).expect("seed invalid");
        let error = domain
            .read_text(&target_of(&domain, "bin.bin"))
            .expect_err("binary refuses");
        assert_eq!(error.code, FsErrorCode::NotText);
        assert!(error.message.ends_with("binary file"));
        let error = domain
            .read_text(&target_of(&domain, "bad.txt"))
            .expect_err("invalid UTF-8 refuses");
        assert_eq!(error.code, FsErrorCode::NotText);
        assert!(error.message.ends_with("invalid UTF-8 text"));
    }

    #[test]
    fn write_creates_updates_and_guards_versions() {
        let scratch = Scratch::new("write-guards");
        let domain = make(&scratch.path(""), "danger-full-access");
        let target = target_of(&domain, "a.txt");

        let outcome = domain
            .write_text(&target, "hello\n", None, None)
            .expect("create succeeds");
        assert_eq!(outcome.operation, WriteOperation::Create);
        assert!(outcome.before.is_none());
        assert_eq!(
            std::fs::read_to_string(scratch.path("a.txt")).expect("written"),
            "hello\n"
        );

        // Guarded replace at the observed version succeeds.
        let intent = FsWriteIntent::ReplaceIfVersion {
            version: outcome.version,
        };
        let outcome = domain
            .write_text(&target, "hello\nworld\n", Some(&intent), None)
            .expect("guarded update succeeds");
        assert_eq!(outcome.operation, WriteOperation::Update);
        assert_eq!(outcome.before.as_deref(), Some("hello\n"));

        // A stale basis refuses with the two verbatim refusals.
        let stale = FsWriteIntent::ReplaceIfVersion {
            version: "bogus".into(),
        };
        let error = domain
            .write_text(&target, "x", Some(&stale), None)
            .expect_err("stale version refuses");
        assert_eq!(error.code, FsErrorCode::StaleVersion);
        assert_eq!(
            error.message,
            format!(
                "cannot write \"{}\": file changed since it was read",
                target.display_path
            )
        );

        // createIfAbsent over an existing file requires a read first.
        let error = domain
            .write_text(&target, "x", Some(&FsWriteIntent::CreateIfAbsent), None)
            .expect_err("unobserved overwrite refuses");
        assert_eq!(error.code, FsErrorCode::NotObserved);
        assert_eq!(
            error.message,
            format!(
                "cannot overwrite existing \"{}\" without reading it first",
                target.display_path
            )
        );
    }

    #[test]
    fn write_crlf_normalizes_the_diff_basis() {
        let scratch = Scratch::new("write-crlf");
        let domain = make(&scratch.path(""), "danger-full-access");
        let target = target_of(&domain, "a.txt");
        std::fs::write(scratch.path("a.txt"), "l1\r\nl2\r\n").expect("seed");
        let observed = domain.stat(&target).expect("stat").expect("present");
        let intent = FsWriteIntent::ReplaceIfVersion {
            version: observed.version,
        };
        let outcome = domain
            .write_text(&target, "l1\r\nl2\r\nl3\r\n", Some(&intent), None)
            .expect("update succeeds");
        assert_eq!(outcome.before.as_deref(), Some("l1\nl2\n"));
        assert_eq!(outcome.after, "l1\nl2\nl3\n");
        assert_eq!(
            std::fs::read_to_string(scratch.path("a.txt")).expect("raw"),
            "l1\r\nl2\r\nl3\r\n"
        );
    }

    #[test]
    fn edit_replaces_literals_and_reports_ambiguity() {
        let scratch = Scratch::new("edit-literals");
        let domain = make(&scratch.path(""), "danger-full-access");
        let target = target_of(&domain, "a.txt");
        std::fs::write(scratch.path("a.txt"), "aa bb aa\n").expect("seed");
        let observed = domain.stat(&target).expect("stat").expect("present");
        let version = observed.version;

        let edit = FsEditRequest {
            old_string: "bb".into(),
            new_string: "cc".into(),
            replace_all: false,
        };
        let outcome = domain
            .edit_text(&target, &edit, Some(&version), None)
            .expect("edit succeeds");
        assert_eq!(outcome.after, "aa cc aa\n");
        assert_eq!(outcome.before, "aa bb aa\n");

        let error = domain
            .edit_text(
                &target,
                &FsEditRequest {
                    old_string: "aa".into(),
                    new_string: "zz".into(),
                    replace_all: false,
                },
                None,
                None,
            )
            .expect_err("ambiguous match refuses");
        assert_eq!(error.code, FsErrorCode::AmbiguousEdit);
        assert_eq!(
            error.message,
            format!(
                "old_string matched 2 times in \"{}\"; provide a more specific old_string \
                 or set replace_all to true",
                target.display_path
            )
        );

        let error = domain
            .edit_text(
                &target,
                &FsEditRequest {
                    old_string: "nope".into(),
                    new_string: "zz".into(),
                    replace_all: false,
                },
                None,
                None,
            )
            .expect_err("missing match refuses");
        assert_eq!(error.code, FsErrorCode::EditNotFound);
        assert_eq!(
            error.message,
            format!("old_string was not found in \"{}\"", target.display_path)
        );
    }

    #[test]
    fn edit_missing_file_reports_stale() {
        let scratch = Scratch::new("edit-missing");
        let domain = make(&scratch.path(""), "danger-full-access");
        let target = target_of(&domain, "gone.txt");
        let error = domain
            .edit_text(
                &target,
                &FsEditRequest {
                    old_string: "a".into(),
                    new_string: "b".into(),
                    replace_all: false,
                },
                None,
                None,
            )
            .expect_err("missing file refuses");
        assert_eq!(error.code, FsErrorCode::StaleVersion);
        assert_eq!(
            error.message,
            format!(
                "cannot edit \"{}\": file changed since it was read",
                target.display_path
            )
        );
    }

    #[test]
    fn edit_restores_crlf_write_back() {
        let scratch = Scratch::new("edit-crlf");
        let domain = make(&scratch.path(""), "danger-full-access");
        let target = target_of(&domain, "a.txt");
        std::fs::write(scratch.path("a.txt"), "l1\r\nl2\r\n").expect("seed");
        let edit = FsEditRequest {
            old_string: "l2".into(),
            new_string: "L2".into(),
            replace_all: false,
        };
        domain
            .edit_text(&target, &edit, None, None)
            .expect("edit succeeds");
        assert_eq!(
            std::fs::read_to_string(scratch.path("a.txt")).expect("raw"),
            "l1\r\nL2\r\n"
        );
    }

    #[test]
    fn fence_denies_read_only_and_out_of_workspace_writes() {
        let scratch = Scratch::new("fence");
        let domain = make(&scratch.path(""), "read-only");
        let target = target_of(&domain, "a.txt");
        let error = domain
            .write_text(&target, "x", None, None)
            .expect_err("read-only refuses");
        assert_eq!(error.code, FsErrorCode::SandboxDenied);
        assert_eq!(
            error.message,
            format!(
                "cannot write \"{}\": file access denied under read-only mode",
                target.display_path
            )
        );

        // Outside every writable root under workspace-write (the
        // platform temp area IS a writable root, so the outside probe
        // targets a spelling beneath no root at all).
        let writer = make(&scratch.path(""), "workspace-write");
        let outside_target = writer
            .resolve("/outside-fr-fs-fence/b.txt", None)
            .expect("resolves");
        let error = writer
            .write_text(&outside_target, "x", None, None)
            .expect_err("outside root refuses");
        assert_eq!(error.code, FsErrorCode::SandboxDenied);
        assert!(
            error
                .message
                .ends_with("file access denied under workspace-write mode")
        );

        // danger-full-access passes the caller's target unfenced.
        let free = make(&scratch.path(""), "danger-full-access");
        let t = target_of(&free, "a.txt");
        free.write_text(&t, "x", None, None)
            .expect("unfenced write succeeds");
    }

    #[test]
    fn fence_allows_tmp_under_workspace_write() {
        let scratch = Scratch::new("fence-tmp");
        let domain = make(&scratch.path(""), "workspace-write");
        let temp_file =
            std::env::temp_dir().join(format!("fr-fs-fence-{}.txt", uuid::Uuid::new_v4().simple()));
        let temp_path = temp_file.to_string_lossy().into_owned();
        let target = domain.resolve(&temp_path, None).expect("resolves");
        domain
            .write_text(&target, "x", None, None)
            .expect("the platform temp area stays writable");
        let _ = std::fs::remove_file(&temp_path);
    }

    #[test]
    fn list_dir_sorts_names_and_reports_kinds() {
        let scratch = Scratch::new("list");
        std::fs::write(scratch.path("b.txt"), "x").expect("seed");
        std::fs::create_dir(scratch.path("a-dir")).expect("seed");
        let domain = make(&scratch.path(""), "danger-full-access");
        let entries = domain.list_dir(&target_of(&domain, ".")).expect("lists");
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["a-dir", "b.txt"]);
        assert_eq!(entries[0].kind, FsKind::Directory);
        assert_eq!(entries[1].kind, FsKind::File);
        assert_eq!(entries[1].size, Some(1));
    }

    #[test]
    fn serve_face_round_trips_the_wire_shapes() {
        use fr_wire::services::fs as face;
        let scratch = Scratch::new("serve");
        let domain = make(&scratch.path(""), "danger-full-access");

        let describe = domain
            .serve(face::METHOD_DESCRIBE, b"{}")
            .expect("describe");
        let describe: Value = serde_json::from_slice(&describe).expect("json");
        assert_eq!(describe["service"], "fs");

        let payload = serde_json::to_vec(&json!({ "path": "a.txt" })).expect("encode");
        let resolved = domain
            .serve(face::METHOD_RESOLVE, &payload)
            .expect("resolve");
        let resolved: Value = serde_json::from_slice(&resolved).expect("json");
        let target = resolved["target"].clone();

        let write_payload = serde_json::to_vec(&json!({
            "target": target,
            "content": "hello\n",
        }))
        .expect("encode");
        let written = domain
            .serve(face::METHOD_WRITE_TEXT, &write_payload)
            .expect("write");
        let written: Value = serde_json::from_slice(&written).expect("json");
        assert_eq!(written["outcome"]["operation"], "create");

        let read_payload = serde_json::to_vec(&json!({ "target": target })).expect("encode");
        let read = domain
            .serve(face::METHOD_READ_TEXT, &read_payload)
            .expect("read");
        let read: Value = serde_json::from_slice(&read).expect("json");
        assert_eq!(read["text"], "hello\n");

        // A domain refusal rides the body-error shape on a successful
        // wire reply.
        let stale = serde_json::to_vec(&json!({
            "target": target,
            "content": "next\n",
            "expected": { "kind": "replace-if-version", "version": "bogus" },
        }))
        .expect("encode");
        let refused = domain
            .serve(face::METHOD_WRITE_TEXT, &stale)
            .expect("the refusal still succeeds on the wire");
        let refused: Value = serde_json::from_slice(&refused).expect("json");
        assert_eq!(refused["error"]["code"], "FS_STALE_VERSION");
        assert_eq!(refused["error"]["name"], "FsError");

        // An unknown method fails the wire call itself.
        let _ = domain
            .serve(9_999, b"{}")
            .expect_err("an unknown method fails the wire call");
    }
}
