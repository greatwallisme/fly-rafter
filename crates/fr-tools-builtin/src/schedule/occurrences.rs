// Rust guideline compliant 2026-10-31
//! The occurrence resolution (dsh `domain.ts` occurrence half): the
//! strict five-field cron dialect with its Vixie DOM/DOW semantics and
//! canonical encoding, the four per-rule resolvers, the durable record
//! decode, the delivery retention append, the model view, and the
//! reminder framing.

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde_json::{Value, json};

use super::domain::{
    CRON_SEARCH_HORIZON_YEARS, MIN_EVERY_INTERVAL_SECONDS, MIN_FOUR_DIGIT_YEAR_MS,
    MS_OF_MAX_FOUR_DIGIT_YEAR, REQUIRED_TITLE_MESSAGE, ScheduleInputError, ScheduleLogError,
    ScheduleRecord, canonical_instant, canonicalize_time_zone_name, decode_id, decode_instant,
    decode_stored_title, has_exact_keys, local_clock_time, local_date, local_instant,
    normalize_weekdays,
};

/// Field bounds of the supported five-field cron dialect (dsh
/// `CronFieldSpec`; day-of-week accepts 0-7 and folds 7 onto 0).
struct CronFieldSpec {
    name: &'static str,
    min: i64,
    max: i64,
    canonical_max: i64,
}

const CRON_FIELDS: [CronFieldSpec; 5] = [
    CronFieldSpec {
        name: "minute",
        min: 0,
        max: 59,
        canonical_max: 59,
    },
    CronFieldSpec {
        name: "hour",
        min: 0,
        max: 23,
        canonical_max: 23,
    },
    CronFieldSpec {
        name: "day-of-month",
        min: 1,
        max: 31,
        canonical_max: 31,
    },
    CronFieldSpec {
        name: "month",
        min: 1,
        max: 12,
        canonical_max: 12,
    },
    CronFieldSpec {
        name: "day-of-week",
        min: 0,
        max: 7,
        canonical_max: 6,
    },
];

/// One parsed cron field: matched values, canonical spelling, and the
/// Vixie star flag (dsh `ParsedCronField`).
struct ParsedCronField {
    values: Vec<i64>,
    canonical: String,
    star: bool,
}

/// Parsed five-field cron rule (dsh `ParsedCronExpression`).
#[derive(Debug)]
pub struct ParsedCronExpression {
    pub expression: String,
    pub minutes: Vec<i64>,
    pub hours: Vec<i64>,
    pub days_of_month: Vec<i64>,
    pub months: Vec<i64>,
    pub days_of_week: Vec<i64>,
    pub day_of_month_star: bool,
    pub day_of_week_star: bool,
}

/// Builds the stable diagnostic for one malformed cron field element
/// (dsh `invalidCronField`).
fn invalid_cron_field(spec: &CronFieldSpec, element: &str) -> ScheduleInputError {
    ScheduleInputError::new(
        "invalid_rule",
        format!(
            "cron.expression {} field element {} must be *, a value, a-b, */n, a-b/n, or a comma-separated list of those.",
            spec.name,
            json!(element)
        ),
    )
}

/// Reads one in-range cron field value (dsh `cronFieldValue`).
///
/// # Errors
///
/// Out-of-range or non-integer.
fn cron_field_value(text: &str, spec: &CronFieldSpec) -> Result<i64, ScheduleInputError> {
    let Some(value) = parse_safe_integer(text) else {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            format!(
                "cron.expression {} field value {text} is outside {}-{}.",
                spec.name, spec.min, spec.max
            ),
        ));
    };
    if value < spec.min || value > spec.max {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            format!(
                "cron.expression {} field value {text} is outside {}-{}.",
                spec.name, spec.min, spec.max
            ),
        ));
    }
    Ok(value)
}

/// Reads one decimal text as a safe integer the way `Number(text)` does
/// for the cron grammar (leading zeros and signs stay plain integers).
fn parse_safe_integer(text: &str) -> Option<i64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Reads one positive cron field step (dsh `cronFieldStep`).
///
/// # Errors
///
/// Zero or negative.
fn cron_field_step(text: &str, spec: &CronFieldSpec) -> Result<i64, ScheduleInputError> {
    let Some(step) = parse_safe_integer(text) else {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            format!(
                "cron.expression {} field step must be a positive integer.",
                spec.name
            ),
        ));
    };
    if step < 1 {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            format!(
                "cron.expression {} field step must be a positive integer.",
                spec.name
            ),
        ));
    }
    Ok(step)
}

/// Expands one comma-separated cron field into its matched value set
/// (dsh `cronFieldValues`).
///
/// # Errors
///
/// Empty elements, bad shapes, inverted ranges, out-of-range values.
fn cron_field_values(raw: &str, spec: &CronFieldSpec) -> Result<Vec<i64>, ScheduleInputError> {
    let mut matched = std::collections::BTreeSet::new();
    for element in raw.split(',') {
        if element.is_empty() {
            return Err(ScheduleInputError::new(
                "invalid_rule",
                format!(
                    "cron.expression {} field must not contain an empty list element.",
                    spec.name
                ),
            ));
        }
        if element == "*" {
            for value in spec.min..=spec.max {
                matched.insert(value);
            }
            continue;
        }
        if let Some(step_text) = element.strip_prefix('*') {
            let Some(step_text) = step_text.strip_prefix('/') else {
                return Err(invalid_cron_field(spec, element));
            };
            let step = cron_field_step(step_text, spec)?;
            let mut value = spec.min;
            while value <= spec.max {
                matched.insert(value);
                value += step;
            }
            continue;
        }
        let (range_text, step_text) = element
            .split_once('/')
            .map_or((element, None), |(left, right)| (left, Some(right)));
        let (start_text, end_text) = range_text
            .split_once('-')
            .map_or((range_text, None), |(left, right)| (left, Some(right)));
        let start = cron_field_value(start_text, spec)?;
        let Some(end_text) = end_text else {
            if step_text.is_some() {
                return Err(invalid_cron_field(spec, element));
            }
            matched.insert(start);
            continue;
        };
        let last = cron_field_value(end_text, spec)?;
        if start > last {
            return Err(ScheduleInputError::new(
                "invalid_rule",
                format!(
                    "cron.expression {} field range {start}-{last} is inverted.",
                    spec.name
                ),
            ));
        }
        let step = step_text.map_or(Ok(1), |text| cron_field_step(text, spec))?;
        let mut value = start;
        while value <= last {
            matched.insert(fold_value(value, spec));
            value += step;
        }
    }
    // The Sunday fold can collide with an already-matched value, so the
    // dedup re-runs after the fold (the dsh value-Set construction).
    let folded: std::collections::BTreeSet<i64> = matched
        .into_iter()
        .map(|value| fold_value(value, spec))
        .collect();
    Ok(folded.into_iter().collect())
}

/// Folds one matched value onto the canonical range (dsh folds Sunday 7
/// onto 0 in the value set construction).
const fn fold_value(value: i64, spec: &CronFieldSpec) -> i64 {
    if spec.canonical_max != spec.max && value == spec.max {
        spec.min
    } else {
        value
    }
}

