// Rust guideline compliant 2026-10-31
//! Linux `/proc` process inspection for the terminal primitive (the dsh
//! `subprocess-local` `process-inspector` Linux lane parity).
//!
//! The port keeps the dsh semantics verbatim: `/proc/<pid>/stat` parsing with
//! parenthesized `comm` text, the exact per-architecture syscall tables, and
//! the four stdin-wait arms (read, select/pselect fd-set, poll/ppoll struct
//! array, epoll fdinfo). The OS surface rides an injectable
//! [`ProcIntrospection`] trait so the classification logic stays
//! deterministic under test while the production arm reads the real `/proc`.

use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;

/// One `/proc/<pid>/stat` row with the fields the inspector consumes.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LinuxStat {
    /// The process id.
    pub pid: i64,
    /// The parent process id.
    pub parent_pid: i64,
    /// The process group id.
    pub pgrp: i64,
    /// The POSIX session id.
    pub stat_session: i64,
    /// The single-letter process state.
    pub state: String,
    /// The controlling terminal device number (the raw `tty_nr` bits).
    pub tty_device: u32,
    /// The foreground process group id of the controlling terminal.
    pub tpgid: i64,
    /// The start identity (the `starttime` clock-tick text).
    pub started: String,
}

/// A pid plus its start identity (`pid` + `starttime` fencing).
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProcessIdentity {
    /// The process id.
    pub pid: i64,
    /// The start identity text.
    pub started: String,
}

/// One row of a whole-table snapshot.
#[derive(Debug, Clone)]
pub struct ProcessRow {
    /// The process id.
    pub pid: i64,
    /// The parent process id.
    pub parent_pid: i64,
    /// The POSIX session id.
    pub session: i64,
    /// The single-letter state.
    pub state: String,
    /// The start identity text.
    pub started: String,
}

/// A whole-table `/proc` snapshot with a completeness fence (a failed row
/// read marks the snapshot incomplete, which disables adoption arms that
/// require a provably whole table).
#[derive(Debug, Clone)]
pub struct ProcessSnapshot {
    rows: Vec<ProcessRow>,
    complete: bool,
}

/// One `/proc/<pid>/task/<tid>/syscall` record.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SyscallInfo {
    /// The syscall number.
    pub number: i64,
    /// The first six syscall arguments (hex-decoded, kernel-register order).
    pub args: [u64; 6],
}

/// The syscall numbers the stdin-wait classification matches (the dsh
/// `SyscallTable`; `None` arms are numbers the kernel ABI does not provide).
#[derive(Debug, Clone, Copy)]
pub struct SyscallTable {
    /// The `read` number.
    pub read: i64,
    /// The `select` number, when the ABI has one.
    pub select: Option<i64>,
    /// The `pselect6` number.
    pub pselect: i64,
    /// The `poll` number, when the ABI has one.
    pub poll: Option<i64>,
    /// The `ppoll` number.
    pub ppoll: i64,
    /// The `epoll_wait` number, when the ABI has one.
    pub epoll_wait: Option<i64>,
    /// The `epoll_pwait` number.
    pub epoll_pwait: i64,
}

/// The x86-64 kernel-ABI table.
const X64: SyscallTable = SyscallTable {
    read: 0,
    select: Some(23),
    pselect: 270,
    poll: Some(7),
    ppoll: 271,
    epoll_wait: Some(232),
    epoll_pwait: 281,
};

/// The arm64 kernel-ABI table.
const ARM64: SyscallTable = SyscallTable {
    read: 63,
    select: None,
    pselect: 72,
    poll: None,
    ppoll: 73,
    epoll_wait: None,
    epoll_pwait: 22,
};

/// The tables to consult, primary-arch first (the dsh
/// `linuxSyscallTables` fallback: user-mode emulation can expose a supported
/// table that differs from the host architecture).
#[must_use]
pub fn syscall_tables() -> Vec<&'static SyscallTable> {
    if cfg!(target_arch = "x86_64") {
        vec![&X64, &ARM64]
    } else if cfg!(target_arch = "aarch64") {
        vec![&ARM64, &X64]
    } else {
        Vec::new()
    }
}

