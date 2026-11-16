//! The one module that names `libc`: signal masks and `sigwait` for the
//! runner, `kill` of the runner's own child (and of the runner itself, to
//! wake its main thread), `wait4` for a child's end and usage,
//! `getpwuid_r` for the invoking user, `getentropy` for ids and, on macOS,
//! `sysctlbyname` for the boot session and the process calls of the
//! launchd backend: `proc_pidinfo` for a process's identity and start
//! time, `proc_listchildpids` and `proc_listpgrppids` for its tree and
//! group, `proc_pid_rusage` and `mach_timebase_info` for its live
//! counters. Every `unsafe` block carries a `SAFETY` comment.

use std::ffi::{CStr, OsStr};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Command, ExitStatus};
use std::ptr;

use crate::error::Error;
use crate::platform::launchd::rusage::EndUsage;

pub use libc::{SIGCHLD, SIGHUP, SIGINT, SIGTERM};

/// First size of the string buffer handed to `getpwuid_r`; it doubles on
/// `ERANGE`.
const PASSWD_BUFFER_START: usize = 1024;
/// Largest string buffer tried before the lookup is reported as failed.
const PASSWD_BUFFER_MAX: usize = 1024 * 1024;
/// Most processes one walk of a process tree visits, so a fork storm
/// under one run cannot hold its caller.
#[cfg(any(target_os = "macos", test))]
const TREE_MAX: usize = 4096;
/// First number of pids asked of a libproc listing; it doubles while the
/// answer fills the buffer, up to [`TREE_MAX`].
#[cfg(target_os = "macos")]
const PID_LIST_START: usize = 64;
/// Bytes per unit of `ru_maxrss`: bytes on macOS.
#[cfg(target_os = "macos")]
const MAXRSS_UNIT: u64 = 1;
/// Bytes per unit of `ru_maxrss`: kilobytes on Linux and the BSDs.
#[cfg(not(target_os = "macos"))]
const MAXRSS_UNIT: u64 = 1024;
/// Microseconds per second, for `timeval`.
const MICROS_PER_SEC: u64 = 1_000_000;

/// One process as the kernel describes it. Its pid and start time
/// together tell it apart from a later process that reuses the pid; an
/// `exec` keeps both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// The pid.
    pub pid: u32,
    /// The parent's pid.
    pub parent: u32,
    /// The process group.
    pub group: u32,
    /// Start time, unix seconds: what `run/state.json` records as
    /// `processStart`.
    pub start_secs: u64,
    /// Microseconds past `start_secs`.
    pub start_usecs: u64,
    /// The process exited and its parent has not reaped it yet.
    pub zombie: bool,
}

impl ProcessIdentity {
    /// Whether `now`, read later for the same pid, is still this process:
    /// the same pid started at the same microsecond.
    #[must_use]
    pub fn same_process(&self, now: &Self) -> bool {
        self.pid == now.pid
            && self.start_secs == now.start_secs
            && self.start_usecs == now.start_usecs
    }
}

/// How a child ended and what it used, as `wait4` reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reaped {
    /// The wait status.
    pub status: ExitStatus,
    /// CPU time and peak memory of the child and of the descendants it
    /// waited for; `None` when a value does not convert (a negative time,
    /// an overflowing sum), never an invented zero.
    pub usage: Option<EndUsage>,
}

/// The invoking user, from the passwd database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    /// Login name.
    pub name: String,
    /// Home directory.
    pub home: PathBuf,
    /// Numeric uid.
    pub uid: u32,
}

/// Blocks `signals` in the calling thread. Called in `main` before any
/// thread is spawned, so every thread inherits the mask and the child
/// gets a clean one through `pre_exec` unblocking.
pub fn block_signals(signals: &[i32]) -> Result<(), Error> {
    let set = signal_set(signals)?;
    // SAFETY: `set` is an initialised signal set and a null pointer for the
    // previous mask is allowed.
    let code = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, ptr::null_mut()) };
    check(code).map_err(|source| Error::io("block", format!("signals {signals:?}"), source))
}

