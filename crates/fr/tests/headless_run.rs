//! The process-level headless run e2e (the Q3A ruling): the real `fr`
//! binary over real cdylib artifacts, driven by a scripted local Messages
//! SSE server plus a profile patch that points the deepseek adapter's
//! base-url at it. Zero fixture crates; missing artifacts fail loud with
//! the build instruction (the 18a Q1A judgment).

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// Every first-party plugin crate the composition resolves to a real
/// cdylib artifact (the 18a list).
const REQUIRED_ARTIFACTS: &[&str] = &[
    "fr-net",
    "fr-attachment",
    "fr-storage",
    "fr-llm",
    "fr-token-meter",
    "fr-guard-timeout",
    "fr-guard-repeat-reminder",
    "fr-session",
    "fr-system-prompt",
    "fr-tools",
    "fr-shell",
    "fr-sandbox",
    "fr-permission-presets",
    "fr-approval",
    "fr-compaction",
    "fr-tools-builtin",
    "fr-agent-loop",
    "fr-llm-deepseek",
    "fr-headless",
];

/// The artifact search dirs (the assembly resolver twin).
fn artifact_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(paths) = std::env::var_os("FR_PLUGIN_PATH") {
        dirs.extend(std::env::split_paths(&paths));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dirs.push(dir.to_path_buf());
        if let Some(parent) = dir.parent() {
            dirs.push(parent.to_path_buf());
        }
    }
    dirs
}

/// The process-level serialization gate: each test boots a real `fr`
/// child over the real artifact tree; two concurrent children contend
/// for the boot bound under load, so the process-level e2e runs one at a
/// time by design (the integration suite's own harness contract).
static E2E_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Fails loud with the build instruction when any cdylib artifact is
/// missing from every search dir (never a silent skip).
fn require_artifacts() {
    let dirs = artifact_dirs();
    let missing: Vec<&str> = REQUIRED_ARTIFACTS
        .iter()
        .filter(|name| {
            let slug = name.replace('-', "_");
            !dirs
                .iter()
                .any(|dir| dir.join(format!("lib{slug}.so")).exists())
        })
        .copied()
        .collect();
    assert!(
        missing.is_empty(),
        "missing cdylib artifacts {missing:?}; run `cargo build --workspace` first (the \
         integration gates consume real artifacts, cargo test alone builds none)"
    );
}

/// A throwaway home dir for one boot (removed on drop; the child process
/// receives it as `FR_HOME`).
struct TempHome(PathBuf);

impl TempHome {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("fr-18b-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp home created");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One scripted Messages SSE exchange: the listener answers the first
/// connection with `body` (the deepseek adapter's POST lands there), and
/// the thread stops when the case drops the mock (a run that never
/// connects must not park the join).
struct MockMessages {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    port: u16,
}

impl MockMessages {
    fn spawn(body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the mock binds");
        let port = listener.local_addr().expect("the mock addr").port();
        listener
            .set_nonblocking(true)
            .expect("the mock goes nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let mut served = false;
            while !stop_thread.load(Ordering::Acquire) {
                if let Ok((mut stream, _)) = listener.accept() {
                    // Drain the request head + body (the runner blocks
                    // until the response starts; a sloppy read is fine
                    // for one exchange).
                    let mut buf = [0_u8; 16 * 1024];
                    let _ = stream.read(&mut buf);
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}"
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    served = true;
                } else {
                    std::thread::sleep(Duration::from_millis(10));
                }
                if served {
                    return;
                }
            }
        });
        Self {
            stop,
            handle: Some(handle),
            port,
        }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// One scripted sequence: connection N receives `bodies[N]` (the
    /// retry scenarios replay the provider until the script advances);
    /// past the script the last body repeats.
    fn spawn_scripted(bodies: Vec<&'static str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the mock binds");
        let port = listener.local_addr().expect("the mock addr").port();
        listener
            .set_nonblocking(true)
            .expect("the mock goes nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let mut served = 0_usize;
            while !stop_thread.load(Ordering::Acquire) {
                if let Ok((mut stream, _)) = listener.accept() {
                    let mut buf = [0_u8; 16 * 1024];
                    let _ = stream.read(&mut buf);
                    let body = bodies[served.min(bodies.len() - 1)];
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}"
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    served += 1;
                } else {
                    std::thread::sleep(Duration::from_millis(10));
                }
                if served >= bodies.len() {
                    return;
                }
            }
        });
        Self {
            stop,
            handle: Some(handle),
            port,
        }
    }
}

impl Drop for MockMessages {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            handle.join().expect("the mock thread joins");
        }
    }
}

