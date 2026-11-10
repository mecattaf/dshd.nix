//! Unit tests of the systemctl module: `show` fixtures, the state table,
//! remedies and the manager's paths, without a user manager.

use super::*;

const RUNNING: &str = include_str!("../../tests/fixtures/show/running.txt");
const INACTIVE: &str = include_str!("../../tests/fixtures/show/inactive.txt");
const AUTO_RESTART: &str = include_str!("../../tests/fixtures/show/auto-restart.txt");
const START_LIMIT: &str = include_str!("../../tests/fixtures/show/failed-start-limit.txt");
const NOT_FOUND: &str = include_str!("../../tests/fixtures/show/not-found.txt");
const MASKED: &str = include_str!("../../tests/fixtures/show/masked.txt");
const BOOT: &str = "7f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b";
const OTHER_BOOT: &str = "00000000-0000-0000-0000-000000000000";

fn one(text: &str) -> UnitFacts {
    let mut facts = parse_show(text).unwrap();
    assert_eq!(facts.len(), 1, "{text}");
    facts.pop().unwrap()
}

fn record() -> InstanceRecord {
    serde_json::from_str(include_str!("../../tests/fixtures/instance.json")).unwrap()
}

/// A last run in the shape `dshd-run` writes: `end` is JSON null for a run
/// whose end was never recorded.
fn last_run(boot_id: &str, end: &serde_json::Value) -> LastRun {
    let run = serde_json::json!({
        "id": "4b2d6a5e-1f3c-4e7a-9c0d-2a1b3c4d5e6f",
        "releasePath": "/nix/store/0000000000000000000000000000000a-dshd-release-dsh-0.2.2-rc.1",
        "bootId": boot_id,
        "startedAt": 1_794_009_610,
        "invocationId": "0123456789abcdef0123456789abcdef",
        "end": end,
    });
    serde_json::from_value(run).unwrap()
}

/// A recorded end: `serviceResult`, whether dshd meant the stop, and the
/// stop signal the runner got.
fn end(service_result: &str, stop_intent: bool, stop_signal: Option<i32>) -> serde_json::Value {
    serde_json::json!({
        "endedAt": 1_794_013_210,
        "serviceResult": service_result,
        "exitCode": "exited",
        "exitStatus": "0",
        "child": { "exitCode": 0 },
        "stopIntent": stop_intent,
        "stopSignal": stop_signal,
    })
}

// catches: a show property misread (a pid, a timestamp, a load state), so every state built on it is wrong
#[test]
fn parse_show_fixtures() {
    let running = one(RUNNING);
    assert_eq!(running.unit, "dshd@web-1.service");
    assert_eq!(running.load_state, LoadState::Loaded);
    assert_eq!(running.active_state, ActiveState::Active);
    assert_eq!(running.sub_state, "running");
    assert_eq!(running.result, UnitResult::Success);
    assert_eq!(running.main_pid, 4242);
    assert_eq!(running.active_enter, Some(1_794_009_610));
    assert_eq!(running.inactive_enter, None);
    assert_eq!(running.invocation_id, "0123456789abcdef0123456789abcdef");

    let inactive = one(INACTIVE);
    assert_eq!(inactive.active_state, ActiveState::Inactive);
    assert_eq!(inactive.sub_state, "dead");
    assert_eq!((inactive.main_pid, inactive.restarts), (0, 0));
    assert_eq!(inactive.inactive_enter, Some(1_794_013_210));
    assert_eq!(inactive.invocation_id, "");

    let restarting = one(AUTO_RESTART);
    assert_eq!(restarting.active_state, ActiveState::Activating);
    assert_eq!(restarting.sub_state, "auto-restart");
    assert_eq!(restarting.result, UnitResult::ExitCode);
    assert_eq!((restarting.exec_main_status, restarting.restarts), (75, 1));

    let limited = one(START_LIMIT);
    assert_eq!(limited.active_state, ActiveState::Failed);
    assert_eq!(limited.result, UnitResult::StartLimitHit);
    assert_eq!(limited.restarts, 5);
    assert_eq!(limited.active_enter, None);

    let gone = one(NOT_FOUND);
    assert_eq!(gone.unit, "dshd@gone.service");
    assert_eq!(gone.load_state, LoadState::NotFound);
    assert_eq!(one(MASKED).load_state, LoadState::Masked);

    let all = parse_show(&[RUNNING, INACTIVE, NOT_FOUND].join("\n\n")).unwrap();
    let units: Vec<&str> = all.iter().map(|facts| facts.unit.as_str()).collect();
    assert_eq!(
        units,
        [
            "dshd@web-1.service",
            "dshd@web-1.service",
            "dshd@gone.service"
        ]
    );
    assert_eq!(all.get(1), Some(&inactive));
}