/// Encodes one matched value set as the shortest equivalent
/// comma-separated field (dsh `encodeCronField`; never a leading star).
fn encode_cron_field(values: &[i64]) -> String {
    let first = values[0];
    if values.len() == 1 {
        return first.to_string();
    }
    let last = values[values.len() - 1];
    let step = values[1] - first;
    let uniform = values.windows(2).all(|pair| pair[1] - pair[0] == step);
    if uniform {
        return if step == 1 {
            format!("{first}-{last}")
        } else {
            format!("{first}-{last}/{step}")
        };
    }
    let mut parts: Vec<String> = Vec::new();
    let mut run_start = first;
    for index in 1..values.len() {
        let current = values[index];
        let previous = values[index - 1];
        if current == previous + 1 {
            continue;
        }
        parts.push(if run_start == previous {
            run_start.to_string()
        } else {
            format!("{run_start}-{previous}")
        });
        run_start = current;
    }
    parts.push(if run_start == last {
        run_start.to_string()
    } else {
        format!("{run_start}-{last}")
    });
    parts.join(",")
}

/// One uniform walk from a field's minimum, folded exactly as a parsed
/// field folds (dsh `starWalk`).
fn star_walk(spec: &CronFieldSpec, step: i64) -> Vec<i64> {
    let mut walked = std::collections::BTreeSet::new();
    let mut value = spec.min;
    while value <= spec.max {
        walked.insert(fold_value(value, spec));
        value += step;
    }
    walked.into_iter().collect()
}

/// Canonical spelling of a field whose text started with a star (dsh
/// `encodeStarCronField`): the bare star for every value, else the
/// widest star-step walk plus any remaining values as a list.
fn encode_star_cron_field(values: &[i64], spec: &CronFieldSpec) -> String {
    #[expect(
        clippy::cast_possible_wrap,
        reason = "the field bounds are at most 60 wide, far below i64"
    )]
    if values.len() as i64 == spec.canonical_max - spec.min + 1 {
        return "*".to_owned();
    }
    let present: std::collections::BTreeSet<i64> = values.iter().copied().collect();
    let mut best_step: Option<i64> = None;
    let mut best_walk: Vec<i64> = Vec::new();
    let steps = spec.max - spec.min + 1;
    for step in 1..=steps {
        let walk = star_walk(spec, step);
        if walk.len() > best_walk.len() && walk.iter().all(|value| present.contains(value)) {
            best_step = Some(step);
            best_walk = walk;
        }
    }
    let Some(best_step) = best_step else {
        return values
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
    };
    let walked: std::collections::BTreeSet<i64> = best_walk.iter().copied().collect();
    let remaining: Vec<i64> = values
        .iter()
        .copied()
        .filter(|value| !walked.contains(value))
        .collect();
    let walk_text = format!("*/{best_step}");
    if remaining.is_empty() {
        walk_text
    } else {
        format!("{walk_text},{}", encode_cron_field(&remaining))
    }
}

/// Parses one cron field into matched values, canonical spelling, and
/// star flag (dsh `parseCronField`).
///
/// # Errors
///
/// The [`cron_field_values`] refusals.
fn parse_cron_field(
    raw: &str,
    spec: &CronFieldSpec,
) -> Result<ParsedCronField, ScheduleInputError> {
    let values = cron_field_values(raw, spec)?;
    let star = raw.starts_with('*');
    let canonical = if star {
        encode_star_cron_field(&values, spec)
    } else {
        encode_cron_field(&values)
    };
    Ok(ParsedCronField {
        values,
        canonical,
        star,
    })
}

/// Parses and canonicalizes one strict five-field cron expression (dsh
/// `parseCronExpression`).
///
/// # Errors
///
/// Empty, untrimmed, wrong field count, or per-field refusals.
pub fn parse_cron_expression(expression: &str) -> Result<ParsedCronExpression, ScheduleInputError> {
    if expression.is_empty() || expression.trim() != expression {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            "cron.expression must be a non-empty trimmed string.",
        ));
    }
    let fields: Vec<&str> = expression.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            "cron.expression must contain exactly five whitespace-separated fields: minute hour day-of-month month day-of-week.",
        ));
    }
    let parsed = fields
        .iter()
        .zip(CRON_FIELDS.iter())
        .map(|(field, spec)| parse_cron_field(field, spec))
        .collect::<Result<Vec<_>, _>>()?;
    let [minute, hour, day_of_month, month, day_of_week] = parsed.as_slice() else {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            "cron.expression must contain exactly five whitespace-separated fields: minute hour day-of-month month day-of-week.",
        ));
    };
    Ok(ParsedCronExpression {
        expression: [minute, hour, day_of_month, month, day_of_week]
            .iter()
            .map(|field| field.canonical.clone())
            .collect::<Vec<_>>()
            .join(" "),
        minutes: minute.values.clone(),
        hours: hour.values.clone(),
        days_of_month: day_of_month.values.clone(),
        months: month.values.clone(),
        days_of_week: day_of_week.values.clone(),
        day_of_month_star: day_of_month.star,
        day_of_week_star: day_of_week.star,
    })
}

/// One latest-only recurring decision (dsh `RecurringOccurrence`).
#[derive(Debug)]
pub struct RecurringOccurrence {
    /// Latest occurrence due at the decision time.
    pub occurrence_at: String,
    /// First eligible target after the decision, or exhaustion.
    pub next_scheduled_at: Option<String>,
}

/// Reads one stored integer field the JS `Date.parse` way (the stored
/// grammar pins canonical instants, so the parse cannot fail on them).
fn instant_ms(value: &str) -> i64 {
    super::domain::decode_instant(value)
        .ok()
        .unwrap_or_else(|| text_instant_ms(value))
}

/// Fallback parse for decision inputs the storage decode already
/// admitted (a `Date.parse` stand-in for canonical strings only).
fn text_instant_ms(value: &str) -> i64 {
    super::domain::parse_offset_instant(value).expect("admitted instants parse")
}

/// Validates a safe-integer interval at the durable boundary.
fn interval_ms(every_seconds: i64) -> Result<i64, ScheduleLogError> {
    let interval = every_seconds.saturating_mul(1_000);
    if every_seconds < MIN_EVERY_INTERVAL_SECONDS || interval <= 0 {
        return Err(ScheduleLogError(format!(
            "everySeconds must be a safe integer of at least {MIN_EVERY_INTERVAL_SECONDS}"
        )));
    }
    Ok(interval)
}

