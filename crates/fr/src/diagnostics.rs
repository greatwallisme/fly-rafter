//! The startup diagnostics face (启动诊断, PRD 18.1; dsh
//! startup-diagnostics.ts twin).
//!
//! A fatal boot error prints one stderr summary line + saves the full
//! report under `$FR_HOME/logs/` with the verbatim warning header, then
//! exits 1.

use serde_json::json;

/// The verbatim dsh warning header (headless spec decision line: the text
/// is 1:1).
const WARNING_HEADER: &str = "WARNING: Raw diagnostics may contain configuration or credential values from plugin errors. Review before sharing.";

/// The report body (the dsh inspect payload's fr form: nodeVersion has no
/// fr counterpart; the fr version, platform, and arch come from the
/// binary's own facts).
#[must_use]
pub fn diagnostic(fatal: &BootFatal, profile: &str) -> serde_json::Value {
    json!({
        "timestamp": iso_timestamp(),
        "frVersion": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "profile": profile,
        "error": fatal.detail,
    })
}

/// A fatal boot error: the stderr summary line and the detail body the
/// report carries.
#[derive(Debug, Clone)]
pub struct BootFatal {
    /// The one-line stderr summary (dsh `error.message` twin).
    pub summary: String,
    /// The full detail the report body carries.
    pub detail: String,
}

/// The Hinnant civil-from-days algorithm (the fr-llm-deepseek transport
/// twin): epoch day number → (year, month, day).
///
/// # Panics
///
/// Never: the civil algorithm bounds month to 1..=12 and day to 1..=31.
#[must_use]
pub const fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097).cast_unsigned();
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe.cast_signed() + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 14_600);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    // The civil algorithm bounds month to 1..=12 and day to 1..=31, so
    // both always fit `u32` (the Hinnant derivation, fr-llm-deepseek
    // transport.rs twin).
    #[expect(
        clippy::cast_possible_truncation,
        reason = "civil calendar month (1..=12) and day (1..=31) always fit u32"
    )]
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SS.mmmZ`, the dsh
/// `toISOString()` shape).
#[must_use]
pub fn iso_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs().cast_signed();
    let millis = now.subsec_millis();
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{h:02}:{min:02}:{s:02}.{millis:03}Z",
        h = tod / 3_600,
        min = (tod % 3_600) / 60,
        s = tod % 60
    )
}

/// Prints the stderr summary and saves the report under `home/logs/`.
///
/// dsh `reportStartupFailure` twin: single stderr line,
/// `startup-<iso-with-dashes>-<uuid>.log`, dir 0o700, file 0o600 flag
/// `wx`; a failed save prints the full report to stderr instead of
/// claiming a saved path.
///
/// # Panics
///
/// Panics only if `serde_json` cannot serialize the body (never for the
/// JSON value built here).
pub fn report(fatal: &BootFatal, home: &std::path::Path, profile: &str) {
    eprintln!("{}", fatal.summary);
    let body = serde_json::to_string_pretty(&diagnostic(fatal, profile))
        .expect("diagnostic bodies are JSON values");
    let report = format!("{WARNING_HEADER}\n\n{body}\n");
    let log_dir = home.join("logs");
    let stamp = iso_timestamp().replace(':', "-");
    let path = log_dir.join(format!("startup-{stamp}-{}.log", uuid::Uuid::new_v4()));
    let mut log_dir_mode = std::fs::DirBuilder::new();
    #[cfg(unix)]
    let log_dir_mode = {
        use std::os::unix::fs::DirBuilderExt;
        log_dir_mode.mode(0o700)
    };
    let created = log_dir_mode.recursive(true).create(&log_dir);
    if let Err(error) = created {
        full_to_stderr(&report, &error);
        return;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            use std::io::Write as _;
            match file.write_all(report.as_bytes()) {
                Ok(()) => eprintln!("\nFull diagnostics: {}", path.display()),
                Err(error) => full_to_stderr(&report, &error),
            }
        }
        Err(error) => full_to_stderr(&report, &error),
    }
}

fn full_to_stderr(report: &str, error: &std::io::Error) {
    eprintln!(
        "\nfr: warning: could not write startup diagnostics: {error}\nFull diagnostics:\n{report}"
    );
}
