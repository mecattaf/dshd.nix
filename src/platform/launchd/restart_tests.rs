//! The runner's restart policy.

use super::*;

fn history(starts: &[(u64, bool)]) -> StartHistory {
    StartHistory {
        starts: starts
            .iter()
            .map(|&(at, automatic)| Start { at, automatic })
            .collect(),
    }
}

// catches: a restart of anything but exit 75, of a stopped run, or of a runner ended by a signal
#[test]
#[ignore = "packet D12: restart policy"]
fn only_an_unasked_75_restarts() {
    assert_eq!(
        after_exit(75, false, false),
        AfterExit::Restart(RESTART_DELAY)
    );
    assert_eq!(after_exit(75, false, true), AfterExit::Exit(75));
    assert_eq!(after_exit(75, true, false), AfterExit::Exit(75));
    for status in [0, 1, 42, 78, 143] {
        assert_eq!(
            after_exit(status, false, false),
            AfterExit::Exit(status),
            "{status}"
        );
    }
}

// catches: a limit that counts old starts, admits one too many, or applies when none is set
#[test]
#[ignore = "packet D12: restart policy"]
fn the_start_limit_counts_only_the_interval() {
    let limit = Some(StartLimit {
        burst: 3,
        interval_secs: 100,
    });
    let three = history(&[(10, false), (50, true), (90, true)]);
    assert!(!admits(&three, 100, limit));
    assert!(
        admits(&three, 111, limit),
        "the start at 10 left the interval"
    );
    assert!(admits(&three, 100, None));
    assert!(admits(&StartHistory::default(), 0, limit));
}

// catches: restarts counting the first start or DSH's restart button
#[test]
#[ignore = "packet D12: restart policy"]
fn restarts_count_automatic_starts() {
    assert_eq!(
        restarts(&history(&[(1, false), (2, true), (3, false), (4, true)])),
        2
    );
    assert_eq!(restarts(&StartHistory::default()), 0);
}

// catches: the start history's JSON shape drifting from what the runner and the CLI read
#[test]
fn history_json_is_pinned() {
    let json = serde_json::to_string(&history(&[(7, true)])).unwrap();
    assert_eq!(json, r#"{"starts":[{"at":7,"automatic":true}]}"#);
}
