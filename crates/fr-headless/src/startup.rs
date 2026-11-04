// Rust guideline compliant 2026-11-04
//! The one-shot app's command-line parse (the dsh `startup.ts` face over
//! commander v15 semantics, ported exactly for this command's option set).
//!
//! The face covers the task positional (variadic, joined by spaces),
//! `--session-id <id>`, `--json`, and `-h/--help`.
//!
//! Grammar errors (an unknown option, a missing option value) reject
//! before the action runs, and a raw `--json` scan decides whether such a
//! rejection still owes the caller the error event. All messages keep the
//! dsh wording verbatim (the invocation spellings brand-mapped); the
//! parse is pure — stdout, stdin, and exit stay with the caller.

/// What the runner row reads from the startup parse (the dsh
/// `HeadlessStartupValues` shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupValues {
    /// The task text this invocation asked for; absent when the runner
    /// reads stdin (the argument was omitted or is the bare `-`).
    pub task: Option<String>,
    /// Exact session identity to adopt; absent for a fresh random
    /// identity. Whitespace is part of the identity: emptiness is judged
    /// on the trimmed value but the runner receives the exact string.
    pub session_id: Option<String>,
    /// Whether stdout carries the machine-readable event stream instead of
    /// the final message.
    pub json: bool,
}

/// The outcome of parsing one invocation's argv slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupOutcome {
    /// `-h/--help`: the caller prints [`crate::help_text::HEADLESS_HELP`]
    /// to stdout and exits 0.
    Help,
    /// The task, session identity, and output mode.
    Values(StartupValues),
    /// A grammar or action error: exit 1. The message is the full stderr
    /// line (it already carries its `error: ` prefix, the dsh wording);
    /// `json_requested` selects the error-event face.
    Usage(StartupUsage),
}

/// One parse rejection and the face that reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupUsage {
    /// The full stderr message, verbatim dsh wording with its `error: `
    /// prefix.
    pub message: String,
    /// Whether the raw invocation asked for `--json` (the scan stops at
    /// `--` and skips a `--session-id` value, so a literal `--json` used as
    /// an option value or a positional never installs the JSON face).
    pub json_requested: bool,
}

/// The action's task-required error (the example spelling brand-mapped).
const TASK_REQUIRED: &str = "error: a task is required, for example: fr headless \"run the tests\"";

/// The bare `-` must be the only task argument.
const HYPHEN_ONLY: &str = "error: `-` must be the only task argument";

/// A blank `--session-id` value.
const SESSION_ID_EMPTY: &str = "error: --session-id requires a non-empty session id";

/// A missing option value (commander's `optionMissingArgument` wording
/// with this option's flags string).
const SESSION_ID_MISSING: &str = "error: option '--session-id <id>' argument missing";

/// The visible long flags, the suggestion candidates for unknown options.
const LONG_FLAGS: [&str; 3] = ["--json", "--session-id", "--help"];

/// Whether the raw invocation asks for the machine-readable stream: the
/// scan stops at `--` and skips a `--session-id` value.
fn json_requested(argv: &[String]) -> bool {
    let mut index = 0;
    while index < argv.len() {
        let argument = argv[index].as_str();
        if argument == "--" {
            return false;
        }
        if argument == "--json" {
            return true;
        }
        if argument == "--session-id" {
            index += 1;
        }
        index += 1;
    }
    false
}

/// The recognized options of this command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptionId {
    /// `--json`, a boolean flag.
    Json,
    /// `--session-id <id>`, a required-value option.
    SessionId,
    /// `-h, --help`, a boolean flag that reports immediately.
    Help,
}

/// Commander's exact option match (`Option.is`).
fn find_option(argument: &str) -> Option<OptionId> {
    match argument {
        "--json" => Some(OptionId::Json),
        "--session-id" => Some(OptionId::SessionId),
        "-h" | "--help" => Some(OptionId::Help),
        _ => None,
    }
}

/// Whether the argument is longer than one character and starts with a
/// dash (commander's `maybeOption`).
fn maybe_option(argument: &str) -> bool {
    argument.len() > 1 && argument.starts_with('-')
}

