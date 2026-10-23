//! Stored-log decoding: the incremental JSONL scanner with its recoverable
//! failure policy, the zstd prefix reader with torn-frame recovery, stored
//! event validation, and stored-identity checks
//!
//! (dsh format.ts `SessionLogScanner` + index.ts
//! `readStoredLog`/`readZstdPrefix`).

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::zstd::{decompress_zstd_frame, decompress_zstd_prefix, scan_zstd_frames};
use super::{JsonlCompression, PersistError, SESSION_FORMAT_VERSION};
use crate::vocabulary::EventVocabulary;
use fr_wire::event::{SessionEvent, SessionHeader};
use fr_wire::ids::SessionId;

/// Serializes a current event batch as JSONL lines (no trailing newline).
///
/// Every event occupies one row; `sourceEventSeqs` fold to the range
/// encoding exactly at the physical boundary (dsh `eventLines` +
/// `encodeSourceEventRanges`).
///
/// # Errors
///
/// Returns `Io` when serialization fails (never for session-shaped values).
#[must_use]
pub fn event_lines(events: &[SessionEvent]) -> String {
    let mut lines = Vec::with_capacity(events.len());
    for event in events {
        let mut value = serde_json::to_value(event).unwrap_or_else(|_| Value::Null);
        if let (Some(object), Some(source)) =
            (value.as_object_mut(), event.source_event_seqs.as_ref())
        {
            object.insert(
                "sourceEventSeqs".to_owned(),
                Value::Array(super::framing::encode_seq_ranges(source)),
            );
        }
        lines.push(value.to_string());
    }
    lines.join("\n")
}

/// One scanned stored log: the preserved committed prefix.
#[derive(Debug, Clone)]
pub struct StoredScan {
    /// The logical session header.
    pub meta: SessionHeader,
    /// The exact inherited prefix length.
    pub inherited_event_count: u64,
    /// The preserved events, typed and validated.
    pub events: Vec<SessionEvent>,
    /// The byte offset safe to append at.
    pub committed_bytes: u64,
}

/// One decoded stored log with its torn-tail classification (dsh
/// `readStoredLog` outcome shape).
#[derive(Debug, Clone)]
pub struct ReadOutcome {
    /// The logical session header.
    pub meta: SessionHeader,
    /// The exact inherited prefix length.
    pub inherited_event_count: u64,
    /// The preserved events, typed and validated.
    pub events: Vec<SessionEvent>,
    /// The byte offset a write handle must truncate to before its first
    /// append (`None` on a clean tail).
    pub torn_truncate_to: Option<u64>,
    /// Complete records a torn final zstd frame had already flushed; the
    /// write path truncates the torn bytes and rewrites them durably.
    pub recovered_tail: Vec<SessionEvent>,
}

/// The recoverable failure policy of one scan (dsh
/// `SessionFormatRecovery`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Recovery {
    /// Stash the first row failure and suppress later rows until a
    /// `turn/end` escalates it.
    #[default]
    Recoverable,
    /// Throw on the first row failure.
    Strict,
}

/// Incrementally scans complete JSONL event records after an independently
/// supplied header record (dsh `SessionLogScanner`). Newline search and
/// byte offsets stay on raw bytes; only complete records decode.
#[derive(Debug)]
pub struct SessionLogScanner {
    meta: SessionHeader,
    recovery: Recovery,
    known: EventVocabulary,
    input_bytes: u64,
    committed_bytes: u64,
    pending_line: Vec<u8>,
    issue: Option<PersistError>,
    finished: bool,
    rows: Vec<Value>,
    decoded_line: u64,
}

impl SessionLogScanner {
    /// Creates an event scanner from exactly one newline-terminated header
    /// record.
    ///
    /// # Errors
    ///
    /// Returns the header-line refusals of [`super::header_line::parse_header_record`].
    pub fn new(
        header_record: &[u8],
        recovery: Recovery,
        known: EventVocabulary,
    ) -> Result<Self, PersistError> {
        let meta = super::header_line::parse_header_record(header_record)?;
        Ok(Self {
            meta,
            recovery,
            known,
            input_bytes: header_record.len() as u64,
            committed_bytes: header_record.len() as u64,
            pending_line: Vec::new(),
            issue: None,
            finished: false,
            rows: Vec::new(),
            decoded_line: 0,
        })
    }

