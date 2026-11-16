//! `start` and `restart` under launchd. `RunAtLoad` makes the bootstrap
//! the start, so dshd never uses `kickstart`: one start spawns at most one
//! runner, and a headless task cannot run twice from one start. Then the
//! start waits for readiness, as `systemctl start` waits for `READY=1` on
//! Linux, so the CLI's settle and report stay the same on both platforms.

use crate::error::Error;
use crate::name::InstanceName;
use crate::platform::launchd::{LaunchdManager, not_built};

/// `start NAME`:
///
/// 1. [`LaunchdManager::job`]: a job launchd holds and that has a pid is
///    running; return `Ok` and call nothing else.
/// 2. A job launchd holds without a pid (it ended, or never spawned):
///    `launchctl bootout <service target>`, then read
///    [`LaunchdManager::job`] every [`LaunchdManager::poll_interval`]
///    until it is `None`.
/// 3. Note the invocation id in `run/state.json` (`runstate::read` on
///    [`ServiceManager::runtime_dir`](crate::platform::ServiceManager::runtime_dir)),
///    if any.
/// 4. Render [`LaunchdManager::job_spec`] with `plist::render` and write
///    it to [`LaunchdManager::plist_path`] with `fsx::write_atomic` and
///    `plist::PLIST_MODE`.
/// 5. `launchctl bootstrap <domain> <plist path>`. A failure goes through
///    `exit::classify`: a missing domain is exit 7; anything else is
///    returned as the `launchctl` tool error, which the CLI reads as a
///    failed start.
/// 6. Wait, reading every poll interval, until one of: `run/state.json`
///    holds phase `ready` under an invocation id other than the one noted
///    in step 3; the job has no pid any more (the run ended); or
///    [`LaunchdManager::startup_wait`] passed. Each ends the wait with
///    `Ok(())`: the CLI then reads the facts and reports.
pub fn start(manager: &LaunchdManager, name: &InstanceName) -> Result<(), Error> {
    let _ = (manager, name);
    Err(not_built("start"))
}

/// `restart NAME`: `launchctl bootout <service target>` if launchd holds
/// the job (running or not), wait until [`LaunchdManager::job`] is `None`,
/// then steps 3 to 6 of [`start`].
pub fn restart(manager: &LaunchdManager, name: &InstanceName) -> Result<(), Error> {
    let _ = (manager, name);
    Err(not_built("restart"))
}

#[cfg(test)]
#[path = "start_tests.rs"]
mod tests;
