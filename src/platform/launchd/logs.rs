//! The per-run log files under `<instance>/runs`: one file per run,
//! `<invocationId>.log` (mode 0600), and `latest.log`, a symlink to the
//! newest, so `tail -F` follows any instance. The runner makes the file
//! its own stdout and stderr; dsh writes to the same descriptor. Lines are
//! whole in the file, so nothing is joined; bytes that are not UTF-8 are
//! replaced (`U+FFFD`) as the journal's text form shows them.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::fsx;

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
pub fn create(runs_dir: &Path, invocation: &str) -> Result<File, Error> {
    let path = run_log(runs_dir, invocation);
    fsx::create_dir_private(runs_dir)?;
    let file = OpenOptions::new()
        .append(true)
        .create_new(true)
        .mode(LOG_MODE)
        .open(&path)
        .map_err(|e| Error::io("create", &path, e))?;
    fsx::symlink_replace(&path, &runs_dir.join(LATEST))?;
    Ok(file)
}

/// How far back `Newest` reads at a time: a 64 KiB block from the end.
const BLOCK: u64 = 64 * 1024;

/// Reads `window` of a run's log. A missing file is no lines and not
/// truncated (the run's log was removed or never written). A last line
/// without its newline, which the run is still writing, is left out of
/// `Newest` and `Oldest` alike.
pub fn read_window(path: &Path, window: Window) -> Result<Lines, Error> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Lines {
                lines: Vec::new(),
                truncated: false,
            });
        }
        Err(e) => return Err(Error::io("open", path, e)),
    };
    match window {
        Window::Newest(count) => newest_lines(file, path, count),
        Window::Oldest(count) => oldest_lines(file, path, count),
    }
}

fn newest_lines(mut file: File, path: &Path, count: u32) -> Result<Lines, Error> {
    let count = count as usize;
    let end = file
        .seek(SeekFrom::End(0))
        .map_err(|e| Error::io("seek", path, e))?;
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut newlines = 0usize;
    let mut pos = end;
    let mut prev: Option<u8> = None;
    while pos > 0 {
        let start = pos.saturating_sub(BLOCK);
        let len = usize::try_from(pos - start)
            .map_err(|_| Error::io("read", path, std::io::ErrorKind::InvalidData.into()))?;
        let mut chunk = vec![0u8; len];
        file.seek(SeekFrom::Start(start))
            .map_err(|e| Error::io("seek", path, e))?;
        file.read_exact(&mut chunk)
            .map_err(|e| Error::io("read", path, e))?;
        newlines += chunk
            .iter()
            .fold(0usize, |n, byte| if *byte == b'\n' { n + 1 } else { n });
        chunks.push(chunk);
        pos = start;
        if pos == 0 {
            break;
        }
        prev = peek_before(&mut file, path, pos)?;
        let complete = if prev == Some(b'\n') {
            newlines
        } else {
            newlines.saturating_sub(1)
        };
        if complete > count {
            break;
        }
        if complete == count && prev == Some(b'\n') {
            break;
        }
    }
    let mut bytes = Vec::new();
    for chunk in chunks.iter().rev() {
        bytes.extend_from_slice(chunk);
    }
    if pos > 0
        && prev != Some(b'\n')
        && let Some(end_of_partial) = bytes.iter().position(|byte| *byte == b'\n')
    {
        bytes.drain(..=end_of_partial);
    }
    let all: Vec<String> = split_complete_lines(&bytes)
        .into_iter()
        .map(|line| String::from_utf8_lossy(line).into_owned())
        .collect();
    let truncated = pos > 0 || all.len() > count;
    let skip = all.len().saturating_sub(count);
    let lines: Vec<String> = all.into_iter().skip(skip).collect();
    Ok(Lines { lines, truncated })
}

fn peek_before(file: &mut File, path: &Path, pos: u64) -> Result<Option<u8>, Error> {
    if pos == 0 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(pos - 1))
        .map_err(|e| Error::io("seek", path, e))?;
    let mut byte = [0u8; 1];
    let read = file
        .read(&mut byte)
        .map_err(|e| Error::io("read", path, e))?;
    Ok((read == 1).then_some(byte[0]))
}

fn split_complete_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut rest = bytes;
    while let Some(end) = rest.iter().position(|byte| *byte == b'\n') {
        if let Some(line) = rest.get(..end) {
            lines.push(line);
        }
        rest = rest.get(end + 1..).unwrap_or(&[]);
    }
    lines
}

fn oldest_lines(mut file: File, path: &Path, count: u32) -> Result<Lines, Error> {
    let count = count as usize;
    let mut reader = std::io::BufReader::new(&mut file);
    let mut lines: Vec<String> = Vec::new();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let read = reader
            .read_until(b'\n', &mut buf)
            .map_err(|e| Error::io("read", path, e))?;
        if read == 0 {
            break;
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
            lines.push(String::from_utf8_lossy(&buf).into_owned());
            if lines.len() > count {
                break;
            }
        }
    }
    let truncated = lines.len() > count;
    lines.truncate(count);
    Ok(Lines { lines, truncated })
}

/// The newest `lines` lines for a `start-failed` error: from the newest
/// run's log through [`LATEST`], or from `launchd_log` when there is no
/// run log (the runner never opened one), or nothing.
pub fn failure_tail(runs_dir: &Path, launchd_log: &Path, lines: u32) -> Result<Vec<String>, Error> {
    let latest = runs_dir.join(LATEST);
    if resolves_to_file(&latest)? {
        return Ok(read_window(&latest, Window::Newest(lines))?.lines);
    }
    if resolves_to_file(launchd_log)? {
        return Ok(read_window(launchd_log, Window::Newest(lines))?.lines);
    }
    Ok(Vec::new())
}

fn resolves_to_file(path: &Path) -> Result<bool, Error> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::io("stat", path, e)),
    }
}

#[cfg(test)]
#[path = "logs_tests.rs"]
mod tests;