/// Whether the argument is a negative number (commander's
/// `negativeNumberArg`): `-`, then digits or `digits.`/`.digits` with a
/// mandatory fractional tail, then an optional decimal exponent. A
/// negative number is an operand because no `-<digit>` short option
/// exists on this command.
fn negative_number(argument: &str) -> bool {
    let Some(body) = argument.strip_prefix('-') else {
        return false;
    };
    let (mantissa, exponent) = match body.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (body, None),
    };
    if let Some(exponent) = exponent {
        let digits = exponent
            .strip_prefix('+')
            .or_else(|| exponent.strip_prefix('-'))
            .unwrap_or(exponent);
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
    }
    match mantissa.split_once('.') {
        // The dotted form needs a non-empty fractional tail (`\d*\.\d+`);
        // the whole part may be empty but only digits when present.
        Some((whole, fraction)) => {
            !fraction.is_empty()
                && fraction.chars().all(|c| c.is_ascii_digit())
                && whole.chars().all(|c| c.is_ascii_digit())
        }
        None => !mantissa.is_empty() && mantissa.chars().all(|c| c.is_ascii_digit()),
    }
}

/// Commander's `suggestSimilar`: Damerau-Levenshtein candidates within
/// three edits and a 0.4 similarity floor, only for `--`-prefixed flags.
fn suggest_similar(word: &str, candidates: &[&str]) -> String {
    const MAX_DISTANCE: usize = 3;
    const MIN_SIMILARITY: f64 = 0.4;
    if candidates.is_empty() {
        return String::new();
    }
    let searching_options = word.starts_with("--");
    let word = if searching_options {
        word.strip_prefix("--").unwrap_or(word)
    } else {
        word
    };
    let candidates: Vec<&str> = candidates
        .iter()
        .map(|candidate| {
            if searching_options {
                candidate.strip_prefix("--").unwrap_or(candidate)
            } else {
                *candidate
            }
        })
        .collect();
    let word_chars: Vec<char> = word.chars().collect();
    let mut similar: Vec<String> = Vec::new();
    let mut best_distance = MAX_DISTANCE;
    for candidate in &candidates {
        let candidate_chars: Vec<char> = candidate.chars().collect();
        if candidate_chars.len() <= 1 {
            continue;
        }
        let distance = edit_distance(&word_chars, &candidate_chars, MAX_DISTANCE);
        let length = word_chars.len().max(candidate_chars.len());
        // The lengths are flag words (a few bytes), far under the f64
        // mantissa; the cast is exact for every real candidate.
        #[expect(clippy::cast_precision_loss, reason = "flag-word lengths are tiny")]
        let similarity = (length - distance) as f64 / length as f64;
        if similarity <= MIN_SIMILARITY {
            continue;
        }
        if distance < best_distance {
            best_distance = distance;
            similar = vec![(*candidate).to_owned()];
        } else if distance == best_distance {
            similar.push((*candidate).to_owned());
        }
    }
    similar.sort();
    let similar: Vec<String> = similar
        .into_iter()
        .map(|candidate| format!("--{candidate}"))
        .collect();
    match similar.len() {
        0 => String::new(),
        1 => format!("\n(Did you mean {}?)", similar[0]),
        _ => format!("\n(Did you mean one of {}?)", similar.join(", ")),
    }
}

/// The Damerau-Levenshtein optimal string alignment distance (commander's
/// `editDistance`): no substring is edited more than once, with the same
/// early exit to the worst case.
fn edit_distance(a: &[char], b: &[char], max_distance: usize) -> usize {
    if a.len().abs_diff(b.len()) > max_distance {
        return a.len().max(b.len());
    }
    let mut matrix = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (row, cell) in matrix.iter_mut().enumerate() {
        cell[0] = row;
    }
    for (column, cell) in matrix[0].iter_mut().enumerate() {
        *cell = column;
    }
    for j in 1..=b.len() {
        for i in 1..=a.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut cell = (matrix[i - 1][j] + 1).min(matrix[i][j - 1] + 1);
            cell = cell.min(matrix[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                cell = cell.min(matrix[i - 2][j - 2] + 1);
            }
            matrix[i][j] = cell;
        }
    }
    matrix[a.len()][b.len()]
}

/// Commander's `unknownOption` wording.
///
/// The suggestion runs only for `--`-prefixed flags, over the visible long
/// flags.
fn unknown_option(flag: &str) -> String {
    let suggestion = if flag.starts_with("--") {
        suggest_similar(flag, &LONG_FLAGS)
    } else {
        String::new()
    };
    format!("error: unknown option '{flag}'{suggestion}")
}

