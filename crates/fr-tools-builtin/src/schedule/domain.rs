// Rust guideline compliant 2026-10-31
//! The schedule domain core (dsh `domain.ts` head): constants, the error
//! unions, the instant/time-zone/time/weekday/title grammar with verbatim
//! diagnostics, and the durable record vocabulary.
//!
//! The calendar layer is jiff (the Q16c-4A ruling); the instant grammar
//! stays the dsh regexes so the accepted spellings are identical. The
//! canonical stored instant is `YYYY-MM-DDTHH:mm:ss.SSSZ` (dsh
//! `toISOString` always carries three-digit milliseconds; jiff's
//! variable-length display does not pin the stored form).

use std::sync::OnceLock;

use jiff::Timestamp;
use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use regex::Regex;
use serde_json::{Value, json};

use super::occurrences::{
    next_cron_target, next_daily_target, next_weekly_target, parse_cron_expression,
};

/// Fixed v1 lower bound for a fixed-rate reminder.
pub const MIN_EVERY_INTERVAL_SECONDS: i64 = 60;

/// Fixed v1 upper bound for a stored task title.
pub const MAX_TITLE_LENGTH: i64 = 120;

/// Longest forward or backward cron date search, in years.
pub const CRON_SEARCH_HORIZON_YEARS: i32 = 400;

/// Epoch milliseconds of `0001-01-01T00:00:00.000Z` (dsh
/// `MIN_FOUR_DIGIT_YEAR_MS`).
pub const MIN_FOUR_DIGIT_YEAR_MS: i64 = -62_135_596_800_000;

/// Epoch milliseconds of `9999-12-31T23:59:59.999Z` (dsh
/// `MAX_FOUR_DIGIT_YEAR_MS`).
pub const MS_OF_MAX_FOUR_DIGIT_YEAR: i64 = 253_402_300_799_999;

/// Largest delay a timer represents without clamping (dsh
/// `MAX_TIMER_DELAY_MS`).
pub const MAX_TIMER_DELAY_MS: i64 = 2_147_483_647;

/// Stable diagnostic for a title that is missing or empty after trimming
/// (dsh `REQUIRED_TITLE_MESSAGE`).
pub const REQUIRED_TITLE_MESSAGE: &str = "title is required and must be non-empty after trimming.";

/// Error from a model-supplied rule that cannot become a record (dsh
/// `ScheduleInputError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleInputError {
    /// The stable public discriminator.
    pub code: &'static str,
    /// The stable public diagnostic.
    pub message: String,
}

impl ScheduleInputError {
    /// One coded input failure.
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Error from malformed durable data (dsh `ScheduleLogError`; consumers
/// map it to `internal_error` at the tool surface).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleLogError(pub String);

impl std::fmt::Display for ScheduleLogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The two-error union the service arms raise (dsh
/// `operationError`: input codes ride through, log errors map to
/// `internal_error`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleError {
    /// A model-supplied input failure.
    Input(ScheduleInputError),
    /// Malformed durable data or an internal invariant failure.
    Log(ScheduleLogError),
    /// The non-mutating compare-and-update miss (dsh
    /// `ScheduleUpdateMiss`: unknown, inactive, or changed since the
    /// read).
    UpdateMiss {
        /// The task identity.
        id: String,
        /// The stable miss code.
        code: &'static str,
    },
}

impl From<ScheduleInputError> for ScheduleError {
    fn from(error: ScheduleInputError) -> Self {
        Self::Input(error)
    }
}

impl From<ScheduleLogError> for ScheduleError {
    fn from(error: ScheduleLogError) -> Self {
        Self::Log(error)
    }
}

impl std::fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(error) => f.write_str(&error.message),
            Self::Log(error) => f.write_str(&error.0),
            Self::UpdateMiss { code, .. } => f.write_str(code),
        }
    }
}

impl ScheduleError {
    /// The tool-surface error object.
    #[must_use]
    pub fn to_error_value(&self) -> Value {
        match self {
            Self::Input(error) => json!({ "code": error.code, "message": error.message }),
            Self::Log(_) => json!({
                "code": "internal_error",
                "message": "The schedule operation failed.",
            }),
            Self::UpdateMiss { id, code } => {
                json!({ "id": id, "updated": false, "code": code })
            }
        }
    }
}

/// The canonical stored UTC instant grammar (dsh `UTC_INSTANT`; the
/// dsh `(?!0000)` lookahead is the [`is_four_digit_instant`] year gate,
/// which the regex crate cannot compile).
pub fn utc_instant_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(
            r"^\d{4}-(?:0[1-9]|1[0-2])-(?:0[1-9]|[12]\d|3[01])T(?:[01]\d|2[0-3]):[0-5]\d:[0-5]\d\.\d{3}Z$",
        )
        .expect("the UTC instant grammar compiles")
    })
}

/// The strict offset-instant grammar (dsh `OFFSET_INSTANT`).
fn offset_instant_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(concat!(
            r"^(?P<year>\d{4})-(?P<month>\d{2})-(?P<day>\d{2})",
            r"T(?P<hour>\d{2}):(?P<minute>\d{2}):(?P<second>\d{2})",
            r"(?:\.(?P<fraction>\d{1,3}))?",
            r"(?P<zone>Z|(?P<sign>[+-])(?P<offsetHour>\d{2}):(?P<offsetMinute>\d{2}))$",
        ))
        .expect("the offset instant grammar compiles")
    })
}

/// The local date grammar (dsh `LOCAL_DATE`).
fn local_date_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(r"^(?P<year>\d{4})-(?P<month>\d{2})-(?P<day>\d{2})$")
            .expect("the local date grammar compiles")
    })
}

/// The local time grammar (dsh `LOCAL_TIME`).
fn local_time_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(
            r"^(?P<hour>\d{2}):(?P<minute>\d{2}):(?P<second>\d{2})(?:\.(?P<fraction>\d{1,3}))?$",
        )
        .expect("the local time grammar compiles")
    })
}

/// The wall-clock selector grammar (dsh `LOCAL_CLOCK_TIME`).
fn local_clock_time_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(r"^(?:[01]\d|2[0-3]):[0-5]\d:[0-5]\d(?:\.\d{1,3})?$")
            .expect("the local clock time grammar compiles")
    })
}

/// The IANA Area/Location name grammar (dsh `IANA_ZONE`).
fn iana_zone_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(r"^[A-Za-z][A-Za-z0-9_+.-]*(?:\/[A-Za-z0-9_+.-]+)+$")
            .expect("the IANA zone grammar compiles")
    })
}

