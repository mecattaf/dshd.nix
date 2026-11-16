//! Tests of the launchd runner with a fake host: the start of a run, its
//! completion in the runner, and the restart in place. The supervision of
//! a real dsh (signals, the relay, the web pump) is the Linux runner's and
//! is checked on a Mac under launchd.

use std::collections::VecDeque;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

use super::tree::{ProcessEntry, ProcessTable};
use super::*;
use crate::fsx::testing::Scratch;
use crate::name::{InstanceName, StateRoot};

const PID: u32 = 4242;
const START: u64 = 1_700_000_000;
const NOW: u64 = 1_800_000_000;

/// No processes at all: nothing is left behind.
struct NoProcesses;

impl ProcessTable for NoProcesses {
    fn snapshot(&self) -> Result<Vec<ProcessEntry>, Error> {
        Ok(Vec::new())
    }

    fn signal(&self, _pid: u32, _signal: i32) -> Result<bool, Error> {
        Ok(false)
    }
}

/// Called with the index of each pause, before it answers.
type PauseHook = Box<dyn Fn(usize)>;

/// A host whose effects are recorded.
struct FakeHost {
    pid: u32,
    start: Option<u64>,
    /// How the pauses end, in order; `Elapsed` once these are used up.
    answers: Mutex<VecDeque<Paused>>,
    on_pause: PauseHook,
    pauses: Mutex<Vec<Duration>>,
    execs: Mutex<Vec<(PathBuf, Vec<OsString>)>>,
    redirects: Mutex<u32>,
    random: Mutex<u8>,
}

impl Default for FakeHost {
    fn default() -> Self {
        Self {
            pid: PID,
            start: Some(START),
            answers: Mutex::new(VecDeque::new()),
            on_pause: Box::new(|_| {}),
            pauses: Mutex::new(Vec::new()),
            execs: Mutex::new(Vec::new()),
            redirects: Mutex::new(0),
            random: Mutex::new(0),
        }
    }
}

impl Host for FakeHost {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn now(&self) -> u64 {
        NOW
    }

    fn process_start(&self, pid: u32) -> Option<u64> {
        assert_eq!(pid, self.pid, "the runner reads its own start only");
        self.start
    }

    fn boot_id(&self) -> Result<String, Error> {
        Ok("boot-session".to_owned())
    }

    fn random_16(&self) -> Result<[u8; 16], Error> {
        let mut next = self.random.lock().unwrap();
        *next += 1;
        Ok([*next; 16])
    }

    fn redirect_output(&self, _log: &fs::File) -> Result<(), Error> {
        *self.redirects.lock().unwrap() += 1;
        Ok(())
    }

    fn processes(&self) -> Arc<dyn ProcessTable> {
        Arc::new(NoProcesses)
    }

    fn pause(&self, duration: Duration) -> Paused {
        let index = {
            let mut pauses = self.pauses.lock().unwrap();
            pauses.push(duration);
            pauses.len() - 1
        };
        (self.on_pause)(index);
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Paused::Elapsed)
    }

    fn exec(&self, program: &Path, args: &[OsString]) -> Error {
        self.execs
            .lock()
            .unwrap()
            .push((program.to_path_buf(), args.to_vec()));
        Error::InvalidArgument("the fake host does not exec".to_owned())
    }
}

impl FakeHost {
    fn execs(&self) -> Vec<(PathBuf, Vec<OsString>)> {
        self.execs.lock().unwrap().clone()
    }

    fn pauses(&self) -> Vec<Duration> {
        self.pauses.lock().unwrap().clone()
    }
}

/// An instance `web-1` under `scratch`, with its run directory and a
/// `current` release holding a runner.
fn place(scratch: &Scratch) -> Place {
    let root = StateRoot::new(scratch.path().join("state"));
    let name = InstanceName::parse("web-1").unwrap();
    let dir = root.instance(&name);
    fs::create_dir_all(dir.path()).unwrap();
    let release = scratch.path().join("release");
    fs::create_dir_all(release.join("bin")).unwrap();
    fs::write(release.join("bin").join("dshd-run"), b"").unwrap();
    std::os::unix::fs::symlink(&release, dir.current()).unwrap();
    let runtime = RuntimeDir::new(dir.path().join("run"));
    fsx::create_dir_private(runtime.path()).unwrap();
    Place {
        name,
        root,
        dir,
        runtime,
    }
}