/// Parses one invocation's argv slice (everything after the `headless`
/// token).
///
/// Grammar errors reject with the commander wording; the action checks
/// reject in the dsh order. Pure: stdin is a fact, output and exit stay
/// with the caller.
#[must_use]
pub fn parse(argv: &[String], stdin_is_tty: bool) -> StartupOutcome {
    let json = json_requested(argv);
    let usage = move |message: &str| {
        StartupOutcome::Usage(StartupUsage {
            message: message.to_owned(),
            json_requested: json,
        })
    };

    let mut operands: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    let mut session_id: Option<String> = None;
    let mut index = 0;
    // Commander reports an unknown option only after the loop, so a later
    // `-h/--help` wins over an earlier unknown flag; a missing option
    // value, though, rejects immediately inside the loop.
    while index < argv.len() {
        let argument = argv[index].as_str();
        index += 1;
        if argument == "--" {
            if unknown.is_empty() {
                operands.extend(argv[index..].iter().cloned());
            } else {
                // Once an unknown was seen, commander keeps collecting into
                // the unknown list (including the `--` itself).
                unknown.push(argument.to_owned());
                unknown.extend(argv[index..].iter().cloned());
            }
            break;
        }
        if maybe_option(argument) {
            match find_option(argument) {
                Some(OptionId::Json) => continue,
                // The help flag reports immediately, before any later
                // grammar error would fire.
                Some(OptionId::Help) => return StartupOutcome::Help,
                Some(OptionId::SessionId) => {
                    let value = argv.get(index);
                    let Some(value) = value else {
                        return usage(SESSION_ID_MISSING);
                    };
                    index += 1;
                    session_id = Some(value.clone());
                    continue;
                }
                None => {}
            }
            // A known long flag with an attached value: only the
            // required-value option accepts the `--flag=value` form.
            if argument.starts_with("--")
                && let Some((flag, value)) = argument.split_once('=')
                && flag == "--session-id"
                && !value.is_empty()
            {
                session_id = Some(value.to_owned());
                continue;
            }
            // A known short flag bundled with trailing characters (`-hx`):
            // the first character decides; booleans consume only themselves
            // and the rest re-enters the loop as a group.
            if argument.len() > 2 && !argument.starts_with("--") {
                let group = format!("-{}", argument.chars().nth(1).unwrap_or('-'));
                if matches!(find_option(&group), Some(OptionId::Help)) {
                    return StartupOutcome::Help;
                }
            }
            if negative_number(argument) {
                operands.push(argument.to_owned());
            } else if unknown.is_empty() {
                // The first unknown option moves the destination for the
                // rest of the invocation.
                unknown.push(argument.to_owned());
            } else {
                unknown.push(argument.to_owned());
            }
            continue;
        }
        operands.push(argument.to_owned());
    }

    if let Some(first) = unknown.first() {
        return usage(&unknown_option(first));
    }

    // The action checks, in the dsh order.
    if operands.len() > 1 && operands.iter().any(|word| word == "-") {
        return usage(HYPHEN_ONLY);
    }
    let joined = operands.join(" ");
    if !operands.is_empty() && joined.trim().is_empty() {
        return usage(TASK_REQUIRED);
    }
    let task = (!operands.is_empty()).then_some(joined);
    if task.is_none() && stdin_is_tty {
        return usage(TASK_REQUIRED);
    }
    if session_id.as_ref().is_some_and(|id| id.trim().is_empty()) {
        return usage(SESSION_ID_EMPTY);
    }
    StartupOutcome::Values(StartupValues {
        task,
        session_id,
        json,
    })
}

