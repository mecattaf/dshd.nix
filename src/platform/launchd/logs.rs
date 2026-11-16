//! The per-run log files under `<instance>/runs`: one file per run,
//! `<invocationId>.log` (mode 0600), and `latest.log`, a symlink to the
//! newest, so `tail -F` follows any instance. The runner makes the file
//! its own stdout and stderr; dsh writes to the same descriptor. Lines are
//! whole in the file, so nothing is joined; bytes that are not UTF-8 are
//! replaced (`U+FFFD`) as the journal's text form shows them.

use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::platform::launchd::not_built;

/// The symlink to the newest run's log.
pub const LATEST: &str = "latest.log";
/// The mode of a run's log.
pub const LOG_MODE: u32 = 0o600;
/// The first line of every run, as on Linux.
pub const FIRST_LINE: &str = "dshd-run: run started";
/// The prefix of the last line of every run that ended under its runner.
pub const LAST_LINE_PREFIX: &str = "dshd-run: run ended: ";

/// Which part of a run's log to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// The newest N lines.
    Newest(u32),
    /// The oldest N lines (`--head N`).
    Oldest(u32),
}

/// Lines read from one run's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lines {
    /// The lines, oldest first, without their newline.
    pub lines: Vec<String>,
    /// The file holds more lines than the window showed.
    pub truncated: bool,
}

/// `<runs dir>/<invocation>.log`.
#[must_use]
pub fn run_log(runs_dir: &Path, invocation: &str) -> PathBuf {
    runs_dir.join(format!("{invocation}.log"))
}

/// Creates the run's log (mode [`LOG_MODE`], the runs directory 0700 when
/// missing, failing if the file exists), points [`LATEST`] at it, and
/// returns it open for appending.
pub fn create(runs_dir: &Path, invocation: &str) -> Result<std::fs::File, Error> {
    let _ = (runs_dir, invocation);
    Err(not_built("the per-run log"))
}

/// Reads `window` of a run's log. A missing file is no lines and not
/// truncated (the run's log was removed or never written). A last line
/// without its newline, which the run is still writing, is left out of
/// `Newest` and `Oldest` alike.
pub fn read_window(path: &Path, window: Window) -> Result<Lines, Error> {
    let _ = (path, window);
    Err(not_built("reading a run's log"))
}

/// The newest `lines` lines for a `start-failed` error: from the newest
/// run's log through [`LATEST`], or from `launchd_log` when there is no
/// run log (the runner never opened one), or nothing.
pub fn failure_tail(runs_dir: &Path, launchd_log: &Path, lines: u32) -> Result<Vec<String>, Error> {
    let _ = (runs_dir, launchd_log, lines);
    Err(not_built("the failure tail"))
}

#[cfg(test)]
#[path = "logs_tests.rs"]
mod tests;