/// Whether the state letter marks a quiescent (zombie or dead) process.
#[must_use]
pub fn quiescent(state: Option<&str>) -> bool {
    matches!(state, Some("Z" | "X" | "x"))
}

/// The injectable OS surface behind one inspector (the dsh
/// `ProcessInspectorInternals` narrow seam).
pub trait ProcIntrospection: Send + Sync {
    /// Reads one `/proc/<pid>/stat` row.
    fn stat(&self, pid: i64) -> Option<LinuxStat>;
    /// Lists the numeric entries of one `/proc` directory (the root or one
    /// `<pid>/task`), `None` where the directory is unreadable.
    fn proc_entries(&self, dir: &str) -> Option<Vec<i64>>;
    /// Reads one symlink target text (`/proc/<pid>/fd/<fd>`).
    fn read_link(&self, path: &str) -> Option<String>;
    /// Returns the char-device `rdev` bits of one fd path, `None` for
    /// missing paths and non-character devices.
    fn fd_device(&self, path: &str) -> Option<u32>;
    /// Reads one thread's in-kernel syscall record.
    fn read_syscall(&self, pid: i64, tid: i64) -> Option<SyscallInfo>;
    /// Reads `len` bytes of one process's memory at `address`
    /// (`/proc/<pid>/mem` pread), `None` where the read fails.
    fn read_mem(&self, pid: i64, address: u64, len: usize) -> Option<Vec<u8>>;
    /// Reads one thread's fdinfo text for one epoll fd.
    fn fdinfo(&self, pid: i64, tid: i64, fd: i64) -> Option<String>;
    /// Delivers one signal to one process group (`kill(-pgid, sig)`).
    fn signal_group(&self, pgid: i64, sig: i32);
    /// Delivers one signal to one exact process (`kill(pid, sig)`).
    fn signal_process(&self, pid: i64, sig: i32);
    /// Reads one raw terminal fd device for the stdin-wait device compare.
    ///
    /// The dsh arm composes this from `readLink` + `stat`; the fr arm keeps
    /// the composition so the `/dev/tty` alias stays observable.
    fn terminal_device(&self, pid: i64, tty_device: u32, tid: Option<i64>) -> Option<u32>;
}

/// The production `/proc`-backed introspection.
#[derive(Debug, Default)]
pub struct ProcFs;

impl ProcIntrospection for ProcFs {
    fn stat(&self, pid: i64) -> Option<LinuxStat> {
        parse_proc_stat(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
    }

    fn proc_entries(&self, dir: &str) -> Option<Vec<i64>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(dir).ok()? {
            let name = entry.ok()?.file_name();
            let name = name.to_string_lossy();
            if let Ok(value) = name.parse::<i64>() {
                entries.push(value);
            }
        }
        Some(entries)
    }

    fn read_link(&self, path: &str) -> Option<String> {
        fs::read_link(path)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }

    fn fd_device(&self, path: &str) -> Option<u32> {
        let meta = fs::metadata(path).ok()?;
        let mode = libc::S_IFMT & meta.mode();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "/proc/<pid>/stat renders tty_nr as the low 32 device bits; the fd compare keeps the same width"
        )]
        let device = meta.rdev() as u32;
        (mode == libc::S_IFCHR).then_some(device)
    }

    fn read_syscall(&self, pid: i64, tid: i64) -> Option<SyscallInfo> {
        parse_syscall(&fs::read_to_string(format!("/proc/{pid}/task/{tid}/syscall")).ok()?)
    }

    fn read_mem(&self, pid: i64, address: u64, len: usize) -> Option<Vec<u8>> {
        let mut file = fs::File::open(format!("/proc/{pid}/mem")).ok()?;
        file.seek(SeekFrom::Start(address)).ok()?;
        let mut buffer = vec![0_u8; len];
        let count = file.read(&mut buffer).ok()?;
        buffer.truncate(count);
        Some(buffer)
    }

    fn fdinfo(&self, pid: i64, tid: i64, fd: i64) -> Option<String> {
        fs::read_to_string(format!("/proc/{pid}/task/{tid}/fdinfo/{fd}")).ok()
    }

    fn signal_group(&self, pgid: i64, sig: i32) {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "POSIX pgids fit i32 by kernel contract; the truncating cast keeps the kill() ABI"
        )]
        crate::subprocess::kill_group(pgid as i32, sig);
    }

    fn signal_process(&self, pid: i64, sig: i32) {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "POSIX pids fit i32 by kernel contract; the truncating cast keeps the kill() ABI"
        )]
        let pid = pid as i32;
        // SAFETY: kill on one inspected pid; a raced exit makes it a
        // harmless no-op error return.
        let _ = unsafe { libc::kill(pid, sig) };
    }

    fn terminal_device(&self, pid: i64, tty_device: u32, tid: Option<i64>) -> Option<u32> {
        if tty_device == 0 {
            return None;
        }
        let path = tid.map_or_else(
            || format!("/proc/{pid}/fd/0"),
            |tid| format!("/proc/{pid}/task/{tid}/fd/0"),
        );
        if self.read_link(&path).as_deref() == Some("/dev/tty") {
            return Some(tty_device);
        }
        self.fd_device(&path).filter(|device| *device == tty_device)
    }
}