/// Waits for one of `signals` with `sigwait`. Blocks.
pub fn wait_signal(signals: &[i32]) -> Result<i32, Error> {
    let set = signal_set(signals)?;
    let mut received: libc::c_int = 0;
    // SAFETY: `set` is an initialised signal set and `received` is a valid,
    // writable place for the signal number, both alive for the call.
    let code = unsafe { libc::sigwait(&raw const set, &raw mut received) };
    check(code).map_err(|source| Error::io("wait for", format!("signals {signals:?}"), source))?;
    Ok(received)
}

/// Sends `signal` to `pid`. `ESRCH` is `Ok(false)`.
///
/// A pid is only safe to signal while it cannot have been reused, and the
/// runner signals only its own unreaped child: its main thread both kills
/// and reaps dsh, so a reaped pid is never signalled. Every other thread
/// that needs dsh stopped wakes the main thread instead, with SIGCHLD to
/// the runner's own pid.
pub fn kill(pid: u32, signal: i32) -> Result<bool, Error> {
    let target = match libc::pid_t::try_from(pid) {
        Ok(target) if target > 0 => target,
        // 0 and anything that wraps negative would address a process group.
        _ => {
            return Err(Error::io(
                "signal",
                format!("pid {pid}"),
                io::Error::from(io::ErrorKind::InvalidInput),
            ));
        }
    };
    // SAFETY: kill(2) takes plain integers and touches no memory; a positive
    // pid names exactly one process.
    if unsafe { libc::kill(target, signal) } == 0 {
        return Ok(true);
    }
    let source = io::Error::last_os_error();
    if source.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(Error::io("signal", format!("pid {pid}"), source))
    }
}

/// Reaps the caller's own child `pid` with `wait4`, blocking until it
/// ends, or with `nohang` answering `Ok(None)` while it still runs. An
/// interrupted wait is retried. Once this returns `Some` the pid is
/// released: a `std::process::Child` for it must not be waited for again,
/// which would fail with `ECHILD`.
pub fn reap(pid: u32, nohang: bool) -> Result<Option<Reaped>, Error> {
    let failed = |source| Error::io("wait for", format!("pid {pid}"), source);
    // 0 and anything that wraps negative would wait for a group or any child.
    let target = positive_pid(pid).ok_or_else(|| failed(io::ErrorKind::InvalidInput.into()))?;
    let options = if nohang { libc::WNOHANG } else { 0 };
    loop {
        let mut status: libc::c_int = 0;
        // SAFETY: `rusage` is plain data (integers and timevals), for which
        // all zero bytes are a valid value.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: `status` and `usage` are valid, writable and alive for the
        // call; a positive pid names exactly one child, so no other child of
        // this process is reaped.
        let reaped = unsafe { libc::wait4(target, &raw mut status, options, &raw mut usage) };
        if reaped == target {
            return Ok(Some(Reaped {
                status: ExitStatus::from_raw(status),
                usage: end_usage(&usage),
            }));
        }
        if reaped == 0 {
            return Ok(None);
        }
        if reaped > 0 {
            return Err(failed(io::Error::other(format!(
                "wait4 answered for pid {reaped}"
            ))));
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(failed(source));
        }
    }
}

/// Makes the owned child's stdin nonblocking before the main thread cancels permissions.
/// A full pipe then asks the caller to use its signal fallback instead of blocking supervision.
pub(crate) fn nonblocking_child_input(input: &impl AsRawFd) -> io::Result<()> {
    let fd = input.as_raw_fd();
    // SAFETY: input keeps this descriptor valid; F_GETFL reads only its flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: F_SETFL updates only the valid descriptor's status flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Makes `output` the process's stdout and stderr: `dup2` onto descriptors
/// 1 and 2. The launchd runner's per-run log, which `dsh` and every line
/// the runner writes then reach, as the journal does on Linux.
pub fn redirect_output(output: &impl AsRawFd) -> Result<(), Error> {
    let fd = output.as_raw_fd();
    for target in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        // SAFETY: dup2(2) takes two descriptor numbers and touches no
        // memory; `output` keeps `fd` open for the call, and replacing 1 and
        // 2 closes only what they named before, which no Rust value owns.
        if unsafe { libc::dup2(fd, target) } < 0 {
            return Err(Error::io(
                "redirect",
                format!("descriptor {target}"),
                io::Error::last_os_error(),
            ));
        }
    }
    Ok(())
}

