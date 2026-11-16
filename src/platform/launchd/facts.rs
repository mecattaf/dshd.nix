//! launchd's few facts plus dshd's own run records, mapped onto the
//! shared [`UnitFacts`], so `systemctl::derive_state`, `ending` and
//! `remedy` give the same vocabulary on both platforms. Pure: the
//! observations are gathered in `query`.
//!
//! The mapping (a run is *live* when `run/state.json` describes the
//! process launchd runs now, see `runstate::is_live`):
//!
//! | observation | `load_state` | `active_state` / `sub_state` |
//! |---|---|---|
//! | disabled in `print-disabled` | `Masked` (whatever else holds) | as below |
//! | launchd holds the job | `Loaded` | as below |
//! | launchd holds no job | `NotFound` | as below |
//! | live, phase `starting` | | `Activating` / `start` |
//! | live, phase `ready` | | `Active` / `running` |
//! | live, phase `auto-restart` | | `Activating` / `auto-restart` |
//! | live, phase `stopping` | | `Deactivating` / `stop-sigterm` |
//! | a pid but no live state file | | `Activating` / `start` |
//! | no pid, job state word unknown | | `Unknown(word)` / the word |
//! | no pid, last run ended other than `success` | | `Failed` / `failed` |
//! | otherwise | | `Inactive` / `dead` |
//!
//! And: `unit` is the service target given; `main_pid` the job's pid or
//! 0; `invocation_id` the live state file's id, else empty; `result` the
//! last recorded end's `serviceResult` (`Success` when none);
//! `exec_main_status` the job's last exit code or signal number, else 0;
//! `restarts` as observed; `active_enter` the live state's `phaseSince`
//! for `Active` and `Deactivating`; `inactive_enter` the live state's
//! `phaseSince` for `Activating`, else the last run's `endedAt`;
//! `wanted_by` `[dshd.target]` when the `LaunchAgents` copy exists, so
//! `UnitFacts::enabled` holds; `resources` all unknown (`None`).

use crate::platform::launchd::print::JobFacts;
use crate::platform::launchd::runstate::RunState;
use crate::record::LastRun;
use crate::systemctl::{ActiveState, LoadState, Resources, UnitFacts, UnitResult};

/// What dshd observed about one instance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    /// The job, when launchd holds one.
    pub job: Option<JobFacts>,
    /// `print-disabled` lists the label as disabled.
    pub disabled: bool,
    /// The copy in `~/Library/LaunchAgents` exists.
    pub enabled_copy: bool,
    /// `run/state.json`, when present.
    pub state: Option<RunState>,
    /// The start time of the job's pid, when known.
    pub process_start: Option<u64>,
    /// `last-run.json`, when present.
    pub last_run: Option<LastRun>,
    /// Automatic restarts in this job's start history.
    pub restarts: u32,
}

/// Maps the observations of one instance onto [`UnitFacts`], by the table
/// in the module documentation.
#[must_use]
pub fn unit_facts(unit: &str, observed: &Observed) -> UnitFacts {
    let _ = observed;
    UnitFacts {
        unit: unit.to_owned(),
        load_state: LoadState::Unknown("not built".to_owned()),
        active_state: ActiveState::Unknown("not built".to_owned()),
        sub_state: String::new(),
        result: UnitResult::Unknown("not built".to_owned()),
        main_pid: 0,
        exec_main_status: 0,
        restarts: 0,
        active_enter: None,
        inactive_enter: None,
        invocation_id: String::new(),
        wanted_by: Vec::new(),
        resources: Resources::default(),
    }
}

#[cfg(test)]
#[path = "facts_tests.rs"]
mod tests;
