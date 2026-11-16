//! `start` and `restart` against the fake launchd.

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use super::*;
use crate::platform::launchd::plist;
use crate::platform::launchd::testing::{Bench, argv, state_json};

const NEW: &str = "11111111111111111111111111111111";
const OLD: &str = "00000000000000000000000000000000";

// catches: a start that kickstarts, bootstraps twice, or bootstraps a plist other than the one it wrote
#[test]
fn start_bootstraps_the_rendered_plist_once() {
    let bench = Bench::new("ld-start", "web-1");
    bench.ready_on_bootstrap(NEW, Duration::ZERO);
    start(&bench.manager, &bench.name).unwrap();
    assert_eq!(
        bench.fake.calls(),
        [
            argv(&["print", &bench.target()]),
            argv(&["bootstrap", "gui/501", &bench.plist()]),
        ]
    );
    let written = std::fs::read_to_string(bench.plist()).unwrap();
    let rendered = plist::render(&bench.manager.job_spec(&bench.name)).unwrap();
    assert_eq!(written, rendered);
    let mode = std::fs::metadata(bench.plist())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o644);
    let received = bench.fake.state().received.clone();
    assert_eq!(received, [rendered]);
    for text in &received {
        assert_eq!(plist::check_keys(text), Ok(()));
    }
}

// catches: a start of a running job that spawns a second runner or boots the first one out
#[test]
fn start_of_a_running_job_calls_nothing_more() {
    let bench = Bench::new("ld-start-running", "web-1");
    bench
        .fake
        .hold("dshd.web-1", &bench.plist(), Some(900), None);
    start(&bench.manager, &bench.name).unwrap();
    assert_eq!(bench.fake.calls(), [argv(&["print", &bench.target()])]);
}

// catches: a start that bootstraps over a loaded job that ended (launchd refuses it) instead of booting it out first
#[test]
fn start_of_an_ended_job_boots_it_out_first() {
    let bench = Bench::new("ld-start-ended", "web-1");
    bench.fake.hold("dshd.web-1", &bench.plist(), None, Some(0));
    bench.ready_on_bootstrap(NEW, Duration::ZERO);
    start(&bench.manager, &bench.name).unwrap();
    assert_eq!(
        bench.fake.calls(),
        [
            argv(&["print", &bench.target()]),
            argv(&["bootout", &bench.target()]),
            argv(&["print", &bench.target()]),
            argv(&["bootstrap", "gui/501", &bench.plist()]),
        ]
    );
}

// catches: a start that returns on the previous run's ready state file instead of waiting for its own run
#[test]
fn start_waits_for_a_new_invocation() {
    let bench = Bench::new("ld-start-wait", "web-1");
    let run = bench.run_dir();
    std::fs::write(run.path().join("state.json"), state_json(OLD, 1, "ready")).unwrap();
    bench.ready_on_bootstrap(NEW, Duration::from_millis(150));
    let began = Instant::now();
    start(&bench.manager, &bench.name).unwrap();
    assert!(
        began.elapsed() >= Duration::from_millis(150),
        "returned early"
    );
    let state = std::fs::read_to_string(run.path().join("state.json")).unwrap();
    assert!(state.contains(NEW), "{state}");
}

// catches: a start that waits out the whole deadline for a runner that already ended
#[test]
fn start_returns_when_the_run_ends_before_ready() {
    let bench = Bench::new("ld-start-ends", "web-1");
    bench.fake.state().exits_at_once = Some(78);
    let began = Instant::now();
    start(&bench.manager, &bench.name).unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(1),
        "{:?}",
        began.elapsed()
    );
    let verbs = bench.fake.verbs();
    assert_eq!(verbs.iter().filter(|verb| *verb == "bootstrap").count(), 1);
    assert_eq!(verbs.last().map(String::as_str), Some("print"), "{verbs:?}");
}

// catches: a start that never returns when the runner neither readies nor ends
#[test]
fn start_gives_up_at_the_startup_wait() {
    let bench = Bench::new("ld-start-deadline", "web-1");
    let manager = bench.manager.with_timing(
        Duration::from_millis(100),
        Duration::from_secs(2),
        Duration::from_millis(5),
    );
    let began = Instant::now();
    start(&manager, &bench.name).unwrap();
    let waited = began.elapsed();
    assert!(waited >= Duration::from_millis(100), "{waited:?}");
    assert!(waited < Duration::from_secs(1), "{waited:?}");
}

// catches: no GUI login session reported as anything but exit 7
#[test]
fn start_without_a_gui_session_exits_7() {
    let bench = Bench::new("ld-start-nogui", "web-1");
    bench.fake.state().no_gui_domain = true;
    let error = start(&bench.manager, &bench.name).unwrap_err();
    assert_eq!(error.exit_code(), 7, "{error}");
    assert!(
        error.to_string().contains("no GUI login session"),
        "{error}"
    );
}

// catches: a refused bootstrap swallowed, or reported under another tool's name so the CLI cannot read it as a failed start
#[test]
fn a_refused_bootstrap_is_a_launchctl_failure() {
    let bench = Bench::new("ld-start-refused", "web-1");
    bench.fake.state().bootstrap_failure =
        Some((5, "Bootstrap failed: 5: Input/output error".to_owned()));
    match start(&bench.manager, &bench.name) {
        Err(Error::Tool(failure)) => {
            assert_eq!(failure.tool, "launchctl");
            assert_eq!(failure.status, Some(5));
        }
        other => panic!("{other:?}"),
    }
}

// catches: a restart that bootstraps over the running job, or never starts the new one
#[test]
fn restart_boots_out_then_bootstraps() {
    let bench = Bench::new("ld-restart", "web-1");
    bench
        .fake
        .hold("dshd.web-1", &bench.plist(), Some(900), None);
    bench.ready_on_bootstrap(NEW, Duration::ZERO);
    restart(&bench.manager, &bench.name).unwrap();
    let calls = bench.fake.calls();
    assert_eq!(calls.first(), Some(&argv(&["print", &bench.target()])));
    assert!(
        calls.contains(&argv(&["bootout", &bench.target()])),
        "{calls:?}"
    );
    assert_eq!(
        calls.last(),
        Some(&argv(&["bootstrap", "gui/501", &bench.plist()]))
    );
    let bootout = calls
        .iter()
        .position(|call| call.first().map(String::as_str) == Some("bootout"));
    let bootstrap = calls
        .iter()
        .position(|call| call.first().map(String::as_str) == Some("bootstrap"));
    assert!(bootout < bootstrap, "{calls:?}");
}

// catches: any verb dshd must never use (kickstart, enable, disable, load) appearing in a start or restart
#[test]
fn start_and_restart_use_only_print_bootout_and_bootstrap() {
    let bench = Bench::new("ld-start-verbs", "web-1");
    bench.ready_on_bootstrap(NEW, Duration::ZERO);
    start(&bench.manager, &bench.name).unwrap();
    restart(&bench.manager, &bench.name).unwrap();
    for verb in bench.fake.verbs() {
        assert!(
            ["print", "bootout", "bootstrap"].contains(&verb.as_str()),
            "{verb}"
        );
    }
}
