//! Which per-run logs a new run removes. The current run's log is always
//! kept and counts as one of the [`KEEP_RUNS`]; of the other run logs the
//! newest `KEEP_RUNS - 1` are kept. Then, while the kept ended runs' logs
//! total more than [`KEEP_BYTES`], the oldest of them goes too. Never the
//! current run's log, never `latest.log`, never a file whose name is not
//! `<32 lowercase hex>.log`. Newest is by `modified`, ties broken by path.
//! Pure: the caller lists the directory and removes what this selects.

use std::path::PathBuf;

/// How many runs keep their logs.
pub const KEEP_RUNS: usize = 20;
/// The total size the ended runs' logs may reach, per instance.
pub const KEEP_BYTES: u64 = 1 << 30;

/// One file in the runs directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFile {
    /// Its path.
    pub path: PathBuf,
    /// Its modification time, unix seconds.
    pub modified: u64,
    /// Its size in bytes.
    pub bytes: u64,
}

/// The files to remove, oldest first. `current` is the running run's log.
#[must_use]
pub fn select(files: &[LogFile], current: &std::path::Path) -> Vec<PathBuf> {
    let _ = (files, current);
    Vec::new()
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;