    /// The logical header parsed from the first record.
    #[must_use]
    pub const fn meta(&self) -> &SessionHeader {
        &self.meta
    }

    /// Consumes the next raw plaintext chunk, retaining only an incomplete
    /// final record.
    ///
    /// # Errors
    ///
    /// Returns the escalated pending issue when a `turn/end` row follows a
    /// recoverable failure, the immediate structural refusals, and
    /// `Corruption` when the scanner is already finished.
    pub fn write(&mut self, chunk: &[u8]) -> Result<(), PersistError> {
        if self.finished {
            return Err(PersistError::Corruption(
                "cannot write to a finished session log scanner".to_owned(),
            ));
        }
        let chunk_start = self.input_bytes;
        self.input_bytes += chunk.len() as u64;
        let mut buffer = std::mem::take(&mut self.pending_line);
        let mut line_start = 0usize;
        for (index, byte) in chunk.iter().enumerate() {
            if *byte != 0x0A {
                continue;
            }
            buffer.extend_from_slice(&chunk[line_start..index]);
            line_start = index + 1;
            let end_byte = chunk_start + index as u64 + 1;
            self.consume_event_line(&buffer, end_byte)?;
            buffer.clear();
        }
        buffer.extend_from_slice(&chunk[line_start..]);
        self.pending_line = buffer;
        Ok(())
    }

    /// Snapshots progress before appending a recoverable torn-frame prefix:
    /// `(input bytes, committed bytes, emitted rows)`.
    #[must_use]
    pub const fn checkpoint(&self) -> (u64, u64, u64) {
        (
            self.input_bytes,
            self.committed_bytes,
            self.rows.len() as u64,
        )
    }

