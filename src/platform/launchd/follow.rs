//! A follower of one run's log file: it hands out the whole lines written
//! since it began, from the start of the run or from the end the file had
//! when it opened. The CLI's follow pump calls [`Follower::poll`] where it
//! reads `journalctl -f` on Linux and keeps its own end-marker check and
//! silence looks. Pure file reading: no thread, no sleep.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::error::Error;

/// The most one poll reads, so following a large log from its start
/// hands it out in parts instead of reading it whole.
const POLL_BYTES: u64 = 1 << 20;

/// Where following begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Begin {
    /// The first line of the run.
    Start,
    /// The end the file has when the follower opens.
    Now,
}

/// Follows one run's log.
#[derive(Debug)]
pub struct Follower {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl Follower {
    /// A follower of `path`. A file that does not exist yet is followed
    /// from its first byte once it appears, whatever `from` says.
    pub fn open(path: &Path, from: Begin) -> Result<Self, Error> {
        let offset = match from {
            Begin::Start => 0,
            Begin::Now => match std::fs::metadata(path) {
                Ok(meta) => meta.len(),
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => 0,
                Err(source) => return Err(Error::io("follow", path, source)),
            },
        };
        Ok(Self {
            path: path.to_path_buf(),
            offset,
            partial: Vec::new(),
        })
    }

    /// The whole lines written since the last poll, oldest first, without
    /// their newline; bytes that are not UTF-8 replaced. A trailing line
    /// without its newline is kept back until its newline arrives. A file
    /// that shrank (it was replaced) is read again from its first byte.
    pub fn poll(&mut self) -> Result<Vec<String>, Error> {
        let mut file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(Error::io("follow", &self.path, source)),
        };
        let len = file
            .metadata()
            .map_err(|source| Error::io("follow", &self.path, source))?
            .len();
        if len < self.offset() {
            self.offset = 0;
            self.partial.clear();
        }
        file.seek(SeekFrom::Start(self.offset()))
            .map_err(|source| Error::io("follow", &self.path, source))?;
        let mut buffer = std::mem::take(&mut self.partial);
        file.take(POLL_BYTES)
            .read_to_end(&mut buffer)
            .map_err(|source| Error::io("follow", &self.path, source))?;
        let complete = buffer
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |end| end + 1);
        self.partial = buffer.split_off(complete);
        self.offset += complete as u64;
        let lines = match buffer.strip_suffix(b"\n") {
            Some(body) => body
                .split(|byte| *byte == b'\n')
                .map(|line| String::from_utf8_lossy(line).into_owned())
                .collect(),
            None => Vec::new(),
        };
        Ok(lines)
    }

    /// The byte offset read up to, partial line included.
    #[must_use]
    pub fn offset(&self) -> u64 {
        self.offset + self.partial.len() as u64
    }
}

#[cfg(test)]
#[path = "follow_tests.rs"]
mod tests;