/// `getuid` then `getpwuid_r`.
pub fn current_user() -> Result<User, Error> {
    // SAFETY: getuid(2) has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let lookup_error = |source| {
        Error::io(
            "look up",
            format!("uid {uid} in the passwd database"),
            source,
        )
    };
    let sizes = std::iter::successors(Some(PASSWD_BUFFER_START), |size| size.checked_mul(2))
        .take_while(|size| *size <= PASSWD_BUFFER_MAX);
    for size in sizes {
        match passwd_entry(uid, size) {
            Ok(Some(user)) => return Ok(user),
            Ok(None) => return Err(lookup_error(io::Error::from(io::ErrorKind::NotFound))),
            Err(source) if source.raw_os_error() == Some(libc::ERANGE) => {}
            Err(source) => return Err(lookup_error(source)),
        }
    }
    Err(lookup_error(io::Error::from_raw_os_error(libc::ERANGE)))
}

/// 16 bytes from the kernel's entropy pool, through `getentropy`: the
/// source of instance and invocation ids where no `/proc` exists.
pub fn random_16() -> Result<[u8; 16], Error> {
    let mut bytes = [0_u8; 16];
    // SAFETY: the buffer is valid for writes of its length, which is below
    // getentropy's limit of 256 bytes.
    let code = unsafe { libc::getentropy(bytes.as_mut_ptr().cast(), bytes.len()) };
    if code == 0 {
        Ok(bytes)
    } else {
        Err(Error::io(
            "read",
            "the kernel entropy pool (getentropy)",
            io::Error::last_os_error(),
        ))
    }
}

/// `kern.bootsessionuuid` through `sysctlbyname`, trimmed and lowercased:
/// the boot identity on macOS. It is only compared for equality.
#[cfg(target_os = "macos")]
pub fn boot_session_uuid() -> Result<String, Error> {
    const NAME: &[u8] = b"kern.bootsessionuuid\0";
    let failed = |source| Error::io("read", "the sysctl kern.bootsessionuuid", source);
    let mut buffer = [0_u8; 64];
    let mut length: libc::size_t = buffer.len();
    // SAFETY: NAME is NUL-terminated; buffer and length describe one
    // writable buffer of that size; a null new value sets nothing.
    let code = unsafe {
        libc::sysctlbyname(
            NAME.as_ptr().cast(),
            buffer.as_mut_ptr().cast(),
            &raw mut length,
            ptr::null_mut(),
            0,
        )
    };
    if code != 0 {
        return Err(failed(io::Error::last_os_error()));
    }
    let text = buffer.get(..length).unwrap_or_default();
    let text = text.strip_suffix(&[0]).unwrap_or(text);
    let text = std::str::from_utf8(text)
        .map_err(|_| failed(io::Error::from(io::ErrorKind::InvalidData)))?;
    Ok(text.trim().to_ascii_lowercase())
}

/// The kernel's description of process `pid` (`proc_pidinfo` with
/// `PROC_PIDTBSDINFO`), zombies included; `None` when no such process
/// exists or the caller may not read it.
#[cfg(target_os = "macos")]
#[must_use]
pub fn process_identity(pid: u32) -> Option<ProcessIdentity> {
    let target = positive_pid(pid)?;
    let size = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `proc_bsdinfo` is plain data (integers and byte arrays), for
    // which all zero bytes are a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    // SAFETY: flavor PROC_PIDTBSDINFO writes at most one `proc_bsdinfo`;
    // `info` is one, writable and alive for the call, and `size` is its
    // size in bytes.
    let written = unsafe {
        libc::proc_pidinfo(
            target,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast(),
            size,
        )
    };
    // A failed call answers 0 (errno set), a short one describes nothing.
    if written != size || info.pbi_pid != pid {
        return None;
    }
    Some(ProcessIdentity {
        pid,
        parent: info.pbi_ppid,
        group: info.pbi_pgid,
        start_secs: info.pbi_start_tvsec,
        start_usecs: info.pbi_start_tvusec,
        zombie: info.pbi_status == libc::SZOMB,
    })
}

/// The start time of process `pid`, unix seconds: the `processStart` the
/// runner records for itself and `get` compares with the job's pid.
/// `None` when the process is gone (a zombie included) or unreadable, so
/// no state file reads live for it.
#[cfg(target_os = "macos")]
#[must_use]
pub fn process_start(pid: u32) -> Option<u64> {
    process_identity(pid)
        .filter(|identity| !identity.zombie)
        .map(|identity| identity.start_secs)
}

