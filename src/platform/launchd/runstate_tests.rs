//! Tests of the run state file.

use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::fsx::testing::Scratch;

fn sample() -> RunState {
    RunState {
        invocation_id: "0123456789abcdef0123456789abcdef".to_owned(),
        pid: 4242,
        process_start: 1_700_000_000,
        phase: Phase::Ready,
        phase_since: 1_700_000_003,
    }
}

fn run_dir(scratch: &Scratch) -> RuntimeDir {
    let dir = scratch.path().join("run");
    std::fs::create_dir_all(&dir).unwrap();
    RuntimeDir::new(dir)
}

// catches: the state file's JSON keys or phase words drifting from what the fake, the CLI and older runners read
#[test]
fn json_shape_is_pinned() {
    let json = serde_json::to_value(sample()).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "invocationId": "0123456789abcdef0123456789abcdef",
            "pid": 4242,
            "processStart": 1_700_000_000_u64,
            "phase": "ready",
            "phaseSince": 1_700_000_003_u64,
        })
    );
    let words: Vec<String> = [
        Phase::Starting,
        Phase::Ready,
        Phase::Stopping,
        Phase::AutoRestart,
    ]
    .iter()
    .map(|phase| serde_json::to_string(phase).unwrap())
    .collect();
    assert_eq!(
        words,
        [
            "\"starting\"",
            "\"ready\"",
            "\"stopping\"",
            "\"auto-restart\""
        ]
    );
}

// catches: a state file written world-readable, non-atomically, or read back different
#[test]
fn writes_and_reads_back_private() {
    let scratch = Scratch::new("runstate-rw");
    let run = run_dir(&scratch);
    write(&run, &sample()).unwrap();
    let mode = std::fs::metadata(path(&run)).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(read(&run).unwrap(), Some(sample()));
}

// catches: a missing file read as an error, or a file with an unknown key accepted
#[test]
fn missing_is_none_and_unknown_keys_are_refused() {
    let scratch = Scratch::new("runstate-missing");
    let run = run_dir(&scratch);
    assert_eq!(read(&run).unwrap(), None);
    std::fs::write(
        path(&run),
        r#"{"invocationId":"a","pid":1,"processStart":1,"phase":"ready","phaseSince":1,"extra":true}"#,
    )
    .unwrap();
    assert!(read(&run).is_err());
    std::fs::write(path(&run), "not json").unwrap();
    assert!(read(&run).is_err());
}

// catches: a phase change that rewrites the invocation, pid or process start, or that creates a file
#[test]
fn set_phase_changes_only_the_phase() {
    let scratch = Scratch::new("runstate-phase");
    let run = run_dir(&scratch);
    assert!(matches!(
        set_phase(&run, Phase::Stopping, 9),
        Err(Error::Io { .. })
    ));
    assert_eq!(read(&run).unwrap(), None, "set_phase created a file");
    write(&run, &sample()).unwrap();
    set_phase(&run, Phase::Stopping, 1_700_000_100).unwrap();
    assert_eq!(
        read(&run).unwrap(),
        Some(RunState {
            phase: Phase::Stopping,
            phase_since: 1_700_000_100,
            ..sample()
        })
    );
}

// catches: a reused pid, a job that no longer runs, or an unknown start time read as the live run
#[test]
fn liveness_needs_the_same_pid_and_start() {
    let state = sample();
    assert!(is_live(&state, Some(4242), Some(1_700_000_000)));
    assert!(
        !is_live(&state, Some(4243), Some(1_700_000_000)),
        "other pid"
    );
    assert!(!is_live(&state, None, Some(1_700_000_000)), "no process");
    assert!(
        !is_live(&state, Some(4242), Some(1_700_000_500)),
        "the pid was reused by a later process"
    );
    assert!(!is_live(&state, Some(4242), None), "start unknown");
}

// catches: a stop intent or socket a killed runner left behind surviving into the next run (its end would read stopped)
#[test]
fn clear_stale_removes_the_per_run_files_only() {
    let scratch = Scratch::new("runstate-clear");
    let run = run_dir(&scratch);
    assert!(
        clear_stale(&RuntimeDir::new(scratch.path().join("absent")))
            .unwrap()
            .is_empty()
    );
    for file in ["stop-intent", "web-url", "state.json", "keep-me"] {
        std::fs::write(run.path().join(file), b"x").unwrap();
    }
    let removed = clear_stale(&run).unwrap();
    assert_eq!(
        removed,
        [
            run.path().join("state.json"),
            run.path().join("web-url"),
            run.path().join("stop-intent"),
        ]
    );
    assert!(run.path().join("keep-me").exists());
    assert!(!run.stop_intent().exists());
}