/// Resolves one fixed-rate decision without enumerating missed
/// occurrences (dsh `resolveEveryOccurrence`).
///
/// # Errors
///
/// A decision preceding the committed target or a non-representable
/// next occurrence.
pub fn resolve_every_occurrence(
    every_seconds: i64,
    scheduled_at: &str,
    accepted_at: i64,
) -> Result<RecurringOccurrence, ScheduleLogError> {
    let target = instant_ms(scheduled_at);
    let interval = interval_ms(every_seconds)?;
    if !(MIN_FOUR_DIGIT_YEAR_MS..=MS_OF_MAX_FOUR_DIGIT_YEAR).contains(&accepted_at) {
        return Err(ScheduleLogError(
            "every acceptedAt must be a representable four-digit-year instant".to_owned(),
        ));
    }
    if accepted_at < target {
        return Err(ScheduleLogError(
            "every dispatch cannot precede the active scheduledAt".to_owned(),
        ));
    }
    let steps = (accepted_at - target) / interval;
    let occurrence = target + steps * interval;
    if occurrence < target || occurrence > accepted_at {
        return Err(ScheduleLogError(
            "every occurrence arithmetic must stay within the accepted interval".to_owned(),
        ));
    }
    let occurrence_at = canonical_instant(occurrence);
    let next = occurrence + interval;
    if next > MS_OF_MAX_FOUR_DIGIT_YEAR {
        return Ok(RecurringOccurrence {
            occurrence_at,
            next_scheduled_at: None,
        });
    }
    Ok(RecurringOccurrence {
        occurrence_at,
        next_scheduled_at: Some(canonical_instant(next)),
    })
}

/// Whether one local calendar date carries one of the rule's explicit
/// ISO weekdays (dsh `weekdaySet`).
fn weekday_match(weekdays: &[u8]) -> impl Fn(&Date) -> bool + '_ {
    move |date: &Date| {
        weekdays.contains(&u8::try_from(date.weekday().to_monday_one_offset()).unwrap_or(0))
    }
}

/// Finds the first actual occurrence after now and, when supplied, after
/// a delivered date (dsh `nextMatchingTarget`).
fn next_matching_target(
    time: &str,
    zone: &TimeZone,
    now: i64,
    matches: impl Fn(&Date) -> bool,
    after_date: Option<Date>,
) -> Option<String> {
    let mut date = local_date(now, zone);
    // The dsh bound walks one day past the ceiling date; that extra day
    // can only yield instants past the four-digit maximum, so the
    // inclusive ceiling is the same exhaustion.
    let last_date = local_date(MS_OF_MAX_FOUR_DIGIT_YEAR, &TimeZone::UTC);
    if let Some(after_date) = after_date
        && date <= after_date
    {
        date = after_date
            .checked_add(jiff::Span::new().days(1))
            .expect("the successor date exists");
    }
    while date <= last_date {
        if matches(&date) {
            let (hour, minute, second, millis) = time_parts(time);
            let local = date.at(hour, minute, second, millis);
            if let Some(target) = local_instant(&local, zone) {
                if target > MS_OF_MAX_FOUR_DIGIT_YEAR {
                    return None;
                }
                if target >= MIN_FOUR_DIGIT_YEAR_MS && target > now {
                    return Some(canonical_instant(target));
                }
            }
        }
        date = date
            .tomorrow()
            .expect("the walk stays inside the four-digit-year range");
    }
    None
}

/// Splits one normalized `HH:mm:ss.SSS` wall-clock string into its
/// fields (the stored grammar pins the shape).
fn time_parts(time: &str) -> (i8, i8, i8, i32) {
    let digits: Vec<&str> = time.split([':', '.']).collect();
    let field = |index: usize| -> i8 {
        digits
            .get(index)
            .and_then(|part| part.parse::<u8>().ok())
            .map_or(0, |value| i8::try_from(value).unwrap_or(0))
    };
    let millis = digits
        .get(3)
        .and_then(|part| part.parse::<i32>().ok())
        .unwrap_or(0);
    (field(0), field(1), field(2), millis)
}

/// Finds the latest actual occurrence at or before a decision without
/// leaving the committed floor (dsh `latestDueOccurrence`).
fn latest_due_occurrence(
    zone: &TimeZone,
    matches: impl Fn(&Date) -> bool,
    time: &str,
    accepted_at: i64,
    saved_target: i64,
) -> i64 {
    let mut date = local_date(accepted_at, &TimeZone::UTC)
        .tomorrow()
        .expect("the walk start stays inside the four-digit-year range");
    loop {
        if matches(&date) {
            let (hour, minute, second, millis) = time_parts(time);
            let local = date.at(hour, minute, second, millis);
            if let Some(candidate) = local_instant(&local, zone)
                && candidate <= accepted_at
            {
                // A committed target remains due even if current tzdata
                // places the rule before it.
                return candidate.max(saved_target);
            }
        }
        date = date
            .yesterday()
            .expect("the tzdata walk stays inside the four-digit-year range");
    }
}

/// Validates a wall-clock decision at a representable four-digit-year
/// instant (dsh `acceptedDecision`).
///
/// # Errors
///
/// Out of the four-digit-year range.
fn accepted_decision(selector: &str, accepted_at: i64) -> Result<i64, ScheduleLogError> {
    if !(MIN_FOUR_DIGIT_YEAR_MS..=MS_OF_MAX_FOUR_DIGIT_YEAR).contains(&accepted_at) {
        return Err(ScheduleLogError(format!(
            "{selector} acceptedAt must be a representable four-digit-year instant"
        )));
    }
    Ok(accepted_at)
}

/// Finds the first actual daily occurrence after now (dsh
/// `nextDailyTarget`).
pub fn next_daily_target(
    time: &str,
    zone: &TimeZone,
    now: i64,
    after_date: Option<Date>,
) -> Option<String> {
    next_matching_target(time, zone, now, |_| true, after_date)
}

/// Resolves a daily decision near the decision's local date (dsh
/// `resolveDailyOccurrence`).
///
/// # Errors
///
/// A decision preceding the committed target.
pub fn resolve_daily_occurrence(
    time: &str,
    time_zone: &str,
    scheduled_at: &str,
    accepted_at: i64,
) -> Result<RecurringOccurrence, ScheduleLogError> {
    let decision = accepted_decision("daily", accepted_at)?;
    let saved_target = instant_ms(scheduled_at);
    if decision < saved_target {
        return Err(ScheduleLogError(
            "daily dispatch cannot precede the active scheduledAt".to_owned(),
        ));
    }
    let zone = jiff::tz::db()
        .get(time_zone)
        .map_err(|error| ScheduleLogError(format!("daily zone resolution failed: {error}")))?;
    let occurrence = latest_due_occurrence(&zone, |_| true, time, decision, saved_target);
    let occurrence_at = canonical_instant(occurrence);
    let after = local_date(occurrence, &zone);
    let next_scheduled_at = next_daily_target(time, &zone, decision, Some(after));
    Ok(RecurringOccurrence {
        occurrence_at,
        next_scheduled_at,
    })
}

/// Finds the first actual weekly occurrence after now (dsh
/// `nextWeeklyTarget`).
pub fn next_weekly_target(
    time: &str,
    zone: &TimeZone,
    weekdays: &[u8],
    now: i64,
    after_date: Option<Date>,
) -> Option<String> {
    next_matching_target(time, zone, now, weekday_match(weekdays), after_date)
}

