//! Physical row framing: the exact envelope key set, ignorable semantics,
//! seq continuity, and the folded `sourceEventSeqs` range encoding
//!
//! (dsh session-format v1-to-v2 codec framing, the released row shape v4
//! retains).

use serde_json::Value;

use fr_wire::ids::{SAFE_INTEGER_MAX, SessionSeq};

/// The envelope's required keys.
pub const EVENT_REQUIRED_KEYS: [&str; 4] = ["type", "seq", "time", "data"];

/// The envelope's optional keys.
pub const EVENT_OPTIONAL_KEYS: [&str; 3] = ["ignorable", "sourceEventSeqs", "surfaceOp"];

/// Encodes source seqs with run-length folding: runs of three or more
/// consecutive seqs collapse to `[start, end]` pairs;
///
/// shorter runs stay individual numbers; a non-strictly-increasing input is
/// returned verbatim (dsh `encodeSeqRanges`).
#[must_use]
pub fn encode_seq_ranges(values: &[SessionSeq]) -> Vec<Value> {
    if values.windows(2).any(|pair| pair[1].get() <= pair[0].get()) {
        return values
            .iter()
            .map(|value| Value::from(value.get()))
            .collect();
    }
    let mut output = Vec::new();
    let mut index = 0;
    while index < values.len() {
        let start = values[index].get();
        let mut end = start;
        while index + 1 < values.len() && values[index + 1].get() == end + 1 {
            index += 1;
            end += 1;
        }
        if end - start >= 2 {
            output.push(Value::from(vec![Value::from(start), Value::from(end)]));
        } else {
            output.push(Value::from(start));
            if end - start == 1 {
                output.push(Value::from(end));
            }
        }
        index += 1;
    }
    output
}

fn safe_count(value: &Value, label: &str) -> Result<u64, String> {
    match value.as_u64() {
        Some(value) if value <= SAFE_INTEGER_MAX => Ok(value),
        _ => Err(format!("{label} must be a non-negative safe integer")),
    }
}

/// Decodes one folded `sourceEventSeqs` value into the expanded seq list
///
/// (dsh `decodeSeqRanges`): numbers or `[start, end]` pairs with
/// `start <= end`, all unique, all earlier than `max_entries`, and the
/// range-carrying form strictly increasing overall.
///
/// # Errors
///
/// Returns the dsh refusal message text.
///
/// # Panics
///
/// The `.expect("checked array")` on a range member cannot fire: every
/// member reaching it passed the `is_array` guard one line above.
pub fn decode_seq_ranges(value: &Value, max_entries: u64) -> Result<Vec<SessionSeq>, String> {
    let Value::Array(entries) = value else {
        return Err("sourceEventSeqs must be an array".to_owned());
    };
    let mut output: Vec<SessionSeq> = Vec::new();
    let mut has_range = false;
    for entry in entries {
        if !entry.is_array() {
            let count = safe_count(entry, "sourceEventSeqs member")?;
            output.push(SessionSeq::new(count).map_err(|_error| {
                "sourceEventSeqs member must be a non-negative safe integer".to_owned()
            })?);
            continue;
        }
        let pair = entry.as_array().expect("checked array");
        if pair.len() != 2 {
            return Err("sourceEventSeqs range must be a [start, end] pair".to_owned());
        }
        let start = safe_count(&pair[0], "sourceEventSeqs range start")?;
        let end = safe_count(&pair[1], "sourceEventSeqs range end")?;
        if start > end || end >= max_entries || end - start + 1 > max_entries - output.len() as u64
        {
            return Err("sourceEventSeqs range exceeds its event seq".to_owned());
        }
        for current in start..=end {
            output.push(
                SessionSeq::new(current)
                    .map_err(|_error| "sourceEventSeqs range exceeds its event seq".to_owned())?,
            );
        }
        has_range = true;
    }
    let mut seen = std::collections::HashSet::new();
    for source in &output {
        if source.get() >= max_entries || !seen.insert(source.get()) {
            return Err("sourceEventSeqs ranges must contain unique earlier seqs".to_owned());
        }
    }
    if has_range && output.windows(2).any(|pair| pair[1].get() <= pair[0].get()) {
        return Err("sourceEventSeqs ranges must be strictly increasing".to_owned());
    }
    Ok(output)
}