    /// Finishes scanning, ignoring a final record without a newline as a
    /// torn tail; validates the artifact and returns the preserved prefix.
    ///
    /// # Errors
    ///
    /// Returns the escalation of a pending issue, the marker-agreement and
    /// artifact refusals, and the session-level adoption failures.
    pub fn finish(mut self) -> Result<StoredScan, PersistError> {
        self.finished = true;
        let artifact_rows = std::mem::take(&mut self.rows);
        // The v2 framing finish: seeded logs need their marker, unseeded
        // logs refuse one.
        let marker = artifact_rows
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, row)| {
                let tagged = row.get("type").and_then(Value::as_str) == Some("session/end-seed")
                    && row
                        .get("data")
                        .and_then(|data| data.get("inherited"))
                        .and_then(Value::as_bool)
                        == Some(true);
                tagged.then_some(index as u64)
            });
        let is_seeded = self.meta.is_seeded;
        if is_seeded && marker.is_none() {
            return Err(PersistError::Corruption(
                "released v2 seeded Session lacks an inherited end-seed marker".to_owned(),
            ));
        }
        if !is_seeded && marker.is_some() {
            return Err(PersistError::Corruption(
                "released v2 unseeded Session contains an inherited end-seed marker".to_owned(),
            ));
        }
        let header_value = super::header_line::logical_header_value(&self.meta);
        super::v4_checks::restore_released_v4_artifact(
            &header_value,
            marker.unwrap_or(0),
            &artifact_rows,
            &self.known,
        )
        .map_err(|error| match error {
            crate::persist::v4_checks::V4CheckError::UnsupportedMigration(message) => {
                PersistError::Unsupported(message)
            }
            crate::persist::v4_checks::V4CheckError::Format(message) => {
                PersistError::Corruption(message)
            }
        })?;
        // The one retired shape hiding under a known type: the removed
        // delta codec's full-header fallback reason (dsh
        // `validateStoredEvents`); the raw-log suffix joins at
        // `read_stored_log`.
        for (index, row) in artifact_rows.iter().enumerate() {
            if row.get("type").and_then(Value::as_str) == Some("request/header")
                && row
                    .get("data")
                    .and_then(|data| data.get("reason"))
                    .and_then(Value::as_str)
                    == Some("fallback")
            {
                return Err(PersistError::Unsupported(format!(
                    "session \"{}\" contains a request/header event (seq {index}) with the \
unsupported legacy reason \"fallback\"; refusing to interpret the log — it was \
written by a retired pre-release harness",
                    self.meta.id.as_str()
                )));
            }
        }
        // Session-level adoption: the same validation a Session constructor
        // runs over the restored prefix (dsh `validateInstalledCurrentSessionArtifact`).
        let mut events = Vec::with_capacity(artifact_rows.len());
        for row in &artifact_rows {
            let event: SessionEvent = serde_json::from_value(row.clone()).map_err(|error| {
                PersistError::Corruption(format!("stored event failed validation: {error}"))
            })?;
            crate::admission::validate_event_data(&event)
                .map_err(|error| PersistError::Corruption(error.message))?;
            events.push(event);
        }
        crate::session::Session::from_seed(self.meta.clone(), events.clone())
            .map_err(|error| PersistError::Corruption(error.to_string()))?;
        Ok(StoredScan {
            meta: self.meta,
            inherited_event_count: marker.unwrap_or(0),
            events,
            committed_bytes: self.committed_bytes,
        })
    }

    /// Decodes one complete event row and updates the contiguous prefix
    /// (dsh `consumeEventLine`): JSON parse failures become recoverable
    /// issues; structural v4 admission refusals always throw; framing and
    /// seq-gap failures follow the recovery policy with the `turn/end`
    /// escalation.
    fn consume_event_line(&mut self, line: &[u8], end_byte: u64) -> Result<(), PersistError> {
        self.decoded_line += 1;
        let line_number = self.decoded_line;
        let mut row: Value = if let Ok(decoded) = serde_json::from_slice(line) {
            decoded
        } else {
            let issue = PersistError::Corruption(format!(
                "corrupt session log: unparsable committed event at line {line_number}"
            ));
            return self.record_issue(issue);
        };
        // Structural refusal must precede recoverable-tail suppression,
        // independently of the recovery mode (both classes throw).
        super::v4_checks::assert_v4_row_admission(&row, &self.known).map_err(
            |error| match error {
                crate::persist::v4_checks::V4CheckError::UnsupportedMigration(message) => {
                    PersistError::Unsupported(message)
                }
                crate::persist::v4_checks::V4CheckError::Format(message) => {
                    PersistError::Corruption(message)
                }
            },
        )?;
        if self.issue.is_some() {
            if row.get("type").and_then(Value::as_str) == Some("turn/end") {
                let issue = self.issue.take().expect("checked present");
                return Err(issue);
            }
            return Ok(());
        }
        let framing_result = super::framing::decode_event(&mut row, self.rows.len());
        let framed = match framing_result {
            Ok(framed) => framed,
            Err(message) => {
                let issue = PersistError::Corruption(format!(
                    "corrupt session log: invalid committed event at line {line_number}: {message}"
                ));
                return self.record_issue_escalating(issue, &row);
            }
        };
        if framed.seq != self.rows.len() as u64 {
            let issue = PersistError::Corruption(format!(
                "released v2 row {} has seq gap (expected {}, got {})",
                self.rows.len(),
                self.rows.len(),
                framed.seq
            ));
            return self.record_issue_escalating(issue, &row);
        }
        self.committed_bytes = end_byte;
        self.rows.push(row);
        Ok(())
    }

    /// Applies the recovery policy to a row failure: strict throws, the
    /// recoverable mode stashes the first issue and suppresses later rows.
    fn record_issue(&mut self, issue: PersistError) -> Result<(), PersistError> {
        if self.recovery == Recovery::Strict {
            return Err(issue);
        }
        self.set_issue(issue);
        Ok(())
    }

    /// Same as [`Self::record_issue`], with the dsh rule that a failing row
    /// that is itself a `turn/end` escalates the pending issue immediately.
    fn record_issue_escalating(
        &mut self,
        issue: PersistError,
        row: &Value,
    ) -> Result<(), PersistError> {
        if self.recovery == Recovery::Strict
            || row.get("type").and_then(Value::as_str) == Some("turn/end")
        {
            return Err(issue);
        }
        self.set_issue(issue);
        Ok(())
    }

    fn set_issue(&mut self, issue: PersistError) {
        if self.issue.is_none() {
            self.issue = Some(issue);
        }
    }
}

