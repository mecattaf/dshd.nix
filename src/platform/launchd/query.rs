//! Gathers what [`facts::unit_facts`] maps: launchd's view of the job,
//! the `LaunchAgents` copy, and dshd's own run files. Reads only; the
//! reconciliation of a run whose runner died happens in the CLI, under
//! the instance lock.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, ToolFailure};
use crate::fsx;
use crate::name::InstanceName;
use crate::platform::ServiceManager;
use crate::platform::launchd::enable::{self, CopyState};
use crate::platform::launchd::exit::{self, Failure};
use crate::platform::launchd::facts::{self, Observed};
use crate::platform::launchd::print::{self, JobFacts, JobState, LastExit, ServiceEntry};
use crate::platform::launchd::restart::{self, STARTS_FILE, StartHistory};
use crate::platform::launchd::{LaunchdManager, runstate};
use crate::record::LastRun;
use crate::systemctl::UnitFacts;

/// The facts of one instance:
/// - `job`: [`LaunchdManager::job`];
/// - `disabled`: `print-disabled <domain>` through `print::parse_disabled`
///   lists the label as disabled;
/// - `enabled_copy`: `enable::copy_state` is not `Absent`;
/// - `state`: `runstate::read` of the run directory;
/// - `process_start`: [`LaunchdManager::process_start`] of the job's pid;
/// - `last_run`: `last-run.json` of the instance (`fsx::read_json`);
/// - `restarts`: `restart::restarts` of `run/starts.json`, 0 when absent.
///
/// A missing GUI domain is exit 7; a file that does not parse is an error.
pub fn show(manager: &LaunchdManager, name: &InstanceName) -> Result<UnitFacts, Error> {
    let job = manager.job(name)?;
    let disabled = disabled_labels(manager)?;
    let observed = observe(manager, name, job, &disabled)?;
    Ok(facts::unit_facts(&manager.service_target(name), &observed))
}

/// The facts of several instances, in the order given, from one
/// `print <domain>` and one `print-disabled <domain>` for all of them (no
/// `print` per job): a listed label is a job held, `Running` with its pid
/// or `NotRunning` with its last exit code; the files are read as in
/// [`show`].
pub fn show_many(
    manager: &LaunchdManager,
    names: &[InstanceName],
) -> Result<Vec<UnitFacts>, Error> {
    let services = print::parse_domain_services(&domain_text(manager, "print")?);
    let disabled = disabled_labels(manager)?;
    let held: BTreeMap<&str, &ServiceEntry> = services
        .iter()
        .map(|entry| (entry.label.as_str(), entry))
        .collect();
    names
        .iter()
        .map(|name| {
            let label = LaunchdManager::label(name);
            let job = held.get(label.as_str()).map(|entry| listed_job(entry));
            let observed = observe(manager, name, job, &disabled)?;
            Ok(facts::unit_facts(&manager.service_target(name), &observed))
        })
        .collect()
}

/// The instances launchd holds: the labels in `print <domain>` that are
/// `dshd.` and a valid instance name, sorted by name, each once.
pub fn list_loaded(manager: &LaunchdManager) -> Result<Vec<InstanceName>, Error> {
    let services = print::parse_domain_services(&domain_text(manager, "print")?);
    let names: BTreeSet<InstanceName> = services
        .iter()
        .filter_map(|entry| LaunchdManager::name_of_label(&entry.label))
        .collect();
    Ok(names.into_iter().collect())
}

/// The observations of one instance besides launchd's view of its job,
/// which the caller read: the disabled list, the `LaunchAgents` copy and
/// dshd's run files.
fn observe(
    manager: &LaunchdManager,
    name: &InstanceName,
    job: Option<JobFacts>,
    disabled: &BTreeMap<String, bool>,
) -> Result<Observed, Error> {
    let run_dir = manager.runtime_dir(name);
    let state = runstate::read(&run_dir)?;
    let process_start = job
        .as_ref()
        .and_then(|job| job.pid)
        .and_then(|pid| manager.process_start(pid));
    let last_run: Option<LastRun> = fsx::read_json(&manager.instance_dir(name).last_run_json())?;
    let history: Option<StartHistory> = fsx::read_json(&run_dir.path().join(STARTS_FILE))?;
    Ok(Observed {
        job,
        disabled: disabled
            .get(&LaunchdManager::label(name))
            .copied()
            .unwrap_or(false),
        enabled_copy: enable::copy_state(manager, name)? != CopyState::Absent,
        state,
        process_start,
        last_run,
        restarts: history.as_ref().map_or(0, restart::restarts),
    })
}

/// A job as one line of the domain's `services` block shows it: running
/// with its pid, or not running with its last exit code. A negative last
/// exit is the signal that ended the job, as `launchctl list` documents
/// and a macOS 26 runner printed (`-9` for a runner killed by SIGKILL, whose
/// own `print` says `last terminating signal = Killed: 9`).
fn listed_job(entry: &ServiceEntry) -> JobFacts {
    JobFacts {
        state: if entry.pid.is_some() {
            JobState::Running
        } else {
            JobState::NotRunning
        },
        pid: entry.pid,
        runs: None,
        last_exit: entry.last_exit.map(|code| match code.checked_neg() {
            Some(signal) if signal > 0 => LastExit::Signal(signal),
            _ => LastExit::Code(code),
        }),
        path: None,
    }
}

/// `print-disabled <domain>`, parsed.
fn disabled_labels(manager: &LaunchdManager) -> Result<BTreeMap<String, bool>, Error> {
    Ok(print::parse_disabled(&domain_text(
        manager,
        "print-disabled",
    )?))
}

/// The text of `launchctl VERB <domain>`. A missing GUI domain is exit 7.
/// launchd answering "no such job" for the domain itself is reported as
/// the tool failure it is, not read as an empty domain.
fn domain_text(manager: &LaunchdManager, verb: &str) -> Result<String, Error> {
    let args = [verb.to_owned(), manager.domain()];
    match manager.call(&args) {
        Ok(captured) => Ok(captured.stdout_text()),
        Err(error) => Err(match exit::classify(error) {
            Failure::Other(error) => error,
            Failure::NoSuchJob => Error::Tool(Box::new(ToolFailure {
                tool: "launchctl",
                args: args.to_vec(),
                status: Some(exit::NO_SERVICE),
                stderr: "launchd found no such service for the domain itself".to_owned(),
            })),
        }),
    }
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