/// The pids whose parent is `pid` now (`proc_listchildpids`), zombies
/// included. Empty for a pid without children, a pid that is gone, or a
/// failed call: libproc answers all three alike.
#[cfg(target_os = "macos")]
#[must_use]
pub fn child_pids(pid: u32) -> Vec<u32> {
    let Some(parent) = positive_pid(pid) else {
        return Vec::new();
    };
    listed_pids(|buffer, bytes| {
        // SAFETY: `listed_pids` hands a buffer writable for `bytes` bytes
        // and alive for the call; the kernel writes whole pids into it, at
        // most that many bytes.
        unsafe { libc::proc_listchildpids(parent, buffer, bytes) }
    })
}

/// The pids in process group `group` now (`proc_listpgrppids`), zombies
/// included; empty as for [`child_pids`].
#[cfg(target_os = "macos")]
#[must_use]
pub fn group_pids(group: u32) -> Vec<u32> {
    let Some(group) = positive_pid(group) else {
        return Vec::new();
    };
    listed_pids(|buffer, bytes| {
        // SAFETY: as in `child_pids`: a buffer writable for `bytes` bytes,
        // alive for the call, filled with whole pids only.
        unsafe { libc::proc_listpgrppids(group, buffer, bytes) }
    })
}

/// The process tree under `root`, `root` first, breadth first: every
/// descendant whose identity still names the parent it was found under,
/// so a pid that ended and was reused during the walk is left out.
/// Zombies are kept and marked. At most [`TREE_MAX`] processes; an empty
/// list when `root` itself is gone.
#[cfg(target_os = "macos")]
#[must_use]
pub fn process_tree(root: u32) -> Vec<ProcessIdentity> {
    walk(root, child_pids)
        .into_iter()
        .filter_map(|(pid, parent)| {
            let identity = process_identity(pid)?;
            parent
                .is_none_or(|parent| identity.parent == parent)
                .then_some(identity)
        })
        .collect()
}

/// One process's counters for the live `resources` of `get`:
/// `proc_pid_rusage` (`RUSAGE_INFO_V4`) for its CPU ticks, footprint and
/// disk bytes, `proc_pidinfo` (`PROC_PIDTASKINFO`) for its thread count.
/// `None` when either call fails, as it does for a zombie, which has no
/// task left.
#[cfg(target_os = "macos")]
#[must_use]
pub fn task_sample(pid: u32) -> Option<crate::platform::launchd::rusage::TaskSample> {
    let target = positive_pid(pid)?;
    // SAFETY: `rusage_info_v4` is plain data (integers and a byte array),
    // for which all zero bytes are a valid value.
    let mut usage: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: flavor RUSAGE_INFO_V4 makes the kernel write one
    // `rusage_info_v4`, which `usage` is, writable and alive for the call.
    // The C prototype types the buffer `rusage_info_t *` (`void **`) while
    // it points at that struct, hence the cast.
    let code = unsafe {
        libc::proc_pid_rusage(
            target,
            libc::RUSAGE_INFO_V4,
            (&raw mut usage).cast::<libc::rusage_info_t>(),
        )
    };
    if code != 0 {
        return None;
    }
    let size = libc::c_int::try_from(size_of::<libc::proc_taskinfo>()).ok()?;
    // SAFETY: `proc_taskinfo` is plain data (integers), for which all zero
    // bytes are a valid value.
    let mut task: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    // SAFETY: flavor PROC_PIDTASKINFO writes at most one `proc_taskinfo`;
    // `task` is one, writable and alive for the call, and `size` is its
    // size in bytes.
    let written = unsafe {
        libc::proc_pidinfo(
            target,
            libc::PROC_PIDTASKINFO,
            0,
            (&raw mut task).cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    Some(crate::platform::launchd::rusage::TaskSample {
        user_ticks: usage.ri_user_time,
        system_ticks: usage.ri_system_time,
        phys_footprint: usage.ri_phys_footprint,
        threads: u64::try_from(task.pti_threadnum).ok()?,
        disk_read: usage.ri_diskio_bytesread,
        disk_written: usage.ri_diskio_byteswritten,
    })
}

/// `mach_timebase_info`: the ratio that turns the mach ticks of a
/// `TaskSample` into nanoseconds (1/1 on Intel, 125/3 on Apple silicon).
/// `None` when the call fails or answers a zero denominator.
#[cfg(target_os = "macos")]
#[must_use]
#[expect(
    deprecated,
    reason = "libc deprecates its mach bindings in favour of the mach2 crate; one stable call does not earn a dependency"
)]
pub fn timebase() -> Option<crate::platform::launchd::rusage::Timebase> {
    let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
    // SAFETY: `info` is a valid, writable `mach_timebase_info`, alive for
    // the call, which fills its two fields and touches nothing else.
    let code = unsafe { libc::mach_timebase_info(&raw mut info) };
    (code == 0 && info.denom != 0).then_some(crate::platform::launchd::rusage::Timebase {
        numerator: info.numer,
        denominator: info.denom,
    })
}

