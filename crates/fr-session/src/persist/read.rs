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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use fr_wire::content::{ContentBlock, KnownContentBlock, RawObject, TrueOnly};
    use fr_wire::event::{
        AppendOp, AssistantMessageData, EndSeedData, SessionEventData, StepBoundsData, SurfaceOp,
        ToolCallData, ToolResultData, TurnEndData, TurnEndReason, TurnEndReasonKind, TurnStartData,
    };
    use fr_wire::ids::{MessageId, SessionId, SessionSeq, ToolCallId};
    use fr_wire::message::{KnownMessage, KnownMessageSource, Message, MessageSource};

    use super::{Recovery, StoredScan, read_stored_log, scan_log};
    use crate::persist::io::{append_lines, materialize};
    use crate::persist::{JsonlCompression, PersistError, SESSION_FORMAT_VERSION};
    use crate::vocabulary::EventVocabulary;
    use fr_wire::event::SessionEvent;

    struct TempRoot(PathBuf);
    impl TempRoot {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fr-session-persist-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_nanos())
            ));
            let _ = fs::remove_dir_all(&dir);
            Self(dir)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn time_of(seq: u64) -> i64 {
        1_000 + i64::try_from(seq).expect("seq fits i64")
    }

    fn event(seq: u64, data: SessionEventData) -> SessionEvent {
        SessionEvent {
            seq: SessionSeq::new(seq).expect("seq"),
            time: time_of(seq),
            data,
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    fn turn_start(seq: u64, turn: u64) -> SessionEvent {
        event(seq, SessionEventData::TurnStart(TurnStartData { turn }))
    }

    fn step_start(seq: u64, turn: u64, step: u64) -> SessionEvent {
        event(
            seq,
            SessionEventData::StepStart(StepBoundsData { turn, step }),
        )
    }

    fn step_end(seq: u64, turn: u64, step: u64) -> SessionEvent {
        event(
            seq,
            SessionEventData::StepEnd(StepBoundsData { turn, step }),
        )
    }

    fn turn_end(seq: u64, turn: u64) -> SessionEvent {
        event(
            seq,
            SessionEventData::TurnEnd(TurnEndData {
                turn,
                reason: TurnEndReason::Known(TurnEndReasonKind::Completed),
            }),
        )
    }

    fn tool_call(seq: u64, turn: u64, step: u64, call_id: &str) -> SessionEvent {
        event(
            seq,
            SessionEventData::ToolCall(ToolCallData {
                turn,
                step,
                call_id: ToolCallId::new(call_id),
                name: "bash".to_owned(),
                arguments: "{}".to_owned(),
            }),
        )
    }

    fn tool_result(seq: u64, turn: u64, step: u64, call_id: &str) -> SessionEvent {
        SessionEvent {
            seq: SessionSeq::new(seq).expect("seq"),
            time: time_of(seq),
            data: SessionEventData::ToolResult(ToolResultData {
                turn,
                step,
                message: Message::Known(KnownMessage::Tool {
                    id: MessageId::new(format!("r-{seq}")),
                    content: vec![ContentBlock::Known(KnownContentBlock::Text {
                        text: "done".to_owned(),
                    })],
                    source: MessageSource::Known(KnownMessageSource::Tool {
                        call_id: ToolCallId::new(call_id),
                        extra: RawObject::new(),
                    }),
                    tool_call_id: ToolCallId::new(call_id),
                    is_error: None,
                }),
                error: None,
                meta: None,
            }),
            ignorable: None,
            surface_op: Some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: None,
        }
    }

    fn assistant_advertise(seq: u64, turn: u64, step: u64, call_id: &str) -> SessionEvent {
        SessionEvent {
            seq: SessionSeq::new(seq).expect("seq"),
            time: time_of(seq),
            data: SessionEventData::AssistantMessage(AssistantMessageData {
                turn,
                step,
                message: Message::Known(KnownMessage::Assistant {
                    id: MessageId::new(format!("a-{seq}")),
                    content: vec![ContentBlock::Known(KnownContentBlock::ToolCall {
                        id: ToolCallId::new(call_id),
                        name: "bash".to_owned(),
                        arguments: "{}".to_owned(),
                    })],
                    source: MessageSource::Known(KnownMessageSource::Model {
                        provider: "deepseek-official".to_owned(),
                        model: "deepseek-chat".to_owned(),
                        replay_state: None,
                        extra: RawObject::new(),
                    }),
                }),
                stream: Vec::new(),
                usage: None,
                interrupted: None,
            }),
            ignorable: None,
            surface_op: Some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: None,
        }
    }

    fn header(id: &str, seeded: bool, cwd: Option<&str>) -> fr_wire::event::SessionHeader {
        fr_wire::event::SessionHeader {
            version: SESSION_FORMAT_VERSION,
            id: SessionId::new(id),
            created_at: 900,
            cwd: cwd.map(ToOwned::to_owned),
            parent_session: None,
            is_seeded: seeded,
            origin: None,
            delegation_depth: None,
            agent_preset: None,
        }
    }

    fn chain() -> Vec<SessionEvent> {
        vec![
            turn_start(0, 1),
            step_start(1, 1, 1),
            assistant_advertise(2, 1, 1, "call-a"),
            tool_call(3, 1, 1, "call-a"),
            tool_result(4, 1, 1, "call-a"),
            step_end(5, 1, 1),
            turn_end(6, 1),
        ]
    }

    #[test]
    fn materialize_append_read_round_trips_plain_and_zstd() {
        for compression in [JsonlCompression::None, JsonlCompression::Zstd] {
            let root = TempRoot::new("roundtrip");
            let id = SessionId::new("sess-1");
            let meta = header("sess-1", false, Some("/repo"));
            materialize(root.path(), &meta, 0, &chain()[..2], compression).expect("materialize");
            let path =
                crate::persist::filename::log_path(root.path(), Some("/repo"), &id, compression)
                    .expect("path");
            append_lines(&path, &chain()[2..], compression).expect("append");
            let outcome = read_stored_log(
                root.path(),
                &path,
                &id,
                compression,
                EventVocabulary::core(),
            )
            .expect("read");
            assert_eq!(outcome.events.len(), chain().len());
            assert_eq!(outcome.events, chain());
            assert_eq!(outcome.inherited_event_count, 0);
            assert_eq!(outcome.torn_truncate_to, None);
            assert_eq!(outcome.recovered_tail, [] as [SessionEvent; 0]);
        }
    }

    #[test]
    fn fork_seed_round_trips_with_the_inherited_cut() {
        let compression = JsonlCompression::Zstd;
        let root = TempRoot::new("fork");
        let id = SessionId::new("child");
        let meta = header("child", true, None);
        // The inherited prefix also carries the extension records the
        // scenario names: an approval event and a ptc-dispatch record ride
        // the file round-trip verbatim (both ignorable, plugin-owned).
        let extension = |seq_value: u64, type_name: &str| SessionEvent {
            seq: SessionSeq::new(seq_value).expect("seq"),
            time: 1_000 + i64::try_from(seq_value).expect("seq fits i64"),
            data: SessionEventData::Unknown(fr_wire::event::UnknownEventData {
                type_name: type_name.to_owned(),
                data: RawObject::new(),
            }),
            ignorable: Some(TrueOnly),
            surface_op: None,
            source_event_seqs: None,
        };
        let mut source = chain();
        source.insert(2, extension(2, "approval/asked"));
        source.insert(3, extension(3, "tool/ptc-dispatch"));
        for (index, event) in source.iter_mut().enumerate() {
            event.seq = SessionSeq::new(index as u64).expect("dense seq");
        }
        let seed = crate::fork::build_fork_seed(&source, 3).expect("seed");
        materialize(root.path(), &meta, 4, &seed, compression).expect("materialize");
        let path =
            crate::persist::filename::log_path(root.path(), None, &id, compression).expect("path");
        let outcome = read_stored_log(
            root.path(),
            &path,
            &id,
            compression,
            EventVocabulary::core(),
        )
        .expect("read");
        assert_eq!(outcome.events.len(), seed.len());
        assert_eq!(outcome.inherited_event_count, 4);
        for type_name in ["approval/asked", "tool/ptc-dispatch"] {
            let record = outcome
                .events
                .iter()
                .find(|event| event.event_type() == type_name)
                .unwrap_or_else(|| panic!("{type_name} round-tripped"));
            assert!(record.ignorable.is_some(), "{type_name} marker survives");
        }
    }

    #[test]
    fn write_path_validation_failure_publishes_nothing() {
        let compression = JsonlCompression::None;
        let root = TempRoot::new("write-guard");
        // An unencodable header cwd refuses at the path encoder — before any
        // directory entry or temp file exists.
        let mut meta = header("guarded", false, None);
        meta.cwd = Some(String::new());
        let error =
            materialize(root.path(), &meta, 0, &chain(), compression).expect_err("empty cwd");
        assert!(matches!(error, PersistError::Corruption(_)));
        assert!(
            error
                .message()
                .contains("cannot encode an empty project path")
        );
        assert!(
            !root.path().exists(),
            "no filesystem footprint: the root itself stays absent"
        );
        // The header-line encoder's own gate: a seeded header demands its
        // inherited cut before any bytes are shaped.
        let mut seeded = header("guarded", true, None);
        seeded.is_seeded = true;
        let error = crate::persist::header_line::to_header_line(&seeded, None)
            .expect_err("seeded without cut");
        assert!(
            error
                .message()
                .contains("seeded session header requires an inherited event count")
        );
    }

    #[test]
    fn republishing_an_existing_log_refuses_and_keeps_bytes_identical() {
        let compression = JsonlCompression::None;
        let root = TempRoot::new("eexist");
        let id = SessionId::new("dup");
        let meta = header("dup", false, None);
        materialize(root.path(), &meta, 0, &chain(), compression).expect("first materialize");
        let path =
            crate::persist::filename::log_path(root.path(), None, &id, compression).expect("path");
        let before = fs::read(&path).expect("read published bytes");
        // The pre-publish guard refuses with the dsh plain-Error text; the
        // link() EEXIST race behind it maps to AlreadyExists (TOCTOU
        // backstop, not reachable through this API surface).
        let error = materialize(root.path(), &meta, 0, &chain(), compression)
            .expect_err("second materialize");
        assert!(matches!(error, PersistError::Corruption(_)));
        assert!(
            error
                .message()
                .contains("refusing to materialize \"dup\": a log already exists on disk")
        );
        assert_eq!(fs::read(&path).expect("read again"), before);
        // No temp litter beside the artifact after the failed publish.
        let siblings: Vec<String> = fs::read_dir(path.parent().expect("dir"))
            .expect("list dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(
            siblings.iter().all(|name| {
                std::path::Path::new(name)
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
                    || name == "session.lock"
            }),
            "no temp files remain: {siblings:?}"
        );
    }

    #[test]
    fn torn_raw_tail_classifies_and_repair_clears_the_state() {
        let compression = JsonlCompression::None;
        let root = TempRoot::new("torn");
        let id = SessionId::new("torn-1");
        let meta = header("torn-1", false, None);
        materialize(root.path(), &meta, 0, &chain(), compression).expect("materialize");
        let path =
            crate::persist::filename::log_path(root.path(), None, &id, compression).expect("path");
        // Simulate a crash mid-append: half a JSONL record lands.
        let mut raw = fs::read(&path).expect("read raw");
        raw.extend_from_slice(b"{\"type\":\"turn/star");
        fs::write(&path, &raw).expect("write torn");
        let outcome = read_stored_log(
            root.path(),
            &path,
            &id,
            compression,
            EventVocabulary::core(),
        )
        .expect("read torn");
        assert_eq!(outcome.events.len(), chain().len());
        let truncate_to = outcome.torn_truncate_to.expect("torn tail classified");
        assert_eq!(
            usize::try_from(truncate_to).expect("offset fits usize"),
            raw.len() - b"{\"type\":\"turn/star".len()
        );
        crate::persist::io::repair(&path, truncate_to).expect("repair");
        let after = read_stored_log(
            root.path(),
            &path,
            &id,
            compression,
            EventVocabulary::core(),
        )
        .expect("read repaired");
        assert_eq!(after.torn_truncate_to, None);
    }

    #[test]
    fn zstd_torn_tail_keeps_the_committed_prefix_and_classifies_the_frame() {
        let compression = JsonlCompression::Zstd;
        let root = TempRoot::new("torn-zstd");
        let id = SessionId::new("torn-z");
        let meta = header("torn-z", false, None);
        materialize(root.path(), &meta, 0, &chain(), compression).expect("materialize");
        let path =
            crate::persist::filename::log_path(root.path(), None, &id, compression).expect("path");
        // Append one more event, then truncate mid-frame: the appended frame
        // starts at the committed byte offset and its last bytes are gone,
        // but the committed frames survive.
        let committed_len = fs::read(&path).expect("read committed").len();
        // The continuation opens its own turn: the committed prefix ends
        // with a closed turn.
        let extra = vec![
            turn_start(7, 2),
            step_start(8, 2, 1),
            assistant_advertise(9, 2, 1, "call-b"),
            tool_call(10, 2, 1, "call-b"),
            tool_result(11, 2, 1, "call-b"),
        ];
        append_lines(&path, &extra, compression).expect("append");
        let mut raw = fs::read(&path).expect("read");
        assert!(raw.len() > committed_len + 3);
        raw.truncate(raw.len() - 3);
        fs::write(&path, &raw).expect("write torn frame");
        let outcome = read_stored_log(
            root.path(),
            &path,
            &id,
            compression,
            EventVocabulary::core(),
        )
        .expect("torn frame recovers its flushed prefix");
        assert_eq!(outcome.events.len(), chain().len());
        assert_eq!(outcome.torn_truncate_to, Some(committed_len as u64));
        // The recovered session continues: truncate the torn bytes and
        // append again, and the log reads back complete with no torn tail.
        crate::persist::io::repair(&path, outcome.torn_truncate_to.expect("offset classified"))
            .expect("repair");
        append_lines(&path, &extra, compression).expect("append after recovery");
        let resumed = read_stored_log(
            root.path(),
            &path,
            &id,
            compression,
            EventVocabulary::core(),
        )
        .expect("read resumed");
        let mut full = chain();
        full.extend(extra);
        assert_eq!(resumed.events, full);
        assert_eq!(resumed.torn_truncate_to, None);
    }

    #[test]
    fn identity_and_requested_id_refuse() {
        let compression = JsonlCompression::None;
        let root = TempRoot::new("identity");
        let id = SessionId::new("real");
        let meta = header("real", false, Some("/repo"));
        materialize(root.path(), &meta, 0, &chain(), compression).expect("materialize");
        let path = crate::persist::filename::log_path(root.path(), Some("/repo"), &id, compression)
            .expect("path");
        let error = read_stored_log(
            root.path(),
            &path,
            &SessionId::new("other"),
            compression,
            EventVocabulary::core(),
        )
        .expect_err("requested id mismatch");
        assert!(
            error
                .message()
                .contains("requested id \"other\" does not match header id \"real\"")
        );

        // A log copied into a foreign project directory fails the identity
        // re-derivation.
        let foreign_dir = root.path().join("--elsewhere--").join("real");
        fs::create_dir_all(&foreign_dir).expect("foreign dir");
        let foreign = foreign_dir.join("session.v4.jsonl");
        fs::copy(&path, &foreign).expect("copy");
        let error = read_stored_log(
            root.path(),
            &foreign,
            &id,
            compression,
            EventVocabulary::core(),
        )
        .expect_err("identity mismatch");
        assert!(error.message().contains("and cwd identify"));
    }

    #[test]
    fn retired_fallback_reason_refuses_as_unsupported() {
        let compression = JsonlCompression::None;
        let root = TempRoot::new("fallback");
        let id = SessionId::new("fb");
        let meta = header("fb", false, None);
        let path =
            crate::persist::filename::log_path(root.path(), None, &id, compression).expect("path");
        // The chain prefix leaves the turn open, so the retired row passes
        // the lifecycle gate and reaches the storage-contract refusal.
        materialize(root.path(), &meta, 0, &chain()[..3], compression).expect("materialize");
        let retired = format!(
            "{}\n",
            serde_json::json!({
                "type": "request/header", "seq": 3, "time": 1_000,
                "data": {"header": {"config": {"provider": "p"}}, "reason": "fallback"}
            })
        );
        let retired_bytes = retired.into_bytes();
        {
            use std::io::Write as _;
            let mut handle = fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("append");
            handle.write_all(&retired_bytes).expect("write");
        }

        let error = read_stored_log(
            root.path(),
            &path,
            &id,
            compression,
            EventVocabulary::core(),
        );
        let Err(PersistError::Unsupported(message)) = error else {
            panic!("fallback must refuse as unsupported");
        };
        assert!(message.contains("unsupported legacy reason \"fallback\""));
    }

    #[test]
    fn scanner_strict_and_recoverable_modes_pin_the_gap_escalation() {
        let known = EventVocabulary::core();
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(b"{\"type\":\"session\",\"version\":4,\"id\":\"s\",\"createdAt\":1,\"isSeeded\":false,\"delegationDepth\":0}\n");
        bytes.extend_from_slice(
            b"{\"type\":\"turn/start\",\"seq\":0,\"time\":1,\"data\":{\"turn\":1}}\n",
        );
        bytes.extend_from_slice(
            b"{\"type\":\"step/start\",\"seq\":2,\"time\":1,\"data\":{\"turn\":1,\"step\":1}}\n",
        );
        bytes.extend_from_slice(
            b"{\"type\":\"step/end\",\"seq\":1,\"time\":1,\"data\":{\"turn\":1,\"step\":1}}\n",
        );
        let error = scan_log(&bytes, Recovery::Strict, known.clone()).expect_err("strict gap");
        assert!(
            error
                .message()
                .contains("released v2 row 1 has seq gap (expected 1, got 2)")
        );
        // Without a `turn/end` after the gap, the recoverable mode keeps the
        // committed prefix and reports the safe truncation offset; the write
        // path truncates the failing rows.
        let scan = scan_log(&bytes, Recovery::Recoverable, known.clone()).expect("prefix kept");
        assert_eq!(scan.events.len(), 1);
        assert!(scan.committed_bytes < bytes.len() as u64);

        // A torn final record is ignored in the recoverable mode.
        let mut torn = bytes.clone();
        torn.truncate(torn.len() - 10);
        let scan = scan_log(&torn, Recovery::Recoverable, known).expect("torn ok");
        // The committed prefix survives; the partial final record is ignored.
        assert_eq!(scan.events.len(), 1);
    }

    #[test]
    fn fork_seed_tagged_marker_carries_through_type_adoption() {
        let seed = crate::fork::build_fork_seed(&chain(), 3).expect("seed");
        let marker = seed
            .iter()
            .find(|event| event.event_type() == "session/end-seed")
            .expect("marker");
        let SessionEventData::SessionEndSeed(data) = &marker.data else {
            panic!("marker data");
        };
        let EndSeedData {
            inherited: Some(TrueOnly),
        } = *data
        else {
            panic!("inherited marker");
        };
        let _ = StoredScan {
            meta: header("", false, None),
            inherited_event_count: 0,
            events: Vec::new(),
            committed_bytes: 0,
        };
    }
}
