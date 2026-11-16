//! The launchd facts mapping, case by case, checked through to the state
//! vocabulary `derive_state` gives.

use super::*;
use crate::cli::report::StateSlug;
use crate::platform::launchd::print::{JobState, LastExit};
use crate::platform::launchd::runstate::Phase;
use crate::record::{InstanceRecord, OsText, RunEnd};
use crate::systemctl::derive_state;

const UNIT: &str = "gui/501/dshd.web-1";
const ID: &str = "0123456789abcdef0123456789abcdef";
const BOOT: &str = "6f1c2a9e-0b7d-4e55-9a10-3c2b1d0e9f87";
const START: u64 = 1_700_000_000;

fn running_job(pid: u32) -> JobFacts {
    JobFacts {
        state: JobState::Running,
        pid: Some(pid),
        runs: Some(1),
        last_exit: Some(LastExit::Never),
        path: None,
    }
}

fn ended_job(last_exit: LastExit) -> JobFacts {
    JobFacts {
        state: JobState::NotRunning,
        pid: None,
        runs: Some(1),
        last_exit: Some(last_exit),
        path: None,
    }
}

fn state(pid: u32, phase: Phase) -> RunState {
    RunState {
        invocation_id: ID.to_owned(),
        pid,
        process_start: START,
        phase,
        phase_since: START + 2,
    }
}

fn last_run(boot: &str, result: Option<&str>) -> LastRun {
    LastRun {
        id: "4b2d6a5e-1f3c-4e7a-9c0d-2a1b3c4d5e6f".to_owned(),
        release_path: OsText::from("/r"),
        boot_id: boot.to_owned(),
        started_at: START,
        invocation_id: ID.to_owned(),
        end: result.map(|result| RunEnd {
            ended_at: START + 60,
            service_result: result.to_owned(),
            exit_code: "exited".to_owned(),
            exit_status: "0".to_owned(),
            child: None,
            stop_intent: false,
            stop_signal: None,
            resources: None,
            unrecognized: serde_json::Map::new(),
        }),
        unrecognized: serde_json::Map::new(),
    }
}

fn live(phase: Phase) -> Observed {
    Observed {
        job: Some(running_job(4242)),
        state: Some(state(4242, phase)),
        process_start: Some(START),
        ..Observed::default()
    }
}

fn record() -> InstanceRecord {
    let text = include_str!("../../../tests/fixtures/instance.json");
    serde_json::from_str(text).unwrap()
}

/// The state `get` would print for these facts.
fn slug(facts: &UnitFacts, observed: &Observed, boot: &str) -> StateSlug {
    derive_state(
        Some(facts),
        Some(&record()),
        observed.last_run.as_ref(),
        boot,
    )
}

// catches: a ready run not reported running, or reported without its pid, invocation or since
#[test]
fn a_ready_run_is_active() {
    let observed = live(Phase::Ready);
    let facts = unit_facts(UNIT, &observed);
    assert_eq!(facts.unit, UNIT);
    assert_eq!(facts.load_state, LoadState::Loaded);
    assert_eq!(facts.active_state, ActiveState::Active);
    assert_eq!(facts.sub_state, "running");
    assert_eq!(facts.main_pid, 4242);
    assert_eq!(facts.invocation_id, ID);
    assert_eq!(facts.active_enter, Some(START + 2));
    assert_eq!(facts.result, UnitResult::Success);
    assert_eq!(slug(&facts, &observed, BOOT), StateSlug::Running);
}

// catches: a run still starting, between crash restarts, or stopping read as running or as stopped
#[test]
fn moving_runs_are_activating_or_deactivating() {
    let rows = [
        (
            Phase::Starting,
            ActiveState::Activating,
            "start",
            StateSlug::Starting,
        ),
        (
            Phase::AutoRestart,
            ActiveState::Activating,
            "auto-restart",
            StateSlug::Starting,
        ),
        (
            Phase::Stopping,
            ActiveState::Deactivating,
            "stop-sigterm",
            StateSlug::Stopping,
        ),
    ];
    for (phase, active, sub, expected) in rows {
        let observed = live(phase);
        let facts = unit_facts(UNIT, &observed);
        assert_eq!(facts.active_state, active, "{phase:?}");
        assert_eq!(facts.sub_state, sub, "{phase:?}");
        assert_eq!(facts.invocation_id, ID, "{phase:?}");
        assert_eq!(slug(&facts, &observed, BOOT), expected, "{phase:?}");
    }
    let activating = unit_facts(UNIT, &live(Phase::Starting));
    assert_eq!(activating.inactive_enter, Some(START + 2));
}

// catches: a state file left by an earlier process (pid reused, or no state yet) taken as the live run's identity
#[test]
fn a_stale_state_file_names_no_run() {
    let reused = Observed {
        process_start: Some(START + 500),
        ..live(Phase::Ready)
    };
    let other_pid = Observed {
        state: Some(state(17, Phase::Ready)),
        ..live(Phase::Ready)
    };
    let none_yet = Observed {
        state: None,
        ..live(Phase::Ready)
    };
    for observed in [reused, other_pid, none_yet] {
        let facts = unit_facts(UNIT, &observed);
        assert_eq!(facts.active_state, ActiveState::Activating, "{observed:?}");
        assert_eq!(facts.sub_state, "start", "{observed:?}");
        assert_eq!(facts.invocation_id, "", "{observed:?}");
        assert_eq!(facts.main_pid, 4242, "{observed:?}");
    }
}