/// Sends `signal` to the process `identity` describes, only while its pid
/// still names that process (the same start time) and it has not exited:
/// a descendant that ended and whose pid was reused is never signalled.
/// `Ok(false)` when it is gone. Between the check and the `kill` the pid
/// could change hands only if the process ended and the kernel handed out
/// every other free pid within that interval.
#[cfg(target_os = "macos")]
pub fn kill_identified(identity: &ProcessIdentity, signal: i32) -> Result<bool, Error> {
    match process_identity(identity.pid) {
        Some(now) if identity.same_process(&now) && !now.zombie => kill(identity.pid, signal),
        _ => Ok(false),
    }
}

/// Unblocks every signal in the child before `exec`, for
/// `CommandExt::pre_exec`.
fn unblock_all_for_child() -> io::Result<()> {
    let set = empty_set();
    // SAFETY: `set` is an initialised empty signal set and a null pointer for
    // the previous mask is allowed. pthread_sigmask is async-signal-safe and
    // allocates nothing, as a pre_exec hook requires.
    let code = unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &raw const set, ptr::null_mut()) };
    check(code)
}

/// Registers `unblock_all_for_child` as the `pre_exec` hook of `command`,
/// so the child starts with no blocked signals whatever the runner blocked.
/// `pre_exec` is an unsafe API, which is why this lives here.
pub(crate) fn unmask_on_exec(command: &mut Command) {
    // SAFETY: the hook runs between fork and exec, where only
    // async-signal-safe work is allowed; it calls sigemptyset and
    // pthread_sigmask only, and neither allocates nor takes a lock.
    unsafe { command.pre_exec(unblock_all_for_child) };
}

/// One `getpwuid_r` attempt with a string buffer of `size` bytes. `ERANGE`
/// comes back as the error so the caller can grow the buffer.
fn passwd_entry(uid: libc::uid_t, size: usize) -> io::Result<Option<User>> {
    let mut buffer: Vec<libc::c_char> = vec![0; size];
    // SAFETY: `passwd` is plain data (integers and pointers), for which all
    // zero bytes are a valid value.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = ptr::null_mut();
    // SAFETY: `entry` and `result` are valid out-pointers and `buffer` is
    // writable for `buffer.len()` bytes; all three outlive the call.
    let code = unsafe {
        libc::getpwuid_r(
            uid,
            &raw mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &raw mut result,
        )
    };
    check(code)?;
    if result.is_null() {
        return Ok(None);
    }
    if entry.pw_name.is_null() || entry.pw_dir.is_null() {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    // SAFETY: on success both fields point at NUL-terminated strings inside
    // `buffer`, which is still alive; the borrows end before it is dropped.
    let (name, home) = unsafe { (CStr::from_ptr(entry.pw_name), CStr::from_ptr(entry.pw_dir)) };
    let name = name
        .to_str()
        .map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source))?
        .to_owned();
    let home = PathBuf::from(OsStr::from_bytes(home.to_bytes()));
    Ok(Some(User { name, home, uid }))
}

/// A signal set holding exactly `signals`.
fn signal_set(signals: &[i32]) -> Result<libc::sigset_t, Error> {
    let mut set = empty_set();
    for &signal in signals {
        // SAFETY: `set` is an initialised signal set owned by this frame;
        // sigaddset writes only inside it.
        if unsafe { libc::sigaddset(&raw mut set, signal) } != 0 {
            return Err(Error::io(
                "add",
                format!("signal {signal} to a signal set"),
                io::Error::last_os_error(),
            ));
        }
    }
    Ok(set)
}

