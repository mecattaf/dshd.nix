//! `enable` and `disable` under launchd: an enabled instance has a copy of
//! its rendered job in `~/Library/LaunchAgents`, which launchd loads at the
//! next login (and `RunAtLoad` starts). Neither verb calls `launchctl`:
//! nothing is loaded or unloaded, so nothing starts or stops now.

use std::fs::DirBuilder;
use std::io;
use std::os::unix::fs::DirBuilderExt;

use crate::error::Error;
use crate::fsx;
use crate::name::InstanceName;
use crate::platform::launchd::{LaunchdManager, plist};

/// How the copy in `~/Library/LaunchAgents` relates to what dshd renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyState {
    /// No copy: the instance is not enabled.
    Absent,
    /// The copy holds exactly what dshd renders now.
    Current,
    /// A copy exists but differs (or is not a regular file): a problem
    /// `get` reports, and the next `enable` rewrites it.
    Differs,
}

/// `enable NAME`: renders [`LaunchdManager::job_spec`] with
/// `plist::render`, writes it atomically with `plist::PLIST_MODE` to
/// [`LaunchdManager::plist_path`] and to [`LaunchdManager::agent_copy`]
/// (creating the `LaunchAgents` directory with mode 0755 when missing),
/// both regular files. Calls no `launchctl` verb. Returns the one echo
/// line `install -m 0644 <plist path> <agent copy>`.
pub fn enable(manager: &LaunchdManager, name: &InstanceName) -> Result<Vec<String>, Error> {
    let rendered = plist::render(&manager.job_spec(name))?;
    let plist_path = manager.plist_path(name);
    let copy = manager.agent_copy(name);
    let agents = copy
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| Error::io("write", &copy, io::Error::from(io::ErrorKind::InvalidInput)))?;
    if !agents.exists() {
        DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(agents)
            .map_err(|e| Error::io("create directory", agents, e))?;
    }
    fsx::write_atomic(&plist_path, rendered.as_bytes(), plist::PLIST_MODE)?;
    fsx::write_atomic(&copy, rendered.as_bytes(), plist::PLIST_MODE)?;
    Ok(vec![format!(
        "install -m 0644 {} {}",
        plist_path.display(),
        copy.display()
    )])
}

/// `disable NAME`: removes the copy in `~/Library/LaunchAgents`; a missing
/// copy is fine. Calls no `launchctl` verb.
pub fn disable(manager: &LaunchdManager, name: &InstanceName) -> Result<(), Error> {
    let copy = manager.agent_copy(name);
    match std::fs::remove_file(&copy) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io("remove", &copy, e)),
    }
}

/// The [`CopyState`] of the instance's copy. A symlink, directory or
/// other non-regular file at the copy's path `Differs`.
pub fn copy_state(manager: &LaunchdManager, name: &InstanceName) -> Result<CopyState, Error> {
    let copy = manager.agent_copy(name);
    let metadata = match std::fs::symlink_metadata(&copy) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(CopyState::Absent),
        Err(e) => return Err(Error::io("read", &copy, e)),
    };
    if !metadata.file_type().is_file() {
        return Ok(CopyState::Differs);
    }
    let rendered = plist::render(&manager.job_spec(name))?;
    let bytes = match std::fs::read(&copy) {
        Ok(bytes) => bytes,
        Err(e) => return Err(Error::io("read", &copy, e)),
    };
    if bytes == rendered.as_bytes() {
        Ok(CopyState::Current)
    } else {
        Ok(CopyState::Differs)
    }
}

#[cfg(test)]
#[path = "enable_tests.rs"]
mod tests;