/// Formats one epoch-millisecond reading as the canonical stored instant
/// (`toISOString` shape: three-digit milliseconds, `Z` suffix).
///
/// The date conversion is the Hinnant `civil_from_days` walk: jiff's
/// `Timestamp` stops short of the four-digit-year ceiling (its range
/// pads for offset conversions), while the stored window reaches
/// `9999-12-31T23:59:59.999Z`. The jiff round-trip pin keeps the two
/// agreeable inside jiff's own range.
#[must_use]
pub fn canonical_instant(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let millis_of_day = ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let hour = millis_of_day / 3_600_000;
    let minute = (millis_of_day % 3_600_000) / 60_000;
    let second = (millis_of_day % 60_000) / 1_000;
    let millis = millis_of_day % 1_000;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Converts epoch days to a proleptic-Gregorian calendar date (the
/// Hinnant `civil_from_days` algorithm; exact over the whole
/// four-digit-year window).
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the algorithm yields months 1 through 12 and days 1 through 31"
)]
pub const fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    (
        if month <= 2 { year + 1 } else { year },
        month as u32,
        day as u32,
    )
}

/// Parses one canonical stored instant back to epoch milliseconds,
/// refusing anything the grammar admits but the calendar does not.
///
/// # Errors
///
/// Malformed stored data (dsh `decodeInstant`).
/// Maps any cause onto one fixed diagnostic (the dsh texts carry no
/// cause; the TS arms ride `ErrorOptions`, never the observable
/// message).
pub fn fixed<T: Clone, E>(value: T) -> impl Fn(E) -> T + Clone {
    move |_| value.clone()
}

/// Whether one canonical instant carries a non-zero four-digit year
/// (the dsh lookahead's refusal).
pub fn is_four_digit_instant(value: &str) -> bool {
    !value.starts_with("0000-") && utc_instant_regex().is_match(value)
}

pub fn decode_instant(value: &str) -> Result<i64, ScheduleLogError> {
    if !is_four_digit_instant(value) {
        return Err(ScheduleLogError(
            "scheduledAt must be a canonical four-digit-year RFC 3339 UTC instant".to_owned(),
        ));
    }
    let epoch = parse_offset_instant(value).map_err(|error| ScheduleLogError(error.message))?;
    if canonical_instant(epoch) != value {
        return Err(ScheduleLogError(
            "scheduledAt is not a real UTC calendar instant".to_owned(),
        ));
    }
    Ok(epoch)
}

/// Requires a safe, representable, strictly future UTC target (dsh
/// `futureInstant`).
///
/// # Errors
///
/// Out of the four-digit-year range or not strictly future.
pub fn future_instant(epoch: i64, now: i64) -> Result<String, ScheduleInputError> {
    if !(MIN_FOUR_DIGIT_YEAR_MS..=MS_OF_MAX_FOUR_DIGIT_YEAR).contains(&epoch)
        || !(MIN_FOUR_DIGIT_YEAR_MS..=MS_OF_MAX_FOUR_DIGIT_YEAR).contains(&now)
    {
        return Err(ScheduleInputError::new(
            "time_out_of_range",
            "The scheduled time must be representable as a four-digit-year RFC 3339 UTC instant.",
        ));
    }
    if epoch <= now {
        return Err(ScheduleInputError::new(
            "not_future",
            "The scheduled time must be strictly in the future.",
        ));
    }
    let instant = canonical_instant(epoch);
    if !is_four_digit_instant(&instant) {
        return Err(ScheduleInputError::new(
            "time_out_of_range",
            "The scheduled time must be representable as a four-digit-year RFC 3339 UTC instant.",
        ));
    }
    Ok(instant)
}

/// Exact calendar fields of one instant parse (dsh `CalendarParts`).
#[derive(Debug, Clone, Copy)]
struct CalendarParts {
    year: i16,
    month: i8,
    day: i8,
    hour: i8,
    minute: i8,
    second: i8,
    millisecond: i16,
}

/// Converts exact calendar fields to epoch milliseconds while refusing
/// any spelling the calendar normalizes (dsh `calendarEpoch`: jiff
/// rejects invalid dates at construction, which is the same refusal).
///
/// # Errors
///
/// A non-real ISO calendar date or time.
fn calendar_epoch(parts: CalendarParts) -> Result<i64, ScheduleInputError> {
    let refusal = || {
        ScheduleInputError::new(
            "invalid_rule",
            "The at value must be a real ISO calendar date and time.",
        )
    };
    let date = Date::new(parts.year, parts.month, parts.day).map_err(fixed(refusal()))?;
    let time = jiff::civil::Time::new(
        parts.hour,
        parts.minute,
        parts.second,
        i32::from(parts.millisecond) * 1_000_000,
    )
    .map_err(fixed(refusal()))?;
    let civil = DateTime::from_parts(date, time);
    let epoch = civil
        .to_zoned(TimeZone::UTC)
        .ok()
        .map(|zoned| zoned.timestamp().as_millisecond())
        .ok_or_else(refusal)?;
    Ok(epoch)
}

/// Normalizes an optional one-to-three digit fractional second to
/// milliseconds (dsh `milliseconds`).
fn fractional_millis(fraction: Option<&str>) -> i16 {
    fraction.map_or(0, |text| {
        let padded = format!("{text:0<3}");
        text_parse_or_zero(&padded)
    })
}

/// Reads the padded fraction digits, tolerating only the fixed grammar
/// (the regex admits one to three digits, so the parse cannot fail).
fn text_parse_or_zero(text: &str) -> i16 {
    text.parse().unwrap_or(0)
}

