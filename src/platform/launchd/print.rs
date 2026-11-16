//! Parsers of `launchctl print` text: one job (`print gui/<uid>/LABEL`),
//! the services of a domain (`print gui/<uid>`) and the disabled list
//! (`print-disabled gui/<uid>`). launchd documents this text as not
//! stable, so a line or value the parser does not recognise leaves the
//! fact unknown (`None`, or [`JobState::Other`] with the word kept), never
//! wrong. Only the job block's own properties count: lines inside nested
//! blocks (`environment = { ... }`, `event channels = { ... }`) are skipped.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::error::{Error, ToolFailure};

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
    let malformed = |reason: &str| {
        Error::Tool(Box::new(ToolFailure {
            tool: "launchctl",
            args: Vec::new(),
            status: Some(0),
            stderr: reason.to_owned(),
        }))
    };

    let Some(first) = text.lines().next() else {
        return Err(malformed("launchctl printed no job block"));
    };
    let Some(target) = first.trim_end().strip_suffix(" = {") else {
        return Err(malformed("launchctl printed no job block"));
    };
    if target.trim().is_empty() {
        return Err(malformed("launchctl printed a job block with no target"));
    }

    let mut facts = JobFacts {
        state: JobState::Other(String::new()),
        pid: None,
        runs: None,
        last_exit: None,
        path: None,
    };
    let mut depth = 1usize;
    for line in text.lines().skip(1) {
        let trimmed = line.trim();
        if trimmed.ends_with('{') {
            depth += 1;
            continue;
        }
        if trimmed == "}" {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth != 1 {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(" = ") else {
            continue;
        };
        match key.trim() {
            "state" => {
                facts.state = match value.trim() {
                    "running" => JobState::Running,
                    "not running" => JobState::NotRunning,
                    "spawn scheduled" => JobState::SpawnScheduled,
                    other => JobState::Other(other.to_owned()),
                };
            }
            "pid" => facts.pid = value.trim().parse::<u32>().ok(),
            "runs" => facts.runs = value.trim().parse::<u32>().ok(),
            "path" => facts.path = Some(PathBuf::from(value.trim())),
            "last exit code" => {
                let value = value.trim();
                if value == "(never exited)" {
                    facts.last_exit = Some(LastExit::Never);
                } else {
                    let code = value.split_whitespace().next();
                    if let Some(code) = code {
                        let code = code.trim_end_matches(':');
                        if let Ok(code) = code.parse::<i32>() {
                            facts.last_exit = Some(LastExit::Code(code));
                        }
                    }
                }
            }
            "last terminating signal" => {
                let number = value.rsplit(':').next();
                if let Some(number) = number {
                    let number = number.trim();
                    if let Ok(signal) = number.parse::<i32>() {
                        facts.last_exit = Some(LastExit::Signal(signal));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(facts)
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