impl SessionLogScanner {
    /// The rows decoded so far (the preserved committed prefix, raw form).
    #[must_use]
    pub fn rows(&self) -> &[Value] {
        &self.rows
    }
}

/// Parses a complete or torn JSONL buffer into its preserved event prefix
/// (dsh `scanLog`): the first record supplies the header, the event rows
/// follow.
///
/// # Errors
///
/// Returns the header and row refusals of the scanner.
pub fn scan_log(
    buffer: &[u8],
    recovery: Recovery,
    known: EventVocabulary,
) -> Result<StoredScan, PersistError> {
    let Some(header_end) = buffer.iter().position(|byte| *byte == 0x0A) else {
        return Err(PersistError::Corruption(
            "empty or header-less session log".to_owned(),
        ));
    };
    let mut scanner = SessionLogScanner::new(&buffer[..=header_end], recovery, known)?;
    scanner.write(&buffer[header_end + 1..])?;
    scanner.finish()
}

/// Decodes complete frames and retains complete JSONL records from a torn
/// final frame (dsh `readZstdPrefix`).
///
/// # Errors
///
/// Returns `Corruption` for structural frame failures and the scanner
/// refusals; the artifact-level Unsupported class carries the raw-log path
/// at the [`read_stored_log`] layer.
///
/// # Panics
///
/// The `.expect("header frame created the scanner")` inside the frame loop,
/// the post-loop `.expect("frames non-empty")`, and the final
/// `.expect("header frame created the scanner")` cannot fire: the
/// empty-frames refusal above guarantees the first frame creates the
/// scanner.
pub fn read_zstd_prefix(
    buffer: &[u8],
    mut known: EventVocabulary,
) -> Result<ReadOutcome, PersistError> {
    let ZstdFrame { frames, torn_start } = ZstdFrame::from(scan_zstd_frames(buffer, None)?);
    if frames.is_empty() {
        return Err(PersistError::Corruption(
            "empty or header-less Zstandard session log".to_owned(),
        ));
    }
    let mut header_frame = None;
    let mut scanner: Option<SessionLogScanner> = None;
    for range in &frames {
        let plaintext = decompress_zstd_frame(&buffer[range.start..range.end])?;
        if header_frame.is_none() {
            if plaintext.is_empty()
                || plaintext.last() != Some(&0x0A)
                || plaintext[..plaintext.len() - 1].contains(&0x0A)
            {
                return Err(PersistError::Corruption(
                    "corrupt Zstandard session log: first frame is not exactly one header line"
                        .to_owned(),
                ));
            }
            scanner = Some(SessionLogScanner::new(
                &plaintext,
                Recovery::Recoverable,
                known,
            )?);
            known = EventVocabulary::default();
            header_frame = Some(plaintext);
        } else {
            scanner
                .as_mut()
                .expect("header frame created the scanner")
                .write(&plaintext)?;
        }
    }
    let header_frame = header_frame.expect("frames non-empty");
    let _ = header_frame;
    let mut scanner = scanner.expect("header frame created the scanner");
    let (input_bytes, committed_bytes, complete_rows) = scanner.checkpoint();
    if committed_bytes != input_bytes {
        return Err(PersistError::Corruption(
            "corrupt Zstandard session log: complete frame contains a torn JSONL record".to_owned(),
        ));
    }
    let Some(torn_start) = torn_start else {
        let prefix = scanner.finish()?;
        return Ok(ReadOutcome {
            meta: prefix.meta,
            inherited_event_count: prefix.inherited_event_count,
            events: prefix.events,
            torn_truncate_to: None,
            recovered_tail: Vec::new(),
        });
    };
    // A torn final frame's append never resolved, but complete JSONL
    // records already flushed into it are real emitted events: recover
    // them, and let the write path truncate the torn bytes and rewrite
    // them durably.
    let recovered_plaintext = decompress_zstd_prefix(&buffer[torn_start..]);
    scanner.write(&recovered_plaintext)?;
    let prefix = scanner.finish()?;
    let recovered_tail = usize::try_from(complete_rows)
        .ok()
        .and_then(|start| prefix.events.get(start..))
        .unwrap_or_default()
        .to_vec();
    Ok(ReadOutcome {
        meta: prefix.meta,
        inherited_event_count: prefix.inherited_event_count,
        events: prefix.events,
        torn_truncate_to: Some(torn_start as u64),
        recovered_tail,
    })
}