fn state(invocation: &str, pid: u32, start: u64, phase: Phase) -> RunState {
    RunState {
        invocation_id: invocation.to_owned(),
        pid,
        process_start: start,
        phase,
        phase_since: 1,
    }
}

/// The start half the runner's prepare writes.
fn write_start_half(place: &Place, invocation: &str) {
    let run = LastRun {
        id: "4b2d6a5e-1f3c-4e7a-9c0d-2a1b3c4d5e6f".to_owned(),
        release_path: OsText::from("/release"),
        boot_id: "boot-session".to_owned(),
        started_at: NOW - 10,
        invocation_id: invocation.to_owned(),
        end: None,
        unrecognized: serde_json::Map::new(),
    };
    fsx::write_json_atomic(&place.dir.last_run_json(), &run, PRIVATE_MODE).unwrap();
}

fn last_end(place: &Place) -> RunEnd {
    instance::read_last_run(&place.dir)
        .unwrap()
        .unwrap()
        .end
        .unwrap()
}

fn ended(status: u8, exit_code: Option<i32>, signal: Option<i32>, stop: Option<i32>) -> Supervised {
    Supervised {
        status,
        timed_out: false,
        seen: Some(RunnerEnd {
            child: ChildEnd { exit_code, signal },
            stop_signal: stop,
        }),
        tracker: Tracker::default(),
        usage: None,
    }
}

fn begun(place: &Place, invocation: &str) -> Begun {
    Begun {
        place: place.clone(),
        invocation: invocation.to_owned(),
    }
}

/// A run in phase `ready` as `begin` left it, with its start half and a
/// one-start history.
fn running(scratch: &Scratch) -> (Place, Begun) {
    let place = place(scratch);
    let invocation = "0101010101010101010101010101010a";
    runstate::write(&place.runtime, &state(invocation, PID, START, Phase::Ready)).unwrap();
    write_history(
        &place.runtime,
        &StartHistory {
            starts: vec![Start {
                at: NOW - 10,
                automatic: false,
            }],
        },
    )
    .unwrap();
    write_start_half(&place, invocation);
    let begun = begun(&place, invocation);
    (place, begun)
}

// catches: a new job mistaken for the runner's own restart (keeping a stale stop intent and history), or the
// restart in place mistaken for a new job (so `restarts` never counts)
#[test]
fn continues_only_after_its_own_restart() {
    let restarting = state("a", PID, START, Phase::AutoRestart);
    assert!(continues(Some(&restarting), PID, Some(START)));
    assert!(!continues(None, PID, Some(START)), "no state file");
    assert!(
        !continues(Some(&restarting), PID + 1, Some(START)),
        "a new job"
    );
    assert!(
        !continues(Some(&restarting), PID, Some(START + 9)),
        "a reused pid"
    );
    for phase in [Phase::Starting, Phase::Ready, Phase::Stopping] {
        assert!(
            !continues(Some(&state("a", PID, START, phase)), PID, Some(START)),
            "{phase:?}"
        );
    }
    let unknown = state("a", PID, 0, Phase::AutoRestart);
    assert!(
        continues(Some(&unknown), PID, None),
        "a start time unknown both times compares the pid"
    );
    assert!(!continues(Some(&restarting), PID, None));
}

// catches: dsh not getting the run's invocation id where systemd sets INVOCATION_ID, or getting two
#[test]
fn the_invocation_id_reaches_dsh_s_environment() {
    let process = vec![
        (OsString::from("HOME"), OsString::from("/h")),
        (OsString::from(INVOCATION_ID), OsString::from("stale")),
    ];
    let environment = runner_environment(process, "0123456789abcdef0123456789abcdef");
    let ids: Vec<&OsString> = environment
        .iter()
        .filter(|(name, _)| name == INVOCATION_ID)
        .map(|(_, value)| value)
        .collect();
    assert_eq!(ids, [&OsString::from("0123456789abcdef0123456789abcdef")]);
    assert!(environment.iter().any(|(name, _)| name == "HOME"));
}

// catches: a stop deadline at or past launchd's ExitTimeOut, so launchd kills the runner before it records the run
#[test]
fn the_stop_deadline_leaves_the_runner_its_grace() {
    assert_eq!(stop_deadline(), Duration::from_secs(25));
    assert!(stop_deadline() + GRACE <= Duration::from_secs(u64::from(EXIT_TIMEOUT_SECS)));
}