/// Resolves a weekly decision near the decision's local date (dsh
/// `resolveWeeklyOccurrence`).
///
/// # Errors
///
/// A decision preceding the committed target.
pub fn resolve_weekly_occurrence(
    time: &str,
    time_zone: &str,
    weekdays: &[u8],
    scheduled_at: &str,
    accepted_at: i64,
) -> Result<RecurringOccurrence, ScheduleLogError> {
    let decision = accepted_decision("weekly", accepted_at)?;
    let saved_target = instant_ms(scheduled_at);
    if decision < saved_target {
        return Err(ScheduleLogError(
            "weekly dispatch cannot precede the active scheduledAt".to_owned(),
        ));
    }
    let zone = jiff::tz::db()
        .get(time_zone)
        .map_err(|error| ScheduleLogError(format!("weekly zone resolution failed: {error}")))?;
    let occurrence =
        latest_due_occurrence(&zone, weekday_match(weekdays), time, decision, saved_target);
    let occurrence_at = canonical_instant(occurrence);
    let after = local_date(occurrence, &zone);
    let next_scheduled_at = next_weekly_target(time, &zone, weekdays, decision, Some(after));
    Ok(RecurringOccurrence {
        occurrence_at,
        next_scheduled_at,
    })
}

/// Builds the local-date predicate of one parsed cron rule under Vixie
/// day-of-month/day-of-week semantics (dsh `cronDateMatch`).
///
/// A star-led field keeps the star flag only; when either field text
/// starts with a star, a date must satisfy both fields; only when
/// neither starts with a star does either field matching suffice.
fn cron_date_match(parsed: &ParsedCronExpression) -> impl Fn(&Date) -> bool + '_ {
    let months = parsed.months.clone();
    let days_of_month = parsed.days_of_month.clone();
    let days_of_week = parsed.days_of_week.clone();
    let day_of_month_restricted = !parsed.day_of_month_star;
    let day_of_week_restricted = !parsed.day_of_week_star;
    move |date: &Date| {
        if !months.contains(&i64::from(date.month())) {
            return false;
        }
        let month_day_hit = days_of_month.contains(&i64::from(date.day()));
        let weekday_hit =
            days_of_week.contains(&i64::from(date.weekday().to_monday_one_offset() % 7));
        if day_of_month_restricted && day_of_week_restricted {
            return month_day_hit || weekday_hit;
        }
        month_day_hit && weekday_hit
    }
}

/// The parsed rule's local times of day in ascending wall-clock order
/// (dsh `cronTimes`; hour-major pairs).
fn cron_times(parsed: &ParsedCronExpression) -> Vec<(i8, i8)> {
    let mut times = Vec::new();
    for hour in &parsed.hours {
        for minute in &parsed.minutes {
            times.push((
                i8::try_from(*hour).unwrap_or(0),
                i8::try_from(*minute).unwrap_or(0),
            ));
        }
    }
    times
}

/// Finds the first strictly future cron occurrence without leaving the
/// four-digit UTC year range (dsh `nextCronTarget`).
pub fn next_cron_target(
    parsed: &ParsedCronExpression,
    zone: &TimeZone,
    floor: i64,
) -> Option<String> {
    let times = cron_times(parsed);
    let matches = cron_date_match(parsed);
    let months = &parsed.months;
    let first_date = local_date(floor, zone);
    let mut date = first_date;
    let floor_time = Timestamp::from_millisecond(floor)
        .expect("the floor is representable")
        .to_zoned(zone.clone())
        .time();
    let ceiling = local_date(MS_OF_MAX_FOUR_DIGIT_YEAR, &TimeZone::UTC);
    let horizon = first_date
        .checked_add(jiff::Span::new().years(CRON_SEARCH_HORIZON_YEARS))
        .expect("the horizon stays representable");
    let last_date = if horizon < ceiling { horizon } else { ceiling };
    while date <= last_date {
        if !months.contains(&i64::from(date.month())) {
            date = date
                .first_of_month()
                .checked_add(jiff::Span::new().months(1))
                .expect("the month jump stays representable");
            continue;
        }
        if matches(&date) {
            for (hour, minute) in &times {
                if date == first_date {
                    let candidate_time = jiff::civil::Time::new(*hour, *minute, 0, 0)
                        .expect("the parsed cron fields are in range");
                    if candidate_time < floor_time {
                        continue;
                    }
                }
                let local = date.at(*hour, *minute, 0, 0);
                if let Some(target) = local_instant(&local, zone) {
                    if target > MS_OF_MAX_FOUR_DIGIT_YEAR {
                        return None;
                    }
                    if target >= MIN_FOUR_DIGIT_YEAR_MS && target > floor {
                        return Some(canonical_instant(target));
                    }
                }
            }
        }
        date = date
            .tomorrow()
            .expect("the walk stays inside the four-digit-year range");
    }
    None
}

/// Finds the latest matching cron occurrence at or before a decision
/// without leaving the committed floor (dsh `latestCronOccurrence`).
fn latest_cron_occurrence(
    parsed: &ParsedCronExpression,
    zone: &TimeZone,
    decision: i64,
    saved_target: i64,
) -> i64 {
    let times = cron_times(parsed);
    let matches = cron_date_match(parsed);
    let months = &parsed.months;
    let mut date = local_date(decision, &TimeZone::UTC)
        .tomorrow()
        .expect("the walk start stays inside the four-digit-year range");
    let floor_date = local_date(MIN_FOUR_DIGIT_YEAR_MS, zone);
    let horizon = date
        .checked_sub(jiff::Span::new().years(CRON_SEARCH_HORIZON_YEARS))
        .expect("the horizon stays representable");
    let first_date = if horizon > floor_date {
        horizon
    } else {
        floor_date
    };
    while date >= first_date {
        if !months.contains(&i64::from(date.month())) {
            date = date
                .first_of_month()
                .yesterday()
                .expect("the month jump stays representable");
            continue;
        }
        if matches(&date) {
            let mut earliest: Option<i64> = None;
            for (hour, minute) in &times {
                let local = date.at(*hour, *minute, 0, 0);
                let candidate = local_instant(&local, zone);
                if earliest.is_none() {
                    earliest = candidate;
                }
            }
            if let Some(earliest) = earliest
                && earliest > decision
            {
                date = date.yesterday().expect("the walk stays inside the range");
                continue;
            }
            for (hour, minute) in times.iter().rev() {
                let local = date.at(*hour, *minute, 0, 0);
                if let Some(candidate) = local_instant(&local, zone) {
                    if candidate > decision || candidate < MIN_FOUR_DIGIT_YEAR_MS {
                        continue;
                    }
                    // A committed target remains due even if current
                    // tzdata places the rule before it.
                    return candidate.max(saved_target);
                }
            }
        }
        date = date
            .yesterday()
            .expect("the tzdata walk stays inside the range");
    }
    saved_target
}

/// Resolves a cron decision near the decision's local date (dsh
/// `resolveCronOccurrence`).
///
/// # Errors
///
/// A decision preceding the committed target.
pub fn resolve_cron_occurrence(
    expression: &str,
    time_zone: &str,
    scheduled_at: &str,
    accepted_at: i64,
) -> Result<RecurringOccurrence, ScheduleLogError> {
    let decision = accepted_decision("cron", accepted_at)?;
    let saved_target = instant_ms(scheduled_at);
    if decision < saved_target {
        return Err(ScheduleLogError(
            "cron dispatch cannot precede the active scheduledAt".to_owned(),
        ));
    }
    let parsed =
        parse_cron_expression(expression).map_err(|error| ScheduleLogError(error.message))?;
    let zone = jiff::tz::db()
        .get(time_zone)
        .map_err(|error| ScheduleLogError(format!("cron zone resolution failed: {error}")))?;
    let occurrence = latest_cron_occurrence(&parsed, &zone, decision, saved_target);
    let occurrence_at = canonical_instant(occurrence);
    let next_scheduled_at = next_cron_target(&parsed, &zone, decision);
    Ok(RecurringOccurrence {
        occurrence_at,
        next_scheduled_at,
    })
}