/// Parses one strict RFC 3339 instant whose numeric offset is part of
/// the input (dsh `parseOffsetInstant`).
///
/// # Errors
///
/// Shape, calendar, or numeric-offset refusals.
pub fn parse_offset_instant(value: &str) -> Result<i64, ScheduleInputError> {
    let groups = offset_instant_regex()
        .captures(value)
        .ok_or_else(|| {
            ScheduleInputError::new(
                "invalid_rule",
                "at must use YYYY-MM-DDTHH:mm:ss with optional 1-3 digit fractional seconds and an explicit Z or numeric offset.",
            )
        })?;
    let group = |name: &str| -> Result<i16, ScheduleInputError> {
        groups
            .name(name)
            .and_then(|part| part.as_str().parse().ok())
            .ok_or_else(|| {
                ScheduleInputError::new("invalid_rule", "The at value has an invalid shape.")
            })
    };
    let year: i16 = group("year")?;
    let month: i16 = group("month")?;
    let day: i16 = group("day")?;
    let hour: i16 = group("hour")?;
    let minute: i16 = group("minute")?;
    let second: i16 = group("second")?;
    let millisecond = fractional_millis(groups.name("fraction").map(|part| part.as_str()));
    if year == 0 || hour > 23 || minute > 59 || second > 59 {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            "The at value must be a real ISO calendar date and time.",
        ));
    }
    let parts = CalendarParts {
        year,
        month: i8::try_from(month).map_err(fixed(refusal_text()))?,
        day: i8::try_from(day).map_err(fixed(refusal_text()))?,
        hour: i8::try_from(hour).map_err(fixed(refusal_text()))?,
        minute: i8::try_from(minute).map_err(fixed(refusal_text()))?,
        second: i8::try_from(second).map_err(fixed(refusal_text()))?,
        millisecond,
    };
    let local_epoch = calendar_epoch(parts)?;
    if groups.name("zone").is_some_and(|zone| zone.as_str() == "Z") {
        return Ok(local_epoch);
    }
    let offset_hour: i16 = group("offsetHour")?;
    let offset_minute: i16 = group("offsetMinute")?;
    let sign = groups.name("sign").map(|part| part.as_str());
    if offset_hour > 23
        || offset_minute > 59
        || (sign == Some("-") && offset_hour == 0 && offset_minute == 0)
    {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            "The at numeric offset is invalid.",
        ));
    }
    let direction = if sign == Some("+") { 1 } else { -1 };
    Ok(local_epoch - direction * (i64::from(offset_hour) * 60 + i64::from(offset_minute)) * 60_000)
}

/// The calendar refusal shared by the numeric narrowings.
fn refusal_text() -> ScheduleInputError {
    ScheduleInputError::new(
        "invalid_rule",
        "The at value must be a real ISO calendar date and time.",
    )
}

/// Validates and canonicalizes one raw IANA time-zone selector (dsh
/// `canonicalizeTimeZone`).
///
/// # Errors
///
/// A blank, untrimmed, non-IANA-shaped, or unknown zone.
pub fn canonicalize_time_zone(value: &str) -> Result<TimeZone, ScheduleInputError> {
    if value.is_empty()
        || value.trim() != value
        || (value != "UTC" && !iana_zone_regex().is_match(value))
    {
        return Err(ScheduleInputError::new(
            "invalid_time_zone",
            "time_zone must be UTC or a valid IANA Area/Location name.",
        ));
    }
    let zone = jiff::tz::db()
        .get(value)
        .map_err(fixed(ScheduleInputError::new(
            "invalid_time_zone",
            "time_zone must be UTC or a valid IANA Area/Location name.",
        )))?;
    if zone.iana_name().is_none() && value != "UTC" {
        return Err(ScheduleInputError::new(
            "invalid_time_zone",
            "time_zone must resolve to UTC or an IANA Area/Location name.",
        ));
    }
    Ok(zone)
}

/// The canonical zone name a stored spelling resolves to (the update
/// path's equivalence comparison; the dsh `Intl` canonical form).
///
/// # Errors
///
/// The [`canonicalize_time_zone`] refusals.
pub fn canonicalize_time_zone_name(value: &str) -> Result<String, ScheduleInputError> {
    Ok(canonicalize_time_zone(value)?
        .iana_name()
        .unwrap_or("UTC")
        .to_owned())
}

/// Parses one strict local clock time shared by the wall-clock selectors
/// (dsh `localClockTime`), returning the normalized `HH:mm:ss.SSS` form.
///
/// # Errors
///
/// A leap second, 24:00, or out-of-range field.
pub fn local_clock_time(value: &str, selector: &str) -> Result<String, ScheduleInputError> {
    if !local_clock_time_regex().is_match(value) {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            format!(
                "{selector}.time must use HH:mm:ss with optional 1-3 fractional digits, without leap seconds or 24:00."
            ),
        ));
    }
    let groups = local_time_regex()
        .captures(value)
        .ok_or_else(|| {
            ScheduleInputError::new(
                "invalid_rule",
                format!(
                    "{selector}.time must use HH:mm:ss with optional 1-3 fractional digits, without leap seconds or 24:00."
                ),
            )
        })?;
    let hour: u8 = groups["hour"].parse().unwrap_or(0);
    let minute: u8 = groups["minute"].parse().unwrap_or(0);
    let second: u8 = groups["second"].parse().unwrap_or(0);
    let millis = fractional_millis(groups.name("fraction").map(|part| part.as_str()));
    Ok(format!(
        "{:02}:{:02}:{:02}.{:03}",
        hour,
        minute,
        second,
        millis.max(0)
    ))
}

/// Validates the title supplied at creation (dsh `scheduleTitle`).
///
/// # Errors
///
/// Missing, blank-after-trim, or over-long.
pub fn schedule_title(value: &Value) -> Result<String, ScheduleInputError> {
    let text = value.as_str().unwrap_or("");
    if text.trim().is_empty() {
        return Err(ScheduleInputError::new(
            "invalid_prompt",
            REQUIRED_TITLE_MESSAGE,
        ));
    }
    let normalized = text.trim();
    #[expect(
        clippy::cast_possible_wrap,
        reason = "the UTF-16 length is bounded by the string's own length, far below i64"
    )]
    let length = normalized.encode_utf16().count() as i64;
    if length > MAX_TITLE_LENGTH {
        return Err(ScheduleInputError::new(
            "invalid_prompt",
            format!("title must be at most {MAX_TITLE_LENGTH} characters."),
        ));
    }
    Ok(normalized.to_owned())
}

/// Validates one required stored title at the durable boundary (dsh
/// `decodeStoredTitle`).
///
/// # Errors
///
/// Missing, blank, untrimmed, or over-long.
pub fn decode_stored_title(value: &Value) -> Result<String, ScheduleLogError> {
    let text = value.as_str().unwrap_or("");
    if text.trim().is_empty() {
        return Err(ScheduleLogError(REQUIRED_TITLE_MESSAGE.to_owned()));
    }
    #[expect(
        clippy::cast_possible_wrap,
        reason = "the UTF-16 length is bounded by the string's own length, far below i64"
    )]
    let length = text.encode_utf16().count() as i64;
    if length > MAX_TITLE_LENGTH {
        return Err(ScheduleLogError(format!(
            "title must be at most {MAX_TITLE_LENGTH} characters"
        )));
    }
    if text.trim() != text {
        return Err(ScheduleLogError(
            "title must be a trimmed string".to_owned(),
        ));
    }
    Ok(text.to_owned())
}

/// Validates one stable session-local id at the durable boundary (dsh
/// `decodeId`).
///
/// # Errors
///
/// Empty or untrimmed.
pub fn decode_id(value: &Value) -> Result<String, ScheduleLogError> {
    let text = value.as_str().unwrap_or("");
    if text.is_empty() || text.trim() != text {
        return Err(ScheduleLogError(
            "schedule id must be a non-empty string without surrounding whitespace".to_owned(),
        ));
    }
    Ok(text.to_owned())
}

