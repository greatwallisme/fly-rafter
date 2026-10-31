// Rust guideline compliant 2026-10-31
//! The tool-side time filters (dsh `tool-session-query/input.ts`):
//! the strict ISO 8601 timestamp parser with sub-millisecond remainders
//! and the inclusive-range bounds.

use crate::session_query::signed_to_f64;

/// One exact parsed timestamp (dsh `ExactTimestamp`): the whole
/// millisecond and the canonical decimal digits strictly below one
/// millisecond (no trailing zeroes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactTimestamp {
    /// The whole millisecond value (the dsh `Date.parse` result).
    pub millisecond: i64,
    /// Canonical decimal digits strictly below one millisecond; no
    /// trailing zeroes.
    pub remainder: String,
}

impl ExactTimestamp {
    /// The inclusive lower bound for a filter range (dsh
    /// `timestampLowerBound`: a remainder steps one float ULP up).
    #[must_use]
    pub fn lower_bound(&self) -> f64 {
        if self.remainder.is_empty() {
            signed_to_f64(self.millisecond)
        } else {
            next_up_finite(signed_to_f64(self.millisecond))
        }
    }

    /// The inclusive upper bound for a filter range (dsh
    /// `timestampUpperBound`: a remainder steps one float ULP below the
    /// next millisecond).
    #[must_use]
    pub fn upper_bound(&self) -> f64 {
        if self.remainder.is_empty() {
            signed_to_f64(self.millisecond)
        } else {
            next_down_finite(signed_to_f64(self.millisecond + 1))
        }
    }
}

/// Steps one float to the next value toward positive infinity (dsh
/// `nextUpFinite`; the bit-increment walk with the zero sign handles).
#[must_use]
pub fn next_up_finite(value: f64) -> f64 {
    if value == 0.0 {
        return f64::from_bits(1);
    }
    let bits = value.to_bits();
    if value > 0.0 {
        f64::from_bits(bits + 1)
    } else {
        f64::from_bits(bits - 1)
    }
}

/// Steps one float to the next value toward negative infinity (dsh
/// `nextDownFinite`; zero steps to `-Number.MIN_VALUE`).
#[must_use]
pub fn next_down_finite(value: f64) -> f64 {
    if value == 0.0 {
        // -Number.MIN_VALUE: the sign bit plus the smallest mantissa.
        return f64::from_bits(0x8000_0000_0000_0001);
    }
    let bits = value.to_bits();
    if value > 0.0 {
        f64::from_bits(bits - 1)
    } else {
        f64::from_bits(bits + 1)
    }
}

/// The strict ISO 8601 shape the dsh parser accepts
/// (`YYYY-MM-DDTHH:MM[:SS[.frac]]` with `Z` or a numeric offset).
pub const ISO_TIMESTAMP_PATTERN: &str = concat!(
    r"^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})",
    r"(?::(\d{2})(?:\.(\d+))?)?(Z|([+-])(\d{2}):(\d{2}))$"
);

/// Parses one ISO 8601 timestamp with the dsh strictness.
///
/// The field ranges validate on the raw digits, the fraction truncates
/// into the millisecond and keeps the sub-ms remainder, and the offset
/// applies before the epoch conversion.
///
/// # Errors
///
/// The dsh refusal detail (the caller wraps it with the range text).
///
/// # Panics
///
/// Panics when the ISO pattern stops compiling (a literal; a regex
/// crate regression).
pub fn parse_iso_timestamp(value: &str) -> Result<ExactTimestamp, String> {
    let regex = regex::Regex::new(ISO_TIMESTAMP_PATTERN).expect("the ISO pattern compiles");
    let Some(captures) = regex.captures(value) else {
        return Err("must be an ISO 8601 timestamp with Z or a numeric offset".to_owned());
    };
    let year = parse_group(&captures, 1);
    let month = parse_group(&captures, 2);
    let day = parse_group(&captures, 3);
    let hour = parse_group(&captures, 4);
    let minute = parse_group(&captures, 5);
    let second = captures
        .get(6)
        .and_then(|m| m.as_str().parse::<u64>().ok())
        .unwrap_or(0);
    let offset_hour = captures
        .get(10)
        .and_then(|m| m.as_str().parse::<u64>().ok())
        .unwrap_or(0);
    let offset_minute = captures
        .get(11)
        .and_then(|m| m.as_str().parse::<u64>().ok())
        .unwrap_or(0);
    let valid_month = (1..=12).contains(&month);
    let valid_day = (1..=days_in_month(year, month)).contains(&day);
    let valid_clock = hour <= 23 && minute <= 59 && second <= 59;
    let valid_offset = offset_hour <= 23 && offset_minute <= 59;
    if !valid_month || !valid_day || !valid_clock || !valid_offset {
        return Err("must be a valid ISO 8601 timestamp".to_owned());
    }
    let fraction = captures.get(7).map(|m| m.as_str()).unwrap_or_default();
    let mut millisecond_digits = fraction.chars().take(3).collect::<String>();
    while millisecond_digits.len() < 3 {
        millisecond_digits.push('0');
    }
    let millisecond: u64 = millisecond_digits.parse().unwrap_or(0);
    let offset_sign: i64 = if captures.get(9).map(|m| m.as_str()) == Some("-") {
        -1
    } else {
        1
    };
    let offset_minutes = i64::try_from(offset_hour * 60 + offset_minute).unwrap_or(0);
    let epoch_millisecond = civil_to_epoch_millisecond(
        year,
        month,
        day,
        hour,
        minute,
        second,
        millisecond,
        offset_sign * offset_minutes,
    );
    let Some(epoch_millisecond) = epoch_millisecond else {
        return Err("must be a valid ISO 8601 timestamp".to_owned());
    };
    let remainder = fraction
        .chars()
        .skip(3)
        .collect::<String>()
        .trim_end_matches('0')
        .to_owned();
    Ok(ExactTimestamp {
        millisecond: epoch_millisecond,
        remainder,
    })
}

