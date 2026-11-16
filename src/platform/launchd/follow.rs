//! A follower of one run's log file: it hands out the whole lines written
//! since it began, from the start of the run or from the end the file had
//! when it opened. The CLI's follow pump calls [`Follower::poll`] where it
//! reads `journalctl -f` on Linux and keeps its own end-marker check and
//! silence looks. Pure file reading: no thread, no sleep.

use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::platform::launchd::not_built;

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
        let _ = from;
        Err(Self::stub(path))
    }

    /// The whole lines written since the last poll, oldest first, without
    /// their newline; bytes that are not UTF-8 replaced. A trailing line
    /// without its newline is kept back until its newline arrives. A file
    /// that shrank (it was replaced) is read again from its first byte.
    pub fn poll(&mut self) -> Result<Vec<String>, Error> {
        Err(Self::stub(&self.path))
    }

    /// The byte offset read up to, partial line included.
    #[must_use]
    pub fn offset(&self) -> u64 {
        self.offset + self.partial.len() as u64
    }

    fn stub(path: &Path) -> Error {
        not_built(&format!("following {}", path.display()))
    }
}

#[cfg(test)]
#[path = "follow_tests.rs"]
mod tests;