// catches: a timed-out start, a restart request or DSH's restart button logged as an ordinary exit
#[test]
fn the_ending_line_says_what_comes_next() {
    let exited = |code| ChildEnd {
        exit_code: Some(code),
        signal: None,
    };
    assert_eq!(
        ending(exited(42), EXIT_TEMPFAIL, false),
        "dsh asked to be restarted (status 42); status 75 asks for a restart"
    );
    assert_eq!(
        ending(exited(1), EXIT_TEMPFAIL, false),
        "dsh exited with status 1 after it was ready; status 75 asks for a restart"
    );
    assert_eq!(
        ending(exited(0), 0, false),
        "dsh exited with status 0; exiting 0"
    );
    let killed = ChildEnd {
        exit_code: None,
        signal: Some(15),
    };
    assert_eq!(
        ending(killed, EXIT_CONFIG, true),
        "dsh was killed by signal 15; it was not ready within 150 seconds, so the start timed out; exiting 78"
    );
}

// catches: the end half losing how dsh ended, the stop signal or the stop intent, or reporting totals it never read
#[test]
fn the_end_half_holds_the_runner_s_view() {
    let end_words = EndWords {
        service_result: "success".to_owned(),
        exit_code: "exited".to_owned(),
        exit_status: "0".to_owned(),
    };
    let seen = RunnerEnd {
        child: ChildEnd {
            exit_code: None,
            signal: Some(15),
        },
        stop_signal: Some(15),
    };
    let end = run_end(&end_words, 7, Some(&seen), true, RunResources::default());
    assert_eq!(
        serde_json::to_value(&end).unwrap(),
        serde_json::json!({
            "endedAt": 7,
            "serviceResult": "success",
            "exitCode": "exited",
            "exitStatus": "0",
            "child": {"signal": 15},
            "stopIntent": true,
            "stopSignal": 15,
            "resources": {
                "cpuUsageNSec": null,
                "memoryPeak": null,
                "ioReadBytes": null,
                "ioWriteBytes": null,
            },
        })
    );
    let refused = run_end(&end_words, 7, None, false, RunResources::default());
    assert_eq!((refused.child, refused.stop_signal), (None, None));
}

// catches: DSH's restart button counted as an automatic restart, or a stop signal lost between supervision and the
// restart decision
#[test]
fn supervised_reports_the_button_and_the_stop() {
    assert!(ended(75, Some(42), None, None).restart_button());
    assert!(!ended(75, Some(1), None, None).restart_button());
    assert!(!Supervised::refused().restart_button());
    assert_eq!(ended(0, None, Some(15), Some(15)).stop_signal(), Some(15));
    assert_eq!(Supervised::refused().stop_signal(), None);
}