/// The dsh `ZstdFrameScan` shape renamed for the local import.
struct ZstdFrame {
    frames: Vec<super::zstd::ZstdFrameRange>,
    torn_start: Option<usize>,
}

impl From<super::zstd::ZstdFrameScan> for ZstdFrame {
    fn from(scan: super::zstd::ZstdFrameScan) -> Self {
        Self {
            frames: scan.frames,
            torn_start: scan.torn_start,
        }
    }
}

/// Decodes one plaintext stored log with its torn-tail classification: a
/// torn raw tail is one incomplete JSONL line; it holds no complete record
/// to recover (dsh `decodeStoredLog` plain branch).
///
/// # Errors
///
/// Returns the scanner refusals; Unsupported-class refusals carry no path
/// suffix here (the caller adds the raw-log path).
pub fn read_log_prefix(buffer: &[u8], known: EventVocabulary) -> Result<ReadOutcome, PersistError> {
    let scan = scan_log(buffer, Recovery::Recoverable, known)?;
    let torn_truncate_to =
        (scan.committed_bytes < buffer.len() as u64).then_some(scan.committed_bytes);
    Ok(ReadOutcome {
        meta: scan.meta,
        inherited_event_count: scan.inherited_event_count,
        events: scan.events,
        torn_truncate_to,
        recovered_tail: Vec::new(),
    })
}

/// Refuses stored metadata that is not bound to the requested session id
/// (dsh `assertStoredId`).
///
/// # Errors
///
/// Returns the dsh mismatch message.
pub fn assert_stored_id(id: &SessionId, meta: &SessionHeader) -> Result<(), PersistError> {
    if meta.id != *id {
        return Err(PersistError::Corruption(format!(
            "stored session identity mismatch: requested \"{}\", header contains \"{}\"",
            id.as_str(),
            meta.id.as_str()
        )));
    }
    Ok(())
}

/// Refuses metadata that does not identify the selected physical log (dsh
/// `assertStoredIdentity`): the header id and cwd must re-derive the exact path,
///
/// admitting case aliases on case-insensitive filesystems through realpath
/// equality.
///
/// # Errors
///
/// Returns the dsh mismatch messages.
pub fn assert_stored_identity(
    root: &Path,
    path: &Path,
    stored_version: u64,
    meta: &SessionHeader,
    expected_id: Option<&SessionId>,
    compression: JsonlCompression,
) -> Result<(), PersistError> {
    if let Some(expected_id) = expected_id
        && meta.id != *expected_id
    {
        return Err(PersistError::Corruption(format!(
            "corrupt session log \"{}\": requested id \"{}\" does not match header id \"{}\"",
            path.display(),
            expected_id.as_str(),
            meta.id.as_str()
        )));
    }
    let expected_path = super::filename::generation_log_path(
        root,
        meta.cwd.as_deref(),
        &meta.id,
        stored_version,
        compression,
    )
    .map_err(|error| {
        PersistError::Corruption(format!(
            "corrupt session log \"{}\": header id cannot name a storage path ({error})",
            path.display()
        ))
    })?;
    if path != expected_path && !same_file(path, &expected_path)? {
        return Err(PersistError::Corruption(format!(
            "corrupt session log \"{}\": header id \"{}\" and cwd identify \"{}\"",
            path.display(),
            meta.id.as_str(),
            expected_path.display()
        )));
    }
    Ok(())
}