/// The one rule a stored row binds: the durable record vocabulary (dsh
/// `ScheduleRecord` union).
///
/// The variants carry exactly the dsh stored keys; [`Self::to_value`]
/// emits the dsh construction order, which the workspace
/// `preserve_order` feature keeps in the rendered bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleRecord {
    /// Relative one-shot (dsh `AfterScheduleRecord`).
    After {
        /// Globally unique task identity.
        id: String,
        /// Required stored task name.
        title: String,
        /// Trimmed reminder content.
        prompt: String,
        /// Positive safe-integer delay accepted at creation.
        after_seconds: i64,
        /// Four-digit-year RFC 3339 UTC target.
        scheduled_at: String,
    },
    /// Absolute one-shot (dsh `AtScheduleRecord`).
    At {
        /// Globally unique task identity.
        id: String,
        /// Required stored task name.
        title: String,
        /// Trimmed reminder content.
        prompt: String,
        /// Four-digit-year RFC 3339 UTC target.
        scheduled_at: String,
    },
    /// Fixed-rate recurring (dsh `EveryScheduleRecord`).
    Every {
        /// Globally unique task identity.
        id: String,
        /// Required stored task name.
        title: String,
        /// Trimmed reminder content.
        prompt: String,
        /// Fixed safe-integer interval, never below one minute.
        every_seconds: i64,
        /// Next anchor-aligned occurrence while active.
        scheduled_at: String,
    },
    /// Daily wall-clock (dsh `DailyScheduleRecord`).
    Daily {
        /// Globally unique task identity.
        id: String,
        /// Required stored task name.
        title: String,
        /// Trimmed reminder content.
        prompt: String,
        /// Local time normalized to HH:mm:ss.SSS.
        time: String,
        /// Explicit IANA zone.
        time_zone: String,
        /// Committed next UTC instant while active.
        scheduled_at: String,
    },
    /// Weekly wall-clock (dsh `WeeklyScheduleRecord`).
    Weekly {
        /// Globally unique task identity.
        id: String,
        /// Required stored task name.
        title: String,
        /// Trimmed reminder content.
        prompt: String,
        /// Local time normalized to HH:mm:ss.SSS.
        time: String,
        /// Explicit IANA zone.
        time_zone: String,
        /// Unique ascending ISO weekdays 1 (Monday) through 7 (Sunday).
        weekdays: Vec<u8>,
        /// Committed next UTC instant while active.
        scheduled_at: String,
    },
    /// Five-field cron (dsh `CronScheduleRecord`).
    Cron {
        /// Globally unique task identity.
        id: String,
        /// Required stored task name.
        title: String,
        /// Trimmed reminder content.
        prompt: String,
        /// Canonical five-field expression.
        expression: String,
        /// Explicit IANA zone.
        time_zone: String,
        /// Committed next UTC instant while active.
        scheduled_at: String,
    },
}

impl ScheduleRecord {
    /// The task identity.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::After { id, .. }
            | Self::At { id, .. }
            | Self::Every { id, .. }
            | Self::Daily { id, .. }
            | Self::Weekly { id, .. }
            | Self::Cron { id, .. } => id,
        }
    }

    /// The stored task name.
    #[must_use]
    pub fn title(&self) -> &str {
        match self {
            Self::After { title, .. }
            | Self::At { title, .. }
            | Self::Every { title, .. }
            | Self::Daily { title, .. }
            | Self::Weekly { title, .. }
            | Self::Cron { title, .. } => title,
        }
    }

    /// The reminder content.
    #[must_use]
    pub fn prompt(&self) -> &str {
        match self {
            Self::After { prompt, .. }
            | Self::At { prompt, .. }
            | Self::Every { prompt, .. }
            | Self::Daily { prompt, .. }
            | Self::Weekly { prompt, .. }
            | Self::Cron { prompt, .. } => prompt,
        }
    }

    /// The committed UTC target (the canonical instant string).
    #[must_use]
    pub fn scheduled_at(&self) -> &str {
        match self {
            Self::After { scheduled_at, .. }
            | Self::At { scheduled_at, .. }
            | Self::Every { scheduled_at, .. }
            | Self::Daily { scheduled_at, .. }
            | Self::Weekly { scheduled_at, .. }
            | Self::Cron { scheduled_at, .. } => scheduled_at,
        }
    }

    /// Whether the rule repeats (the delivery grouping discriminator).
    #[must_use]
    pub const fn is_recurring(&self) -> bool {
        matches!(
            self,
            Self::Every { .. } | Self::Daily { .. } | Self::Weekly { .. } | Self::Cron { .. }
        )
    }
}

impl ScheduleRecord {
    /// The dsh record object in construction order (the render bytes
    /// ride this order).
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("id".to_owned(), json!(self.id()));
        object.insert(
            "kind".to_owned(),
            json!(match self {
                Self::After { .. } => "after",
                Self::At { .. } => "at",
                Self::Every { .. } => "every",
                Self::Daily { .. } => "daily",
                Self::Weekly { .. } => "weekly",
                Self::Cron { .. } => "cron",
            }),
        );
        object.insert("title".to_owned(), json!(self.title()));
        object.insert("prompt".to_owned(), json!(self.prompt()));
        match self {
            Self::After {
                after_seconds,
                scheduled_at,
                ..
            } => {
                object.insert("afterSeconds".to_owned(), json!(after_seconds));
                object.insert("scheduledAt".to_owned(), json!(scheduled_at));
            }
            Self::At { scheduled_at, .. } => {
                object.insert("scheduledAt".to_owned(), json!(scheduled_at));
            }
            Self::Every {
                every_seconds,
                scheduled_at,
                ..
            } => {
                object.insert("everySeconds".to_owned(), json!(every_seconds));
                object.insert("scheduledAt".to_owned(), json!(scheduled_at));
            }
            Self::Daily {
                time,
                time_zone,
                scheduled_at,
                ..
            } => {
                object.insert("time".to_owned(), json!(time));
                object.insert("timeZone".to_owned(), json!(time_zone));
                object.insert("scheduledAt".to_owned(), json!(scheduled_at));
            }
            Self::Weekly {
                time,
                time_zone,
                weekdays,
                scheduled_at,
                ..
            } => {
                object.insert("time".to_owned(), json!(time));
                object.insert("timeZone".to_owned(), json!(time_zone));
                object.insert(
                    "weekdays".to_owned(),
                    json!(
                        weekdays
                            .iter()
                            .map(|weekday| i64::from(*weekday))
                            .collect::<Vec<_>>()
                    ),
                );
                object.insert("scheduledAt".to_owned(), json!(scheduled_at));
            }
            Self::Cron {
                expression,
                time_zone,
                scheduled_at,
                ..
            } => {
                object.insert("expression".to_owned(), json!(expression));
                object.insert("timeZone".to_owned(), json!(time_zone));
                object.insert("scheduledAt".to_owned(), json!(scheduled_at));
            }
        }
        Value::Object(object)
    }
}

