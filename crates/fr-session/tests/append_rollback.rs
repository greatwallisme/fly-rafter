//! A write that hits `RLIMIT_FSIZE` mid-batch must surface the kernel error
//! and leave the log at its committed byte length: no half line (dsh
//! `rollbackAppend`, scenario 016).
//!
//! The limit is process-wide, so this runs as its own test binary instead
//! of racing the parallel in-crate suite.

use std::path::PathBuf;

use fr_session::persist::filename::log_path;
use fr_session::persist::io::{append_lines, materialize};
use fr_session::persist::{JsonlCompression, PersistError, SESSION_FORMAT_VERSION};
use fr_wire::event::{
    SessionEvent, SessionEventData, SessionHeader, StepBoundsData, ToolCallData, TurnStartData,
};
use fr_wire::ids::{SessionId, SessionSeq, ToolCallId};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fr-session-rlimit-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn chain() -> Vec<SessionEvent> {
    let event = |seq_value: u64, data| SessionEvent {
        seq: SessionSeq::new(seq_value).expect("seq"),
        time: 1_000 + i64::try_from(seq_value).expect("seq fits i64"),
        data,
        ignorable: None,
        surface_op: None,
        source_event_seqs: None,
    };
    vec![
        event(0, SessionEventData::TurnStart(TurnStartData { turn: 1 })),
        event(
            1,
            SessionEventData::StepStart(StepBoundsData { turn: 1, step: 1 }),
        ),
    ]
}

fn oversized_batch() -> Vec<SessionEvent> {
    vec![SessionEvent {
        seq: SessionSeq::new(2).expect("seq"),
        time: 1_002,
        data: SessionEventData::ToolCall(ToolCallData {
            turn: 1,
            step: 1,
            call_id: ToolCallId::new("c-1"),
            name: "bash".to_owned(),
            arguments: "x".repeat(64 * 1024),
        }),
        ignorable: None,
        surface_op: None,
        source_event_seqs: None,
    }]
}

#[test]
fn failed_append_rolls_back_to_committed_bytes() {
    // SAFETY: `libc::signal` only replaces this process's SIGXFSZ
    // disposition; it touches no memory the compiler cannot see.
    unsafe {
        libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
    }
    let compression = JsonlCompression::None;
    let root = TempDir::new("rollback");
    let id = SessionId::new("rlimit-1");
    let meta = SessionHeader {
        version: SESSION_FORMAT_VERSION,
        id: id.clone(),
        created_at: 1,
        cwd: None,
        parent_session: None,
        is_seeded: false,
        origin: None,
        delegation_depth: None,
        agent_preset: None,
    };
    materialize(root.path(), &meta, 0, &chain(), compression).expect("materialize");
    let path = log_path(root.path(), None, &id, compression).expect("path");
    let committed = std::fs::read(&path).expect("committed bytes");

    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a valid `rlimit` out-parameter owned by this
    // frame; `getrlimit` writes only through the provided pointer.
    unsafe {
        assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &raw mut limit), 0);
    }
    let original = limit;
    // The committed file stays under the limit; the next batch cannot.
    limit.rlim_cur = committed.len() as u64 + 1;
    // SAFETY: `limit` holds a complete `rlimit` value; `setrlimit` reads
    // only through the provided pointer.
    unsafe {
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &raw const limit), 0);
    }
    let error = append_lines(&path, &oversized_batch(), compression);
    // SAFETY: `original` holds the saved `rlimit`; `setrlimit` reads only
    // through the provided pointer.
    unsafe {
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &raw const original), 0);
    }
    let error = error.expect_err("EFBIG surfaces");
    assert!(
        matches!(error, PersistError::Io(_)),
        "kernel error: {error}"
    );
    assert!(
        error.to_string().to_lowercase().contains("file too large"),
        "error: {error}"
    );
    // The rollback restored the committed length: no half line.
    assert_eq!(std::fs::read(&path).expect("after rollback"), committed);
}