/// Resolves one due recurring Host record with its rule-specific
/// arithmetic (dsh `resolveRecurringOccurrence`).
///
/// # Errors
///
/// The per-rule resolvers' durable refusals.
pub fn resolve_recurring_occurrence(
    record: &ScheduleRecord,
    accepted_at: i64,
) -> Result<RecurringOccurrence, ScheduleLogError> {
    match record {
        ScheduleRecord::Every {
            every_seconds,
            scheduled_at,
            ..
        } => resolve_every_occurrence(*every_seconds, scheduled_at, accepted_at),
        ScheduleRecord::Daily {
            time,
            time_zone,
            scheduled_at,
            ..
        } => resolve_daily_occurrence(time, time_zone, scheduled_at, accepted_at),
        ScheduleRecord::Weekly {
            time,
            time_zone,
            weekdays,
            scheduled_at,
            ..
        } => resolve_weekly_occurrence(time, time_zone, weekdays, scheduled_at, accepted_at),
        ScheduleRecord::Cron {
            expression,
            time_zone,
            scheduled_at,
            ..
        } => resolve_cron_occurrence(expression, time_zone, scheduled_at, accepted_at),
        ScheduleRecord::After { .. } | ScheduleRecord::At { .. } => Err(ScheduleLogError(
            "recurring resolution requires a recurring rule".to_owned(),
        )),
    }
}

/// Requires exactly the named durable keys and decodes the title first,
/// so a record without the key reports the title diagnostic (dsh
/// `decodeRecordTitle`).
fn decode_record_title(
    value: &Value,
    expected: &[&str],
    message: &str,
) -> Result<String, ScheduleLogError> {
    let title = decode_stored_title(value.get("title").unwrap_or(&Value::Null))?;
    if !has_exact_keys(value, expected) {
        return Err(ScheduleLogError(message.to_owned()));
    }
    Ok(title)
}

/// Validates one string field as non-empty and already trimmed (dsh
/// `decodeStoredTitle`'s prompt sibling).
fn decode_prompt(value: &Value, kind: &str) -> Result<String, ScheduleLogError> {
    let text = value.as_str().unwrap_or("");
    if text.is_empty() || text.trim() != text {
        return Err(ScheduleLogError(format!(
            "{kind} prompt must be non-empty and already trimmed"
        )));
    }
    Ok(text.to_owned())
}

/// Validates one stored string-pair field (dsh `time and timeZone must
/// be strings` family).
fn decode_string_pair<'a>(
    value: &'a Value,
    first: &str,
    second: &str,
    kind: &str,
) -> Result<(&'a str, &'a str), ScheduleLogError> {
    let (Some(first), Some(second)) = (
        value.get(first).and_then(Value::as_str),
        value.get(second).and_then(Value::as_str),
    ) else {
        return Err(ScheduleLogError(format!("{kind} fields must be strings")));
    };
    Ok((first, second))
}

/// Decodes a Host daily rule without re-resolving its committed target
/// (dsh `decodeDailyRecord`).
fn decode_daily_record(value: &Value) -> Result<ScheduleRecord, ScheduleLogError> {
    let title = decode_record_title(
        value,
        &[
            "id",
            "kind",
            "title",
            "prompt",
            "time",
            "timeZone",
            "scheduledAt",
        ],
        "daily schedule must contain exactly id, kind, title, prompt, time, timeZone, and scheduledAt",
    )?;
    let prompt = decode_prompt(value.get("prompt").unwrap_or(&Value::Null), "daily")?;
    let (time, time_zone) = decode_string_pair(value, "time", "timeZone", "daily")?;
    local_clock_time(time, "daily").map_err(|error| ScheduleLogError(error.message))?;
    canonicalize_time_zone_name(time_zone).map_err(|error| ScheduleLogError(error.message))?;
    let instant = decode_instant(
        value
            .get("scheduledAt")
            .and_then(Value::as_str)
            .unwrap_or(""),
    )?;
    Ok(ScheduleRecord::Daily {
        id: decode_id(value.get("id").unwrap_or(&Value::Null))?,
        title,
        prompt,
        time: time.to_owned(),
        time_zone: time_zone.to_owned(),
        scheduled_at: canonical_instant(instant),
    })
}

/// Decodes a Host weekly rule without re-resolving its committed target
/// (dsh `decodeWeeklyRecord`).
fn decode_weekly_record(value: &Value) -> Result<ScheduleRecord, ScheduleLogError> {
    let title = decode_record_title(
        value,
        &[
            "id",
            "kind",
            "title",
            "prompt",
            "time",
            "timeZone",
            "weekdays",
            "scheduledAt",
        ],
        "weekly schedule must contain exactly id, kind, title, prompt, time, timeZone, weekdays, and scheduledAt",
    )?;
    let prompt = decode_prompt(value.get("prompt").unwrap_or(&Value::Null), "weekly")?;
    let (time, time_zone) = decode_string_pair(value, "time", "timeZone", "weekly")?;
    canonicalize_time_zone_name(time_zone).map_err(|error| ScheduleLogError(error.message))?;
    local_clock_time(time, "weekly").map_err(|error| ScheduleLogError(error.message))?;
    let stored = value.get("weekdays").cloned().unwrap_or(Value::Null);
    let weekdays = normalize_weekdays(&stored).map_err(|error| ScheduleLogError(error.message))?;
    if weekdays.iter().enumerate().any(|(index, weekday)| {
        stored.get(index).and_then(Value::as_i64) != Some(i64::from(*weekday))
    }) {
        return Err(ScheduleLogError(
            "weekly weekdays must be normalized to unique ascending ISO weekday numbers".to_owned(),
        ));
    }
    let instant = decode_instant(
        value
            .get("scheduledAt")
            .and_then(Value::as_str)
            .unwrap_or(""),
    )?;
    Ok(ScheduleRecord::Weekly {
        id: decode_id(value.get("id").unwrap_or(&Value::Null))?,
        title,
        prompt,
        time: time.to_owned(),
        time_zone: time_zone.to_owned(),
        weekdays,
        scheduled_at: canonical_instant(instant),
    })
}