/// Parses one `/proc/<pid>/stat` line (the dsh `parseProcStat`: greedy last
/// `)` for `comm`, then the state and the numbered tail fields).
#[must_use]
pub fn parse_proc_stat(text: &str) -> Option<LinuxStat> {
    let text = text.trim_start();
    let open = text.find('(')?;
    let pid: i64 = text[..open].trim().parse().ok()?;
    let close = text.rfind(')')?;
    let after = text[close + 1..].trim_start();
    let (state, rest) = after.split_once(char::is_whitespace)?;
    let rest: Vec<&str> = rest.split_whitespace().collect();
    if rest.len() < 20 || state.chars().count() != 1 {
        return None;
    }
    let parent_pid: i64 = rest[0].parse().ok()?;
    let pgrp: i64 = rest[1].parse().ok()?;
    let session: i64 = rest[2].parse().ok()?;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "/proc/<pid>/stat renders tty_nr as the low 32 device bits; the same width compares against fd rdev"
    )]
    let tty_device: u32 = rest[3].parse::<i64>().ok()?.unsigned_abs() as u32;
    let tpgid: i64 = rest[4].parse().ok()?;
    let started = rest[19].to_owned();
    Some(LinuxStat {
        pid,
        parent_pid,
        pgrp,
        stat_session: session,
        state: state.to_owned(),
        tty_device,
        tpgid,
        started,
    })
}

/// Parses one `/proc/<pid>/task/<tid>/syscall` line (`running` and `-1 …`
/// return `None`; the six argument words decode as hex).
#[must_use]
pub fn parse_syscall(text: &str) -> Option<SyscallInfo> {
    let text = text.trim();
    if text == "running" || text.starts_with("-1 ") || text == "-1" {
        return None;
    }
    let fields: Vec<&str> = text.split_whitespace().collect();
    let number: i64 = fields.first()?.parse().ok()?;
    let mut args = [0_u64; 6];
    for (slot, field) in args.iter_mut().zip(fields.iter().skip(1).take(6)) {
        let hex = field
            .strip_prefix("0x")
            .or_else(|| field.strip_prefix("0X"))?;
        *slot = u64::from_str_radix(hex, 16).ok()?;
    }
    Some(SyscallInfo { number, args })
}

/// The post-order tree walk shared by snapshot consumers (children first,
/// then the visited entry itself, matching the dsh `processTree` push).
fn visit_tree(
    pid: i64,
    by_parent: &HashMap<i64, Vec<i64>>,
    by_pid: &HashMap<i64, &ProcessRow>,
    visited: &mut HashSet<i64>,
    result: &mut Vec<ProcessIdentity>,
) {
    if !visited.insert(pid) {
        return;
    }
    if let Some(children) = by_parent.get(&pid) {
        for child in children {
            visit_tree(*child, by_parent, by_pid, visited, result);
        }
    }
    if let Some(row) = by_pid.get(&pid) {
        result.push(ProcessIdentity {
            pid: row.pid,
            started: row.started.clone(),
        });
    }
}

impl ProcessSnapshot {
    /// Assembles a snapshot from scanned rows plus the completeness fence.
    #[must_use]
    pub const fn new(rows: Vec<ProcessRow>, complete: bool) -> Self {
        Self { rows, complete }
    }

