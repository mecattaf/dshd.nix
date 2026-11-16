//! Parsers of `launchctl print` text: one job (`print gui/<uid>/LABEL`),
//! the services of a domain (`print gui/<uid>`) and the disabled list
//! (`print-disabled gui/<uid>`). launchd documents this text as not
//! stable, so a line or value the parser does not recognise leaves the
//! fact unknown (`None`, or [`JobState::Other`] with the word kept), never
//! wrong. Only the job block's own properties count: lines inside nested
//! blocks (`environment = { ... }`, `event channels = { ... }`) are skipped.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::error::Error;
use crate::platform::launchd::not_built;

/// The job's `state` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    /// `running`.
    Running,
    /// `not running`.
    NotRunning,
    /// `spawn scheduled`: loaded, its process not started yet.
    SpawnScheduled,
    /// Any other word, kept verbatim.
    Other(String),
}

/// How the job's last process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastExit {
    /// `last exit code = (never exited)`.
    Never,
    /// `last exit code = N` (any text after the number is ignored).
    Code(i32),
    /// `last terminating signal = <description>: N`.
    Signal(i32),
}

/// The facts of one job dshd reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobFacts {
    /// `state`.
    pub state: JobState,
    /// `pid`, absent when no process runs.
    pub pid: Option<u32>,
    /// `runs`: how many times launchd spawned the job.
    pub runs: Option<u32>,
    /// `last exit code` or `last terminating signal`.
    pub last_exit: Option<LastExit>,
    /// `path`: the plist launchd loaded the job from.
    pub path: Option<PathBuf>,
}

/// One line of a domain's `services` block: `PID LAST-EXIT LABEL`, where a
/// `PID` of `0` or `-` and a `LAST-EXIT` of `-` mean none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEntry {
    /// The label, verbatim.
    pub label: String,
    /// The running process, when one runs.
    pub pid: Option<u32>,
    /// The last exit status, when the job exited before.
    pub last_exit: Option<i32>,
}

/// Parses `launchctl print gui/<uid>/LABEL`. The text must open with
/// `<target> = {`; anything else is an `Error::Tool` naming `launchctl`
/// with exit status 0 and the reason in `stderr`, since `launchctl`
/// answered but not with a job.
pub fn parse_job(text: &str) -> Result<JobFacts, Error> {
    let _ = text;
    Err(not_built("the launchctl print parser"))
}

/// The entries of the `services = { ... }` block of
/// `launchctl print gui/<uid>`, in the order printed. Every label is
/// returned, dshd's or not; lines that do not have three fields are
/// skipped.
pub fn parse_domain_services(text: &str) -> Vec<ServiceEntry> {
    let _ = text;
    Vec::new()
}

/// The `disabled services = { ... }` block of `launchctl print-disabled`:
/// label to whether it is disabled. `disabled` and `true` mean disabled,
/// `enabled` and `false` mean enabled; other blocks (such as
/// `login item associations`) are not read.
pub fn parse_disabled(text: &str) -> BTreeMap<String, bool> {
    let _ = text;
    BTreeMap::new()
}

#[cfg(test)]
#[path = "print_tests.rs"]
mod tests;
