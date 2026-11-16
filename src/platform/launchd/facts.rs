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

use crate::platform::launchd::print::{JobFacts, JobState, LastExit};
use crate::platform::launchd::runstate::{self, Phase, RunState};
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
    let job = observed.job.as_ref();
    let job_pid = job.and_then(|job| job.pid);
    let live = observed
        .state
        .as_ref()
        .filter(|state| runstate::is_live(state, job_pid, observed.process_start));

    let phase = live.map(|state| (state.phase, state.phase_since));
    let active_enter = phase
        .filter(|(phase, _)| matches!(phase, Phase::Ready | Phase::Stopping))
        .map(|(_, since)| since);
    let inactive_enter = match phase {
        Some((Phase::Starting | Phase::AutoRestart, since)) => Some(since),
        _ => observed
            .last_run
            .as_ref()
            .and_then(|run| run.end.as_ref())
            .map(|end| end.ended_at),
    };

    let (active_state, sub_state) = match live {
        Some(state) => match state.phase {
            Phase::Starting => (ActiveState::Activating, "start"),
            Phase::Ready => (ActiveState::Active, "running"),
            Phase::AutoRestart => (ActiveState::Activating, "auto-restart"),
            Phase::Stopping => (ActiveState::Deactivating, "stop-sigterm"),
        },
        None if job_pid.is_some() => (ActiveState::Activating, "start"),
        None => match job.and_then(|job| match &job.state {
            JobState::Other(word) => Some(word),
            _ => None,
        }) {
            Some(word) => (ActiveState::Unknown(word.clone()), word.as_str()),
            None if unclean_end(observed) => (ActiveState::Failed, "failed"),
            None => (ActiveState::Inactive, "dead"),
        },
    };

    let load_state = if observed.disabled {
        LoadState::Masked
    } else if observed.job.is_some() {
        LoadState::Loaded
    } else {
        LoadState::NotFound
    };

    UnitFacts {
        unit: unit.to_owned(),
        load_state,
        active_state,
        sub_state: sub_state.to_owned(),
        result: observed
            .last_run
            .as_ref()
            .and_then(|run| run.end.as_ref())
            .map_or(UnitResult::Success, |end| {
                crate::systemctl::unit_result(&end.service_result)
            }),
        main_pid: job_pid.unwrap_or(0),
        exec_main_status: job
            .and_then(|job| job.last_exit)
            .map_or(0, |exit| match exit {
                LastExit::Code(code) => code,
                LastExit::Signal(signal) => signal,
                LastExit::Never => 0,
            }),
        restarts: observed.restarts,
        active_enter,
        inactive_enter,
        invocation_id: live
            .map(|state| state.invocation_id.clone())
            .unwrap_or_default(),
        wanted_by: if observed.enabled_copy {
            vec!["dshd.target".to_owned()]
        } else {
            Vec::new()
        },
        resources: Resources::default(),
    }
}

fn unclean_end(observed: &Observed) -> bool {
    observed
        .last_run
        .as_ref()
        .and_then(|run| run.end.as_ref())
        .is_some_and(|end| end.service_result != "success")
}

#[cfg(test)]
#[path = "facts_tests.rs"]
mod tests;
