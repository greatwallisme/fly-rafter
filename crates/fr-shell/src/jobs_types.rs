// Rust guideline compliant 2026-10-29
//! Client-safe job vocabulary (dsh `jobs/src/view.ts` + `types.ts`): the
//! read-only projection of one job, the ring chunks it hands out, and the
//! producer outcome shapes.

use serde::Deserialize;
use serde::Serialize;

/// Producer-defined job kinds (dsh `JobKindMap` declaration merges; the
/// registry treats every value as an opaque id namespace). The 16a
/// composition registers `bash` and `pwsh`.
pub const JOB_KIND_BASH: &str = "bash";
/// The PowerShell job kind (tool-pwsh's declaration merge).
pub const JOB_KIND_PWSH: &str = "pwsh";

/// Job lifecycle: `running`, optionally `stopping`, then exactly one
/// terminal status (dsh `JobStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobStatus {
    /// The producer's work is live.
    Running,
    /// Cancellation was requested; the producer has not settled yet.
    Stopping,
    /// The producer reported a finished outcome.
    Completed,
    /// The job was cancelled.
    Killed,
    /// The producer's work broke (a rejected done or a forced teardown).
    Failed,
}

impl JobStatus {
    /// True for the three terminal statuses.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Killed | Self::Failed)
    }

    /// The kebab-case wire word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Completed => "completed",
            Self::Killed => "killed",
            Self::Failed => "failed",
        }
    }
}

/// Stream label of one output chunk; `stdout` and `stderr` reach the
/// model's consuming read, `log` marks producer narration that only
/// observers see (dsh `JobChannel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobChannel {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
    /// Producer narration.
    Log,
}

impl JobChannel {
    /// The wire word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Log => "log",
        }
    }
}

/// One chunk of a job's output ring: absolute offset, text, channel, and
/// loss marker (dsh `JobChunk`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobChunk {
    /// Absolute offset of the chunk's first byte; offsets never move once
    /// assigned.
    pub at: u64,
    /// Chunk text exactly as appended (possibly tail-trimmed by retention).
    pub text: String,
    /// Stream label, when the producer supplied one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<JobChannel>,
    /// Bytes immediately before this chunk were lost.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap_before: Option<bool>,
}

/// The ring coordinates and complete-stream files behind one job's output
/// (dsh `JobView.output`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobOutputFacts {
    /// The offset the next chunk starts at.
    pub total: u64,
    /// The oldest retained byte; greater than zero exactly when retention
    /// dropped the head.
    pub earliest: u64,
    /// The spill files the job's pull sources currently keep, in source
    /// order and deduplicated; absent while no source keeps one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spill_paths: Option<Vec<String>>,
}

/// Read-only projection of one job — a fresh value per call, never live
/// registry state (dsh `JobView`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobView {
    /// The registry-issued id (`<kind>-<short-uuid>`).
    pub id: String,
    /// The producer kind the job was registered with.
    pub kind: String,
    /// The producer-supplied one-line label.
    pub label: String,
    /// Owning session; absent for an unowned job, which every caller can
    /// see.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Producer-owned cap for complete model-facing notices and reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_limit_bytes: Option<f64>,
    /// Current lifecycle state.
    pub status: JobStatus,
    /// The producer's live progress line; cleared at settlement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    /// Terminal reason; a recorded kill reason is merged in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Epoch milliseconds when the job was registered.
    pub started_at: f64,
    /// Epoch milliseconds when the job settled; absent while live.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<f64>,
    /// The output ring's absolute coordinates and complete-stream files.
    pub output: JobOutputFacts,
}

/// Terminal result supplied by a producer (dsh `JobOutcome`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobOutcome {
    /// How the job ended.
    pub status: JobStatus,
    /// Terminal reason rendered into status lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Return value for jobs whose result is a value rather than a
    /// stream; handed out once by the first post-settlement read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

/// The creating tool invocation retained in durable lifecycle records
/// (dsh `JobToolCall`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobToolCall {
    /// Exact invocation, including a PTC sub-call when applicable.
    pub call_id: String,
    /// Root model-requested invocation owning this execution.
    pub root_call_id: String,
}

/// One incremental read from a pull source (dsh `JobSourceRead`).
#[derive(Debug, Clone)]
pub struct JobSourceRead {
    /// Text captured since the requested offset (the whole retained tail
    /// when lossy).
    pub text: String,
    /// Whole-stream offset to resume from.
    pub next_offset: u64,
    /// True when the requested offset slid out of the retained window.
    pub lossy: bool,
    /// Host path of a file holding the complete stream, when the source
    /// currently keeps an intact one.
    pub spill_path: Option<String>,
}

/// A pull source the registry pumps into the job's output ring (dsh
/// `JobOutputSource`): an offset reader closure over one captured stream.
pub type JobOutputSource = Box<dyn Fn(u64) -> JobSourceRead + Send + Sync>;

/// Options for one producer append (dsh `JobAppendOptions`).
#[derive(Debug, Clone, Copy, Default)]
pub struct JobAppendOptions {
    /// Stream label for the chunk.
    pub channel: Option<JobChannel>,
    /// Marks that producer-side bytes between the previous chunk and this
    /// one were lost.
    pub gap_before: bool,
}

/// Output and post-read state returned by the consuming read (dsh
/// `JobRead`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRead {
    /// Ring chunks appended since the model cursor, in offset order.
    pub chunks: Vec<JobChunk>,
    /// True when the cursor fell below the oldest retained byte.
    pub lossy: bool,
    /// The producer's result, handed out by the first read after
    /// settlement only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// The job's state at read time.
    pub job: JobView,
}

/// Result of one non-consuming offset read (dsh `JobOutputRead`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobOutputRead {
    /// Retained chunks overlapping `[from, total)`, in offset order.
    pub chunks: Vec<JobChunk>,
    /// Offset to resume from — always a chunk boundary.
    pub next: u64,
    /// True when `from` fell below the oldest retained byte.
    pub lossy: bool,
}

/// Why a job settled (dsh `JobSettleCause`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobSettleCause {
    /// The producer finished.
    Producer,
    /// A registry kill ran first.
    Kill,
    /// Owner or service teardown cancelled it.
    Teardown,
}

/// Durable registration facts recorded into the owner session's log (dsh
/// `job/registered`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRegisteredData {
    /// Unique execution id accepted by job controls.
    pub job_id: String,
    /// Original owner; copied history does not transfer ownership.
    pub owner: String,
    /// Producer kind.
    pub kind: String,
    /// Producer-supplied label.
    pub label: String,
    /// Unix epoch milliseconds when registration committed.
    pub started_at: f64,
    /// Creating tool invocation, when the producer was called through a
    /// tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<JobToolCall>,
}

/// Durable terminal record paired by the same unique job id (dsh
/// `job/settled`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSettledData {
    /// Registration facts.
    #[serde(flatten)]
    pub registered: JobRegisteredData,
    /// Unix epoch milliseconds when the registry committed the terminal
    /// record.
    pub finished_at: f64,
    /// Recorded terminal outcome.
    pub status: JobStatus,
    /// Source of the termination request, or producer completion.
    pub cause: JobSettleCause,
    /// True when done fulfilled; false for rejection or forced failure.
    pub producer_reported: bool,
    /// Terminal reason, including a recorded kill reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The dsh session event types the registry records for owned jobs.
pub const JOB_REGISTERED_EVENT: &str = "job/registered";
/// The terminal companion record type.
pub const JOB_SETTLED_EVENT: &str = "job/settled";