/// Validates one non-empty trimmed prompt (the shared creation
/// precheck).
///
/// # Errors
///
/// Empty after trimming.
pub fn schedule_prompt(value: &str) -> Result<String, ScheduleInputError> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(ScheduleInputError::new(
            "invalid_prompt",
            "prompt must be non-empty after trimming.",
        ));
    }
    Ok(normalized.to_owned())
}

/// Validates the ISO weekday set of one weekly rule (dsh
/// `normalizeWeekdays`).
///
/// # Errors
///
/// Empty, out-of-range, repeated, or non-integer entries.
pub fn normalize_weekdays(value: &Value) -> Result<Vec<u8>, ScheduleInputError> {
    let entries = value.as_array().ok_or_else(|| {
        ScheduleInputError::new(
            "invalid_rule",
            "weekly.weekdays must be a non-empty array of ISO weekday numbers.",
        )
    })?;
    if entries.is_empty() {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            "weekly.weekdays must be a non-empty array of ISO weekday numbers.",
        ));
    }
    let mut unique = Vec::new();
    for entry in entries {
        let number = entry.as_i64().ok_or_else(|| {
            ScheduleInputError::new(
                "invalid_rule",
                "Each weekly.weekdays entry must be an integer from 1 (Monday) through 7 (Sunday).",
            )
        })?;
        if !(1..=7).contains(&number) {
            return Err(ScheduleInputError::new(
                "invalid_rule",
                "Each weekly.weekdays entry must be an integer from 1 (Monday) through 7 (Sunday).",
            ));
        }
        let weekday = u8::try_from(number).map_err(fixed(ScheduleInputError::new(
            "invalid_rule",
            "Each weekly.weekdays entry must be an integer from 1 (Monday) through 7 (Sunday).",
        )))?;
        if unique.contains(&weekday) {
            return Err(ScheduleInputError::new(
                "invalid_rule",
                format!("weekly.weekdays must not repeat weekday {number}."),
            ));
        }
        unique.push(weekday);
    }
    unique.sort_unstable();
    Ok(unique)
}

/// Parses one daily selector without calculating a target (dsh
/// `parseDailyInput`).
///
/// # Errors
///
/// Shape, time grammar, or zone refusals.
pub fn parse_daily_input(daily: &Value) -> Result<(String, String), ScheduleInputError> {
    if !has_exact_keys(daily, &["time", "time_zone"])
        || daily.get("time").and_then(Value::as_str).is_none()
    {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            "daily must contain exactly time and time_zone, with time HH:mm:ss and optional 1-3 fractional digits.",
        ));
    }
    let time = daily
        .get("time")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let time_zone = daily.get("time_zone").ok_or_else(|| {
        ScheduleInputError::new("invalid_time_zone", "time_zone must be a string.")
    })?;
    let Some(time_zone) = time_zone.as_str() else {
        return Err(ScheduleInputError::new(
            "invalid_time_zone",
            "time_zone must be a string.",
        ));
    };
    Ok((
        local_clock_time(time, "daily")?,
        canonicalize_time_zone_name(time_zone)?,
    ))
}

/// Requires exactly the named durable object keys (dsh `hasExactKeys`).
pub fn has_exact_keys(value: &Value, expected: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
    })
}

/// Parses one weekly selector without calculating a target (dsh
/// `parseWeeklyInput`).
///
/// # Errors
///
/// Shape, time grammar, zone, or weekday refusals.
pub fn parse_weekly_input(weekly: &Value) -> Result<(String, String, Vec<u8>), ScheduleInputError> {
    if !has_exact_keys(weekly, &["time", "time_zone", "weekdays"])
        || weekly.get("time").and_then(Value::as_str).is_none()
    {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            "weekly must contain exactly time, time_zone, and weekdays, with time HH:mm:ss and optional 1-3 fractional digits.",
        ));
    }
    let time = weekly
        .get("time")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let time_zone = weekly.get("time_zone").ok_or_else(|| {
        ScheduleInputError::new("invalid_time_zone", "time_zone must be a string.")
    })?;
    let Some(time_zone) = time_zone.as_str() else {
        return Err(ScheduleInputError::new(
            "invalid_time_zone",
            "time_zone must be a string.",
        ));
    };
    let weekdays = weekly.get("weekdays").ok_or_else(|| {
        ScheduleInputError::new(
            "invalid_rule",
            "weekly must contain exactly time, time_zone, and weekdays, with time HH:mm:ss and optional 1-3 fractional digits.",
        )
    })?;
    Ok((
        local_clock_time(time, "weekly")?,
        canonicalize_time_zone_name(time_zone)?,
        normalize_weekdays(weekdays)?,
    ))
}

/// Parses one cron selector without calculating a target (dsh
/// `parseCronInput`).
///
/// # Errors
///
/// Shape, expression, or zone refusals.
pub fn parse_cron_input(cron: &Value) -> Result<(String, String), ScheduleInputError> {
    if !has_exact_keys(cron, &["expression", "time_zone"])
        || cron.get("expression").and_then(Value::as_str).is_none()
    {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            "cron must contain exactly expression and time_zone, with a five-field cron expression.",
        ));
    }
    let time_zone = cron.get("time_zone").ok_or_else(|| {
        ScheduleInputError::new("invalid_time_zone", "time_zone must be a string.")
    })?;
    let Some(time_zone) = time_zone.as_str() else {
        return Err(ScheduleInputError::new(
            "invalid_time_zone",
            "time_zone must be a string.",
        ));
    };
    let expression = cron
        .get("expression")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Ok((
        parse_cron_expression(expression)?.expression,
        canonicalize_time_zone_name(time_zone)?,
    ))
}

