//! The human-facing `/compact` command presentation (dsh
//! `packages/compaction/command-compact` 1:1): the USAGE line, the two
//! success texts, and the classified-error text mapping.
//!
//! The mapping is the face the Phase-18 command shell consumes over the
//! `compaction` service's compact-now face. The dsh module also
//! registers the command and drains in-flight invocations at teardown
//! (the `commands` registry); the fr command framework is the Phase-18
//! entry assembly, so the registration effect and the raw-input gate
//! land there — only the compaction-owned text facts live here.

use crate::types::{ManualCompactionError, ManualCompactionErrorCode};

/// The command's argument refusal (the dsh `USAGE`).
pub const USAGE: &str = "Usage: /compact (no arguments)";

/// The no-range outcome text (the dsh `result === null` success).
pub const NO_COMPACTABLE: &str = "No compactable history yet.";

/// The classified failure text (the dsh `expectedFailure`): one fixed
/// presentation per closed union member.
///
/// The dsh `assertNever` backstop has no Rust face: the code enum is
/// exhaustive and the match covers every member without a wildcard.
#[must_use]
pub const fn expected_failure_text(code: &ManualCompactionErrorCode) -> &'static str {
    match code {
        ManualCompactionErrorCode::Busy => {
            "Compaction is unavailable because this process has an active compaction, \
             or the agent is not idle."
        }
        ManualCompactionErrorCode::Cancelled => "Compaction cancelled.",
        ManualCompactionErrorCode::Changed => {
            "The history selected for compaction changed before it could be replaced. \
             The attempt is recorded in the session log."
        }
        ManualCompactionErrorCode::Summary => {
            "Compaction could not produce a useful summary. \
             The attempt is recorded in the session log."
        }
        ManualCompactionErrorCode::Commit => {
            "Compaction did not finish cleanly; some session history may have changed. \
             Inspect the current session state before retrying."
        }
        ManualCompactionErrorCode::Persistence => {
            "Compaction finished, but the session could not be saved."
        }
    }
}

/// The classified failure's presentation (the dsh `expectedFailure`
/// call face).
#[must_use]
pub const fn expected_failure(error: &ManualCompactionError) -> &'static str {
    expected_failure_text(&error.code)
}

/// The success text (the dsh `` `Compacted ${n} history items (~${m}
/// tokens).` `` template).
#[must_use]
pub fn success_text(shadowed_count: u64, shadowed_token_count: u64) -> String {
    format!("Compacted {shadowed_count} history items (~{shadowed_token_count} tokens).")
}

#[cfg(test)]
mod tests {
    use super::{
        ManualCompactionErrorCode, NO_COMPACTABLE, USAGE, expected_failure_text, success_text,
    };
    use crate::types::ManualCompactionError;

    #[test]
    fn usage_and_no_compactable_pin_verbatim() {
        assert_eq!(USAGE, "Usage: /compact (no arguments)");
        assert_eq!(NO_COMPACTABLE, "No compactable history yet.");
    }

    /// Every closed-union member maps to its fixed dsh presentation.
    #[test]
    fn the_six_classified_texts_pin_verbatim() {
        assert_eq!(
            expected_failure_text(&ManualCompactionErrorCode::Busy),
            "Compaction is unavailable because this process has an active \
             compaction, or the agent is not idle."
        );
        assert_eq!(
            expected_failure_text(&ManualCompactionErrorCode::Cancelled),
            "Compaction cancelled."
        );
        assert_eq!(
            expected_failure_text(&ManualCompactionErrorCode::Changed),
            "The history selected for compaction changed before it could be \
             replaced. The attempt is recorded in the session log."
        );
        assert_eq!(
            expected_failure_text(&ManualCompactionErrorCode::Summary),
            "Compaction could not produce a useful summary. The attempt is \
             recorded in the session log."
        );
        assert_eq!(
            expected_failure_text(&ManualCompactionErrorCode::Commit),
            "Compaction did not finish cleanly; some session history may have \
             changed. Inspect the current session state before retrying."
        );
        assert_eq!(
            expected_failure_text(&ManualCompactionErrorCode::Persistence),
            "Compaction finished, but the session could not be saved."
        );
    }

    #[test]
    fn the_success_template_formats_the_counts() {
        let error = ManualCompactionError::new(
            ManualCompactionErrorCode::Busy,
            "manual compaction requires an idle agent with no waking queued work",
        );
        assert_eq!(
            super::expected_failure(&error),
            expected_failure_text(&ManualCompactionErrorCode::Busy)
        );
        assert_eq!(
            success_text(4, 240_000),
            "Compacted 4 history items (~240000 tokens)."
        );
    }
}
