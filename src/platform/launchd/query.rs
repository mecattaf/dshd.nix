//! Gathers what [`facts::unit_facts`] maps: launchd's view of the job,
//! the `LaunchAgents` copy, and dshd's own run files. Reads only; the
//! reconciliation of a run whose runner died happens in the CLI, under
//! the instance lock.

use crate::error::Error;
use crate::name::InstanceName;
use crate::platform::launchd::facts::{self, Observed};
use crate::platform::launchd::{LaunchdManager, not_built};
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
    let _ = (manager, name, Observed::default(), facts::unit_facts);
    Err(not_built("reading an instance's facts"))
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
    let _ = (manager, names);
    Err(not_built("reading instances' facts"))
}

/// The instances launchd holds: the labels in `print <domain>` that are
/// `dshd.` and a valid instance name, sorted by name, each once.
pub fn list_loaded(manager: &LaunchdManager) -> Result<Vec<InstanceName>, Error> {
    let _ = manager;
    Err(not_built("listing loaded instances"))
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
