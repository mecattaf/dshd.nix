//! The words a run's end is recorded in on macOS, systemd's own, so
//! `last-run.json` reads the same on both platforms: `serviceResult`
//! (`success`, `exit-code`, `signal`, `timeout`, `start-limit-hit`),
//! `exitCode` (`exited` or `killed`) and `exitStatus` (the number, or the
//! signal name without `SIG`). The runner records its own end with
//! [`own_end`]; a later CLI completes the run of a runner that died with
//! [`reconciled_end`].

use crate::platform::launchd::print::LastExit;

/// One end in systemd's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndWords {
    /// `serviceResult`.
    pub service_result: String,
    /// `exitCode`.
    pub exit_code: String,
    /// `exitStatus`.
    pub exit_status: String,
}

/// The end of a run the runner completed itself: `status` is its exit
/// status from the exit table; `timed_out` when it ended the run at the
/// startup deadline (then `timeout`, `killed`, `TERM`, as systemd records
/// a start timeout). Status 0 is `success`, any other `exit-code`.
#[must_use]
pub fn own_end(status: u8, timed_out: bool) -> EndWords {
    if timed_out {
        return words("timeout", "killed", "TERM");
    }
    if status == 0 {
        words("success", "exited", "0")
    } else {
        words("exit-code", "exited", &status.to_string())
    }
}

/// The end of a run whose runner died without completing it, from
/// launchd's last exit: a code is `exit-code`/`exited`/the code (0
/// included: a runner that completed would have recorded its end), a
/// signal `signal`/`killed`/its name. `None` for a job that never exited.
#[must_use]
pub fn reconciled_end(last_exit: &LastExit) -> Option<EndWords> {
    match last_exit {
        LastExit::Never => None,
        LastExit::Code(code) => Some(words("exit-code", "exited", &code.to_string())),
        LastExit::Signal(signal) => Some(words("signal", "killed", &signal_word(*signal))),
    }
}

/// The end of a start the start limit refused.
#[must_use]
pub fn start_limit_end() -> EndWords {
    words("start-limit-hit", "exited", "0")
}

/// A signal's name without `SIG` on this platform (`libc` constants), or
/// its number when it has none here.
#[must_use]
pub fn signal_word(signal: i32) -> String {
    let word = match signal {
        libc::SIGHUP => "HUP",
        libc::SIGINT => "INT",
        libc::SIGQUIT => "QUIT",
        libc::SIGILL => "ILL",
        libc::SIGTRAP => "TRAP",
        libc::SIGABRT => "ABRT",
        libc::SIGBUS => "BUS",
        libc::SIGFPE => "FPE",
        libc::SIGKILL => "KILL",
        libc::SIGUSR1 => "USR1",
        libc::SIGSEGV => "SEGV",
        libc::SIGUSR2 => "USR2",
        libc::SIGPIPE => "PIPE",
        libc::SIGALRM => "ALRM",
        libc::SIGTERM => "TERM",
        libc::SIGCHLD => "CHLD",
        libc::SIGCONT => "CONT",
        libc::SIGSTOP => "STOP",
        libc::SIGTSTP => "TSTP",
        libc::SIGTTIN => "TTIN",
        libc::SIGTTOU => "TTOU",
        libc::SIGURG => "URG",
        libc::SIGXCPU => "XCPU",
        libc::SIGXFSZ => "XFSZ",
        libc::SIGVTALRM => "VTALRM",
        libc::SIGPROF => "PROF",
        libc::SIGWINCH => "WINCH",
        libc::SIGIO => "IO",
        libc::SIGSYS => "SYS",
        other => return other.to_string(),
    };
    word.to_owned()
}

fn words(service_result: &str, exit_code: &str, exit_status: &str) -> EndWords {
    EndWords {
        service_result: service_result.to_owned(),
        exit_code: exit_code.to_owned(),
        exit_status: exit_status.to_owned(),
    }
}

#[cfg(test)]
#[path = "completion_tests.rs"]
mod tests;