// catches: an unknown systemd value dropped or turned into a known one
#[test]
fn parse_show_keeps_unknown_values() {
    let source = RUNNING
        .replace("LoadState=loaded", "LoadState=bad-setting")
        .replace("ActiveState=active", "ActiveState=reloading")
        .replace("Result=success", "Result=oom-kill");
    let facts = one(&source);
    assert_eq!(facts.load_state, LoadState::Unknown("bad-setting".into()));
    assert_eq!(facts.active_state, ActiveState::Unknown("reloading".into()));
    assert_eq!(facts.result, UnitResult::Unknown("oom-kill".into()));
    assert_eq!(facts.main_pid, 4242);
    assert!(parse_show("\n\n").unwrap().is_empty());
}

// catches: incomplete, duplicated or malformed facts accepted as a real process identity
#[test]
fn malformed_show_blocks_are_refused() {
    for broken in [
        RUNNING.replace("MainPID=4242\n", ""),
        format!("{RUNNING}MainPID=1\n"),
        format!("{RUNNING}Unexpected=x\n"),
        format!("{RUNNING}not a property\n"),
        RUNNING.replace("MainPID=4242", "MainPID=junk"),
        RUNNING.replace("NRestarts=0", "NRestarts=-1"),
        RUNNING.replace("CPUUsageNSec=1234567890", "CPUUsageNSec=bad"),
        RUNNING.replace(
            "InvocationID=0123456789abcdef0123456789abcdef",
            "InvocationID=invalid",
        ),
    ] {
        assert_eq!(
            parse_show(&broken).unwrap_err().kind(),
            "systemctl-failed",
            "{broken}"
        );
    }
    let systemctl = Systemctl::from_env();
    let raw = Captured {
        stdout: vec![0xff],
        stderr: vec![],
    };
    assert_eq!(
        systemctl.text(&raw, "show").unwrap_err().kind(),
        "systemctl-failed"
    );
}

// catches: unavailable counters invented as zero, or UINT64_MAX reported as an enormous measurement
#[test]
fn native_counters_keep_zero_and_absence_distinct() {
    let measured = one(RUNNING);
    assert_eq!(measured.resources.cpu_usage_nsec, Some(1_234_567_890));
    assert_eq!(measured.resources.memory_peak, Some(52_428_800));
    assert_eq!(measured.resources.tasks_current, Some(12));
    assert_eq!(measured.resources.io_read_bytes, None);
    for absent in ["[not set]", "18446744073709551615", ""] {
        let facts = one(&RUNNING.replace("MemoryPeak=52428800", &format!("MemoryPeak={absent}")));
        assert_eq!(facts.resources.memory_peak, None);
    }
    assert_eq!(
        one(&RUNNING.replace("MemoryPeak=52428800", "MemoryPeak=0"))
            .resources
            .memory_peak,
        Some(0)
    );
}

// catches: a maintenance or future active state relabelled as a known lifecycle phase
#[test]
fn unknown_active_states_remain_unknown() {
    for value in ["maintenance", "reloading", "refreshing"] {
        let facts = one(&RUNNING.replace("ActiveState=active", &format!("ActiveState={value}")));
        assert_eq!(
            derive_state(Some(&facts), Some(&record()), None, BOOT),
            StateSlug::Unknown
        );
        assert_eq!(facts.active_state, ActiveState::Unknown(value.to_owned()));
    }
}

