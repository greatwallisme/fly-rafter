// Rust guideline compliant 2026-10-29
//! `fr-landlock-run`: the self-restrict-then-exec Landlock launcher (a
//! full port of dsh `native/system/packages/entry/src/main.c`; the argv
//! grammar, exit codes, and report lines are pinned by the dsh CLI
//! contract `native/system/docs/cli-contract.md`).
//!
//! The launcher installs a Landlock ruleset on itself and `exec`s the
//! wrapped command; the ruleset is inherited across `execve`, so the
//! command (and every process it spawns) runs confined while the
//! invoking process stays unrestricted.
//!
//! CLI contract (mirrors the `bwrap` runner argv shape the executor
//! wraps):
//!
//! ```text
//! fr-landlock-run [--ro <path>]... [--rw <path>]... -- <argv>...
//! fr-landlock-run --probe
//! ```
//!
//! `--ro` grants read+execute beneath the path; `--rw` grants full
//! filesystem access beneath the path. Everything else is denied
//! (Landlock is an allow-list). `--probe` builds a maximal ruleset and
//! reports whether the running kernel actually enforces it.
//!
//! Fail-closed: if the ruleset cannot be created or is NOT enforced by
//! the kernel, the launcher exits non-zero WITHOUT exec'ing the
//! command. A partial (best-effort) enforcement on an older ABI is
//! accepted and reported on stderr.
//!
//! Compiled only for Linux targets; other targets build an empty shell
//! so the workspace and cross-compile gates stay green (the wasm-crate
//! cfg-gating judgment).

#[cfg(target_os = "linux")]
mod launcher {
    //! The Landlock UAPI, defined locally instead of via
    //! `linux/landlock.h`: the kernel's user-space ABI is stable by
    //! contract, and self-defining it keeps the build independent of the
    //! toolchain's header vintage (the dsh launcher's judgment, kept
    //! verbatim in structure).
    #[repr(C)]
    struct LandlockRulesetAttr {
        handled_access_fs: u64,
    }

