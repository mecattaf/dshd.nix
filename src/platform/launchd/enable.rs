//! `enable` and `disable` under launchd: an enabled instance has a copy of
//! its rendered job in `~/Library/LaunchAgents`, which launchd loads at the
//! next login (and `RunAtLoad` starts). Neither verb calls `launchctl`:
//! nothing is loaded or unloaded, so nothing starts or stops now.

use crate::error::Error;
use crate::name::InstanceName;
use crate::platform::launchd::{LaunchdManager, not_built};

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
    let _ = (manager, name);
    Err(not_built("enable"))
}

/// `disable NAME`: removes the copy in `~/Library/LaunchAgents`; a missing
/// copy is fine. Calls no `launchctl` verb.
pub fn disable(manager: &LaunchdManager, name: &InstanceName) -> Result<(), Error> {
    let _ = (manager, name);
    Err(not_built("disable"))
}

/// The [`CopyState`] of the instance's copy. A symlink, directory or
/// other non-regular file at the copy's path `Differs`.
pub fn copy_state(manager: &LaunchdManager, name: &InstanceName) -> Result<CopyState, Error> {
    let _ = (manager, name);
    Err(not_built("the LaunchAgents copy check"))
}

#[cfg(test)]
#[path = "enable_tests.rs"]
mod tests;