/// Parses an absolute selector without requiring it to be future (dsh
/// `parseAtInput`).
///
/// # Errors
///
/// Shape, calendar, zone, or gap refusals.
pub fn parse_at_input(at: &Value) -> Result<i64, ScheduleInputError> {
    if let Some(text) = at.as_str() {
        return parse_offset_instant(text);
    }
    if at.is_object() {
        if !has_exact_keys(at, &["date", "time", "time_zone"]) {
            return Err(ScheduleInputError::new(
                "invalid_rule",
                "Local at must contain exactly date, time, and time_zone.",
            ));
        }
        if at.get("date").and_then(Value::as_str).is_none()
            || at.get("time").and_then(Value::as_str).is_none()
        {
            return Err(ScheduleInputError::new(
                "invalid_rule",
                "Local at date and time must be strings.",
            ));
        }
        let Some(time_zone) = at.get("time_zone").and_then(Value::as_str) else {
            return Err(ScheduleInputError::new(
                "invalid_time_zone",
                "time_zone must be a string.",
            ));
        };
        let zone = canonicalize_time_zone(time_zone)?;
        let parts = parse_local_at(at)?;
        return resolve_local_instant(&parts, &zone);
    }
    Err(ScheduleInputError::new(
        "invalid_rule",
        "at must be an explicit-offset string or local calendar object.",
    ))
}

/// Parses strict local calendar fields without consulting a process time
/// zone (dsh `parseLocalAt`).
///
/// # Errors
///
/// Grammar or calendar refusals.
fn parse_local_at(at: &Value) -> Result<CalendarParts, ScheduleInputError> {
    let date = at.get("date").and_then(Value::as_str).unwrap_or_default();
    let time = at.get("time").and_then(Value::as_str).unwrap_or_default();
    let date_groups = local_date_regex().captures(date);
    let time_groups = local_time_regex().captures(time);
    let (Some(date_groups), Some(time_groups)) = (date_groups, time_groups) else {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            "Local at requires date YYYY-MM-DD and time HH:mm:ss with optional one-to-three digit milliseconds.",
        ));
    };
    let group = |groups: &regex::Captures<'_>, name: &str| -> Result<i16, ScheduleInputError> {
        groups
            .name(name)
            .and_then(|part| part.as_str().parse().ok())
            .ok_or_else(|| {
                ScheduleInputError::new("invalid_rule", "The at value has an invalid shape.")
            })
    };
    let year = group(&date_groups, "year")?;
    let month = group(&date_groups, "month")?;
    let day = group(&date_groups, "day")?;
    let hour = group(&time_groups, "hour")?;
    let minute = group(&time_groups, "minute")?;
    let second = group(&time_groups, "second")?;
    let millisecond = fractional_millis(time_groups.name("fraction").map(|part| part.as_str()));
    if year == 0 || hour > 23 || minute > 59 || second > 59 {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            "The local at value must be a real ISO calendar date and time.",
        ));
    }
    let parts = CalendarParts {
        year,
        month: i8::try_from(month).map_err(fixed(at_refusal()))?,
        day: i8::try_from(day).map_err(fixed(at_refusal()))?,
        hour: i8::try_from(hour).map_err(fixed(at_refusal()))?,
        minute: i8::try_from(minute).map_err(fixed(at_refusal()))?,
        second: i8::try_from(second).map_err(fixed(at_refusal()))?,
        millisecond,
    };
    calendar_epoch(parts)?;
    Ok(parts)
}

/// The local-at calendar refusal (dsh shares the at-value diagnostic).
fn at_refusal() -> ScheduleInputError {
    ScheduleInputError::new(
        "invalid_rule",
        "The local at value must be a real ISO calendar date and time.",
    )
}

/// Resolves only the earlier overlap instant; a gap fails the local
/// round trip (dsh `localInstant`).
pub fn local_instant(local: &DateTime, zone: &TimeZone) -> Option<i64> {
    let zoned = zone.to_ambiguous_zoned(*local).earlier().ok()?;
    (zoned.datetime() == *local).then_some(zoned.timestamp().as_millisecond())
}

/// Resolves a local one-shot, rejecting nonexistent wall-clock times
/// (dsh `resolveLocalInstant`).
///
/// # Errors
///
/// The local time does not exist in the zone.
fn resolve_local_instant(
    parts: &CalendarParts,
    zone: &TimeZone,
) -> Result<i64, ScheduleInputError> {
    let date = Date::new(parts.year, parts.month, parts.day).map_err(fixed(at_refusal()))?;
    let time = jiff::civil::Time::new(
        parts.hour,
        parts.minute,
        parts.second,
        i32::from(parts.millisecond) * 1_000_000,
    )
    .map_err(fixed(at_refusal()))?;
    let target = local_instant(&DateTime::from_parts(date, time), zone);
    target.ok_or_else(|| {
        ScheduleInputError::new(
            "invalid_rule",
            "The local at time does not exist in the selected time zone.",
        )
    })
}

/// Projects an explicit instant into a calendar date in the rule's zone
/// (dsh `localDate`).
pub fn local_date(epoch: i64, zone: &TimeZone) -> Date {
    // The four-digit ceiling's final day sits outside jiff's
    // `Timestamp` range; the walk's bound reads as the UTC civil date
    // there (a zone offset cannot move the date by more than a day at
    // the window edge).
    Timestamp::from_millisecond(epoch).map_or_else(
        |_| date_from_civil_days(epoch.div_euclid(86_400_000)),
        |timestamp| timestamp.to_zoned(zone.clone()).date(),
    )
}

/// Builds one civil date from epoch days (the Hinnant inverse; the
/// walk yields real dates by construction).
fn date_from_civil_days(days: i64) -> Date {
    let (year, month, day) = civil_from_days(days);
    // The algorithm bounds the year to the four-digit window and the
    // month/day to 1-12/1-31, so the conversions are exact.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the Hinnant walk yields months 1-12 and days 1-31"
    )]
    let built = Date::new(
        i16::try_from(year).unwrap_or_default(),
        month as i8,
        day as i8,
    );
    built.expect("the Hinnant walk yields real dates")
}

/// Validates one required stored title at the update path's retained
/// face (dsh `retainedTitle`: a decoded record always carries one, so
/// the durable diagnostic stands in for a derived name).
pub fn retained_title(record: &ScheduleRecord) -> Result<String, ScheduleLogError> {
    decode_stored_title(&json!(record.title()))
}

/// Builds one relative one-shot record (dsh `createAfterScheduleRecord`).
///
/// # Errors
///
/// The prompt, delay, title, or range refusals.
pub fn create_after_schedule_record(
    id: &str,
    prompt: &str,
    after_seconds: i64,
    now: i64,
    title: &str,
) -> Result<ScheduleRecord, ScheduleInputError> {
    let normalized_prompt = schedule_prompt(prompt)?;
    if after_seconds <= 0 {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            "after_seconds must be a positive safe integer.",
        ));
    }
    let title = schedule_title(&json!(title))?;
    let target = now + after_seconds.saturating_mul(1_000);
    Ok(ScheduleRecord::After {
        id: id.to_owned(),
        title,
        prompt: normalized_prompt,
        after_seconds,
        scheduled_at: future_instant(target, now)?,
    })
}