// catches: a start deadline that fires on a ready run, never fires on an unready one, or claims a run that ended
// first
#[test]
fn the_startup_deadline_fires_only_for_an_unready_run() {
    let wait_for = |flag: &AtomicBool| {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !flag.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let ready = Arc::new(AtomicBool::new(false));
    let stop_asked = Arc::new(AtomicBool::new(false));
    let fired = Deadline::arm(Duration::from_millis(1), &ready, &stop_asked);
    wait_for(&stop_asked);
    assert!(stop_asked.load(Ordering::SeqCst));
    assert!(fired.disarm(), "the deadline fired");

    let stop_asked = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(AtomicBool::new(true));
    let calm = Deadline::arm(Duration::from_millis(1), &ready, &stop_asked);
    std::thread::sleep(Duration::from_millis(50));
    assert!(!stop_asked.load(Ordering::SeqCst));
    assert!(!calm.disarm());

    let ready = Arc::new(AtomicBool::new(false));
    let ended_first = Deadline::arm(Duration::from_millis(50), &ready, &stop_asked);
    assert!(!ended_first.disarm(), "the run ended before the deadline");
    std::thread::sleep(Duration::from_millis(100));
    assert!(!stop_asked.load(Ordering::SeqCst));
}

// catches: a new job that keeps a killed runner's stop intent or web URL (its end would read stopped), keeps an old
// history, writes no state file for `start` to wait on, or never makes its log the runner's output
#[test]
#[ignore = "packets D08, D09, D10, D14: the launchd runner's start of a run"]
fn a_fresh_run_sets_up_its_files() {
    let scratch = Scratch::new("launchd-runner-fresh");
    let place = place(&scratch);
    let prior = state("ffffffffffffffffffffffffffffffff", 9, 9, Phase::Ready);
    runstate::write(&place.runtime, &prior).unwrap();
    fs::write(place.runtime.stop_intent(), b"").unwrap();
    fs::write(place.runtime.web_url(), b"http://127.0.0.1:1/?token=x").unwrap();
    write_history(
        &place.runtime,
        &StartHistory {
            starts: vec![Start {
                at: 1,
                automatic: true,
            }],
        },
    )
    .unwrap();
    let host = FakeHost::default();
    let begun = begin(place.clone(), &host).unwrap();

    assert!(
        ids::is_invocation_id(&begun.invocation),
        "{}",
        begun.invocation
    );
    assert!(!place.runtime.stop_intent().exists());
    assert!(!place.runtime.web_url().exists());
    assert_eq!(
        runstate::read(&place.runtime).unwrap(),
        Some(RunState {
            invocation_id: begun.invocation.clone(),
            pid: PID,
            process_start: START,
            phase: Phase::Starting,
            phase_since: NOW,
        })
    );
    assert_eq!(
        read_history(&place.runtime).starts,
        [Start {
            at: NOW,
            automatic: false
        }]
    );
    let runs = place.dir.path().join(RUNS_DIR);
    let log = logs::run_log(&runs, &begun.invocation);
    assert_eq!(
        fs::metadata(&log).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read_link(runs.join(logs::LATEST)).unwrap(), log);
    assert_eq!(*host.redirects.lock().unwrap(), 1);
    assert_eq!(
        fs::metadata(history_path(&place.runtime))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

// catches: the runner's own restart clearing the stop intent a CLI wrote meanwhile, beginning a new history (so
// `restarts` stays 0), or reusing the previous run's invocation id
#[test]
#[ignore = "packets D08, D09, D10, D14: the launchd runner's start of a run"]
fn an_exec_restart_continues_the_job() {
    let scratch = Scratch::new("launchd-runner-continue");
    let place = place(&scratch);
    let previous = "01010101010101010101010101010101";
    runstate::write(
        &place.runtime,
        &state(previous, PID, START, Phase::AutoRestart),
    )
    .unwrap();
    fs::write(place.runtime.stop_intent(), b"").unwrap();
    let history = StartHistory {
        starts: vec![
            Start {
                at: 1,
                automatic: false,
            },
            Start {
                at: 2,
                automatic: true,
            },
        ],
    };
    write_history(&place.runtime, &history).unwrap();
    let host = FakeHost {
        random: Mutex::new(7),
        ..FakeHost::default()
    };
    let begun = begin(place.clone(), &host).unwrap();
    assert_ne!(begun.invocation, previous);
    assert!(
        place.runtime.stop_intent().exists(),
        "the stop intent stays"
    );
    assert_eq!(read_history(&place.runtime), history);
    let now = runstate::read(&place.runtime).unwrap().unwrap();
    assert_eq!(
        (now.invocation_id, now.phase),
        (begun.invocation, Phase::Starting)
    );
}

// catches: run logs that grow without bound, or a start that removes its own log
#[test]
#[ignore = "packets D08, D09, D10, D14: the launchd runner's start of a run"]
fn old_run_logs_are_pruned_at_the_start() {
    let scratch = Scratch::new("launchd-runner-prune");
    let place = place(&scratch);
    let runs = place.dir.path().join(RUNS_DIR);
    fs::create_dir_all(&runs).unwrap();
    for index in 0..25_u64 {
        let path = runs.join(format!("{index:032x}.log"));
        fs::write(&path, b"line\n").unwrap();
        let file = fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_000 + index))
            .unwrap();
    }
    fs::write(runs.join("notes.txt"), b"kept").unwrap();
    let begun = begin(place.clone(), &FakeHost::default()).unwrap();
    let mut logs: Vec<String> = fs::read_dir(&runs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| {
            name.strip_suffix(".log")
                .is_some_and(|stem| stem.len() == 32)
        })
        .collect();
    logs.sort();
    assert_eq!(logs.len(), retention::KEEP_RUNS, "{logs:?}");
    assert!(logs.contains(&format!("{}.log", begun.invocation)));
    assert!(
        !logs.contains(&format!("{:032x}.log", 5)),
        "an old log stays"
    );
    assert!(logs.contains(&format!("{:032x}.log", 24)));
    assert!(runs.join("notes.txt").exists());
}

// catches: a run the runner completed without systemd's end words, without the stop intent or with the per-run files
// left, or a stop that restarts
#[test]
#[ignore = "packets D08, D12, D13: the launchd runner's completion"]
fn a_stopped_run_is_completed_in_systemd_words() {
    let scratch = Scratch::new("launchd-runner-complete");
    let (place, begun) = running(&scratch);
    fs::write(place.runtime.stop_intent(), b"").unwrap();
    fs::write(place.runtime.web_url(), b"http://127.0.0.1:1/?token=x").unwrap();
    let host = FakeHost::default();
    let status = finish(
        &begun,
        &host,
        &ended(0, None, Some(15), Some(15)),
        START_LIMIT,
    );
    assert_eq!(status, 0);
    let end = last_end(&place);
    assert_eq!(
        (
            end.service_result.as_str(),
            end.exit_code.as_str(),
            end.exit_status.as_str()
        ),
        ("success", "exited", "0")
    );
    assert!(end.stop_intent);
    assert_eq!(end.stop_signal, Some(15));
    assert_eq!(end.ended_at, NOW);
    assert!(!place.runtime.stop_intent().exists());
    assert!(!place.runtime.web_url().exists());
    assert!(
        runstate::path(&place.runtime).exists(),
        "the state file stays"
    );
    assert_eq!(
        fs::read_link(place.dir.booted()).unwrap(),
        fs::read_link(place.dir.current()).unwrap()
    );
    assert!(host.execs().is_empty() && host.pauses().is_empty());
}

// catches: a crash after readiness that is not restarted, restarted without the pause, through another program than
// the current runner, without the automatic start in the history, or without the auto-restart phase
#[test]
#[ignore = "packets D08, D12, D13: the launchd runner's restart in place"]
fn a_crash_after_readiness_restarts_in_place() {
    let scratch = Scratch::new("launchd-runner-restart");
    let (place, begun) = running(&scratch);
    let host = FakeHost::default();
    let status = finish(&begun, &host, &ended(75, Some(1), None, None), START_LIMIT);
    assert_eq!(
        status, 75,
        "the exec failed in the fake, so the runner exits 75"
    );
    assert_eq!(host.pauses(), [restart::RESTART_DELAY]);
    assert_eq!(
        host.execs(),
        [(
            place.dir.current().join(RELEASE_RUNNER),
            vec![OsString::from("run"), OsString::from("web-1")]
        )]
    );
    assert_eq!(
        read_history(&place.runtime).starts,
        [
            Start {
                at: NOW - 10,
                automatic: false
            },
            Start {
                at: NOW,
                automatic: true
            }
        ]
    );
    assert_eq!(
        runstate::read(&place.runtime).unwrap().unwrap().phase,
        Phase::AutoRestart
    );
    let end = last_end(&place);
    assert_eq!(
        (end.service_result.as_str(), end.exit_status.as_str()),
        ("exit-code", "75")
    );
    assert!(!end.stop_intent);
}

// catches: DSH's restart button counted as an automatic restart
#[test]
#[ignore = "packets D08, D12, D13: the launchd runner's restart in place"]
fn the_restart_button_restarts_but_is_not_automatic() {
    let scratch = Scratch::new("launchd-runner-button");
    let (place, begun) = running(&scratch);
    let host = FakeHost::default();
    finish(&begun, &host, &ended(75, Some(42), None, None), START_LIMIT);
    assert_eq!(host.execs().len(), 1);
    assert_eq!(
        read_history(&place.runtime).starts.last(),
        Some(&Start {
            at: NOW,
            automatic: false
        })
    );
}

// catches: a runner that restarts dsh after a bootout's SIGTERM arrived during the pause, or after a CLI wrote a
// stop intent during it
#[test]
#[ignore = "packets D08, D12, D13: the launchd runner's restart in place"]
fn a_stop_during_the_pause_wins() {
    let scratch = Scratch::new("launchd-runner-pause-stop");
    let (_place, begun) = running(&scratch);
    let host = FakeHost {
        answers: Mutex::new(VecDeque::from([Paused::Stopped(15)])),
        ..FakeHost::default()
    };
    assert_eq!(
        finish(&begun, &host, &ended(75, Some(1), None, None), START_LIMIT),
        75
    );
    assert!(host.execs().is_empty());

    let scratch = Scratch::new("launchd-runner-pause-intent");
    let (place, begun) = running(&scratch);
    let intent = place.runtime.stop_intent();
    let host = FakeHost {
        on_pause: Box::new(move |_| fs::write(&intent, b"").unwrap()),
        ..FakeHost::default()
    };
    assert_eq!(
        finish(&begun, &host, &ended(75, Some(1), None, None), START_LIMIT),
        75
    );
    assert!(host.execs().is_empty());
    assert_eq!(read_history(&place.runtime).starts.len(), 1);
}

// catches: a restart that skips the instance lock, blocks in it so a stop cannot get through, or never restarts
// once the lock is free
#[test]
#[ignore = "packets D08, D12, D13: the launchd runner's restart in place"]
fn the_restart_waits_for_the_instance_lock() {
    let scratch = Scratch::new("launchd-runner-lock");
    let (place, begun) = running(&scratch);
    let held = Mutex::new(Some(fsx::lock_exclusive(&place.dir.lock()).unwrap()));
    let host = FakeHost {
        // Pause 0 is the restart pause; the lock is released during the third retry.
        on_pause: Box::new(move |index| {
            if index == 3 {
                held.lock().unwrap().take();
            }
        }),
        ..FakeHost::default()
    };
    finish(&begun, &host, &ended(75, Some(1), None, None), START_LIMIT);
    assert_eq!(
        host.pauses(),
        [restart::RESTART_DELAY, LOCK_RETRY, LOCK_RETRY, LOCK_RETRY]
    );
    assert_eq!(host.execs().len(), 1);

    let scratch = Scratch::new("launchd-runner-lock-stop");
    let (place, begun) = running(&scratch);
    let _held = fsx::lock_exclusive(&place.dir.lock()).unwrap();
    let host = FakeHost {
        answers: Mutex::new(VecDeque::from([Paused::Elapsed, Paused::Stopped(15)])),
        ..FakeHost::default()
    };
    finish(&begun, &host, &ended(75, Some(1), None, None), START_LIMIT);
    assert!(host.execs().is_empty(), "a stop while waiting for the lock");
}

// catches: a start limit that restarts anyway, or refuses without saying so in the run's end
#[test]
#[ignore = "packets D08, D12, D13: the launchd runner's restart in place"]
fn the_start_limit_refuses_as_start_limit_hit() {
    let scratch = Scratch::new("launchd-runner-limit");
    let (place, begun) = running(&scratch);
    let limit = Some(StartLimit {
        burst: 1,
        interval_secs: 3600,
    });
    let host = FakeHost::default();
    assert_eq!(
        finish(&begun, &host, &ended(75, Some(1), None, None), limit),
        75
    );
    assert!(host.execs().is_empty());
    assert_eq!(last_end(&place).service_result, "start-limit-hit");
}

// catches: a start the runner ended at its deadline recorded as an ordinary failure, or restarted
#[test]
#[ignore = "packets D08, D12, D13: the launchd runner's completion"]
fn a_timed_out_start_is_recorded_as_a_timeout() {
    let scratch = Scratch::new("launchd-runner-timeout");
    let (place, begun) = running(&scratch);
    let host = FakeHost::default();
    let supervised = Supervised {
        timed_out: true,
        ..ended(EXIT_CONFIG, None, Some(15), None)
    };
    assert_eq!(finish(&begun, &host, &supervised, START_LIMIT), EXIT_CONFIG);
    let end = last_end(&place);
    assert_eq!(
        (
            end.service_result.as_str(),
            end.exit_code.as_str(),
            end.exit_status.as_str()
        ),
        ("timeout", "killed", "TERM")
    );
    assert!(host.execs().is_empty());
}

// catches: a start refused before dsh ran (no start half of this invocation) left without an end, or recorded with a
// child it never had
#[test]
#[ignore = "packets D08, D12, D13: the launchd runner's completion"]
fn a_refused_start_is_recorded_without_a_child() {
    let scratch = Scratch::new("launchd-runner-refused");
    let place = place(&scratch);
    fs::write(
        place.dir.instance_json(),
        include_str!("../../../tests/fixtures/instance.json"),
    )
    .unwrap();
    write_start_half(&place, "another invocation");
    let begun = begun(&place, "0202020202020202020202020202020a");
    let host = FakeHost::default();
    assert_eq!(
        finish(&begun, &host, &Supervised::refused(), START_LIMIT),
        EXIT_CONFIG
    );
    let run = instance::read_last_run(&place.dir).unwrap().unwrap();
    assert_eq!(run.invocation_id, "0202020202020202020202020202020a");
    assert_eq!(
        (run.started_at, run.boot_id.as_str()),
        (NOW, "boot-session")
    );
    let end = run.end.unwrap();
    assert_eq!(
        (end.service_result.as_str(), end.exit_status.as_str()),
        ("exit-code", "78")
    );
    assert_eq!((end.child, end.stop_signal), (None, None));
}