/// One text-only Messages SSE exchange with a reasoning burst ahead of
/// the answer (the deepseek native-thinking shape).
const SSE_BODY: &str = ": ping

event: message_start
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}

event: content_block_start
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"pondering\"}}

event: content_block_stop
data: {\"type\":\"content_block_stop\",\"index\":0}

event: content_block_start
data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"mocked answer\"}}

event: content_block_stop
data: {\"type\":\"content_block_stop\",\"index\":1}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":9}}

event: message_stop
data: {\"type\":\"message_stop\"}

";

/// The boot command twin: the child gets an isolated home, the test key,
/// and the adapter's trusted-layer endpoint override (the env IS the
/// sanctioned channel: dougong dylib activations receive no slot config,
/// so the row config cannot carry the endpoint — the 18a ruling).
fn boot_command(home: &TempHome, base_url: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fr"));
    command
        .arg("headless")
        .env("FR_HOME", home.path())
        .env("DEEPSEEK_API_KEY", "test-key")
        .env("DEEPSEEK_BASE_URL", base_url);
    command
}

#[test]
fn json_run_completes_with_the_first_and_last_frames_pinned() {
    require_artifacts();
    let _gate = E2E_GATE.lock().expect("the e2e gate");
    let mock = MockMessages::spawn(SSE_BODY);
    let home = TempHome::new("json-run");
    let output = boot_command(&home, &mock.base_url())
        .args(["--json", "say hi"])
        .output()
        .expect("the fr binary runs");
    assert_eq!(output.status.code(), Some(0), "the run exits zero");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{line}: {error}")))
        .collect();
    assert!(
        lines.len() >= 3,
        "session + text + final at minimum: {stdout}"
    );
    // The first event: the session frame with the workspace root (the
    // PRD 18.1 first-event contract).
    assert_eq!(lines[0]["type"], "session");
    assert!(lines[0]["sessionId"].as_str().is_some(), "{}", lines[0]);
    assert!(lines[0]["cwd"].as_str().is_some(), "{}", lines[0]);
    // The committed text projects; the reasoning does not (the `--json`
    // mode carries no stderr reasoning stream).
    let texts: Vec<&str> = lines
        .iter()
        .filter_map(|frame| frame["text"].as_str().filter(|_| frame["type"] == "text"))
        .collect();
    assert_eq!(texts, ["mocked answer"]);
    assert!(
        !stderr.contains("fr: reasoning:"),
        "json mode streams no reasoning: {stderr}"
    );
    // The last event: the lossless final answer.
    let last = lines.last().expect("frames");
    assert_eq!(last["type"], "final");
    assert_eq!(last["text"], "mocked answer");
    // Every line validates against the fr-schema json-stream contract
    // (the Q5A artifact).
    for line in stdout.lines() {
        let frame: serde_json::Value = serde_json::from_str(line).expect("json line");
        if let Err(violations) = fr_schema::validate_line(&frame) {
            panic!("{line}: {violations:?}");
        }
    }
    // A v4 session persisted under the isolated home.
    let sessions = std::fs::read_dir(home.path().join("sessions")).expect("sessions root");
    assert!(sessions.count() >= 1, "one session file persisted");
    assert!(!stderr.contains("fr: reasoning:"), "json mode carries none");
}

#[test]
fn default_mode_prints_the_answer_and_streams_reasoning_to_stderr() {
    require_artifacts();
    let _gate = E2E_GATE.lock().expect("the e2e gate");
    let mock = MockMessages::spawn(SSE_BODY);
    let home = TempHome::new("default-run");
    let output = boot_command(&home, &mock.base_url())
        .arg("say hi")
        .output()
        .expect("the fr binary runs");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "mocked answer\n", "only the final answer prints");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fr: reasoning:\n") && stderr.contains("pondering"),
        "reasoning streams under its fr heading: {stderr:?}"
    );
    assert!(!stdout.contains("pondering"), "stdout carries no reasoning");
}

#[test]
fn missing_session_id_fails_with_the_omission_refusal() {
    require_artifacts();
    let _gate = E2E_GATE.lock().expect("the e2e gate");
    let home = TempHome::new("missing-session");
    // No mock: the refusal precedes any generation.
    let output = boot_command(&home, "http://127.0.0.1:1")
        .args(["--session-id", "ghost", "say hi"])
        .output()
        .expect("the fr binary runs");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "fr: session \"ghost\" does not exist; omit --session-id to start a new Session"
        ),
        "{stderr:?}"
    );
}