/// An empty signal set. Async-signal-safe: it allocates nothing.
fn empty_set() -> libc::sigset_t {
    let mut set = MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: sigemptyset initialises the whole set behind a valid pointer
    // and cannot fail for one, so the value is initialised afterwards.
    unsafe {
        libc::sigemptyset(set.as_mut_ptr());
        set.assume_init()
    }
}

/// `pid` as a `pid_t` that names exactly one process: positive.
fn positive_pid(pid: u32) -> Option<libc::pid_t> {
    libc::pid_t::try_from(pid).ok().filter(|pid| *pid > 0)
}

/// `wait4`'s usage as an [`EndUsage`]; `None` when a time is negative or
/// a value overflows.
fn end_usage(usage: &libc::rusage) -> Option<EndUsage> {
    Some(EndUsage {
        user_usec: micros(&usage.ru_utime)?,
        system_usec: micros(&usage.ru_stime)?,
        max_rss_bytes: u64::try_from(usage.ru_maxrss)
            .ok()?
            .checked_mul(MAXRSS_UNIT)?,
    })
}

/// A `timeval` in microseconds; `None` when negative or overflowing.
fn micros(time: &libc::timeval) -> Option<u64> {
    u64::try_from(time.tv_sec)
        .ok()?
        .checked_mul(MICROS_PER_SEC)?
        .checked_add(u64::try_from(time.tv_usec).ok()?)
}

/// `root` and its descendants by `children_of`, breadth first, each pid
/// once, at most [`TREE_MAX`]; each with the parent it was found under
/// (`root` with none).
#[cfg(any(target_os = "macos", test))]
fn walk(root: u32, mut children_of: impl FnMut(u32) -> Vec<u32>) -> Vec<(u32, Option<u32>)> {
    let mut seen = std::collections::BTreeSet::from([root]);
    let mut found = vec![(root, None)];
    let mut next = 0;
    while let Some(&(pid, _)) = found.get(next) {
        next += 1;
        for child in children_of(pid) {
            if found.len() >= TREE_MAX {
                return found;
            }
            if seen.insert(child) {
                found.push((child, Some(pid)));
            }
        }
    }
    found
}

/// The pids a libproc listing writes through `list(buffer, bytes)`, which
/// answers a count. The buffer starts zeroed and grows while the answer
/// fills it, up to [`TREE_MAX`] pids. libproc's listing wrappers answer a
/// count of pids, its raw call a count of bytes: slots past the pids
/// written stay 0 and are dropped, so either reading gives the same pids.
#[cfg(target_os = "macos")]
fn listed_pids(mut list: impl FnMut(*mut libc::c_void, libc::c_int) -> libc::c_int) -> Vec<u32> {
    let mut capacity = PID_LIST_START;
    loop {
        let mut buffer: Vec<libc::pid_t> = vec![0; capacity];
        let Some(bytes) = capacity
            .checked_mul(size_of::<libc::pid_t>())
            .and_then(|bytes| libc::c_int::try_from(bytes).ok())
        else {
            return Vec::new();
        };
        let Ok(count) = usize::try_from(list(buffer.as_mut_ptr().cast(), bytes)) else {
            return Vec::new();
        };
        if count < capacity || capacity >= TREE_MAX {
            buffer.truncate(count);
            return buffer
                .into_iter()
                .filter_map(|pid| u32::try_from(pid).ok())
                .filter(|pid| *pid > 0)
                .collect();
        }
        capacity = capacity.saturating_mul(2);
    }
}