// catches: a clean end read as failed, a run of an earlier boot read as this boot's, or the end time lost
#[test]
fn a_clean_end_is_stopped_or_previous_boot() {
    for job in [Some(ended_job(LastExit::Code(0))), None] {
        let observed = Observed {
            job: job.clone(),
            last_run: Some(last_run(BOOT, Some("success"))),
            ..Observed::default()
        };
        let facts = unit_facts(UNIT, &observed);
        let loaded = if job.is_some() {
            LoadState::Loaded
        } else {
            LoadState::NotFound
        };
        assert_eq!(facts.load_state, loaded);
        assert_eq!(facts.active_state, ActiveState::Inactive);
        assert_eq!(facts.sub_state, "dead");
        assert_eq!(facts.main_pid, 0);
        assert_eq!(facts.invocation_id, "");
        assert_eq!(facts.inactive_enter, Some(START + 60));
        assert_eq!(slug(&facts, &observed, BOOT), StateSlug::Stopped);
        assert_eq!(
            slug(&facts, &observed, "00000000-0000-0000-0000-000000000000"),
            StateSlug::PreviousBoot
        );
    }
}

// catches: a failed or start-limited run read as stopped, or its result word lost
#[test]
fn an_unclean_end_is_failed() {
    let rows = [
        ("exit-code", UnitResult::ExitCode),
        ("signal", UnitResult::Signal),
        ("timeout", UnitResult::Timeout),
        ("start-limit-hit", UnitResult::StartLimitHit),
    ];
    for (word, result) in rows {
        let observed = Observed {
            job: Some(ended_job(LastExit::Code(78))),
            last_run: Some(last_run(BOOT, Some(word))),
            ..Observed::default()
        };
        let facts = unit_facts(UNIT, &observed);
        assert_eq!(facts.active_state, ActiveState::Failed, "{word}");
        assert_eq!(facts.sub_state, "failed", "{word}");
        assert_eq!(facts.result, result, "{word}");
        assert_eq!(facts.exec_main_status, 78, "{word}");
        assert_eq!(slug(&facts, &observed, BOOT), StateSlug::Failed, "{word}");
    }
}

// catches: a run whose runner died before recording its end not reported interrupted
#[test]
fn a_run_without_an_end_is_interrupted() {
    let observed = Observed {
        job: Some(ended_job(LastExit::Signal(9))),
        last_run: Some(last_run(BOOT, None)),
        ..Observed::default()
    };
    let facts = unit_facts(UNIT, &observed);
    assert_eq!(facts.active_state, ActiveState::Inactive);
    assert_eq!(facts.exec_main_status, 9);
    assert_eq!(slug(&facts, &observed, BOOT), StateSlug::Interrupted);
}

// catches: a job the user disabled with launchctl not reported masked
#[test]
fn a_disabled_job_is_masked() {
    let observed = Observed {
        disabled: true,
        last_run: Some(last_run(BOOT, Some("success"))),
        ..Observed::default()
    };
    let facts = unit_facts(UNIT, &observed);
    assert_eq!(facts.load_state, LoadState::Masked);
    assert_eq!(slug(&facts, &observed, BOOT), StateSlug::Masked);
}

// catches: an enabled instance (its LaunchAgents copy exists) not reported enabled, or a plain one reported enabled
#[test]
fn the_copy_makes_the_instance_enabled() {
    let enabled = unit_facts(
        UNIT,
        &Observed {
            enabled_copy: true,
            ..live(Phase::Ready)
        },
    );
    assert!(enabled.enabled());
    assert_eq!(enabled.wanted_by, ["dshd.target"]);
    assert!(!unit_facts(UNIT, &live(Phase::Ready)).enabled());
}

// catches: a state word launchctl printed that this version does not know mapped onto a known state
#[test]
fn an_unknown_job_state_is_unknown() {
    let observed = Observed {
        job: Some(JobFacts {
            state: JobState::Other("penguin".to_owned()),
            pid: None,
            runs: None,
            last_exit: None,
            path: None,
        }),
        ..Observed::default()
    };
    let facts = unit_facts(UNIT, &observed);
    assert_eq!(
        facts.active_state,
        ActiveState::Unknown("penguin".to_owned())
    );
    assert_eq!(facts.sub_state, "penguin");
    assert_eq!(slug(&facts, &observed, BOOT), StateSlug::Unknown);
}

// catches: restarts dropped, or invented resource counters (the contract allows null, never a made-up zero)
#[test]
fn restarts_pass_through_and_counters_stay_unknown() {
    let facts = unit_facts(
        UNIT,
        &Observed {
            restarts: 3,
            ..live(Phase::Ready)
        },
    );
    assert_eq!(facts.restarts, 3);
    assert_eq!(facts.resources, Resources::default());
}
