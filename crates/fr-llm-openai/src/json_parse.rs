//! Lenient JSON parsing for tool-call arguments (pi-ai
//! `utils/json-parse.ts` verbatim semantics, 2026-10-27 Q75-C).
//!
//! The dsh patch (`patches/@earendil-works__pi-ai@1.1.0.patch`) removes only
//! the six in-stream `parseStreamingJson` calls; the `toolcall_end`
//! finalization (openai-completions.ts:464) keeps the lenient parse, so fr
//! replicates it exactly. The pipeline is:
//!
//! 1. empty/whitespace input yields `{}`;
//! 2. strict parse (JS `JSON.parse` → `serde_json`);
//! 3. [`repair_json`] rewrite, retried strictly only when it changed the
//!    input;
//! 4. the npm `partial-json` 0.1.7 parser at `Allow.ALL` (vendored under
//!    `reference/vendor/partial-json-0.1.7`, tarball sha256 `30eafeed…`),
//!    ported line-for-line including the `lastIndexOf("e")` whole-string
//!    quirk;
//! 5. repaired-then-partial, then `{}`.
//!
//! The npm parser indexes by UTF-16 code units; every quirk path that leaks
//! an index into output cuts at an ASCII delimiter, so char indexing is
//! content-equivalent (see the differential tests, pinned against node runs
//! of the vendored source).

use serde_json::Value;

/// JS `String.prototype.substring` clamping and swap semantics, needed by the
/// quirky fallback paths (`substring(start, -1)` swaps to a prefix).
/// `end` is `None` when JS `lastIndexOf` yields -1; JS clamps negative ends
/// to zero and swaps reversed bounds.
fn js_substring(text: &[char], start: usize, end: Option<usize>) -> String {
    let to = end.unwrap_or(0).min(text.len());
    let from = start.min(text.len());
    let (from, to) = if from > to { (to, from) } else { (from, to) };
    text[from..to].iter().collect()
}

const VALID_JSON_ESCAPES: [char; 8] = ['"', '\\', '/', 'b', 'f', 'n', 'r', 't'];

const fn is_control_character(c: char) -> bool {
    (c as u32) <= 0x1f
}

fn escape_control_character(c: char) -> String {
    match c {
        '\u{8}' => "\\b".to_owned(),
        '\u{c}' => "\\f".to_owned(),
        '\n' => "\\n".to_owned(),
        '\r' => "\\r".to_owned(),
        '\t' => "\\t".to_owned(),
        other => format!("\\u{:04x}", other as u32),
    }
}

/// Repairs malformed JSON string literals by escaping raw control characters
/// inside strings and doubling backslashes before invalid escape characters
/// (pi-ai `repairJson`, json-parse.ts:32-83).
#[must_use]
pub fn repair_json(json: &str) -> String {
    let chars: Vec<char> = json.chars().collect();
    let mut repaired = String::new();
    let mut in_string = false;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if !in_string {
            repaired.push(c);
            if c == '"' {
                in_string = true;
            }
            index += 1;
            continue;
        }
        if c == '"' {
            repaired.push(c);
            in_string = false;
            index += 1;
            continue;
        }
        if c == '\\' {
            let next = chars.get(index + 1).copied();
            let Some(next) = next else {
                repaired.push_str("\\\\");
                index += 1;
                continue;
            };
            if next == 'u' {
                let digits: String = chars
                    .get(index + 2..index + 6)
                    .unwrap_or_default()
                    .iter()
                    .collect();
                if digits.len() == 4 && digits.chars().all(|d| d.is_ascii_hexdigit()) {
                    repaired.push_str("\\u");
                    repaired.push_str(&digits);
                    index += 6;
                    continue;
                }
            }
            if VALID_JSON_ESCAPES.contains(&next) {
                repaired.push('\\');
                repaired.push(next);
                index += 2;
                continue;
            }
            repaired.push_str("\\\\");
            index += 1;
            continue;
        }
        if is_control_character(c) {
            repaired.push_str(&escape_control_character(c));
        } else {
            repaired.push(c);
        }
        index += 1;
    }
    repaired
}

/// The npm `partial-json` 0.1.7 partial-parse failure; callers catch both
/// error flavors identically (npm `PartialJSON`/`MalformedJSON` share every
/// catch site), so one opaque type carries them. The JS error messages
/// ("Unexpected end of input", "Unterminated string literal", "Expected
/// '}' at end of object", "Expected ']' at end of array", "Unterminated
/// number literal", "Not sure what '-' is") never surface in durable
/// output and are dropped at every site.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LenientError;