// catches: a state derived from the wrong fact: a masked unit read as stopped, a run without an end read as
// stopped, a failed run read as interrupted
#[test]
fn derive_state_table() {
    let record = record();
    let (running, inactive, restarting, limited, gone, masked) = (
        one(RUNNING),
        one(INACTIVE),
        one(AUTO_RESTART),
        one(START_LIMIT),
        one(NOT_FOUND),
        one(MASKED),
    );
    let clean = last_run(BOOT, &end("success", true, Some(15)));
    let failed = last_run(BOOT, &end("exit-code", false, None));
    let unfinished = last_run(OTHER_BOOT, &serde_json::Value::Null);
    let earlier = last_run(OTHER_BOOT, &end("success", false, None));
    let mut stopping = running.clone();
    stopping.active_state = ActiveState::Deactivating;
    let mut masked_running = running.clone();
    masked_running.load_state = LoadState::Masked;
    let rec = Some(&record);
    let rows = [
        (None, None, None, StateSlug::Inconsistent),
        (Some(&gone), None, None, StateSlug::Inconsistent),
        (Some(&running), None, None, StateSlug::Orphaned),
        (Some(&limited), None, Some(&clean), StateSlug::Orphaned),
        (Some(&running), rec, Some(&clean), StateSlug::Running),
        (Some(&restarting), rec, Some(&failed), StateSlug::Starting),
        (Some(&stopping), rec, None, StateSlug::Stopping),
        (Some(&limited), rec, Some(&clean), StateSlug::Failed),
        (Some(&inactive), rec, None, StateSlug::Stopped),
        (None, rec, None, StateSlug::Stopped),
        (Some(&gone), rec, Some(&clean), StateSlug::Stopped),
        (Some(&inactive), rec, Some(&clean), StateSlug::Stopped),
        (Some(&inactive), rec, Some(&failed), StateSlug::Failed),
        (None, rec, Some(&unfinished), StateSlug::Interrupted),
        (None, rec, Some(&earlier), StateSlug::PreviousBoot),
        (Some(&masked), rec, Some(&clean), StateSlug::Masked),
        (Some(&masked), rec, None, StateSlug::Masked),
        (Some(&masked_running), rec, Some(&clean), StateSlug::Running),
    ];
    for (index, (facts, record, last_run, expected)) in rows.into_iter().enumerate() {
        assert_eq!(
            derive_state(facts, record, last_run, BOOT),
            expected,
            "row {index}"
        );
    }
}

// catches: a stop dshd asked for reported as one it did not, or a failure hidden behind a stop
#[test]
fn ending_precedence() {
    let rows = [
        (serde_json::Value::Null, Ending::Interrupted),
        (end("signal", true, Some(15)), Ending::Failed),
        (end("exit-code", false, None), Ending::Failed),
        (end("success", true, Some(15)), Ending::Stopped),
        (end("success", true, None), Ending::Stopped),
        (
            end("success", false, Some(15)),
            Ending::StoppedWithoutIntent,
        ),
        (end("success", false, None), Ending::Exited),
    ];
    for (index, (recorded, expected)) in rows.into_iter().enumerate() {
        assert_eq!(ending(&last_run(BOOT, &recorded)), expected, "row {index}");
    }
}

// catches: a remedy that resubmits a finished headless task, unmasks behind the user's back, or removes a live orphan
#[test]
fn remedy_per_state() {
    let name = InstanceName::parse("web-1").unwrap();
    let remedy = |state, up, task| remedy(state, &name, up, task);
    for state in [
        StateSlug::Failed,
        StateSlug::Stopped,
        StateSlug::Interrupted,
        StateSlug::PreviousBoot,
    ] {
        assert_eq!(
            remedy(state, false, Task::NotHeadless).as_deref(),
            Some("dshd start web-1"),
            "{state:?}"
        );
        assert_eq!(
            remedy(state, false, Task::NotRun).as_deref(),
            Some("dshd start web-1"),
            "{state:?}"
        );
        assert_eq!(
            remedy(state, false, Task::Ended(Ending::Exited)),
            None,
            "{state:?}"
        );
        for ended in [
            Ending::Failed,
            Ending::Interrupted,
            Ending::Stopped,
            Ending::StoppedWithoutIntent,
        ] {
            assert_eq!(
                remedy(state, false, Task::Ended(ended)).as_deref(),
                Some("dshd start --rerun web-1"),
                "{state:?}"
            );
        }
    }
    assert_eq!(
        remedy(StateSlug::Inconsistent, false, Task::NotHeadless).as_deref(),
        Some("dshd rm web-1")
    );
    assert_eq!(
        remedy(StateSlug::Orphaned, true, Task::NotHeadless).as_deref(),
        Some("dshd stop web-1")
    );
    assert_eq!(
        remedy(StateSlug::Orphaned, false, Task::NotHeadless).as_deref(),
        Some("dshd rm web-1")
    );
    let unmask = Some("systemctl --user unmask dshd@web-1.service");
    assert_eq!(
        remedy(StateSlug::Masked, false, Task::NotHeadless).as_deref(),
        unmask
    );
    for state in [StateSlug::Running, StateSlug::Starting, StateSlug::Stopping] {
        assert_eq!(remedy(state, true, Task::NotHeadless), None, "{state:?}");
    }
}