/// Builds one absolute one-shot record (dsh `createAtScheduleRecord`).
///
/// # Errors
///
/// The prompt, title, selector, or range refusals.
pub fn create_at_schedule_record(
    id: &str,
    prompt: &str,
    at: &Value,
    now: i64,
    title: &str,
) -> Result<ScheduleRecord, ScheduleInputError> {
    let normalized_prompt = schedule_prompt(prompt)?;
    let title = schedule_title(&json!(title))?;
    let target = parse_at_input(at)?;
    Ok(ScheduleRecord::At {
        id: id.to_owned(),
        title,
        prompt: normalized_prompt,
        scheduled_at: future_instant(target, now)?,
    })
}

/// Builds one fixed-rate record (dsh `createEveryScheduleRecord`).
///
/// # Errors
///
/// The prompt, interval, title, or range refusals.
pub fn create_every_schedule_record(
    id: &str,
    prompt: &str,
    every_seconds: i64,
    now: i64,
    title: &str,
) -> Result<ScheduleRecord, ScheduleInputError> {
    let normalized_prompt = schedule_prompt(prompt)?;
    if every_seconds < MIN_EVERY_INTERVAL_SECONDS {
        return Err(ScheduleInputError::new(
            "frequency_too_high",
            format!("every_seconds must be at least {MIN_EVERY_INTERVAL_SECONDS}."),
        ));
    }
    let title = schedule_title(&json!(title))?;
    let target = now + every_seconds.saturating_mul(1_000);
    Ok(ScheduleRecord::Every {
        id: id.to_owned(),
        title,
        prompt: normalized_prompt,
        every_seconds,
        scheduled_at: future_instant(target, now)?,
    })
}

/// Builds one daily wall-clock record (dsh `createDailyScheduleRecord`).
///
/// # Errors
///
/// The prompt, selector, range, or exhaustion refusals.
pub fn create_daily_schedule_record(
    id: &str,
    prompt: &str,
    daily: &Value,
    now: i64,
    title: &str,
) -> Result<ScheduleRecord, ScheduleInputError> {
    let normalized_prompt = schedule_prompt(prompt)?;
    let (time, time_zone) = parse_daily_input(daily)?;
    if !(MIN_FOUR_DIGIT_YEAR_MS..=MS_OF_MAX_FOUR_DIGIT_YEAR).contains(&now) {
        return Err(ScheduleInputError::new(
            "time_out_of_range",
            "Daily creation time must be a representable four-digit-year UTC instant.",
        ));
    }
    let zone = jiff::tz::db()
        .get(&time_zone)
        .map_err(fixed(creation_zone_refusal()))?;
    let scheduled_at = next_daily_target(&time, &zone, now, None).ok_or_else(|| {
        ScheduleInputError::new(
            "time_out_of_range",
            "No future daily occurrence is representable as a four-digit-year UTC instant.",
        )
    })?;
    let title = schedule_title(&json!(title))?;
    Ok(ScheduleRecord::Daily {
        id: id.to_owned(),
        title,
        prompt: normalized_prompt,
        time,
        time_zone,
        scheduled_at,
    })
}

/// The zone-get refusal on the already-canonicalized creation zones (a
/// canonical name always resolves; the failure maps to the range
/// refusal the dsh arms share).
fn creation_zone_refusal() -> ScheduleInputError {
    ScheduleInputError::new(
        "time_out_of_range",
        "Daily creation time must be a representable four-digit-year UTC instant.",
    )
}

/// Builds one weekly wall-clock record (dsh
/// `createWeeklyScheduleRecord`).
///
/// # Errors
///
/// The prompt, selector, range, or exhaustion refusals.
pub fn create_weekly_schedule_record(
    id: &str,
    prompt: &str,
    weekly: &Value,
    now: i64,
    title: &str,
) -> Result<ScheduleRecord, ScheduleInputError> {
    let normalized_prompt = schedule_prompt(prompt)?;
    let (time, time_zone, weekdays) = parse_weekly_input(weekly)?;
    if !(MIN_FOUR_DIGIT_YEAR_MS..=MS_OF_MAX_FOUR_DIGIT_YEAR).contains(&now) {
        return Err(ScheduleInputError::new(
            "time_out_of_range",
            "Weekly creation time must be a representable four-digit-year UTC instant.",
        ));
    }
    let zone = jiff::tz::db()
        .get(&time_zone)
        .map_err(fixed(ScheduleInputError::new(
            "time_out_of_range",
            "Weekly creation time must be a representable four-digit-year UTC instant.",
        )))?;
    let scheduled_at = next_weekly_target(&time, &zone, &weekdays, now, None).ok_or_else(|| {
        ScheduleInputError::new(
            "time_out_of_range",
            "No future weekly occurrence is representable as a four-digit-year UTC instant.",
        )
    })?;
    let title = schedule_title(&json!(title))?;
    Ok(ScheduleRecord::Weekly {
        id: id.to_owned(),
        title,
        prompt: normalized_prompt,
        time,
        time_zone,
        weekdays,
        scheduled_at,
    })
}

