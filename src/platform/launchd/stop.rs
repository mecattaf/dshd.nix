//! `stop` under launchd: `bootout`, then wait while launchd tears the job
//! down, as `systemctl stop` waits on Linux. launchd sends the runner
//! SIGTERM; the runner stops dsh as it does today; launchd kills what is
//! left of the job's process group when the runner exits, and SIGKILLs a
//! runner still there after `ExitTimeOut`. The CLI wrote the stop intent
//! into the run directory before calling this.

use std::time::Instant;

use crate::error::{Error, ToolFailure};
use crate::name::InstanceName;
use crate::platform::launchd::LaunchdManager;
use crate::platform::launchd::exit::{self, Failure};

/// `stop NAME`:
///
/// 1. `launchctl bootout <service target>`. A job launchd does not hold
///    (`exit::classify` says `NoSuchJob`) is already stopped: `Ok(())`,
///    with no other call. A missing domain is exit 7.
/// 2. Read [`LaunchdManager::job`] every
///    [`LaunchdManager::poll_interval`] until it is `None`.
/// 3. Still held after [`LaunchdManager::stop_wait`]: an `Error::Tool`
///    with `tool` `launchctl`, `args` `["bootout", <service target>]`,
///    `status` `Some(0)` and a `stderr` saying the job was still loaded
///    after that many seconds. The CLI reports it as `stop-failed`.
pub fn stop(manager: &LaunchdManager, name: &InstanceName) -> Result<(), Error> {
    let target = manager.service_target(name);
    let args = ["bootout".to_owned(), target.clone()];
    if let Err(error) = manager.call(&args) {
        match exit::classify(error) {
            Failure::NoSuchJob => return Ok(()),
            Failure::Other(error) => return Err(error),
        }
    }
    let deadline = Instant::now() + manager.stop_wait();
    loop {
        if manager.job(name)?.is_none() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Tool(Box::new(ToolFailure {
                tool: "launchctl",
                args: vec!["bootout".into(), target],
                status: Some(0),
                stderr: format!(
                    "the job was still loaded {} seconds after bootout",
                    manager.stop_wait().as_secs()
                ),
            })));
        }
        std::thread::sleep(manager.poll_interval());
    }
}

#[cfg(test)]
#[path = "stop_tests.rs"]
mod tests;