    #[repr(C, packed)]
    struct LandlockPathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
    }

    const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
    const LANDLOCK_RULE_PATH_BENEATH: i32 = 1;

    // Filesystem access bits, grouped by the Landlock ABI that
    // introduced them.
    const LL_FS_EXECUTE: u64 = 1 << 0; // ABI 1
    const LL_FS_WRITE_FILE: u64 = 1 << 1;
    const LL_FS_READ_FILE: u64 = 1 << 2;
    const LL_FS_READ_DIR: u64 = 1 << 3;
    const LL_FS_REMOVE_DIR: u64 = 1 << 4;
    const LL_FS_REMOVE_FILE: u64 = 1 << 5;
    const LL_FS_MAKE_CHAR: u64 = 1 << 6;
    const LL_FS_MAKE_DIR: u64 = 1 << 7;
    const LL_FS_MAKE_REG: u64 = 1 << 8;
    const LL_FS_MAKE_SOCK: u64 = 1 << 9;
    const LL_FS_MAKE_FIFO: u64 = 1 << 10;
    const LL_FS_MAKE_BLOCK: u64 = 1 << 11;
    const LL_FS_MAKE_SYM: u64 = 1 << 12;
    const LL_FS_REFER: u64 = 1 << 13; // ABI 2
    const LL_FS_TRUNCATE: u64 = 1 << 14; // ABI 3 (ABI 4 added TCP bits only)
    const LL_FS_IOCTL_DEV: u64 = 1 << 15; // ABI 5

    // Every ABI-1 access (bits 0..12) — spelled as the full OR so the
    // table doubles as the audit record of which accesses this launcher
    // knows; numerically `LL_FS_REFER - 1` (the dsh spelling).
    const LL_ABI1_MASK: u64 = LL_FS_EXECUTE
        | LL_FS_WRITE_FILE
        | LL_FS_READ_FILE
        | LL_FS_READ_DIR
        | LL_FS_REMOVE_DIR
        | LL_FS_REMOVE_FILE
        | LL_FS_MAKE_CHAR
        | LL_FS_MAKE_DIR
        | LL_FS_MAKE_REG
        | LL_FS_MAKE_SOCK
        | LL_FS_MAKE_FIFO
        | LL_FS_MAKE_BLOCK
        | LL_FS_MAKE_SYM;

    /// Newest ABI this build knows; the negotiation below scales the
    /// actual ruleset down to what the running kernel supports.
    const MAX_ABI: i64 = 5;

    // Landlock has no libc wrappers; these are the raw syscalls. The
    // numbers are identical on every architecture (the post-2011
    // unified table) — the fallbacks only matter to a libc older than
    // the feature.
    unsafe fn landlock_create_ruleset(
        attr: *const LandlockRulesetAttr,
        size: usize,
        flags: u32,
    ) -> i64 {
        // SAFETY: the caller owns the attr record for the call (or passes
        // null with size 0 for the ABI probe); the raw syscall has no
        // contract beyond the kernel's stable UAPI.
        unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, attr, size, flags) }
    }

    unsafe fn landlock_add_rule(
        fd: i32,
        rule_type: i32,
        attr: *const LandlockPathBeneathAttr,
    ) -> i64 {
        // SAFETY: the caller owns the packed rule record for the call and
        // the open ruleset descriptor.
        unsafe { libc::syscall(libc::SYS_landlock_add_rule, fd, rule_type, attr, 0) }
    }

    unsafe fn landlock_restrict_self(fd: i32) -> i64 {
        // SAFETY: the caller owns the open ruleset descriptor and calls
        // from the thread it wants restricted.
        unsafe { libc::syscall(libc::SYS_landlock_restrict_self, fd, 0) }
    }

    /// Every fatal launcher error prints `landlock-run: <message>` to
    /// stderr and exits 125 — a code the wrapped command itself is
    /// unlikely to use, so the executor can tell launcher failures from
    /// command failures.
    const EXIT_LAUNCHER_FAILURE: i32 = 125;

    const NOT_ENFORCED_MESSAGE: &str =
        "landlock is not enforced by this kernel (ABI unsupported or disabled)";

    /// Prints one fatal `landlock-run: ...` line; returns the fatal
    /// exit code.
    fn fail(prefix: &str, detail: Option<&str>) -> i32 {
        match detail {
            Some(detail) => eprintln!("landlock-run: {prefix}: {detail}"),
            None => eprintln!("landlock-run: {prefix}"),
        }
        EXIT_LAUNCHER_FAILURE
    }

    fn fail_usage(message: &str, detail: Option<&str>) -> i32 {
        match detail {
            Some(detail) => eprintln!("landlock-run: usage error: {message}{detail}"),
            None => eprintln!("landlock-run: usage error: {message}"),
        }
        EXIT_LAUNCHER_FAILURE
    }

    /// Parsed CLI: either a probe, or grants plus the command argv after
    /// `--`.
    struct Cli {
        probe: bool,
        ro: Vec<String>,
        rw: Vec<String>,
        command: Vec<String>,
    }

    /// Hand-rolled argv parsing — four flags do not justify a parsing
    /// library. Returns the parsed CLI or the process exit code (message
    /// already printed).
    fn parse(args: &[String]) -> Result<Cli, i32> {
        let mut cli = Cli {
            probe: false,
            ro: Vec::new(),
            rw: Vec::new(),
            command: Vec::new(),
        };
        let mut index = 0;
        while index < args.len() {
            let arg = args[index].as_str();
            if arg == "--probe" {
                // dsh checks `argc != 2` including argv[0]; this parser
                // already dropped argv[0], so the sole-argument form is
                // exactly one entry.
                if args.len() != 1 {
                    return Err(fail_usage("--probe takes no other arguments", None));
                }
                cli.probe = true;
                index += 1;
            } else if arg == "--ro" || arg == "--rw" {
                let Some(path) = args.get(index + 1) else {
                    return Err(fail_usage(arg, Some(" requires a path")));
                };
                if arg == "--ro" {
                    cli.ro.push(path.clone());
                } else {
                    cli.rw.push(path.clone());
                }
                index += 2;
            } else if arg == "--" {
                cli.command = args[index + 1..].to_vec();
                break;
            } else {
                return Err(fail_usage("unknown argument: ", Some(arg)));
            }
        }
        if !cli.probe && cli.command.is_empty() {
            return Err(fail_usage("missing `-- <argv>...` command", None));
        }
        Ok(cli)
    }

    /// The filesystem accesses the running kernel's ABI can govern.
    const fn fs_mask_for_abi(abi: i64) -> u64 {
        let mut mask = LL_ABI1_MASK;
        if abi >= 2 {
            mask |= LL_FS_REFER;
        }
        if abi >= 3 {
            mask |= LL_FS_TRUNCATE;
        }
        if abi >= 5 {
            mask |= LL_FS_IOCTL_DEV;
        }
        mask
    }

    /// Adds one path-beneath rule; 0 on success, else the exit code.
    fn add_rule(ruleset_fd: i32, path: &str, access: u64) -> i32 {
        // A path containing a NUL byte cannot be a valid filesystem
        // name; refusing keeps the CString construction total.
        let Ok(cpath) = std::ffi::CString::new(path) else {
            eprintln!("landlock-run: cannot open rule path: {path}: argument contains a NUL byte");
            return EXIT_LAUNCHER_FAILURE;
        };
        // SAFETY: `cpath` owns its NUL-terminated buffer for the call;
        // O_PATH never reads the file's content.
        let path_fd = unsafe { libc::open(cpath.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if path_fd < 0 {
            // Fail closed on an unopenable grant root: silently
            // narrowing the granted set would be safe, but running with
            // a profile the caller did not get is not worth the
            // ambiguity.
            let detail = std::io::Error::last_os_error();
            eprintln!("landlock-run: cannot open rule path: {path}: {detail}");
            return EXIT_LAUNCHER_FAILURE;
        }
        // The kernel rejects directory-only accesses on a non-directory
        // rule (EINVAL), so a file grant keeps only the file-compatible
        // bits — how the `--rw /dev/null` grant works.
        let mut file_access = access;
        // SAFETY: an all-zero `stat` is the valid zeroed representation
        // fstat overwrites.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `path_fd` is the open descriptor above and `stat` is a
        // valid out-slot.
        let stat_result = unsafe { libc::fstat(path_fd, &raw mut stat) };
        if stat_result == 0 && (stat.st_mode & libc::S_IFMT) != libc::S_IFDIR {
            file_access &= LL_FS_EXECUTE
                | LL_FS_WRITE_FILE
                | LL_FS_READ_FILE
                | LL_FS_TRUNCATE
                | LL_FS_IOCTL_DEV;
        }
        let attr = LandlockPathBeneathAttr {
            allowed_access: file_access,
            parent_fd: path_fd,
        };
        // SAFETY: `attr` owns its packed bytes for the call and
        // `ruleset_fd` is the open descriptor from the caller.
        let added =
            unsafe { landlock_add_rule(ruleset_fd, LANDLOCK_RULE_PATH_BENEATH, &raw const attr) };
        if added != 0 {
            let detail = std::io::Error::last_os_error();
            // SAFETY: the descriptor is open and closed exactly once on
            // this failure path.
            let _ignored = unsafe { libc::close(path_fd) };
            return fail("landlock ruleset error", Some(&detail.to_string()));
        }
        // SAFETY: same single-close contract on the success path.
        let _ignored = unsafe { libc::close(path_fd) };
        0
    }

    /// Installs the ruleset on the current thread, negotiating the
    /// kernel's ABI down from `MAX_ABI` (dsh `restrict_self`). `--ro`
    /// paths get the read side of the vocabulary (read file/dir +
    /// execute — the wrapped `bash` and everything it spawns must remain
    /// executable); `--rw` paths get every filesystem access the
    /// negotiated ABI can grant. Sets `no_new_privs` first (mandatory
    /// for an unprivileged restrict, and it neutralizes
    /// setuid/setgid escalation inside the sandbox). On success
    /// `partial` reports whether the kernel governs only a subset of
    /// `MAX_ABI`'s accesses.
    fn restrict_self(cli: &Cli) -> Result<bool, i32> {
        // SAFETY: the ABI probe passes null with size 0 — no caller-owned
        // memory is touched.
        let abi = unsafe {
            landlock_create_ruleset(std::ptr::null(), 0, LANDLOCK_CREATE_RULESET_VERSION)
        };
        if abi < 0 {
            // ENOSYS: kernel built without Landlock; EOPNOTSUPP: built
            // but disabled. Either way: not enforceable — fail CLOSED,
            // never exec unconfined.
            return Err(fail(NOT_ENFORCED_MESSAGE, None));
        }
        let partial = abi < MAX_ABI;
        let handled = fs_mask_for_abi(abi.min(MAX_ABI));

        let attr = LandlockRulesetAttr {
            handled_access_fs: handled,
        };
        // SAFETY: `attr` owns its bytes for the call and the size matches
        // the record the kernel expects.
        let ruleset_fd_raw = unsafe {
            landlock_create_ruleset(
                &raw const attr,
                std::mem::size_of::<LandlockRulesetAttr>(),
                0,
            )
        };
        if ruleset_fd_raw < 0 {
            let detail = std::io::Error::last_os_error();
            return Err(fail("landlock ruleset error", Some(&detail.to_string())));
        }
        // A kernel file descriptor fits i32 by contract; the check keeps
        // the cast honest.
        let ruleset_fd = match i32::try_from(ruleset_fd_raw) {
            Ok(fd) => fd,
            Err(error) => {
                return Err(fail(
                    "landlock ruleset error",
                    Some(&format!("impossible ruleset descriptor: {error}")),
                ));
            }
        };

        let read_side = LL_FS_EXECUTE | LL_FS_READ_FILE | LL_FS_READ_DIR;
        for path in &cli.ro {
            let code = add_rule(ruleset_fd, path, read_side & handled);
            if code != 0 {
                // SAFETY: the descriptor is open and closed exactly once
                // on this failure path.
                let _ignored = unsafe { libc::close(ruleset_fd) };
                return Err(code);
            }
        }
        for path in &cli.rw {
            let code = add_rule(ruleset_fd, path, handled);
            if code != 0 {
                // SAFETY: same single-close contract.
                let _ignored = unsafe { libc::close(ruleset_fd) };
                return Err(code);
            }
        }

        // SAFETY: PR_SET_NO_NEW_PRIVS takes scalar arguments only.
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            let detail = std::io::Error::last_os_error();
            // SAFETY: same single-close contract.
            let _ignored = unsafe { libc::close(ruleset_fd) };
            return Err(fail("landlock ruleset error", Some(&detail.to_string())));
        }
        // SAFETY: `ruleset_fd` is the open descriptor of this scope.
        if unsafe { landlock_restrict_self(ruleset_fd) } != 0 {
            let detail = std::io::Error::last_os_error();
            // SAFETY: same single-close contract.
            let _ignored = unsafe { libc::close(ruleset_fd) };
            return Err(fail("landlock ruleset error", Some(&detail.to_string())));
        }
        // SAFETY: the success-path close.
        let _ignored = unsafe { libc::close(ruleset_fd) };
        Ok(partial)
    }

    fn run(args: &[String]) -> i32 {
        let cli = match parse(args) {
            Ok(cli) => cli,
            Err(code) => return code,
        };
        if cli.probe {
            // The functional probe: build and enforce a maximal ruleset
            // in THIS short-lived process (the probe run exits right
            // after). `--version` style checks would miss a kernel that
            // has the syscalls but refuses enforcement; actually
            // restricting is the only honest signal. The one report
            // line is part of the launcher CLI contract.
            let probe = Cli {
                probe: false,
                ro: vec!["/".to_owned()],
                rw: Vec::new(),
                command: Vec::new(),
            };
            let partial = match restrict_self(&probe) {
                Ok(partial) => partial,
                Err(code) => return code,
            };
            println!(
                "landlock: {}",
                if partial {
                    "partially enforced (older ABI)"
                } else {
                    "fully enforced"
                }
            );
            return 0;
        }
        let partial = match restrict_self(&cli) {
            Ok(partial) => partial,
            Err(code) => return code,
        };
        if partial {
            // Older ABI: some handled accesses are not governed (e.g.
            // truncate before ABI 3). Still confined for everything the
            // kernel supports — report, do not refuse.
            eprintln!("landlock-run: partial enforcement (older Landlock ABI)");
        }
        let program = cli.command[0].clone();
        let cargv: Vec<std::ffi::CString> = cli
            .command
            .iter()
            .map(|argument| std::ffi::CString::new(argument.as_str()).unwrap_or_default())
            .collect();
        let mut raw: Vec<*const libc::c_char> = cargv.iter().map(|c| c.as_ptr()).collect();
        raw.push(std::ptr::null());
        let program = std::ffi::CString::new(program).unwrap_or_default();
        // SAFETY: execvp only returns on failure; the argv array and
        // every string outlive the call up to the exec.
        unsafe { libc::execvp(program.as_ptr(), raw.as_mut_ptr().cast()) };
        let detail = std::io::Error::last_os_error();
        fail("exec failed", Some(&detail.to_string()))
    }

    pub fn main() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        std::process::exit(run(&args));
    }
}

#[cfg(target_os = "linux")]
fn main() {
    launcher::main();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    // The launcher exists only on Linux hosts (the wasm-crate cfg-gating
    // judgment: other targets build an empty shell so the workspace and
    // cross-compile gates stay green).
}