/// Whether two path spellings resolve to the same physical file.
fn same_file(path: &Path, expected_path: &Path) -> Result<bool, PersistError> {
    match (
        std::fs::canonicalize(path),
        std::fs::canonicalize(expected_path),
    ) {
        (Ok(actual), Ok(expected)) => Ok(actual == expected),
        (Err(error), _) | (_, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(false)
        }
        (Err(error), _) | (_, Err(error)) => Err(PersistError::Io(error)),
    }
}

/// Selects the numerically highest canonical generation in one session
/// directory and rejects the opposite physical encoding (dsh
/// `resolveGenerationInDirectory`).
///
/// # Errors
///
/// Returns the dsh encoding-mismatch refusal; `Io` propagates.
pub fn resolve_generation_in_directory(
    dir: &Path,
    compression: JsonlCompression,
) -> Result<Option<ResolvedGeneration>, PersistError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(PersistError::Io(error)),
    };
    let mut generations: Vec<(PathBuf, u64)> = Vec::new();
    let mut opposite: Option<PathBuf> = None;
    let opposite_compression = match compression {
        JsonlCompression::Zstd => JsonlCompression::None,
        JsonlCompression::None => JsonlCompression::Zstd,
    };
    for entry in entries {
        let entry = entry.map_err(PersistError::Io)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(version) = super::filename::parse_generation_log_filename(&name, compression) {
            generations.push((dir.join(entry.file_name()), version));
            continue;
        }
        if opposite.is_none()
            && super::filename::parse_generation_log_filename(&name, opposite_compression).is_some()
        {
            opposite = Some(dir.join(entry.file_name()));
        }
    }
    if let Some(path) = opposite {
        return Err(super::encoding_mismatch(&path, compression));
    }
    generations.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    let latest = generations.first().cloned();
    Ok(
        latest.map(|(source_path, source_version)| ResolvedGeneration {
            source_path,
            source_version,
        }),
    )
}

/// One generation resolved in a session directory.
#[derive(Debug, Clone)]
pub struct ResolvedGeneration {
    /// The artifact path.
    pub source_path: PathBuf,
    /// The filename generation.
    pub source_version: u64,
}

/// Reads and decodes one stored log, classifying its torn tail, checking
/// its stored identity, and refusing retired payload shapes
///
/// (dsh `readStoredLog` + `decodeStoredLog` + `validateStoredEvents` on the
/// jsonl path).
///
/// # Errors
///
/// Returns `Unsupported` (with the raw-log suffix) for format refusals,
/// `Corruption` (with the raw-log suffix) for every decode failure, and
/// the identity refusals.
pub fn read_stored_log(
    root: &Path,
    path: &Path,
    expected_id: &SessionId,
    compression: JsonlCompression,
    known: EventVocabulary,
) -> Result<ReadOutcome, PersistError> {
    let raw = std::fs::read(path).map_err(PersistError::Io)?;
    let parsed = match compression {
        JsonlCompression::Zstd => read_zstd_prefix(&raw, known),
        JsonlCompression::None => read_log_prefix(&raw, known),
    }
    .map_err(|error| match error {
        PersistError::Unsupported(message) => {
            PersistError::Unsupported(format!("{message} (raw log: {})", path.display()))
        }
        other => PersistError::Corruption(format!(
            "session \"{}\": stored log is corrupt: {} (raw log: {})",
            expected_id.as_str(),
            other,
            path.display()
        )),
    })?;
    assert_stored_identity(
        root,
        path,
        SESSION_FORMAT_VERSION,
        &parsed.meta,
        Some(expected_id),
        compression,
    )?;
    assert_stored_id(expected_id, &parsed.meta)?;
    Ok(parsed)
}

// Rust guideline compliant 2026-10-23