type LenientResult = Result<Value, LenientError>;

fn strict_parse(text: &str) -> LenientResult {
    serde_json::from_str(text).map_err(dropped_error)
}

/// Every JS `SyntaxError` in the pipeline is discarded unread (the messages
/// never surface in durable output), so the conversion is a named function.
fn dropped_error<T>(_: T) -> LenientError {
    LenientError
}

/// Top-level parse result carrying the JS `??` distinction: only a `null`
/// literal is replaced by `{}` (`result ?? {}`), while `Infinity`/`NaN`
/// pass through and stringify as `null`.
#[derive(Debug, Clone, PartialEq)]
enum Raw {
    Null,
    NonFinite,
    Value(Value),
}

impl Raw {
    fn into_member(self) -> Value {
        match self {
            Self::Null | Self::NonFinite => Value::Null,
            Self::Value(value) => value,
        }
    }
}

/// `Allow` bit flags (npm partial-json 0.1.7 `options.js`); fr only ever
/// parses at `ALL` (pi-ai `parseStreamingJson` passes no allow argument).
mod allow {
    pub const STR: u16 = 0b0_0000_0001;
    pub const NUM: u16 = 0b0_0000_0010;
    pub const ARR: u16 = 0b0_0000_0100;
    pub const OBJ: u16 = 0b0_0000_1000;
    pub const NULL: u16 = 0b0_0001_0000;
    pub const BOOL: u16 = 0b0_0010_0000;
    pub const NAN: u16 = 0b0_0100_0000;
    pub const INFINITY_: u16 = 0b0_1000_0000;
    pub const NEG_INFINITY: u16 = 0b1_0000_0000;
    pub const ALL: u16 = STR | NUM | ARR | OBJ | NULL | BOOL | NAN | INFINITY_ | NEG_INFINITY;
}

/// JS `Infinity`/`-Infinity`/`NaN` parse results. `serde_json` has no
/// non-finite floats, and `JSON.stringify` renders them as `null`, so the
/// durable face maps them to [`Value::Null`] at parse time (parity note:
/// downstream stringification matches JS exactly).
const NON_FINITE_AS_NULL: Value = Value::Null;

/// The strict JSON number grammar (`-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?`).
/// Rust `FromStr` alone is too lenient (`"12."`, `".5"` pass; JS
/// `JSON.parse` rejects both).
fn is_json_number_grammar(text: &str) -> bool {
    let rest = text.strip_prefix('-').unwrap_or(text);
    let digits = |slice: &str| !slice.is_empty() && slice.bytes().all(|b| b.is_ascii_digit());
    let mantissa = match rest.find(['e', 'E']) {
        Some(split) => {
            let exponent = &rest[split + 1..];
            let exponent = exponent
                .strip_prefix('+')
                .or_else(|| exponent.strip_prefix('-'))
                .unwrap_or(exponent);
            if !digits(exponent) {
                return false;
            }
            &rest[..split]
        }
        None => rest,
    };
    let int_part = match mantissa.split_once('.') {
        Some((int_part, fraction)) => {
            if !digits(fraction) {
                return false;
            }
            int_part
        }
        None => mantissa,
    };
    if !digits(int_part) || (int_part.len() > 1 && int_part.starts_with('0')) {
        return false;
    }
    true
}

/// JS `JSON.parse` number semantics: overflow yields `Infinity` rather than
/// an error, and integral values stringify without a fractional part (dsh
/// Number parity; the safe-integer policy mirrors fr-llm-deepseek
/// translate.rs `update_usage`). Non-finite results map to
/// [`NON_FINITE_AS_NULL`].
fn parse_number_text(text: &str) -> LenientResult {
    // Rust `FromStr`, not serde's f64 visitor: JS `JSON.parse` maps
    // overflowing literals to `Infinity` rather than erroring, and `FromStr`
    // matches that (overflow yields `inf`) while serde rejects them.
    if !is_json_number_grammar(text) {
        return Err(LenientError);
    }
    let Ok(value) = text.parse::<f64>() else {
        return Err(LenientError);
    };
    if !value.is_finite() {
        return Ok(NON_FINITE_AS_NULL);
    }
    if value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_991.0 {
        // JS prints integral Numbers without ".0".
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the JS Number parity arm runs only when fract()==0 and |v| <= 2^53-1 are checked, so the cast is lossless"
        )]
        return Ok(Value::Number(serde_json::Number::from(value as i64)));
    }
    Ok(Value::Number(
        serde_json::Number::from_f64(value).ok_or(LenientError)?,
    ))
}

