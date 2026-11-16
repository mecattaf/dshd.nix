//! `stop` against the fake launchd.

use std::time::Duration;

use super::*;
use crate::platform::launchd::testing::{Bench, argv};

// catches: a stop that returns while launchd still holds the job, so a start right after it is refused
#[test]
#[ignore = "packet D05: launchd stop"]
fn stop_waits_until_launchd_lets_the_job_go() {
    let bench = Bench::new("ld-stop", "web-1");
    bench
        .fake
        .hold("dshd.web-1", &bench.plist(), Some(900), None);
    bench.fake.state().teardown_prints = 3;
    stop(&bench.manager, &bench.name).unwrap();
    let print = argv(&["print", &bench.target()]);
    assert_eq!(
        bench.fake.calls(),
        [
            argv(&["bootout", &bench.target()]),
            print.clone(),
            print.clone(),
            print.clone(),
            print,
        ]
    );
    assert!(bench.fake.state().jobs.is_empty());
}

// catches: a stop of a job launchd does not hold reported as a failure
#[test]
#[ignore = "packet D05: launchd stop"]
fn stop_of_a_job_launchd_does_not_hold_is_done() {
    let bench = Bench::new("ld-stop-none", "web-1");
    stop(&bench.manager, &bench.name).unwrap();
    assert_eq!(bench.fake.calls(), [argv(&["bootout", &bench.target()])]);
}

// catches: a stop that hangs forever, or reports success, when launchd never lets the job go
#[test]
#[ignore = "packet D05: launchd stop"]
fn a_job_that_stays_is_a_launchctl_failure() {
    let bench = Bench::new("ld-stop-stuck", "web-1");
    bench
        .fake
        .hold("dshd.web-1", &bench.plist(), Some(900), None);
    bench.fake.state().teardown_prints = u32::MAX;
    let manager = bench.manager.with_timing(
        Duration::from_secs(2),
        Duration::from_millis(80),
        Duration::from_millis(5),
    );
    match stop(&manager, &bench.name) {
        Err(Error::Tool(failure)) => {
            assert_eq!(failure.tool, "launchctl");
            assert_eq!(failure.status, Some(0));
            assert_eq!(failure.args, argv(&["bootout", "gui/501/dshd.web-1"]));
            assert!(
                failure.stderr.contains("still loaded"),
                "{}",
                failure.stderr
            );
        }
        other => panic!("{other:?}"),
    }
}

// catches: no GUI login session reported as anything but exit 7
#[test]
#[ignore = "packet D05: launchd stop"]
fn stop_without_a_gui_session_exits_7() {
    let bench = Bench::new("ld-stop-nogui", "web-1");
    bench.fake.state().no_gui_domain = true;
    assert_eq!(
        stop(&bench.manager, &bench.name).unwrap_err().exit_code(),
        7
    );
}