    /// Whether every scanned pid resolved a stat row.
    #[must_use]
    pub const fn complete(&self) -> bool {
        self.complete
    }

    /// The identity list of the process tree rooted at `root_pid`
    /// (insertion-order children, post-order DFS with the root itself, as
    /// the dsh `processTree`).
    #[must_use]
    pub fn tree(&self, root_pid: i64) -> Vec<ProcessIdentity> {
        let mut by_parent: HashMap<i64, Vec<i64>> = HashMap::new();
        for row in &self.rows {
            by_parent.entry(row.parent_pid).or_default().push(row.pid);
        }
        let by_pid: HashMap<i64, &ProcessRow> =
            self.rows.iter().map(|row| (row.pid, row)).collect();
        let mut result = Vec::new();
        let mut visited: HashSet<i64> = HashSet::new();
        visit_tree(root_pid, &by_parent, &by_pid, &mut visited, &mut result);
        result
    }

    /// The observed members of one POSIX session.
    #[must_use]
    pub fn session(&self, session_id: i64) -> Vec<ProcessIdentity> {
        self.rows
            .iter()
            .filter(|row| row.session == session_id)
            .map(|row| ProcessIdentity {
                pid: row.pid,
                started: row.started.clone(),
            })
            .collect()
    }

    /// Whether the exact identity is a live non-quiescent row of this
    /// snapshot.
    #[must_use]
    pub fn alive(&self, identity: &ProcessIdentity) -> bool {
        self.rows.iter().any(|row| {
            row.pid == identity.pid
                && row.started == identity.started
                && !quiescent(Some(&row.state))
        })
    }
}

/// The snapshot arm of the production introspection (a whole-table
/// `/proc` scan; any failed row read marks the snapshot incomplete).
#[must_use]
pub fn scan_snapshot(source: &dyn ProcIntrospection) -> Option<ProcessSnapshot> {
    let pids = source.proc_entries("/proc")?;
    let mut complete = true;
    let mut rows = Vec::with_capacity(pids.len());
    for pid in pids {
        match source.stat(pid) {
            Some(stat) => rows.push(ProcessRow {
                pid: stat.pid,
                parent_pid: stat.parent_pid,
                session: stat.stat_session,
                state: stat.state,
                started: stat.started,
            }),
            None => complete = false,
        }
    }
    Some(ProcessSnapshot::new(rows, complete))
}

/// The foreground process group id of one shell pid (`/proc/<pid>/stat`
/// `tpgid`, positive only).
#[must_use]
pub fn foreground_pgid(source: &dyn ProcIntrospection, shell_pid: i64) -> Option<i64> {
    let tpgid = source.stat(shell_pid)?.tpgid;
    (tpgid > 0).then_some(tpgid)
}

/// Whether any thread of the `pgid` group sits blocked reading the shell's
/// terminal device (the dsh `isStdinWaiting`: the four syscall arms against
/// the exact terminal device).
#[must_use]
pub fn is_stdin_waiting(source: &dyn ProcIntrospection, pgid: i64, shell_pid: i64) -> bool {
    let tables = syscall_tables();
    if tables.is_empty() {
        return false;
    }
    let shell = source.stat(shell_pid);
    let Some(shell) = shell else { return false };
    let Some(terminal_device) = source.terminal_device(shell_pid, shell.tty_device, None) else {
        return false;
    };
    let Some(pids) = source.proc_entries("/proc") else {
        return false;
    };
    for pid in pids {
        let Some(process) = source.stat(pid) else {
            continue;
        };
        if process.pgrp != pgid {
            continue;
        }
        let Some(tids) = source.proc_entries(&format!("/proc/{pid}/task")) else {
            continue;
        };
        for tid in tids {
            let Some(syscall) = source.read_syscall(pid, tid) else {
                continue;
            };
            if syscall_waits_on_stdin(source, pid, tid, &syscall, &tables)
                && source.terminal_device(pid, process.tty_device, Some(tid))
                    == Some(terminal_device)
            {
                return true;
            }
        }
    }
    false
}