/// Decodes a Host cron rule without re-resolving its committed target
/// (dsh `decodeCronRecord`).
fn decode_cron_record(value: &Value) -> Result<ScheduleRecord, ScheduleLogError> {
    let title = decode_record_title(
        value,
        &[
            "id",
            "kind",
            "title",
            "prompt",
            "expression",
            "timeZone",
            "scheduledAt",
        ],
        "cron schedule must contain exactly id, kind, title, prompt, expression, timeZone, and scheduledAt",
    )?;
    let prompt = decode_prompt(value.get("prompt").unwrap_or(&Value::Null), "cron")?;
    let (expression, time_zone) = decode_string_pair(value, "expression", "timeZone", "cron")?;
    canonicalize_time_zone_name(time_zone).map_err(|error| ScheduleLogError(error.message))?;
    let canonical = parse_cron_expression(expression)
        .map_err(|error| ScheduleLogError(error.message))?
        .expression;
    if expression != canonical {
        return Err(ScheduleLogError(
            "cron expression must be canonical".to_owned(),
        ));
    }
    let instant = decode_instant(
        value
            .get("scheduledAt")
            .and_then(Value::as_str)
            .unwrap_or(""),
    )?;
    Ok(ScheduleRecord::Cron {
        id: decode_id(value.get("id").unwrap_or(&Value::Null))?,
        title,
        prompt,
        expression: expression.to_owned(),
        time_zone: time_zone.to_owned(),
        scheduled_at: canonical_instant(instant),
    })
}

/// Decodes one legacy one-shot shape, allowing the historical absent
/// title (dsh `decodeLegacyScheduleRecord` arms).
#[expect(
    clippy::too_many_lines,
    reason = "the legacy decode mirrors the dsh arms one kind per branch; splitting it would hide the historical shapes"
)]
fn decode_legacy_record(value: &Value) -> Result<Option<ScheduleRecord>, ScheduleLogError> {
    if !value.is_object() {
        return Err(ScheduleLogError(
            "schedule record must be an object".to_owned(),
        ));
    }
    let kind = value.get("kind").and_then(Value::as_str);
    let keys = |required: &[&str]| -> Result<(), ScheduleLogError> {
        let allowed: Vec<&str> = required.iter().copied().chain(["title"]).collect();
        let object = value.as_object();
        let ok = object.is_some_and(|object| {
            required.iter().all(|key| object.contains_key(*key))
                && object.keys().all(|key| allowed.contains(&key.as_str()))
        });
        if ok {
            Ok(())
        } else {
            Err(ScheduleLogError(format!(
                "v1 schedule record must contain exactly {} plus an optional title",
                required.join(", ")
            )))
        }
    };
    let title = match value.get("title") {
        None | Some(Value::Null) => None,
        Some(present) => Some(decode_stored_title(present)?),
    };
    let record = match kind {
        Some("after") => {
            keys(&["id", "kind", "prompt", "afterSeconds", "scheduledAt"])?;
            let after_seconds = value.get("afterSeconds").and_then(Value::as_i64);
            if !value
                .get("afterSeconds")
                .and_then(Value::as_f64)
                .is_some_and(|value| value.fract() == 0.0 && value > 0.0)
            {
                return Err(ScheduleLogError(
                    "afterSeconds must be a positive safe integer".to_owned(),
                ));
            }
            ScheduleRecord::After {
                id: decode_id(value.get("id").unwrap_or(&Value::Null))?,
                title: title.unwrap_or_default(),
                prompt: decode_prompt(value.get("prompt").unwrap_or(&Value::Null), "after")?,
                after_seconds: after_seconds.unwrap_or_default(),
                scheduled_at: canonical_instant(decode_instant(
                    value
                        .get("scheduledAt")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                )?),
            }
        }
        Some("at") => {
            keys(&["id", "kind", "prompt", "scheduledAt"])?;
            ScheduleRecord::At {
                id: decode_id(value.get("id").unwrap_or(&Value::Null))?,
                title: title.unwrap_or_default(),
                prompt: decode_prompt(value.get("prompt").unwrap_or(&Value::Null), "at")?,
                scheduled_at: canonical_instant(decode_instant(
                    value
                        .get("scheduledAt")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                )?),
            }
        }
        Some("every") => {
            keys(&["id", "kind", "prompt", "everySeconds", "scheduledAt"])?;
            let every_seconds = value.get("everySeconds").and_then(Value::as_i64);
            if !value
                .get("everySeconds")
                .and_then(Value::as_f64)
                .is_some_and(|value| value.fract() == 0.0)
            {
                return Err(ScheduleLogError(format!(
                    "everySeconds must be a safe integer of at least {MIN_EVERY_INTERVAL_SECONDS}"
                )));
            }
            let Some(every_seconds) = every_seconds else {
                return Err(ScheduleLogError(format!(
                    "everySeconds must be a safe integer of at least {MIN_EVERY_INTERVAL_SECONDS}"
                )));
            };
            if every_seconds < MIN_EVERY_INTERVAL_SECONDS {
                return Err(ScheduleLogError(format!(
                    "everySeconds must be a safe integer of at least {MIN_EVERY_INTERVAL_SECONDS}"
                )));
            }
            ScheduleRecord::Every {
                id: decode_id(value.get("id").unwrap_or(&Value::Null))?,
                title: title.unwrap_or_default(),
                prompt: decode_prompt(value.get("prompt").unwrap_or(&Value::Null), "every")?,
                every_seconds,
                scheduled_at: canonical_instant(decode_instant(
                    value
                        .get("scheduledAt")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                )?),
            }
        }
        _ => {
            return Err(ScheduleLogError(
                "v1 schedule kind must be \"after\", \"at\", or \"every\"".to_owned(),
            ));
        }
    };
    Ok(Some(record))
}

/// Decodes a current Host task record, preserving its committed target
/// and stored zone spelling (dsh `decodeScheduleRecord`).
///
/// # Errors
///
/// Malformed fields (the title diagnostic precedes the key-set
/// diagnostic).
pub fn decode_schedule_record(value: &Value) -> Result<ScheduleRecord, ScheduleLogError> {
    if value.is_object() {
        match value.get("kind").and_then(Value::as_str) {
            Some("daily") if value.is_object() => return decode_daily_record(value),
            Some("weekly") if value.is_object() => return decode_weekly_record(value),
            Some("cron") if value.is_object() => return decode_cron_record(value),
            _ => {}
        }
    }
    let record = decode_legacy_record(value)?;
    if let Some(record) = record {
        if record.title().is_empty() {
            return Err(ScheduleLogError(REQUIRED_TITLE_MESSAGE.to_owned()));
        }
        return Ok(record);
    }
    Err(ScheduleLogError(
        "schedule record must be an object".to_owned(),
    ))
}

/// Milliseconds in one day, the unit the retention window is stated in.
const DAY_MS: i64 = 86_400_000;

/// One durable delivery acknowledgment (dsh `ScheduleDeliveryReceipt`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryReceipt {
    /// Canonical UTC target of the delivered occurrence.
    pub scheduled_at: String,
    /// Canonical UTC time sampled after persistence acknowledged.
    pub delivered_at: String,
    /// Identity of the delivered message (shared by a recurring batch).
    pub message_id: String,
}

/// One retained delivery with its immutable sent prompt (dsh
/// `ScheduleDeliveryRecord`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryRecord {
    /// The receipt fields.
    pub receipt: DeliveryReceipt,
    /// Prompt sent for this occurrence (unavailable for legacy
    /// receipts).
    pub prompt: Option<String>,
}