/// Maps a pthread-style return value (0 or an error number) to a result.
fn check(code: libc::c_int) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // catches: identity lookup returns an empty user name, a relative home or a uid different from the invoking user
    #[test]
    fn current_user_has_home() {
        let user = current_user().unwrap();
        assert!(!user.name.is_empty());
        assert!(user.home.is_absolute(), "{}", user.home.display());
        // SAFETY: getuid(2) has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        assert_eq!(user.uid, uid);
    }

    // catches: a vanished child is treated as a signalling failure, or a pid value that addresses a process group is accepted
    #[test]
    fn kill_esrch_is_false() {
        // pid_max is at most 2^22, so the largest pid_t never names a process.
        let unused = u32::try_from(libc::pid_t::MAX).unwrap();
        assert!(!kill(unused, 0).unwrap());
        assert!(
            kill(0, 0).is_err(),
            "pid 0 would address the whole process group"
        );
        assert!(
            kill(u32::MAX, 0).is_err(),
            "a pid that wraps negative addresses a group"
        );
    }

    // catches: the blocked pending signal is lost or sigwait reports a different signal to the supervisor
    #[test]
    fn wait_returns_the_blocked_signal_sent_to_the_thread() {
        let waiter = std::thread::spawn(|| {
            block_signals(&[libc::SIGUSR2]).unwrap();
            // SAFETY: the signal is blocked in this thread, so directing it at
            // this thread only marks it pending until sigwait takes it.
            let code = unsafe { libc::pthread_kill(libc::pthread_self(), libc::SIGUSR2) };
            assert_eq!(code, 0);
            wait_signal(&[libc::SIGUSR2]).unwrap()
        });
        assert_eq!(waiter.join().unwrap(), libc::SIGUSR2);
    }

    // catches: an invalid signal number is silently accepted when constructing the supervisor signal mask
    #[test]
    fn invalid_signal_is_an_error() {
        assert!(block_signals(&[0x7fff]).is_err());
    }

    /// A child that runs until it is killed.
    fn sleeper() -> std::process::Child {
        Command::new("/bin/sh")
            .args(["-c", "exec sleep 30"])
            .spawn()
            .unwrap()
    }

    // catches: the exit status lost or misread by the wait4 reap, its usage missing, or ru_maxrss left
    // in kilobytes on Linux (a shell's peak is above 64 KiB, so a kilobyte count reads below it)
    #[test]
    fn reap_reports_status_and_usage() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 3"])
            .spawn()
            .unwrap();
        let reaped = reap(child.id(), false).unwrap().unwrap();
        assert_eq!(reaped.status.code(), Some(3));
        let usage = reaped.usage.unwrap();
        assert!(usage.max_rss_bytes > 64 * 1024, "{usage:?}");
        assert!(child.try_wait().is_err(), "the reap released the pid");
    }

    // catches: a nohang reap that blocks or invents an end for a running child, a signal death not
    // reported as one, or a reap of something that is not this process's child answering success
    #[test]
    fn reap_without_hanging_and_of_a_stranger() {
        let mut child = sleeper();
        assert_eq!(reap(child.id(), true).unwrap(), None);
        assert!(kill(child.id(), libc::SIGKILL).unwrap());
        let reaped = reap(child.id(), false).unwrap().unwrap();
        assert_eq!(reaped.status.signal(), Some(libc::SIGKILL));
        assert!(reap(child.id(), true).is_err(), "a reaped pid is no child");
        assert!(child.try_wait().is_err(), "the reap released the pid");
        assert!(reap(std::process::id(), true).is_err());
        assert!(reap(0, true).is_err(), "pid 0 would wait for the group");
        assert!(reap(u32::MAX, true).is_err(), "a pid that wraps negative");
    }

    // catches: a negative time or an overflow converted into a wrapped or saturated count
    #[test]
    fn usage_conversion_refuses_what_does_not_fit() {
        // SAFETY: `rusage` is plain data, for which all zero bytes are valid.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        usage.ru_utime.tv_sec = 2;
        usage.ru_utime.tv_usec = 5;
        usage.ru_stime.tv_usec = 7;
        usage.ru_maxrss = 3;
        assert_eq!(
            end_usage(&usage),
            Some(EndUsage {
                user_usec: 2_000_005,
                system_usec: 7,
                max_rss_bytes: 3 * MAXRSS_UNIT,
            })
        );
        usage.ru_stime.tv_sec = -1;
        assert_eq!(end_usage(&usage), None);
        usage.ru_stime.tv_sec = libc::time_t::MAX;
        assert_eq!(end_usage(&usage), None);
    }

    // catches: a tree walk that loops on a pid seen twice (a reused pid), visits a process twice,
    // loses the parent a child was found under, or follows a fork storm without bound
    #[test]
    fn walk_is_breadth_first_once_each_and_bounded() {
        let tree = |pid: u32| match pid {
            1 => vec![2, 3],
            2 => vec![4, 1],
            3 => vec![4, 5],
            _ => Vec::new(),
        };
        assert_eq!(
            walk(1, tree),
            [
                (1, None),
                (2, Some(1)),
                (3, Some(1)),
                (4, Some(2)),
                (5, Some(3))
            ]
        );
        let storm = walk(1, |pid| vec![pid * 2, pid * 2 + 1]);
        assert_eq!(storm.len(), TREE_MAX);
    }

    // catches: a reused pid (same number, later start) taken for the process recorded earlier
    #[test]
    fn identity_needs_the_same_start() {
        let first = ProcessIdentity {
            pid: 40,
            parent: 1,
            group: 40,
            start_secs: 1_700_000_000,
            start_usecs: 10,
            zombie: false,
        };
        let moved = ProcessIdentity {
            parent: 7,
            zombie: true,
            ..first
        };
        assert!(first.same_process(&moved), "parent and state may change");
        let reused = ProcessIdentity {
            start_usecs: 11,
            ..first
        };
        assert!(!first.same_process(&reused));
        let other = ProcessIdentity { pid: 41, ..first };
        assert!(!first.same_process(&other));
    }

    /// The process calls of the launchd backend, against the live kernel.
    /// They compile for every macOS target and run only on a Mac.
    #[cfg(target_os = "macos")]
    mod macos {
        use super::*;

        fn now() -> u64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        }

        // catches: the start time read from the wrong field (or in mach ticks), or the parent and
        // group misread, so no state file ever reads live on a Mac
        #[test]
        fn own_identity_matches_the_kernel() {
            let me = std::process::id();
            let identity = process_identity(me).unwrap();
            // SAFETY: getppid(2) and getpgrp(2) have no preconditions and cannot fail.
            let (parent, group) = unsafe { (libc::getppid(), libc::getpgrp()) };
            assert_eq!(i64::from(identity.parent), i64::from(parent));
            assert_eq!(i64::from(identity.group), i64::from(group));
            assert!(!identity.zombie);
            assert!(identity.start_usecs < MICROS_PER_SEC);
            let start = process_start(me).unwrap();
            assert_eq!(start, identity.start_secs);
            assert!(start <= now() && start + 3600 > now(), "started at {start}");
        }

        // catches: a gone or impossible pid given a start time, which would make a stale state file
        // read live
        #[test]
        fn no_identity_for_what_is_not_a_process() {
            assert_eq!(process_start(0), None);
            assert_eq!(process_start(u32::MAX), None);
            let mut child = sleeper();
            let pid = child.id();
            assert!(process_start(pid).is_some());
            child.kill().unwrap();
            child.wait().unwrap();
            assert_eq!(process_start(pid), None);
        }

        // catches: children, the tree or the group listed wrongly (bytes read as pids, a short
        // buffer), or a stale identity signalled
        #[test]
        fn tree_group_and_identified_kill() {
            let me = std::process::id();
            let mut child = sleeper();
            let pid = child.id();
            assert!(child_pids(me).contains(&pid));
            let tree = process_tree(me);
            assert_eq!(tree.first().map(|root| root.pid), Some(me));
            let found = tree.iter().find(|entry| entry.pid == pid).unwrap();
            assert_eq!(found.parent, me);
            assert!(group_pids(found.group).contains(&pid));
            let stale = ProcessIdentity {
                start_usecs: (found.start_usecs + 1) % MICROS_PER_SEC,
                ..*found
            };
            assert!(!kill_identified(&stale, libc::SIGKILL).unwrap());
            assert_eq!(
                child.try_wait().unwrap(),
                None,
                "a stale identity was signalled"
            );
            assert!(kill_identified(found, libc::SIGKILL).unwrap());
            child.wait().unwrap();
            assert!(!kill_identified(found, libc::SIGKILL).unwrap());
        }

        // catches: the live counters' FFI failing for a live process, or a timebase that cannot turn
        // ticks into nanoseconds
        #[test]
        fn counters_and_timebase() {
            let sample = task_sample(std::process::id()).unwrap();
            assert!(sample.threads >= 1, "{sample:?}");
            assert!(sample.phys_footprint > 0, "{sample:?}");
            assert!(sample.user_ticks + sample.system_ticks > 0, "{sample:?}");
            let timebase = timebase().unwrap();
            assert!(timebase.numerator > 0 && timebase.denominator > 0);
            assert_eq!(task_sample(u32::MAX), None);
        }
    }
}
