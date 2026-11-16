//! `start` and `restart` under launchd. `RunAtLoad` makes the bootstrap
//! the start, so dshd never uses `kickstart`: one start spawns at most one
//! runner, and a headless task cannot run twice from one start. Then the
//! start waits for readiness, as `systemctl start` waits for `READY=1` on
//! Linux, so the CLI's settle and report stay the same on both platforms.

use std::time::Instant;

use crate::error::{Error, ToolFailure};
use crate::fsx;
use crate::name::InstanceName;
use crate::platform::ServiceManager;
use crate::platform::launchd::exit::{self, Failure};
use crate::platform::launchd::print::{JobFacts, JobState};
use crate::platform::launchd::runstate::{self, Phase};
use crate::platform::launchd::{LaunchdManager, plist};

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
    match manager.job(name)? {
        Some(job) if job.pid.is_some() => return Ok(()),
        Some(_) => boot_out(manager, name)?,
        None => {}
    }
    bootstrap_and_wait(manager, name)
}

/// `restart NAME`: `launchctl bootout <service target>` if launchd holds
/// the job (running or not), wait until [`LaunchdManager::job`] is `None`,
/// then steps 3 to 6 of [`start`].
pub fn restart(manager: &LaunchdManager, name: &InstanceName) -> Result<(), Error> {
    if manager.job(name)?.is_some() {
        boot_out(manager, name)?;
    }
    bootstrap_and_wait(manager, name)
}

/// `launchctl bootout` of a job launchd holds, then a read of the job every
/// poll interval until launchd no longer holds it. A job gone before the
/// bootout reached it is fine. One still held after
/// [`LaunchdManager::stop_wait`] is the error `stop` reports, since a
/// bootstrap over it would be refused.
fn boot_out(manager: &LaunchdManager, name: &InstanceName) -> Result<(), Error> {
    let target = manager.service_target(name);
    let args = ["bootout".to_owned(), target.clone()];
    if let Err(error) = manager.call(&args) {
        match exit::classify(error) {
            Failure::NoSuchJob => return Ok(()),
            Failure::Other(error) => return Err(error),
        }
    }
    let deadline = Instant::now() + manager.stop_wait();
    while manager.job(name)?.is_some() {
        if Instant::now() >= deadline {
            return Err(Error::Tool(Box::new(ToolFailure {
                tool: "launchctl",
                args: args.to_vec(),
                status: Some(0),
                stderr: format!(
                    "the job was still loaded {} seconds after bootout",
                    manager.stop_wait().as_secs()
                ),
            })));
        }
        std::thread::sleep(manager.poll_interval());
    }
    Ok(())
}

/// Steps 3 to 6 of [`start`].
fn bootstrap_and_wait(manager: &LaunchdManager, name: &InstanceName) -> Result<(), Error> {
    let run_dir = manager.runtime_dir(name);
    let before = runstate::read(&run_dir)?.map(|state| state.invocation_id);
    let rendered = plist::render(&manager.job_spec(name))?;
    let plist_path = manager.plist_path(name);
    fsx::write_atomic(&plist_path, rendered.as_bytes(), plist::PLIST_MODE)?;
    let args = [
        "bootstrap".to_owned(),
        manager.domain(),
        plist_path.display().to_string(),
    ];
    if let Err(error) = manager.call(&args) {
        return Err(match exit::classify(error) {
            Failure::Other(error) => error,
            // launchd has no "no such job" for a bootstrap; should it say
            // so, the start failed all the same.
            Failure::NoSuchJob => Error::Tool(Box::new(ToolFailure {
                tool: "launchctl",
                args: args.to_vec(),
                status: Some(exit::NO_SERVICE),
                stderr: "launchd found no such service to bootstrap".to_owned(),
            })),
        });
    }
    let deadline = Instant::now() + manager.startup_wait();
    loop {
        if let Some(state) = runstate::read(&run_dir)?
            && state.phase == Phase::Ready
            && before.as_ref() != Some(&state.invocation_id)
        {
            return Ok(());
        }
        if run_ended(manager.job(name)?.as_ref()) || Instant::now() >= deadline {
            return Ok(());
        }
        std::thread::sleep(manager.poll_interval());
    }
}

/// Whether the job's run ended: launchd holds no process for it and is not
/// about to spawn one (`spawn scheduled` right after the bootstrap is a
/// run not begun, not a run ended).
fn run_ended(job: Option<&JobFacts>) -> bool {
    job.is_none_or(|job| job.pid.is_none() && job.state != JobState::SpawnScheduled)
}

#[cfg(test)]
#[path = "start_tests.rs"]
mod tests;