struct LenientParser {
    chars: Vec<char>,
    index: usize,
}

impl LenientParser {
    fn new(input: &str) -> Self {
        Self {
            chars: input.chars().collect(),
            index: 0,
        }
    }

    fn skip_blank(&mut self) {
        while self.index < self.chars.len()
            && matches!(self.chars[self.index], ' ' | '\n' | '\r' | '\t')
        {
            self.index += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }
}

impl LenientParser {
    /// npm `parseAny` (index.js:55-90): whitespace, EOF, then the literal
    /// prefix arms, then dispatch on the opening character.
    fn parse_any(&mut self, allow: u16) -> Result<Raw, LenientError> {
        self.skip_blank();
        if self.index >= self.chars.len() {
            return Err(LenientError);
        }
        match self.chars[self.index] {
            '"' => return self.parse_str(allow).map(Raw::Value),
            '{' => return self.parse_obj(allow).map(Raw::Value),
            '[' => return self.parse_arr(allow).map(Raw::Value),
            _ => {}
        }
        let remaining = self.chars.len() - self.index;
        let rest: String = self.chars[self.index..].iter().collect();
        if rest.starts_with("null")
            || (allow & allow::NULL != 0 && remaining < 4 && "null".starts_with(&rest))
        {
            self.index += 4;
            return Ok(Raw::Null);
        }
        if rest.starts_with("true")
            || (allow & allow::BOOL != 0 && remaining < 4 && "true".starts_with(&rest))
        {
            self.index += 4;
            return Ok(Raw::Value(Value::Bool(true)));
        }
        if rest.starts_with("false")
            || (allow & allow::BOOL != 0 && remaining < 5 && "false".starts_with(&rest))
        {
            self.index += 5;
            return Ok(Raw::Value(Value::Bool(false)));
        }
        if rest.starts_with("Infinity")
            || (allow & allow::INFINITY_ != 0 && remaining < 8 && "Infinity".starts_with(&rest))
        {
            self.index += 8;
            return Ok(Raw::NonFinite);
        }
        if rest.starts_with("-Infinity")
            || (allow & allow::NEG_INFINITY != 0
                && remaining > 1
                && remaining < 9
                && "-Infinity".starts_with(&rest))
        {
            self.index += 9;
            return Ok(Raw::NonFinite);
        }
        if rest.starts_with("NaN")
            || (allow & allow::NAN != 0 && remaining < 3 && "NaN".starts_with(&rest))
        {
            self.index += 3;
            return Ok(Raw::NonFinite);
        }
        self.parse_num(allow)
    }

    /// npm `parseStr` (index.js:91-117), including the closed-branch
    /// `Number(escape)` trim (dead for closing quotes, ported verbatim) and
    /// the whole-input `lastIndexOf("\\")` retry.
    fn parse_str(&mut self, allow: u16) -> LenientResult {
        let start = self.index;
        let mut escape = false;
        self.index += 1;
        while self.index < self.chars.len()
            && (self.chars[self.index] != '"' || (escape && self.chars[self.index - 1] == '\\'))
        {
            escape = if self.chars[self.index] == '\\' {
                !escape
            } else {
                false
            };
            self.index += 1;
        }
        if self.peek() == Some('"') {
            self.index += 1;
            let end = self.index - usize::from(escape);
            let text: String = self.chars[start..end].iter().collect();
            strict_parse(&text).map_err(dropped_error)
        } else if allow & allow::STR != 0 {
            let end = self.index - usize::from(escape);
            let mut candidate: String = self.chars[start..end].iter().collect();
            candidate.push('"');
            // SyntaxError: invalid escape sequence. npm retries from the
            // last backslash of the WHOLE input; with none found, JS
            // `substring(start, -1)` swaps to the `..start` prefix.
            strict_parse(&candidate).map_or_else(
                |_original| {
                    let cut = self.chars.iter().rposition(|&c| c == '\\');
                    let slice = js_substring(&self.chars, start, cut);
                    let retry = format!("{slice}\"");
                    strict_parse(&retry).map_err(dropped_error)
                },
                Ok,
            )
        } else {
            Err(LenientError)
        }
    }
}

/// How the object/array scan ended; mirrors the npm `return obj` inside
/// catch sites versus the normal close-through.
#[derive(Debug)]
enum ScanOutcome {
    /// The closing delimiter was found and consumed by the caller.
    Closed,
    /// A partial-return site fired with `Allow.OBJ`/`Allow.ARR`.
    PartialReturn,
}

impl LenientParser {
    /// npm `parseObj` (index.js:118-153). The outer catch converts every
    /// propagated error into a partial return under `Allow.OBJ`, otherwise
    /// into the fixed partial message; the inner value-parse catch returns
    /// early the same way, dropping the in-progress key.
    fn parse_obj(&mut self, allow: u16) -> LenientResult {
        self.index += 1;
        self.skip_blank();
        let mut map = serde_json::Map::new();
        let outcome = self.obj_loop(allow, &mut map);
        match outcome {
            Ok(ScanOutcome::Closed) => {
                self.index += 1;
                Ok(Value::Object(map))
            }
            Ok(ScanOutcome::PartialReturn) => Ok(Value::Object(map)),
            Err(LenientError) => {
                if allow & allow::OBJ != 0 {
                    Ok(Value::Object(map))
                } else {
                    Err(LenientError)
                }
            }
        }
    }

