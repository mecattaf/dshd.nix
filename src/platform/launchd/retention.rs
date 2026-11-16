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
    let is_run_log = |path: &std::path::Path| -> bool {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        let Some(stem) = name.strip_suffix(".log") else {
            return false;
        };
        stem.len() == 32
            && stem
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };

    let mut others: Vec<&LogFile> = files
        .iter()
        .filter(|file| file.path.as_path() != current && is_run_log(&file.path))
        .collect();
    others.sort_by(|a, b| (b.modified, &b.path).cmp(&(a.modified, &a.path)));
    let mut removed = Vec::new();
    if others.len() > KEEP_RUNS.saturating_sub(1) {
        let overflow = others.split_off(KEEP_RUNS.saturating_sub(1));
        removed.extend(overflow.into_iter().rev().map(|file| file.path.clone()));
    }

    let mut total = others
        .iter()
        .map(|file| file.bytes)
        .fold(0u64, u64::saturating_add);
    while total > KEEP_BYTES {
        let Some(file) = others.pop() else {
            break;
        };
        total = total.saturating_sub(file.bytes);
        removed.push(file.path.clone());
    }
    removed
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;