impl DeliveryRecord {
    /// The wire form (receipt keys first, then the optional prompt).
    pub fn to_value(&self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("scheduledAt".to_owned(), json!(self.receipt.scheduled_at));
        object.insert("deliveredAt".to_owned(), json!(self.receipt.delivered_at));
        object.insert("messageId".to_owned(), json!(self.receipt.message_id));
        if let Some(prompt) = &self.prompt {
            object.insert("prompt".to_owned(), json!(prompt));
        }
        Value::Object(object)
    }

    /// Decodes one stored receipt-or-record (the history rows admit both
    /// shapes; the prompt is optional).
    pub fn from_value(value: &Value) -> Result<Self, ScheduleLogError> {
        let object = value
            .as_object()
            .ok_or_else(|| ScheduleLogError("delivery record must be an object".to_owned()))?;
        for key in ["scheduledAt", "deliveredAt", "messageId"] {
            if !object.contains_key(key) {
                return Err(ScheduleLogError(
                    "delivery record must contain exactly scheduledAt, deliveredAt, and messageId plus an optional prompt".to_owned(),
                ));
            }
        }
        for (key, text) in [
            ("scheduledAt", "scheduledAt"),
            ("deliveredAt", "deliveredAt"),
        ] {
            let instant = value.get(key).and_then(Value::as_str).unwrap_or("");
            decode_instant(instant)
                .map_err(|error| ScheduleLogError(format!("{text}: {}", error.0)))?;
        }
        let message_id = value.get("messageId").and_then(Value::as_str).unwrap_or("");
        if message_id.is_empty() || message_id.trim() != message_id {
            return Err(ScheduleLogError(
                "messageId must be a non-empty string without surrounding whitespace".to_owned(),
            ));
        }
        if let Some(extra) = object.keys().find(|key| {
            !matches!(
                key.as_str(),
                "scheduledAt" | "deliveredAt" | "messageId" | "prompt"
            )
        }) {
            return Err(ScheduleLogError(format!(
                "delivery record carries an unexpected key {extra}"
            )));
        }
        Ok(Self {
            receipt: DeliveryReceipt {
                scheduled_at: value
                    .get("scheduledAt")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                delivered_at: value
                    .get("deliveredAt")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                message_id: message_id.to_owned(),
            },
            prompt: value
                .get("prompt")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }
}

/// The retained history shape one stored task carries (dsh
/// `deliveryHistory`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryHistory {
    /// Oldest-first retained deliveries.
    pub records: Vec<DeliveryRecord>,
    /// Whether records before the earliest retained one may exist.
    pub earlier_records_unavailable: bool,
    /// True only after an append removed saved records.
    pub earlier_records_pruned: bool,
}

impl DeliveryHistory {
    /// The wire form (dsh construction order).
    pub fn to_value(&self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert(
            "records".to_owned(),
            json!(
                self.records
                    .iter()
                    .map(DeliveryRecord::to_value)
                    .collect::<Vec<_>>()
            ),
        );
        object.insert(
            "earlierRecordsUnavailable".to_owned(),
            json!(self.earlier_records_unavailable),
        );
        if self.earlier_records_pruned {
            object.insert("earlierRecordsPruned".to_owned(), json!(true));
        }
        Value::Object(object)
    }
}

/// Prepares one real inbox acknowledgment for the same task write as its
/// status and target (dsh `appendDelivery`; prunes to the configured
/// window and record cap on the only write path).
pub fn append_delivery(
    task_prompt: &str,
    history: Option<&DeliveryHistory>,
    last_delivery: Option<&DeliveryReceipt>,
    receipt: DeliveryReceipt,
    bounds: (i64, i64),
) -> (DeliveryReceipt, DeliveryHistory) {
    let legacy = history.cloned().unwrap_or_else(|| DeliveryHistory {
        records: last_delivery.map_or_else(Vec::new, |receipt| {
            vec![DeliveryRecord {
                receipt: receipt.clone(),
                prompt: None,
            }]
        }),
        earlier_records_unavailable: true,
        earlier_records_pruned: false,
    });
    let mut appended = legacy.records;
    appended.push(DeliveryRecord {
        receipt: receipt.clone(),
        prompt: Some(task_prompt.to_owned()),
    });
    let floor = instant_ms(&receipt.delivered_at) - bounds.0 * DAY_MS;
    let mut retained: Vec<DeliveryRecord> = appended
        .iter()
        .filter(|record| instant_ms(&record.receipt.delivered_at) >= floor)
        .cloned()
        .collect();
    #[expect(
        clippy::cast_sign_loss,
        reason = "the retention cap is a validated positive config bound"
    )]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the retention cap is a validated positive config bound"
    )]
    let cap = bounds.1 as usize;
    if retained.len() > cap {
        retained = retained.split_off(retained.len() - cap);
    }
    let pruned = retained.len() != appended.len();
    (
        receipt,
        DeliveryHistory {
            earlier_records_unavailable: legacy.earlier_records_unavailable || pruned,
            earlier_records_pruned: legacy.earlier_records_pruned || pruned,
            records: retained,
        },
    )
}

/// Derives one execution-local management view (dsh `scheduleView`).
#[must_use]
pub fn schedule_view(record: &ScheduleRecord, now: i64) -> Value {
    let mut view = record.to_value().as_object().cloned().unwrap_or_default();
    let target = instant_ms(record.scheduled_at());
    view.insert(
        "state".to_owned(),
        json!(if now >= target {
            "overdue"
        } else {
            "scheduled"
        }),
    );
    view.insert("deliveryMode".to_owned(), json!("host"));
    Value::Object(view)
}

/// The fixed model-facing origin line shared by one-shot and recurring
/// reminder delivery (dsh `SCHEDULED_MESSAGE_FRAMING`).
const SCHEDULED_MESSAGE_FRAMING: &str = "This is a scheduled message from the user";

/// Renders the fixed model framing for a due one-shot reminder (dsh
/// `renderReminderFraming`).
#[must_use]
pub fn render_reminder_framing(record: &ScheduleRecord) -> String {
    [
        "[SCHEDULE REMINDER]".to_owned(),
        SCHEDULED_MESSAGE_FRAMING.to_owned(),
        format!("schedule_id_json: {}", json!(record.id())),
        format!("occurrence_at: {}", record.scheduled_at()),
        format!("reminder_prompt_json: {}", json!(record.prompt())),
    ]
    .join("\n")
}

/// Renders one recurring reminder batch in the supplied order (dsh
/// `renderRecurringReminderBatchFraming`).
#[must_use]
pub fn render_recurring_reminder_batch_framing(reminders: &[(&ScheduleRecord, &str)]) -> String {
    let payload: Vec<Value> = reminders
        .iter()
        .map(|(record, occurrence_at)| {
            json!({
                "schedule_id": record.id(),
                "occurrence_at": occurrence_at,
                "reminder_prompt": record.prompt(),
            })
        })
        .collect();
    [
        "[SCHEDULE REMINDER BATCH]".to_owned(),
        SCHEDULED_MESSAGE_FRAMING.to_owned(),
        format!("reminders_json: {}", json!(payload)),
    ]
    .join("\n")
}