    fn obj_loop(
        &mut self,
        allow: u16,
        map: &mut serde_json::Map<String, Value>,
    ) -> Result<ScanOutcome, LenientError> {
        loop {
            if self.peek() == Some('}') {
                return Ok(ScanOutcome::Closed);
            }
            self.skip_blank();
            if self.index >= self.chars.len() && allow & allow::OBJ != 0 {
                return Ok(ScanOutcome::PartialReturn);
            }
            let key = self.parse_str(allow).map_err(dropped_error)?;
            let key_string = key.as_str().map_or_else(String::new, str::to_owned);
            self.skip_blank();
            self.index += 1; // skip colon
            match self.parse_any(allow) {
                Ok(raw) => {
                    map.insert(key_string, raw.into_member());
                }
                Err(_) if allow & allow::OBJ != 0 => return Ok(ScanOutcome::PartialReturn),
                Err(error) => return Err(error),
            }
            self.skip_blank();
            if self.peek() == Some(',') {
                self.index += 1;
            }
        }
    }

    /// npm `parseArr` (index.js:154-174).
    fn parse_arr(&mut self, allow: u16) -> LenientResult {
        self.index += 1;
        let mut members = Vec::new();
        let outcome = self.arr_loop(allow, &mut members);
        match outcome {
            Ok(ScanOutcome::Closed) => {
                self.index += 1;
                Ok(Value::Array(members))
            }
            Ok(ScanOutcome::PartialReturn) => Ok(Value::Array(members)),
            Err(LenientError) => {
                if allow & allow::ARR != 0 {
                    Ok(Value::Array(members))
                } else {
                    Err(LenientError)
                }
            }
        }
    }

    fn arr_loop(
        &mut self,
        allow: u16,
        members: &mut Vec<Value>,
    ) -> Result<ScanOutcome, LenientError> {
        loop {
            if self.peek() == Some(']') {
                return Ok(ScanOutcome::Closed);
            }
            let raw = self.parse_any(allow)?;
            members.push(raw.into_member());
            self.skip_blank();
            if self.peek() == Some(',') {
                self.index += 1;
            }
        }
    }
}

impl LenientParser {
    /// npm `parseNum` (index.js:175-211), including the whole-input
    /// `lastIndexOf("e")` fallback quirk (JS `substring` clamps a negative
    /// end to zero and swaps reversed bounds).
    fn parse_num(&mut self, allow: u16) -> Result<Raw, LenientError> {
        if self.index == 0 {
            let whole: String = self.chars.iter().collect();
            if whole == "-" {
                return Err(LenientError);
            }
            if let Ok(value) = parse_number_text(&whole) {
                return Ok(Raw::Value(value));
            }
            if allow & allow::NUM != 0 {
                let cut = self.chars.iter().rposition(|&c| c == 'e');
                let slice = js_substring(&self.chars, 0, cut);
                if let Ok(value) = parse_number_text(&slice) {
                    return Ok(Raw::Value(value));
                }
            }
            // JS throws a fresh MalformedJSON carrying String(e); the
            // message never surfaces, so the original is not carried.
            return Err(LenientError);
        }
        let start = self.index;
        if self.peek() == Some('-') {
            self.index += 1;
        }
        while self.peek().is_some_and(|c| !matches!(c, ',' | ']' | '}')) {
            self.index += 1;
        }
        if self.index == self.chars.len() && allow & allow::NUM == 0 {
            return Err(LenientError);
        }
        let slice: String = self.chars[start..self.index].iter().collect();
        if let Ok(value) = parse_number_text(&slice) {
            return Ok(Raw::Value(value));
        }
        if slice == "-" {
            return Err(LenientError);
        }
        let cut = self.chars.iter().rposition(|&c| c == 'e');
        let retry = js_substring(&self.chars, start, cut);
        parse_number_text(&retry)
            .map(Raw::Value)
            .map_err(dropped_error)
    }
}

/// JS `Number` output parity: every finite integral f64 stringifies without a
/// fractional part (`JSON.stringify(1e2)` is `"100"`), non-finite floats
/// stringify as `null`, and only integral values inside the safe-integer
/// window take the integer shape (the fr-llm-deepseek `update_usage`
/// policy). Exponent formatting beyond that window may drift from
/// `Number.prototype.toString` ("e21" vs "e+21"); the inputs are model
/// tool arguments, where such magnitudes are degenerate.
fn normalize_js_numbers(value: Value) -> Value {
    match value {
        Value::Number(number) => {
            let Some(as_f64) = number.as_f64() else {
                return Value::Number(number);
            };
            if !as_f64.is_finite() {
                return NON_FINITE_AS_NULL;
            }
            if as_f64.fract() == 0.0 && as_f64.abs() <= 9_007_199_254_740_991.0 {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "the JS Number parity arm runs only when fract()==0 and |v| <= 2^53-1 are checked, so the cast is lossless"
                )]
                return Value::Number(serde_json::Number::from(as_f64 as i64));
            }
            Value::Number(number)
        }
        Value::Array(members) => {
            Value::Array(members.into_iter().map(normalize_js_numbers).collect())
        }
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, member)| (key, normalize_js_numbers(member)))
                .collect(),
        ),
        other => other,
    }
}

