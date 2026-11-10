//! The one module that names `libc`: signal masks and `sigwait` for the
//! runner, `kill` of the runner's own child (and of the runner itself, to
//! wake its main thread), and `getpwuid_r` for the invoking user. Every
//! `unsafe` block carries a `SAFETY` comment.

use std::ffi::{CStr, OsStr};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::ptr;

use crate::error::Error;

pub use libc::{SIGCHLD, SIGHUP, SIGINT, SIGTERM};

/// First size of the string buffer handed to `getpwuid_r`; it doubles on
/// `ERANGE`.
const PASSWD_BUFFER_START: usize = 1024;
/// Largest string buffer tried before the lookup is reported as failed.
const PASSWD_BUFFER_MAX: usize = 1024 * 1024;

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
}