#[test]
fn provider_error_fails_with_the_fr_prefixed_line_and_exit_one() {
    require_artifacts();
    let _gate = E2E_GATE.lock().expect("the e2e gate");
    // Nothing listens on port 1: the connection refusal is the provider
    // failure (the transport error path).
    let home = TempHome::new("provider-error");
    let output = boot_command(&home, "http://127.0.0.1:1")
        .arg("say hi")
        .output()
        .expect("the fr binary runs");
    assert_eq!(output.status.code(), Some(1));
    // The dsh order: the (empty) answer text prints first, then the
    // error reason line, then exit 1.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "\n", "the empty answer line prints");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fr: "),
        "an fr-prefixed error line: {stderr:?}"
    );
}

/// One provider failure exchange: a plain HTTP 500 status line with a
/// minimal error document (the transport's SERVER classification — the
/// default policy's retryable code).
const FAILURE_BODY: &str = "";

/// Finds the session log of an isolated home and returns the decoded
/// event text (exactly one log per e2e home; the layout nests under the
/// project key; the v4 generation compresses with zstd).
fn find_session_log(home: &TempHome) -> String {
    fn walk(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("session.v4.jsonl"))
            {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(home.path(), &mut found);
    assert_eq!(found.len(), 1, "one session log per e2e home");
    let log = found.remove(0);
    let bytes = std::fs::read(&log).expect("the log reads");
    let text = if log.to_string_lossy().ends_with(".zstd") {
        zstd::stream::decode_all(bytes.as_slice()).expect("the log decodes")
    } else {
        bytes
    };
    String::from_utf8(text).expect("the log is utf-8")
}

/// Counts one event type across the durable log.
fn count_type(log: &str, type_name: &str) -> usize {
    log.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["type"] == type_name)
        .count()
}

/// A provider failure followed by a healthy exchange: the fr-llm-retry
/// plugin schedules one durable retry, the loop parks and appends the
/// transition, and the step re-enters to completion (the 18c carrier
/// end to end over real artifacts).
#[test]
fn provider_retry_reruns_the_step_after_a_failed_attempt() {
    require_artifacts();
    let _gate = E2E_GATE.lock().expect("the e2e gate");
    let mock = MockMessages::spawn_scripted(vec![FAILURE_BODY, SSE_BODY]);
    let home = TempHome::new("provider-retry");
    let output = boot_command(&home, &mock.base_url())
        .arg("say hi")
        .output()
        .expect("the fr binary runs");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("mocked answer"), "{stdout:?}");
    let log = find_session_log(&home);
    assert_eq!(count_type(&log, "llm/retry"), 1, "one durable retry");
    assert_eq!(count_type(&log, "llm/retry-started"), 1, "one transition");
    assert_eq!(
        count_type(&log, "assistant/attempt"),
        1,
        "one failed attempt"
    );
    assert_eq!(
        count_type(&log, "assistant/message"),
        1,
        "the retry commits"
    );
}

/// The default policy exhausts (five retries, ~15.5s of backoff) and the
/// sixth failure passes through to the loop's `LlmError`: exit 1, the
/// fr-prefixed line, five durable retries and transitions, six attempts.
#[test]
fn provider_retry_retries_within_the_policy_and_throws_past_the_limit() {
    require_artifacts();
    let _gate = E2E_GATE.lock().expect("the e2e gate");
    let mock = MockMessages::spawn_scripted(vec![FAILURE_BODY; 6]);
    let home = TempHome::new("provider-retry-exhausted");
    let output = boot_command(&home, &mock.base_url())
        .arg("say hi")
        .output()
        .expect("the fr binary runs");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fr: "),
        "an fr-prefixed error line: {stderr:?}"
    );
    let log = find_session_log(&home);
    assert_eq!(count_type(&log, "llm/retry"), 5, "the default cap is five");
    assert_eq!(count_type(&log, "llm/retry-started"), 5, "each wait lands");
    assert_eq!(count_type(&log, "assistant/attempt"), 6, "six failures");
    assert_eq!(count_type(&log, "assistant/message"), 0, "no commit");
}

#[test]
fn help_prints_the_usage_and_exits_zero() {
    require_artifacts();
    let _gate = E2E_GATE.lock().expect("the e2e gate");
    let home = TempHome::new("help");
    let output = Command::new(env!("CARGO_BIN_EXE_fr"))
        .arg("headless")
        .arg("--help")
        .env("FR_HOME", home.path())
        .output()
        .expect("the fr binary runs");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("Usage:"), "{stdout:?}");
    assert!(stdout.contains("--session-id"), "{stdout:?}");
}