/// Reads one capture group as a u64 (the pattern guarantees digits).
fn parse_group(captures: &regex::Captures<'_>, index: usize) -> u64 {
    captures
        .get(index)
        .and_then(|m| m.as_str().parse::<u64>().ok())
        .unwrap_or(0)
}

/// The month length table with the leap-year arm (dsh `daysInMonth`).
#[must_use]
pub const fn days_in_month(year: u64, month: u64) -> u64 {
    if month == 2 {
        let leap =
            year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
        return if leap { 29 } else { 28 };
    }
    if matches!(month, 4 | 6 | 9 | 11) {
        30
    } else {
        31
    }
}

/// Converts one validated civil datetime plus offset into epoch
/// milliseconds (the `Date.parse` normalization's arithmetic).
///
/// The days-from-civil conversion; years whose epoch leaves the
/// reachable domain return `None`.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "the civil fields mirror the parsed groups one arm per digit run"
)]
pub fn civil_to_epoch_millisecond(
    year: u64,
    month: u64,
    day: u64,
    hour: u64,
    minute: u64,
    second: u64,
    millisecond: u64,
    offset_minutes: i64,
) -> Option<i64> {
    // dsh `Number.isSafeInteger(Date.parse(...))` rejects years whose
    // epoch leaves the safe domain; the i64 domain carries the same
    // reach, and years below the proleptic Gregorian floor of the
    // normalized form fail the parse.
    if year == 0 {
        return None;
    }
    let year_i64 = i64::try_from(year).ok()?;
    let (month_i64, day_i64) = (i64::try_from(month).ok()?, i64::try_from(day).ok()?);
    let (hour_i64, minute_i64) = (i64::try_from(hour).ok()?, i64::try_from(minute).ok()?);
    let second_i64 = i64::try_from(second).ok()?;
    let millisecond_i64 = i64::try_from(millisecond).ok()?;
    // Howard Hinnant's days_from_civil over the proleptic Gregorian
    // calendar.
    let y = if month_i64 <= 2 {
        year_i64 - 1
    } else {
        year_i64
    };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m_shifted = if month_i64 > 2 {
        month_i64 - 3
    } else {
        month_i64 + 9
    };
    let doy = (153 * m_shifted + 2) / 5 + day_i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let milliseconds = days * 86_400_000
        + hour_i64 * 3_600_000
        + minute_i64 * 60_000
        + second_i64 * 1_000
        + millisecond_i64
        - offset_minutes * 60_000;
    if milliseconds.abs() > 9_007_199_254_740_991 {
        return None;
    }
    Some(milliseconds)
}

/// Renders one epoch millisecond as the UTC ISO text (dsh
/// `new Date(value).toISOString()`): `YYYY-MM-DDTHH:MM:SS.sssZ`.
#[must_use]
pub fn to_iso_string(value: i64) -> String {
    let days = value.div_euclid(86_400_000);
    let remainder = value.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let hour = remainder / 3_600_000;
    let minute = (remainder % 3_600_000) / 60_000;
    let second = (remainder % 60_000) / 1_000;
    let millisecond = remainder % 1_000;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millisecond:03}Z")
}

/// Converts one days-since-epoch count into the civil date (the
/// days-from-civil inverse over the proleptic Gregorian calendar).
#[must_use]
pub fn civil_from_days(days: i64) -> (i64, u64, u64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (
        year,
        u64::try_from(m).unwrap_or(1),
        u64::try_from(d).unwrap_or(1),
    )
}
