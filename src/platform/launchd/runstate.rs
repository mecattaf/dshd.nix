//! The run state file `<instance>/run/state.json`: what launchd cannot say
//! about a run. The macOS runner writes it (mode 0600, atomically) where
//! the Linux runner talks to systemd: its invocation id, its own pid and
//! process start time, and its phase, which turns `ready` where Linux
//! sends `READY=1` and `stopping` where it sends `STOPPING=1`.
//!
//! A state file is live only for the process that wrote it: the job's pid
//! in `launchctl print` must be the file's `pid`, and that process's start
//! time must equal the file's `processStart`. A pid launchd or the kernel
//! reused for another process has a later start time, so its file reads
//! stale. An `exec` (the runner's own restart after a crash) keeps both.

use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::name::RuntimeDir;

/// The file name inside the run directory.
pub const STATE_FILE: &str = "state.json";
/// Its mode.
pub const STATE_MODE: u32 = 0o600;
/// The per-run files a new run clears before it starts: whatever a runner
/// that was killed left there is stale data, never a live run.
pub const PER_RUN_FILES: &[&str] = &[
    "state.json",
    "web-url",
    "acp.sock",
    "stop-intent",
    "child-end.json",
];

/// The run's phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// Spawned, not ready yet.
    Starting,
    /// Ready: the web URL is recorded, the ACP socket is bound, or the
    /// headless dsh is spawned.
    Ready,
    /// A stop is under way.
    Stopping,
    /// The runner is pausing before it restarts a crashed ready dsh.
    AutoRestart,
}

/// `run/state.json`. A closed record: a key it does not know is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunState {
    /// The run's invocation id: 32 lowercase hex digits, new per run.
    pub invocation_id: String,
    /// The runner's pid, which is the job's pid.
    pub pid: u32,
    /// The runner process's start time, unix seconds, as the kernel
    /// reports it for that pid.
    pub process_start: u64,
    /// The phase.
    pub phase: Phase,
    /// Unix seconds when the phase began.
    pub phase_since: u64,
}

/// `<run dir>/state.json`.
#[must_use]
pub fn path(run_dir: &RuntimeDir) -> PathBuf {
    run_dir.path().join(STATE_FILE)
}

/// Reads the state file; `Ok(None)` when there is none. A file that does
/// not parse is an error.
pub fn read(run_dir: &RuntimeDir) -> Result<Option<RunState>, Error> {
    crate::fsx::read_json(&path(run_dir))
}

/// Writes the state file atomically with [`STATE_MODE`].
pub fn write(run_dir: &RuntimeDir, state: &RunState) -> Result<(), Error> {
    crate::fsx::write_json_atomic(&path(run_dir), state, STATE_MODE)
}

/// Moves the recorded run to `phase` since `now`; nothing else changes.
/// A missing state file is an `Io` error.
pub fn set_phase(run_dir: &RuntimeDir, phase: Phase, now: u64) -> Result<(), Error> {
    let state_path = path(run_dir);
    let Some(mut state) = read(run_dir)? else {
        return Err(Error::io(
            "read",
            state_path,
            io::Error::from(io::ErrorKind::NotFound),
        ));
    };
    state.phase = phase;
    state.phase_since = now;
    write(run_dir, &state)
}

/// Whether `state` describes the process launchd runs now: `job_pid` is
/// the job's pid in `launchctl print`, `process_start` the start time of
/// that pid (both `None` when unknown, which is never live).
#[must_use]
pub fn is_live(state: &RunState, job_pid: Option<u32>, process_start: Option<u64>) -> bool {
    job_pid == Some(state.pid) && process_start == Some(state.process_start)
}

/// Removes every [`PER_RUN_FILES`] entry present in the run directory and
/// returns the paths removed, in list order. A missing directory removes
/// nothing.
pub fn clear_stale(run_dir: &RuntimeDir) -> Result<Vec<PathBuf>, Error> {
    let mut removed = Vec::new();
    for name in PER_RUN_FILES {
        let candidate = run_dir.path().join(name);
        match std::fs::remove_file(&candidate) {
            Ok(()) => removed.push(candidate),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io("remove", candidate, e)),
        }
    }
    Ok(removed)
}

#[cfg(test)]
#[path = "runstate_tests.rs"]
mod tests;