/// npm `parseJSON` entry: trims, rejects blank input (the pi-ai wrapper
/// already guards), and parses at `Allow.ALL`.
fn parse_partial(input: &str) -> Result<Raw, LenientError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(LenientError);
    }
    LenientParser::new(trimmed).parse_any(allow::ALL)
}

/// The pi-ai `parseStreamingJson` pipeline: always yields a JSON value,
/// falling back to an empty object when every arm fails
/// (utils/json-parse.ts:104-124).
///
/// `undefined`/blank input yields `{}`; the strict path runs first, then
/// [`repair_json`] (retried only when it changed the text), then the partial
/// parser on the raw and repaired texts. A `null` produced by the partial
/// arms is replaced by `{}` (`result ?? {}`), matching npm.
#[must_use]
pub fn parse_streaming_json(partial_json: Option<&str>) -> Value {
    let Some(text) = partial_json else {
        return Value::Object(serde_json::Map::new());
    };
    if text.trim().is_empty() {
        return Value::Object(serde_json::Map::new());
    }
    // parseJsonWithRepair: strict, then repaired-strict only when the
    // rewrite changed the input. JS `JSON.parse` yields `Infinity` for
    // overflowing bare numbers where serde errors, so the strict arm falls
    // back to the JS number reader before declaring failure.
    let strict_outcome = strict_parse(text)
        .or_else(|_| parse_number_text(text))
        .or_else(|_original| {
            // The repair runs only after a strict failure; a no-op rewrite
            // rethrows the original error, sending the input to the partial
            // arms (JS parseJsonWithRepair).
            let repaired = repair_json(text);
            if repaired == text {
                Err(LenientError)
            } else {
                strict_parse(&repaired)
            }
        });
    if let Ok(value) = strict_outcome {
        return normalize_js_numbers(value);
    }
    let finish = |raw: Raw| match raw {
        Raw::Null => Value::Object(serde_json::Map::new()),
        Raw::NonFinite => Value::Null,
        Raw::Value(value) => value,
    };
    if let Ok(raw) = parse_partial(text) {
        return normalize_js_numbers(finish(raw));
    }
    let repaired = repair_json(text);
    parse_partial(&repaired).map_or_else(
        |_| Value::Object(serde_json::Map::new()),
        |raw| normalize_js_numbers(finish(raw)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Differential pin against node runs of the vendored npm source and a
    /// verbatim JS transcription of pi-ai `utils/json-parse.ts`
    /// (2026-10-27; corpus covers every pipeline arm).
    #[test]
    fn pipeline_matches_npm_reference_corpus() {
        let cases: &[(&str, Value)] = &[
            ("", json!({})),
            ("   ", json!({})),
            ("null", json!(null)),
            ("nul", json!({})),
            ("123", json!(123)),
            ("12.", json!({})),
            ("1e2", json!(100)),
            ("9e999", json!(null)),
            ("Infinity", json!(null)),
            ("NaN", json!(null)),
            ("-Infinity", json!(null)),
            (r#""abc"#, json!("abc")),
            ("tru", json!(true)),
            ("{", json!({})),
            ("[", json!([])),
        ];
        for (input, expected) in cases {
            let actual = parse_streaming_json(Some(input));
            assert_eq!(&actual, expected, "input {input:?}");
        }
    }

    /// The object-embedded arm corpus: truncations at every scan position,
    /// literal-prefix values, the repair paths (raw control character,
    /// invalid escape, incomplete unicode escape), and the
    /// `lastIndexOf("e")` quirk.
    #[test]
    fn pipeline_matches_npm_reference_objects() {
        let cases: &[(&str, Value)] = &[
            (r#"{"query": "wea"#, json!({"query": "wea"})),
            (r#"{"query":"#, json!({})),
            (r#"{"a": [1, 2"#, json!({"a": [1, 2]})),
            (r#"{"a": {"b": 2"#, json!({"a": {"b": 2}})),
            (r#"{"a": tru"#, json!({"a": true})),
            (r#"{"a": nul"#, json!({"a": null})),
            (r#"{"a": 12."#, json!({})),
            (r#"{"a": 12e"#, json!({"a": 12})),
            (r#"{"a": 1,"#, json!({"a": 1})),
            (r#"{"a": "x\"#, json!({"a": "x"})),
            (r#"{"a": "x\\"#, json!({"a": "x\\"})),
            (r#"{"a": "x\\", "b"#, json!({"a": "x\\"})),
            (r#"{"a" "b"}"#, json!({})),
            (r#"{"a": 1, "b": "tw"#, json!({"a": 1, "b": "tw"})),
            (r#"[{"a": 1}, {"a": 3,"#, json!([{"a": 1}, {"a": 3}])),
            (r#"{"a": -"#, json!({})),
            (r#"{"a": 1e"#, json!({"a": 1})),
            (r#"{"key": "value", invalid"#, json!({"key": "value"})),
            (r#"{"a": [{"b": "c"#, json!({"a": [{"b": "c"}]})),
            (
                r#"{"b": {"c": [1, {"d": "e"#,
                json!({"b": {"c": [1, {"d": "e"}]}}),
            ),
            (r#"{"a": 9e999"#, json!({"a": null})),
            (r#"{"a": "x", "b": "#, json!({"a": "x"})),
            (r#"{"a": Infinity}"#, json!({"a": null})),
            (r#"{"a": Inf"#, json!({"a": null})),
            (r#"{"a": NaN}"#, json!({"a": null})),
            (r#"{"a": -Inf"#, json!({"a": null})),
        ];
        for (input, expected) in cases {
            let actual = parse_streaming_json(Some(input));
            assert_eq!(&actual, expected, "input {input:?}");
        }
    }

    /// Repair-path corpus: raw control characters and invalid escapes are
    /// rewritten before the strict retry (pi-ai `repairJson`).
    #[test]
    fn repair_paths_match_reference() {
        let cases: &[(&str, Value)] = &[
            // A raw tab inside a string is escaped and then parses.
            ("{\"a\": \"b\tc\"}", json!({"a": "b\tc"})),
            // An invalid escape `\q` is doubled, surviving as a backslash.
            (r#"{"a": "x\q"}"#, json!({"a": "x\\q"})),
            // An incomplete unicode escape truncates the string at the last
            // backslash during the partial arm.
            (r#"{"a":"hello \u12"#, json!({"a": "hello "})),
        ];
        for (input, expected) in cases {
            let actual = parse_streaming_json(Some(input));
            assert_eq!(&actual, expected, "input {input:?}");
        }
    }

    /// `undefined` maps to the empty object without touching any parser.
    #[test]
    fn missing_input_yields_empty_object() {
        assert_eq!(
            parse_streaming_json(None),
            Value::Object(serde_json::Map::new())
        );
    }

    /// Known divergence (2026-10-27 Q75-C): lone surrogate escapes parse in
    /// JS but are unrepresentable in Rust `String` (same root fact as the
    /// `sanitizeSurrogates` identity ruling), so the value arm fails and the
    /// object arm drops the key.
    #[test]
    fn lone_surrogate_diverges_to_empty_object() {
        assert_eq!(parse_streaming_json(Some(r#"{"a": "\ud800"}"#)), json!({}));
    }
}
