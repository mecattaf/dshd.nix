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
    let _ = (status, timed_out);
    words("", "", "")
}

/// The end of a run whose runner died without completing it, from
/// launchd's last exit: a code is `exit-code`/`exited`/the code (0
/// included: a runner that completed would have recorded its end), a
/// signal `signal`/`killed`/its name. `None` for a job that never exited.
#[must_use]
pub fn reconciled_end(last_exit: &LastExit) -> Option<EndWords> {
    let _ = last_exit;
    None
}

/// The end of a start the start limit refused.
#[must_use]
pub fn start_limit_end() -> EndWords {
    words("", "", "")
}

/// A signal's name without `SIG` on this platform (`libc` constants), or
/// its number when it has none here.
#[must_use]
pub fn signal_word(signal: i32) -> String {
    signal.to_string()
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