/// Builds one cron wall-clock record (dsh `createCronScheduleRecord`).
///
/// # Errors
///
/// The prompt, selector, range, or exhaustion refusals.
pub fn create_cron_schedule_record(
    id: &str,
    prompt: &str,
    cron: &Value,
    now: i64,
    title: &str,
) -> Result<ScheduleRecord, ScheduleInputError> {
    let normalized_prompt = schedule_prompt(prompt)?;
    let (expression, time_zone) = parse_cron_input(cron)?;
    if !(MIN_FOUR_DIGIT_YEAR_MS..=MS_OF_MAX_FOUR_DIGIT_YEAR).contains(&now) {
        return Err(ScheduleInputError::new(
            "time_out_of_range",
            "Cron creation time must be a representable four-digit-year UTC instant.",
        ));
    }
    let parsed = parse_cron_expression(&expression)?;
    let zone = jiff::tz::db()
        .get(&time_zone)
        .map_err(fixed(ScheduleInputError::new(
            "time_out_of_range",
            "Cron creation time must be a representable four-digit-year UTC instant.",
        )))?;
    let scheduled_at = next_cron_target(&parsed, &zone, now).ok_or_else(|| {
        ScheduleInputError::new(
            "time_out_of_range",
            "No future cron occurrence is representable as a four-digit-year UTC instant.",
        )
    })?;
    let title = schedule_title(&json!(title))?;
    Ok(ScheduleRecord::Cron {
        id: id.to_owned(),
        title,
        prompt: normalized_prompt,
        expression,
        time_zone,
        scheduled_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two range constants round-trip through jiff: the canonical
    /// formatter reproduces the dsh `Date.parse` anchors exactly.
    #[test]
    fn the_four_digit_bounds_round_trip_through_jiff() {
        assert_eq!(
            canonical_instant(MIN_FOUR_DIGIT_YEAR_MS),
            "0001-01-01T00:00:00.000Z"
        );
        assert_eq!(
            canonical_instant(MS_OF_MAX_FOUR_DIGIT_YEAR),
            "9999-12-31T23:59:59.999Z"
        );
        assert_eq!(
            decode_instant("0001-01-01T00:00:00.000Z").expect("the floor decodes"),
            MIN_FOUR_DIGIT_YEAR_MS
        );
    }

    /// The title validators pin the dsh texts and the UTF-16 length
    /// semantics (the batch-4 summary judgment).
    #[test]
    fn the_title_arms_pin_the_dsh_texts() {
        assert_eq!(
            schedule_title(&json!("  focused ")).expect("trimmed"),
            "focused"
        );
        assert_eq!(
            schedule_title(&json!("   ")).unwrap_err().message,
            REQUIRED_TITLE_MESSAGE
        );
        assert_eq!(
            schedule_title(&json!("x".repeat(121))).unwrap_err().message,
            "title must be at most 120 characters."
        );
        assert_eq!(
            schedule_title(&json!("世".repeat(120)))
                .expect("BMP units count one each")
                .chars()
                .count(),
            120
        );
        assert_eq!(
            decode_stored_title(&json!(" padded")).unwrap_err().0,
            "title must be a trimmed string"
        );
        assert_eq!(
            decode_stored_title(&json!(null)).unwrap_err().0,
            REQUIRED_TITLE_MESSAGE
        );
    }

    /// The canonical form agrees with jiff inside jiff's own range (the
    /// Hinnant walk's cross-check), and the zone canonicalization pins
    /// the UTC special case, an alias resolution, and the refusals (the
    /// Q16c-4A jiff judgment).
    #[test]
    fn the_calendar_layer_agrees_with_jiff_and_the_zone_gates() {
        for ms in [
            0,
            86_400_000,
            1_700_000_000_000,
            MIN_FOUR_DIGIT_YEAR_MS,
            MS_OF_MAX_FOUR_DIGIT_YEAR - 86_400_000,
        ] {
            let jiff_form = Timestamp::from_millisecond(ms).ok().map(|timestamp| {
                let civil = timestamp.to_zoned(TimeZone::UTC).datetime();
                format!(
                    "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
                    civil.year(),
                    i32::from(civil.month()),
                    i32::from(civil.day()),
                    i32::from(civil.hour()),
                    i32::from(civil.minute()),
                    i32::from(civil.second()),
                    i32::from(civil.millisecond()),
                )
            });
            if let Some(jiff_form) = jiff_form {
                assert_eq!(canonical_instant(ms), jiff_form, "{ms}");
            }
        }
        assert_eq!(canonicalize_time_zone_name("UTC").expect("UTC"), "UTC");
        assert_eq!(
            canonicalize_time_zone_name("Asia/Shanghai").expect("IANA name"),
            "Asia/Shanghai"
        );
        assert_eq!(
            canonicalize_time_zone_name(" UTC").unwrap_err().code,
            "invalid_time_zone"
        );
        assert_eq!(
            canonicalize_time_zone_name("Not/AZone")
                .unwrap_err()
                .message,
            "time_zone must be UTC or a valid IANA Area/Location name."
        );
        assert_eq!(
            canonicalize_time_zone_name("UTC+8").unwrap_err().code,
            "invalid_time_zone"
        );
    }

    /// The offset-instant grammar pins the accepted spellings, the
    /// -00:00 refusal, the calendar refusals, and the local-at gap
    /// arm (the dsh `parseOffsetInstant`/`parseAtInput` faces).
    #[test]
    fn the_at_grammar_pins_the_offset_and_gap_arms() {
        assert_eq!(
            parse_offset_instant("2030-06-01T12:00:00Z").expect("Z form"),
            1_906_545_600_000
        );
        assert_eq!(
            parse_offset_instant("2030-06-01T12:00:00.5+02:00").expect("fraction and offset"),
            1_906_538_400_500
        );
        assert_eq!(
            parse_offset_instant("2030-06-01 12:00:00Z")
                .unwrap_err()
                .message,
            "at must use YYYY-MM-DDTHH:mm:ss with optional 1-3 digit fractional seconds and an explicit Z or numeric offset.",
        );
        assert_eq!(
            parse_offset_instant("2030-06-01T12:00:00-00:00")
                .unwrap_err()
                .message,
            "The at numeric offset is invalid."
        );
        assert_eq!(
            parse_offset_instant("2030-02-30T12:00:00Z")
                .unwrap_err()
                .message,
            "The at value must be a real ISO calendar date and time."
        );
        // A local one-shot in a spring-forward gap refuses; an overlap
        // takes the earlier instant.
        let gap = json!({
            "date": "2030-03-10", "time": "02:30:00", "time_zone": "America/New_York",
        });
        assert_eq!(
            parse_at_input(&gap).unwrap_err().message,
            "The local at time does not exist in the selected time zone."
        );
        let overlap = json!({
            "date": "2030-11-03", "time": "01:30:00", "time_zone": "America/New_York",
        });
        let resolved = parse_at_input(&overlap).expect("the overlap resolves");
        assert_eq!(canonical_instant(resolved), "2030-11-03T05:30:00.000Z");
    }

    /// The future-instant gate pins the range and strictly-future
    /// refusals verbatim.
    #[test]
    fn the_future_gate_pins_the_range_and_not_future_arms() {
        let now = 1_700_000_000_000;
        assert_eq!(future_instant(now, now).unwrap_err().code, "not_future");
        assert_eq!(
            future_instant(now - 1, now).unwrap_err().message,
            "The scheduled time must be strictly in the future."
        );
        assert_eq!(
            future_instant(MIN_FOUR_DIGIT_YEAR_MS - 1, now)
                .unwrap_err()
                .message,
            "The scheduled time must be representable as a four-digit-year RFC 3339 UTC instant.",
        );
        assert_eq!(
            future_instant(MS_OF_MAX_FOUR_DIGIT_YEAR + 1, now)
                .unwrap_err()
                .code,
            "time_out_of_range"
        );
        assert_eq!(
            future_instant(now + 1_500, now).expect("in range"),
            "2023-11-14T22:13:21.500Z"
        );
    }
}