/// Validates one supplied fixed-rate interval at the tool surface (dsh
/// `invalidInterval`).
pub fn invalid_interval(every_seconds: Option<i64>) -> Result<(), ScheduleInputError> {
    let Some(every_seconds) = every_seconds else {
        return Ok(());
    };
    if every_seconds < MIN_EVERY_INTERVAL_SECONDS {
        return Err(ScheduleInputError::new(
            "frequency_too_high",
            format!("every_seconds must be at least {MIN_EVERY_INTERVAL_SECONDS}."),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cron dialect pins the canonical encodings (steps, ranges,
    /// lists, the Sunday fold, the star-flag preservation) and the
    /// Vixie DOM/DOW selection (dsh `parseCronExpression`).
    #[test]
    fn the_cron_dialect_pins_the_canonical_forms() {
        let parsed = parse_cron_expression("*/15 9-17 * * 1-5").expect("the example parses");
        assert_eq!(parsed.expression, "*/15 9-17 * * 1-5");
        assert_eq!(parsed.minutes, (0..=59).step_by(15).collect::<Vec<_>>());
        assert_eq!(parsed.hours, (9..=17).collect::<Vec<_>>());
        assert!(parsed.day_of_month_star);
        assert!(!parsed.day_of_week_star);
        assert_eq!(
            parse_cron_expression("0 0 * * 7")
                .expect("sunday")
                .expression,
            "0 0 * * 0"
        );
        // A uniform value set encodes as the range-step form (the dsh
        // `encodeCronField` shortest-spelling rule).
        assert_eq!(
            parse_cron_expression("5,35 2 * * 1,3,5")
                .expect("list")
                .expression,
            "5-35/30 2 * * 1-5/2"
        );
        // A non-uniform set keeps the comma list.
        assert_eq!(
            parse_cron_expression("5,37 2 * * 1,4,5")
                .expect("ragged")
                .expression,
            "5-37/32 2 * * 1,4-5"
        );
        assert_eq!(
            parse_cron_expression("0 12 1 */2 *")
                .expect("stepped month")
                .expression,
            "0 12 1 */2 *"
        );
        // A star-led restricted field keeps its star spelling; a plain
        // range never grows one.
        assert_eq!(
            parse_cron_expression("*/2 * * * *")
                .expect("stepped star")
                .expression,
            "*/2 * * * *"
        );
        assert_eq!(
            parse_cron_expression("1-31/2 * * * *")
                .expect("plain range step")
                .expression,
            "1-31/2 * * * *"
        );
        assert_eq!(
            parse_cron_expression("* * * * 8").unwrap_err().message,
            "cron.expression day-of-week field value 8 is outside 0-7.",
        );
        assert_eq!(
            parse_cron_expression("0 12 * *").unwrap_err().message,
            "cron.expression must contain exactly five whitespace-separated fields: minute hour day-of-month month day-of-week.",
        );
        // Both-restricted days OR; either-star days AND (the Vixie
        // DOM_STAR/DOW_STAR semantics).
        let both = parse_cron_expression("0 12 1 * 1").expect("both restricted");
        let date_both = "2030-06-03".parse::<Date>().expect("a Monday June 3");
        let date_dom = "2030-06-01".parse::<Date>().expect("a Saturday June 1");
        assert!(cron_date_match(&both)(&date_both));
        assert!(cron_date_match(&both)(&date_dom));
        let star_dom = parse_cron_expression("0 12 * * 1").expect("DOM star");
        assert!(cron_date_match(&star_dom)(&date_both));
        assert!(!cron_date_match(&star_dom)(&date_dom));
    }

    /// The fixed-rate resolver skips the missed backlog arithmetically
    /// and exhausts at the four-digit ceiling (dsh
    /// `resolveEveryOccurrence`).
    #[test]
    fn the_every_resolver_skips_the_backlog_and_exhausts() {
        let anchor = "2030-01-01T00:00:00.000Z";
        let target = decode_instant(anchor).expect("the anchor decodes");
        let accepted = target + 90 * 60_000 + 30_000;
        let occurrence = resolve_every_occurrence(3_600, anchor, accepted).expect("resolves");
        // steps = floor((accepted-target)/interval): the +90m30s
        // reading lands on the first tick past the anchor.
        assert_eq!(occurrence.occurrence_at, "2030-01-01T01:00:00.000Z");
        assert_eq!(
            occurrence.next_scheduled_at.expect("next"),
            "2030-01-01T02:00:00.000Z"
        );
        let before = resolve_every_occurrence(3_600, anchor, target - 1).unwrap_err();
        assert_eq!(
            before.0,
            "every dispatch cannot precede the active scheduledAt"
        );
        let at_ceiling = MS_OF_MAX_FOUR_DIGIT_YEAR;
        let exhausted = resolve_every_occurrence(3_600, anchor, at_ceiling).expect("resolves");
        assert_eq!(exhausted.occurrence_at, "9999-12-31T23:00:00.000Z");
        assert_eq!(exhausted.next_scheduled_at, None);
    }

    /// The daily resolver finds the latest due local occurrence near the
    /// decision date and advances across gap dates (dsh
    /// `resolveDailyOccurrence`).
    #[test]
    fn the_daily_resolver_walks_the_local_calendar() {
        let time = "09:00:00.000";
        let zone = "America/New_York";
        let anchor = "2030-06-01T13:00:00.000Z";
        let decision = decode_instant(anchor).expect("the anchor decodes");
        let occurrence = resolve_daily_occurrence(time, zone, anchor, decision).expect("resolves");
        assert_eq!(occurrence.occurrence_at, "2030-06-01T13:00:00.000Z");
        assert_eq!(
            occurrence.next_scheduled_at.expect("next day"),
            "2030-06-02T13:00:00.000Z"
        );
    }

    /// The weekly resolver selects the next matching ISO weekday (dsh
    /// `resolveWeeklyOccurrence`).
    #[test]
    fn the_weekly_resolver_selects_the_next_weekday() {
        let time = "09:00:00.000";
        let zone = "UTC";
        // 2030-06-03 is a Monday; the next Wednesday is 2030-06-05.
        let anchor = "2030-06-03T09:00:00.000Z";
        let decision = decode_instant(anchor).expect("the anchor decodes");
        let occurrence =
            resolve_weekly_occurrence(time, zone, &[3], anchor, decision).expect("resolves");
        assert_eq!(occurrence.occurrence_at, "2030-06-03T09:00:00.000Z");
        assert_eq!(
            occurrence.next_scheduled_at.expect("next wednesday"),
            "2030-06-05T09:00:00.000Z"
        );
    }

    /// The cron resolver finds the latest due minute and the next
    /// strictly-future one (dsh `resolveCronOccurrence`).
    #[test]
    fn the_cron_resolver_walks_to_the_next_minute() {
        let parsed_anchor = "2030-06-03T09:15:00.000Z";
        let decision = decode_instant(parsed_anchor).expect("the anchor decodes");
        let occurrence =
            resolve_cron_occurrence("*/15 9-17 * * 1-5", "UTC", parsed_anchor, decision)
                .expect("resolves");
        assert_eq!(occurrence.occurrence_at, "2030-06-03T09:15:00.000Z");
        assert_eq!(
            occurrence.next_scheduled_at.expect("next tick"),
            "2030-06-03T09:30:00.000Z"
        );
    }
}