// catches: a shell from su or sudo -u told to enable linger instead of logging in as the user
#[test]
fn foreign_session_by_runtime_dir() {
    assert!(foreign_session(false, None, Some(1000)).is_some_and(|why| why.contains("unset")));
    assert!(
        foreign_session(true, None, Some(1000)).is_some_and(|why| why.contains("no directory"))
    );
    assert!(
        foreign_session(true, Some(0), Some(1000)).is_some_and(|why| why.contains("another user"))
    );
    assert_eq!(foreign_session(true, Some(1000), Some(1000)), None);
    assert_eq!(
        foreign_session(true, Some(1000), None),
        None,
        "an unknown uid blames nothing"
    );
}

// catches: a manager that does not answer reported as a tool failure instead of systemd-unavailable
#[test]
fn manager_environment_failure_is_systemd_unavailable() {
    // `sh --user show-environment` rejects the option and exits non-zero,
    // standing in for a manager that does not answer.
    let unreachable = Systemctl::new(Tool::at("systemctl", "/bin/sh"));
    let error = unreachable.manager_environment().unwrap_err();
    assert_eq!(
        (error.kind(), error.exit_code()),
        ("systemd-unavailable", 7),
        "{error:?}"
    );
    assert_eq!(
        unreachable.runtime_root().unwrap_err().kind(),
        "systemd-unavailable"
    );
    let missing = Systemctl::new(Tool::at("systemctl", "/nonexistent/dshd-no-such-systemctl"));
    assert!(matches!(
        missing.manager_environment(),
        Err(Error::ToolMissing("systemctl"))
    ));
}

// catches: a relative runtime path from the manager used for an attachment socket
#[test]
fn manager_paths_take_absolute_values_only() {
    let paths = ManagerPaths::from_environment(&parse_environment("XDG_RUNTIME_DIR=relative\n"));
    assert_eq!(paths.runtime_dir, None);
    let paths = ManagerPaths::from_environment(&parse_environment("XDG_RUNTIME_DIR=/run/alice\n"));
    let systemctl = Systemctl::new(Tool::at("systemctl", "/nonexistent/dshd-no-such-systemctl"));
    assert!(systemctl.paths.set(paths).is_ok());
    assert_eq!(
        systemctl.runtime_root().unwrap(),
        PathBuf::from("/run/alice")
    );
}

// catches: an enabled instance inferred from a client config path rather than manager dependencies
#[test]
fn wanted_by_is_the_autostart_authority() {
    assert!(!one(RUNNING).enabled());
    assert!(one(&RUNNING.replace("WantedBy=", "WantedBy=other.target dshd.target")).enabled());
    assert!(!one(&RUNNING.replace("WantedBy=", "WantedBy=other.target")).enabled());
}

// catches: a show call with no units, which systemctl answers with the manager's own properties
#[test]
fn empty_show_many_runs_nothing() {
    let systemctl = Systemctl::new(Tool::at("systemctl", "/nonexistent/dshd-no-such-systemctl"));
    assert!(systemctl.show_many(&[]).unwrap().is_empty());
    assert_eq!(
        systemctl.echo(&["start", "dshd@web-1.service"]),
        "systemctl --user start dshd@web-1.service"
    );
}