#[cfg(test)]
mod tests {
    use super::{StartupOutcome, parse};
    use crate::help_text::HEADLESS_HELP;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(ToString::to_string).collect()
    }

    fn run(words: &[&str], tty: bool) -> StartupOutcome {
        parse(&argv(words), tty)
    }

    #[test]
    fn help_text_constant_is_the_branded_block() {
        assert!(HEADLESS_HELP.starts_with("Usage: fr headless [options] [task...]\n"));
        assert!(HEADLESS_HELP.contains("echo \"run the tests\" | fr headless"));
        assert!(!HEADLESS_HELP.contains("dsh"));
    }

    #[test]
    fn task_words_join_and_json_defaults_off() {
        let StartupOutcome::Values(values) = run(&["run", "the", "tests"], false) else {
            panic!("values");
        };
        assert_eq!(values.task.as_deref(), Some("run the tests"));
        assert!(!values.json);
        assert_eq!(values.session_id, None);
    }

    #[test]
    fn bare_hyphen_is_the_stdin_marker() {
        let StartupOutcome::Values(values) = run(&["-"], false) else {
            panic!("values");
        };
        assert_eq!(values.task.as_deref(), Some("-"));
        // A hyphen among several words is a usage error.
        assert!(matches!(
            run(&["-", "extra"], false),
            StartupOutcome::Usage(_)
        ));
    }

    #[test]
    fn tty_usage_error_and_its_message() {
        let StartupOutcome::Usage(usage) = run(&[], true) else {
            panic!("usage");
        };
        assert_eq!(
            usage.message,
            "error: a task is required, for example: fr headless \"run the tests\""
        );
        assert!(!usage.json_requested);
        // Without a TTY the absence of a task is the stdin path.
        assert!(matches!(run(&[], false), StartupOutcome::Values(_)));
    }

    #[test]
    fn trim_empty_task_fails_and_json_scan_answers_the_raw_invocation() {
        let StartupOutcome::Usage(usage) = run(&["   "], false) else {
            panic!("usage");
        };
        assert!(usage.message.starts_with("error: a task is required"));
        let StartupOutcome::Usage(with_json) = run(&["--json", "   "], false) else {
            panic!("usage json");
        };
        assert!(with_json.json_requested);
    }

    #[test]
    fn session_id_keeps_whitespace_but_rejects_blank() {
        let StartupOutcome::Values(values) = run(&["--session-id", "  session-x  ", "go"], false)
        else {
            panic!("values");
        };
        assert_eq!(values.session_id.as_deref(), Some("  session-x  "));
        let StartupOutcome::Usage(blank) = run(&["--session-id", "   ", "go"], false) else {
            panic!("blank");
        };
        assert_eq!(
            blank.message,
            "error: --session-id requires a non-empty session id"
        );
    }

    #[test]
    fn missing_option_value_rejects_with_commander_wording() {
        let StartupOutcome::Usage(usage) = run(&["--session-id"], false) else {
            panic!("missing value");
        };
        assert_eq!(
            usage.message,
            "error: option '--session-id <id>' argument missing"
        );
    }

    #[test]
    fn unknown_long_option_suggests_a_close_flag() {
        let StartupOutcome::Usage(usage) = run(&["--jso", "go"], false) else {
            panic!("unknown");
        };
        assert_eq!(
            usage.message,
            "error: unknown option '--jso'\n(Did you mean --json?)"
        );
        // An unknown short option carries no suggestion.
        let StartupOutcome::Usage(short) = run(&["-x"], false) else {
            panic!("short");
        };
        assert_eq!(short.message, "error: unknown option '-x'");
    }

    #[test]
    fn boolean_flag_with_attached_value_is_unknown_but_raw_scan_stays_off() {
        let StartupOutcome::Usage(usage) = run(&["--json=x"], false) else {
            panic!("attached");
        };
        assert!(usage.message.contains("unknown option '--json=x'"));
        assert!(!usage.json_requested);
    }

    #[test]
    fn help_wins_over_earlier_unknown_flags_but_not_over_missing_values() {
        assert_eq!(run(&["--unknown", "--help"], false), StartupOutcome::Help);
        assert_eq!(run(&["--help"], false), StartupOutcome::Help);
        // A required-value option consumes even a flag-looking token
        // verbatim (commander's required-value path), so this is a value,
        // not a help request.
        let StartupOutcome::Values(values) = run(&["--session-id", "--help"], false) else {
            panic!("value verbatim");
        };
        assert_eq!(values.session_id.as_deref(), Some("--help"));
    }

    #[test]
    fn double_dash_turns_the_rest_into_operands() {
        let StartupOutcome::Values(values) = run(&["--", "--json", "-5"], false) else {
            panic!("values");
        };
        assert_eq!(values.task.as_deref(), Some("--json -5"));
        assert!(!values.json);
    }

    #[test]
    fn attached_value_form_and_json_scan_skip() {
        let StartupOutcome::Values(values) = run(&["--session-id=session-x", "go"], false) else {
            panic!("attached value");
        };
        assert_eq!(values.session_id.as_deref(), Some("session-x"));
        // A `--json` consumed as the option value is skipped by the raw
        // scan: no JSON face installs.
        let StartupOutcome::Values(skipped) = run(&["--session-id", "--json", "go"], false) else {
            panic!("value skip");
        };
        assert_eq!(skipped.session_id.as_deref(), Some("--json"));
        assert!(!skipped.json);
    }

    #[test]
    fn negative_numbers_are_operands() {
        for word in ["-5", "-0.5", "-5.5e-3", "-12E+4"] {
            let StartupOutcome::Values(values) = run(&[word], false) else {
                panic!("{word} must be an operand");
            };
            assert_eq!(values.task.as_deref(), Some(word));
        }
        // Non-numbers stay unknown options.
        assert!(matches!(run(&["-5x"], false), StartupOutcome::Usage(_)));
    }

    #[test]
    fn json_scan_stops_at_double_dash() {
        let StartupOutcome::Values(values) = run(&["--", "--json"], false) else {
            panic!("values");
        };
        assert!(!values.json);
    }
}