/// The four-arm stdin-wait classification (the dsh `syscallWaitsOnStdin`).
fn syscall_waits_on_stdin(
    source: &dyn ProcIntrospection,
    pid: i64,
    tid: i64,
    syscall: &SyscallInfo,
    tables: &[&'static SyscallTable],
) -> bool {
    let a0 = syscall.args.first().copied().unwrap_or(0);
    let a1 = syscall.args.get(1).copied().unwrap_or(0);
    let a2 = syscall.args.get(2).copied().unwrap_or(0);
    for table in tables {
        if syscall.number == table.read {
            return a0 == 0;
        }
        if Some(syscall.number) == table.select || syscall.number == table.pselect {
            return a0 >= 1 && fd_set_has_stdin(source, pid, a1);
        }
        if Some(syscall.number) == table.poll || syscall.number == table.ppoll {
            return a1 > 0 && poll_has_stdin(source, pid, a0, a1);
        }
        if Some(syscall.number) == table.epoll_wait || syscall.number == table.epoll_pwait {
            return a2 >= 1 && epoll_has_stdin(source, pid, tid, a0);
        }
    }
    false
}

/// The select arm: bit 0 of the first 8-byte fd-set word (fd 0 present).
fn fd_set_has_stdin(source: &dyn ProcIntrospection, pid: i64, address: u64) -> bool {
    if address == 0 {
        return false;
    }
    let Some(memory) = source.read_mem(pid, address, 8) else {
        return false;
    };
    memory.first().is_some_and(|byte| byte & 1 == 1)
}

/// The poll arm: the leading `struct pollfd` entries with fd 0 and a set
/// read event bit (`events & POLLIN`).
fn poll_has_stdin(source: &dyn ProcIntrospection, pid: i64, address: u64, count: u64) -> bool {
    if address == 0 || count == 0 {
        return false;
    }
    let span = count.clamp(1, 1024) * 8;
    let Some(memory) = source.read_mem(pid, address, usize::try_from(span).unwrap_or(usize::MAX))
    else {
        return false;
    };
    memory.as_chunks::<8>().0.iter().any(|entry| {
        let fd = i32::from_le_bytes(
            entry[0..4]
                .try_into()
                .expect("the chunk width is the 4-byte slice"),
        );
        let events = u16::from_le_bytes(
            entry[4..6]
                .try_into()
                .expect("the chunk width spans the 2-byte field"),
        );
        fd == 0 && (events & 0x001) != 0
    })
}

/// The epoll arm: the thread's fdinfo records a registered tfd 0.
fn epoll_has_stdin(source: &dyn ProcIntrospection, pid: i64, tid: i64, epfd: u64) -> bool {
    let Some(text) = source.fdinfo(pid, tid, i64::try_from(epfd).unwrap_or(i64::MAX)) else {
        return false;
    };
    text.split('\n').any(|line| {
        line.trim_start().starts_with("tfd:") && line.split_whitespace().nth(1) == Some("0")
    })
}

#[cfg(test)]
mod tests {
    use super::LinuxStat;
    use super::{
        ProcIntrospection, ProcessIdentity, ProcessSnapshot, SyscallInfo, parse_proc_stat,
        parse_syscall, quiescent,
    };
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// The synthetic introspection the classification tests drive.
    struct FakeProc {
        stats: Mutex<HashMap<String, super::LinuxStat>>,
        mems: Mutex<HashMap<(i64, u64), Vec<u8>>>,
        syscalls: Mutex<HashMap<(i64, i64), SyscallInfo>>,
        fdinfos: Mutex<HashMap<(i64, i64, i64), String>>,
        devices: Mutex<HashMap<String, u32>>,
    }

    impl FakeProc {
        fn new() -> Self {
            Self {
                stats: Mutex::new(HashMap::new()),
                mems: Mutex::new(HashMap::new()),
                syscalls: Mutex::new(HashMap::new()),
                fdinfos: Mutex::new(HashMap::new()),
                devices: Mutex::new(HashMap::new()),
            }
        }

        fn stat_entry(&self, stat: LinuxStat) {
            let pid = stat.pid;
            self.stats
                .lock()
                .expect("fake lock")
                .insert(format!("/proc/{pid}/stat"), stat);
        }

        fn insert_mem(&self, pid: i64, address: u64, bytes: Vec<u8>) {
            self.mems
                .lock()
                .expect("fake lock")
                .insert((pid, address), bytes);
        }

        fn insert_syscall(&self, pid: i64, tid: i64, info: SyscallInfo) {
            self.syscalls
                .lock()
                .expect("fake lock")
                .insert((pid, tid), info);
        }

        fn insert_fdinfo(&self, pid: i64, tid: i64, fd: i64, text: &str) {
            self.fdinfos
                .lock()
                .expect("fake lock")
                .insert((pid, tid, fd), text.to_owned());
        }

        fn insert_device(&self, path: String, device: u32) {
            self.devices.lock().expect("fake lock").insert(path, device);
        }
    }

    impl ProcIntrospection for FakeProc {
        fn stat(&self, pid: i64) -> Option<LinuxStat> {
            self.stats
                .lock()
                .expect("fake lock")
                .get(&format!("/proc/{pid}/stat"))
                .cloned()
        }

        fn proc_entries(&self, dir: &str) -> Option<Vec<i64>> {
            if dir == "/proc" {
                let mut pids: Vec<i64> = self
                    .stats
                    .lock()
                    .expect("fake lock")
                    .values()
                    .map(|stat| stat.pid)
                    .collect();
                pids.sort_unstable();
                return Some(pids);
            }
            let prefix = format!("{dir}/");
            let mut tids: Vec<i64> = self
                .syscalls
                .lock()
                .expect("fake lock")
                .keys()
                .filter_map(|(pid, tid)| {
                    let pid_word = pid.to_string();
                    format!("/proc/{pid_word}/task/")
                        .contains(&prefix)
                        .then_some(*tid)
                })
                .collect();
            tids.sort_unstable();
            tids.dedup();
            Some(tids)
        }

        fn read_link(&self, path: &str) -> Option<String> {
            self.devices
                .lock()
                .expect("fake lock")
                .get(path)
                .map(|device| {
                    if *device == 0 {
                        "/dev/tty".to_owned()
                    } else {
                        format!("/dev/pts/{device}")
                    }
                })
        }

        fn fd_device(&self, path: &str) -> Option<u32> {
            self.devices.lock().expect("fake lock").get(path).copied()
        }

        fn read_syscall(&self, pid: i64, tid: i64) -> Option<SyscallInfo> {
            self.syscalls
                .lock()
                .expect("fake lock")
                .get(&(pid, tid))
                .copied()
        }

        fn read_mem(&self, pid: i64, address: u64, len: usize) -> Option<Vec<u8>> {
            self.mems
                .lock()
                .expect("fake lock")
                .get(&(pid, address))
                .map(|bytes| bytes[..len.min(bytes.len())].to_vec())
        }

        fn fdinfo(&self, pid: i64, tid: i64, fd: i64) -> Option<String> {
            self.fdinfos
                .lock()
                .expect("fake lock")
                .get(&(pid, tid, fd))
                .cloned()
        }

        fn signal_group(&self, _pgid: i64, _sig: i32) {}

        fn signal_process(&self, _pid: i64, _sig: i32) {}

        fn terminal_device(&self, pid: i64, tty_device: u32, tid: Option<i64>) -> Option<u32> {
            if tty_device == 0 {
                return None;
            }
            let path = tid.map_or_else(
                || format!("/proc/{pid}/fd/0"),
                |tid| format!("/proc/{pid}/task/{tid}/fd/0"),
            );
            if self.read_link(&path).as_deref() == Some("/dev/tty") {
                return Some(tty_device);
            }
            self.fd_device(&path).filter(|device| *device == tty_device)
        }
    }

    fn stat_row(pid: i64, pgrp: i64, tty: u32, tpgid: i64, session: i64) -> LinuxStat {
        LinuxStat {
            pid,
            parent_pid: 1,
            pgrp,
            stat_session: session,
            state: "S".to_owned(),
            tty_device: tty,
            tpgid,
            started: format!("{pid}00"),
        }
    }

    #[test]
    fn parse_proc_stat_handles_parenthesized_comm_with_brackets() {
        let line = " 1234 (cat) S 1 1234 1234 34816 1234 0 0 0 0 0 0 0 0 0 0 0 0 0 0 98765 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n";
        let stat = parse_proc_stat(line).expect("parsed");
        assert_eq!(stat.pid, 1234);
        assert_eq!(stat.parent_pid, 1);
        assert_eq!(stat.pgrp, 1234);
        assert_eq!(stat.stat_session, 1234);
        assert_eq!(stat.tty_device, 34816);
        assert_eq!(stat.tpgid, 1234);
        assert_eq!(stat.started, "98765");
    }

    #[test]
    fn parse_proc_stat_takes_the_last_paren_for_comm_with_parens() {
        let line = "42 (weird)name) S 1 42 42 0 42 0 0 0 0 0 0 0 0 0 0 0 0 0 0 777 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n";
        let stat = parse_proc_stat(line).expect("parsed");
        assert_eq!(stat.state, "S");
        assert_eq!(stat.started, "777");
    }

    #[test]
    fn parse_proc_stat_rejects_short_rows_and_multi_char_states() {
        assert!(parse_proc_stat("1 (x) S 1 1 1 0 1").is_none());
        let bad_state = "1 (x) SS 1 1 1 0 1 0 0 0 0 0 0 0 0 0 0 0 0 0 5 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n";
        assert!(parse_proc_stat(bad_state).is_none());
    }

    #[test]
    fn parse_syscall_decodes_hex_args_and_skips_running() {
        let info = parse_syscall("0 0xffffffffffffffff 0x2 0x0 0x0 0x0 0x0 0x7fff 0x7fff")
            .expect("parsed");
        assert_eq!(info.number, 0);
        assert_eq!(info.args[0], u64::MAX);
        assert_eq!(info.args[1], 2);
        assert!(parse_syscall("running").is_none());
        assert!(parse_syscall("-1 0x0 0x0").is_none());
    }

    #[test]
    fn quiescent_matches_zombie_and_dead_states() {
        assert!(quiescent(Some("Z")));
        assert!(quiescent(Some("X")));
        assert!(quiescent(Some("x")));
        assert!(!quiescent(Some("S")));
        assert!(!quiescent(None));
    }

    #[test]
    fn tree_walks_insertion_order_children_preorder() {
        let rows = vec![
            super::ProcessRow {
                pid: 1,
                parent_pid: 0,
                session: 9,
                state: "S".to_owned(),
                started: "a".to_owned(),
            },
            super::ProcessRow {
                pid: 7,
                parent_pid: 1,
                session: 9,
                state: "S".to_owned(),
                started: "b".to_owned(),
            },
            super::ProcessRow {
                pid: 8,
                parent_pid: 1,
                session: 9,
                state: "S".to_owned(),
                started: "c".to_owned(),
            },
            super::ProcessRow {
                pid: 9,
                parent_pid: 7,
                session: 9,
                state: "S".to_owned(),
                started: "d".to_owned(),
            },
        ];
        let snapshot = ProcessSnapshot::new(rows, true);
        let tree = snapshot.tree(1);
        let pids: Vec<i64> = tree.iter().map(|member| member.pid).collect();
        assert_eq!(pids, vec![9, 7, 8, 1]);
    }

    #[test]
    fn session_and_alive_fence_on_identity_and_state() {
        let rows = vec![
            super::ProcessRow {
                pid: 5,
                parent_pid: 1,
                session: 9,
                state: "S".to_owned(),
                started: "a".to_owned(),
            },
            super::ProcessRow {
                pid: 6,
                parent_pid: 1,
                session: 9,
                state: "Z".to_owned(),
                started: "a".to_owned(),
            },
        ];
        let snapshot = ProcessSnapshot::new(rows, true);
        assert_eq!(snapshot.session(9).len(), 2);
        assert!(snapshot.alive(&ProcessIdentity {
            pid: 5,
            started: "a".to_owned()
        }));
        assert!(!snapshot.alive(&ProcessIdentity {
            pid: 6,
            started: "a".to_owned()
        }));
        assert!(!snapshot.alive(&ProcessIdentity {
            pid: 5,
            started: "other".to_owned()
        }));
        assert!(snapshot.complete());
    }

    #[test]
    fn read_arm_matches_fd_zero() {
        let fake = FakeProc::new();
        fake.stat_entry(stat_row(100, 100, 34816, 100, 9));
        fake.insert_device("/proc/100/fd/0".to_owned(), 34816);
        fake.insert_device("/proc/100/task/100/fd/0".to_owned(), 34816);
        fake.insert_syscall(
            100,
            100,
            SyscallInfo {
                number: 0,
                args: [0, 0, 0, 0, 0, 0],
            },
        );
        assert!(super::is_stdin_waiting(&fake, 100, 100));
        fake.insert_syscall(
            100,
            100,
            SyscallInfo {
                number: 0,
                args: [3, 0, 0, 0, 0, 0],
            },
        );
        assert!(!super::is_stdin_waiting(&fake, 100, 100));
    }

    #[test]
    fn select_arm_decodes_the_first_fd_set_word() {
        let fake = FakeProc::new();
        fake.stat_entry(stat_row(100, 100, 34816, 100, 9));
        fake.insert_device("/proc/100/fd/0".to_owned(), 34816);
        fake.insert_device("/proc/100/task/100/fd/0".to_owned(), 34816);
        // pselect6 on x64 (270) with an fd-set pointer whose first word has
        // fd 0 set.
        fake.insert_syscall(
            100,
            100,
            SyscallInfo {
                number: 270,
                args: [1, 0x5000, 0, 0, 0, 0],
            },
        );
        fake.insert_mem(100, 0x5000, vec![0b0000_0001, 0, 0, 0, 0, 0, 0, 0]);
        assert!(super::is_stdin_waiting(&fake, 100, 100));
        fake.insert_mem(100, 0x5000, vec![0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(!super::is_stdin_waiting(&fake, 100, 100));
    }

    #[test]
    fn poll_arm_scans_struct_pollfd_entries() {
        let fake = FakeProc::new();
        fake.stat_entry(stat_row(100, 100, 34816, 100, 9));
        fake.insert_device("/proc/100/fd/0".to_owned(), 34816);
        fake.insert_device("/proc/100/task/100/fd/0".to_owned(), 34816);
        // ppoll on x64 (271): fds pointer 0x6000, count 2; the second entry
        // watches fd 0 with POLLIN.
        let mut entries = vec![0_u8; 16];
        entries[8..12].copy_from_slice(&0_i32.to_le_bytes());
        entries[12..14].copy_from_slice(&1_u16.to_le_bytes());
        fake.insert_syscall(
            100,
            100,
            SyscallInfo {
                number: 271,
                args: [0x6000, 2, 0, 0, 0, 0],
            },
        );
        fake.insert_mem(100, 0x6000, entries);
        assert!(super::is_stdin_waiting(&fake, 100, 100));
    }

    #[test]
    fn epoll_arm_reads_the_thread_fdinfo_tfd_records() {
        let fake = FakeProc::new();
        fake.stat_entry(stat_row(100, 100, 34816, 100, 9));
        fake.insert_device("/proc/100/fd/0".to_owned(), 34816);
        fake.insert_device("/proc/100/task/100/fd/0".to_owned(), 34816);
        // epoll_pwait on x64 (281): epfd 3, size 1 (the a2 gate).
        fake.insert_syscall(
            100,
            100,
            SyscallInfo {
                number: 281,
                args: [3, 0, 1, 0, 0, 0],
            },
        );
        fake.insert_fdinfo(100, 100, 3, "tfd:       5 socket:[123]\n0 pos: 0\n");
        assert!(!super::is_stdin_waiting(&fake, 100, 100));
        fake.insert_fdinfo(100, 100, 3, "tfd:       5 socket:[123]\ntfd:       0\n");
        assert!(super::is_stdin_waiting(&fake, 100, 100));
    }

    #[test]
    fn foreign_terminal_device_blocks_the_stdin_read_claim() {
        let fake = FakeProc::new();
        fake.stat_entry(stat_row(100, 100, 34816, 100, 9));
        fake.insert_device("/proc/100/fd/0".to_owned(), 34816);
        fake.stat_entry(stat_row(200, 100, 34817, 200, 9));
        fake.insert_device("/proc/200/fd/0".to_owned(), 34817);
        fake.insert_device("/proc/200/task/200/fd/0".to_owned(), 34817);
        fake.insert_syscall(
            200,
            200,
            SyscallInfo {
                number: 0,
                args: [0, 0, 0, 0, 0, 0],
            },
        );
        assert!(!super::is_stdin_waiting(&fake, 100, 100));
    }
}