/// One envelope row admitted by the framing layer; the caller's `Value` now
/// carries the expanded `sourceEventSeqs` in place.
#[derive(Debug)]
pub struct FramedRow {
    /// The row's `seq`.
    pub seq: u64,
    /// The row's `type`.
    pub event_type: String,
}

/// Whether the value is a JSON number inside the dsh safe-integer domain
/// (dsh `Number.isSafeInteger`, negative values included; JSON `-0`
/// canonicalizes to `0` in the Rust parser).
#[expect(
    clippy::cast_precision_loss,
    reason = "SAFE_INTEGER_MAX is 2^53 - 1, exactly representable in f64; the \
              u64 -> f64 cast mirrors the dsh reference implementation's \
              double arithmetic"
)]
fn is_safe_integer(value: &Value) -> bool {
    value.as_i64().map_or_else(
        || {
            value.as_f64().is_some_and(|number| {
                number.fract() == 0.0 && number.abs() <= SAFE_INTEGER_MAX as f64
            })
        },
        |time| time.unsigned_abs() <= SAFE_INTEGER_MAX,
    )
}

/// Decodes one physical row envelope in place (dsh v1-to-v2 `decodeEvent`):
/// exact key set, string `type`, safe-integer `time`, `ignorable` only
/// `true`, and `sourceEventSeqs` range decoding against the row's seq.
///
/// # Errors
///
/// Returns the dsh refusal message (the `released v2 row` family the
/// scanner wraps).
///
/// # Panics
///
/// The `.expect("required checked")` on the `seq` lookup cannot fire:
/// `seq` is in `EVENT_REQUIRED_KEYS`, whose presence is verified above.
pub fn decode_event(value: &mut Value, row_index: usize) -> Result<FramedRow, String> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| format!("released v2 row {row_index} must be an object"))?;
    let missing = EVENT_REQUIRED_KEYS
        .iter()
        .find(|key| !object.contains_key(**key));
    if let Some(missing) = missing {
        return Err(format!(
            "released v2 row {row_index} lacks required field {missing}"
        ));
    }
    if let Some(unexpected) = object.keys().find(|key| {
        !EVENT_REQUIRED_KEYS.contains(&key.as_str()) && !EVENT_OPTIONAL_KEYS.contains(&key.as_str())
    }) {
        let unexpected = unexpected.clone();
        return Err(format!(
            "released v2 row {row_index} has unexpected field {unexpected}"
        ));
    }
    if object.get("type").and_then(Value::as_str).is_none() {
        return Err(format!("released v2 row {row_index} type must be a string"));
    }
    if object.get("time").is_none_or(|time| !is_safe_integer(time)) {
        return Err(format!(
            "released v2 row {row_index} time must be a safe integer"
        ));
    }
    if let Some(ignorable) = object.get("ignorable")
        && ignorable.as_bool() != Some(true)
    {
        return Err(format!(
            "released v2 row {row_index} ignorable must be true when present"
        ));
    }
    let seq = safe_count(
        object.get("seq").expect("required checked"),
        &format!("released v2 row {row_index} seq"),
    )?;
    if let Some(source) = object.get_mut("sourceEventSeqs") {
        let expanded = decode_seq_ranges(source, seq)?;
        let expanded: Vec<Value> = expanded.iter().map(|seq| Value::from(seq.get())).collect();
        *source = Value::Array(expanded);
    }
    let event_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Ok(FramedRow { seq, event_type })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{decode_event, decode_seq_ranges, encode_seq_ranges};
    use fr_wire::ids::SessionSeq;

    fn seqs(values: &[u64]) -> Vec<SessionSeq> {
        values
            .iter()
            .map(|value| SessionSeq::new(*value).expect("seq"))
            .collect()
    }

    #[test]
    fn encode_seq_ranges_folds_runs_of_three_and_keeps_pairs_single() {
        assert_eq!(encode_seq_ranges(&seqs(&[5])), vec![json!(5)]);
        // A run of two stays two individual numbers.
        assert_eq!(encode_seq_ranges(&seqs(&[5, 6])), vec![json!(5), json!(6)]);
        assert_eq!(encode_seq_ranges(&seqs(&[5, 6, 7])), vec![json!([5, 7])]);
        assert_eq!(
            encode_seq_ranges(&seqs(&[1, 2, 3, 9, 10])),
            vec![json!([1, 3]), json!(9), json!(10)]
        );
    }

    #[test]
    fn encode_seq_ranges_returns_non_increasing_input_verbatim() {
        assert_eq!(
            encode_seq_ranges(&seqs(&[7, 3, 4, 5])),
            vec![json!(7), json!(3), json!(4), json!(5)]
        );
    }

    #[test]
    fn decode_seq_ranges_expands_and_rejects() {
        let expanded = decode_seq_ranges(&json!([1, [3, 5], 8]), 10).expect("decode");
        assert_eq!(expanded, seqs(&[1, 3, 4, 5, 8]));
        // start > end refuses.
        let error = decode_seq_ranges(&json!([[5, 3]]), 10).expect_err("start above end refuses");
        assert!(error.contains("sourceEventSeqs range exceeds its event seq"));
        // end >= max_entries refuses.
        let error = decode_seq_ranges(&json!([[9, 10]]), 10).expect_err("end at bound refuses");
        assert!(error.contains("sourceEventSeqs range exceeds its event seq"));
        // duplicates refuse.
        let error = decode_seq_ranges(&json!([4, 4]), 10).expect_err("duplicates refuse");
        assert!(error.contains("must contain unique earlier seqs"));
        // out-of-order after a range refuses: the expanded [1, 2, 3, 2] hits
        // the uniqueness guard before the strictly-increasing guard.
        let error = decode_seq_ranges(&json!([[1, 3], 2]), 10)
            .expect_err("out-of-order after range refuses");
        assert!(error.contains("must contain unique earlier seqs"));
        let error = decode_seq_ranges(&json!("x"), 10).expect_err("non-array refuses");
        assert!(error.contains("sourceEventSeqs must be an array"));
        let error = decode_seq_ranges(&json!([[1, 2, 3]]), 10)
            .expect_err("range longer than a pair refuses");
        assert!(error.contains("range must be a [start, end] pair"));
    }

    #[test]
    fn decode_event_enforces_exact_envelope_and_expands_in_place() {
        let mut row = json!({
            "type": "user/message",
            "seq": 4,
            "time": 1000,
            "data": {},
            "sourceEventSeqs": [[1, 2]]
        });
        let framed = decode_event(&mut row, 4).expect("frame");
        assert_eq!(framed.seq, 4);
        assert_eq!(framed.event_type, "user/message");
        assert_eq!(row["sourceEventSeqs"], json!([1, 2]));

        let mut missing = json!({"type": "a", "seq": 0, "time": 0});
        let message = decode_event(&mut missing, 0).expect_err("data required");
        assert!(message.contains("lacks required field data"));

        let mut unexpected = json!({
            "type": "a", "seq": 0, "time": 0, "data": {}, "bogus": 1
        });
        let message = decode_event(&mut unexpected, 0).expect_err("unexpected key");
        assert_eq!(message, "released v2 row 0 has unexpected field bogus");

        let mut not_true =
            json!({"type": "a", "seq": 0, "time": 0, "data": {}, "ignorable": false});
        let message = decode_event(&mut not_true, 3).expect_err("ignorable false");
        assert_eq!(
            message,
            "released v2 row 3 ignorable must be true when present"
        );

        let mut fractional = json!({"type": "a", "seq": 0, "time": 1.5, "data": {}});
        let error = decode_event(&mut fractional, 0).expect_err("fractional time refuses");
        assert!(error.contains("time must be a safe integer"));
    }
}

// Rust guideline compliant 2026-10-23
