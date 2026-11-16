//! The runner as a launchd job (macOS). launchd has no `READY=1`, no
//! `ExecStopPost`, no `RestartForceExitStatus`, no cgroup and no
//! `RuntimeDirectory=`, so the runner does here what systemd does around
//! it on Linux. One run:
//!
//! 1. **Begin**: the instance's `run` directory (0700); a run that is not
//!    this process continuing after its own restart clears the per-run
//!    files a killed runner left (`runstate::clear_stale`) and begins a new
//!    start history (`run/starts.json`). A new invocation id; the run's own
//!    log `runs/<id>.log` becomes the runner's stdout and stderr, so dsh's
//!    output and every runner line land there, `RUN_STARTED` first; older
//!    logs are pruned (`retention`). `run/state.json` records the run in
//!    phase `starting`.
//! 2. **Supervise**, as on Linux, with the phase of `run/state.json`
//!    standing in for `sd_notify` (`ready`, `stopping`). The runner
//!    enforces the startup deadline ([`STARTUP_TIMEOUT`]) itself, and
//!    kills a dsh that ignores its stop after [`stop_deadline`], before
//!    launchd's `ExitTimeOut` would kill the runner. dsh's descendants are
//!    sampled meanwhile ([`tree`]).
//! 3. **End**: what dsh left behind is stopped; `last-run.json` is
//!    completed in systemd's end words (`completion::own_end`); `booted`
//!    collapses to `current`; the per-run files go; the last line is
//!    `RUN_ENDED: <serviceResult>`.
//! 4. **After**: the exit table's 75 with no stop asked
//!    (`restart::after_exit`) pauses `restart::RESTART_DELAY`, takes the instance
//!    lock, checks that no stop intent appeared and that the start limit,
//!    if any, admits the start, records it in the history and `exec`s the
//!    `current` runner: the job keeps its pid and launchd restarts nothing.
//!    Anything else exits with the status.
//!
//! Platform effects go through [`Host`]; the tests drive steps 1, 3 and 4
//! with a fake one. Step 2 spawns and signals a real dsh and is checked on
//! a Mac.

mod host;
mod tree;

use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;

use self::host::{Host, NativeHost, Paused};
use self::tree::{LEFTOVER_GRACE, LEFTOVER_POLL, SAMPLE_INTERVAL, Sampler, Tracker};
use super::notify::Notify;
use super::{
    DSH_RESTART, EXIT_CONFIG, EXIT_TEMPFAIL, INVOCATION_ID, Launch, PRIVATE_MODE, Place, RUN_ENDED,
    RUN_STARTED, RunnerEnd, Serving, completion, exit, how_dsh_ended, log_line, signal,
    spawn_thread, terminate,
};
use crate::error::Error;
use crate::fsx::{self, Lock};
use crate::name::{RELEASE_RUNNER, RuntimeDir};
use crate::platform::launchd::completion::{self as words, EndWords};
use crate::platform::launchd::restart::{self, AfterExit, Start, StartHistory, StartLimit};
use crate::platform::launchd::retention::{self, LogFile};
use crate::platform::launchd::runstate::{self, Phase, RunState};
use crate::platform::launchd::rusage::EndUsage;
use crate::platform::launchd::{
    EXIT_TIMEOUT_SECS, GRACE, RUNS_DIR, STARTUP_TIMEOUT, ids, logs, rusage,
};
use crate::record::{ChildEnd, LastRun, OsText, RunEnd, RunResources};
use crate::{instance, out, release, sys};

/// How often a restart that waits for the instance lock tries again.
const LOCK_RETRY: Duration = Duration::from_millis(100);
/// The start limit of a restart: none. Whether macOS gets one at all is an
/// open decision, so nothing here counts starts against a default.
const START_LIMIT: Option<StartLimit> = None;
/// Name of the thread that ends a start which is not ready in time.
const STARTUP_THREAD: &str = "dshd-startup";
/// Name of the thread that ends a stop dsh does not follow.
const STOP_THREAD: &str = "dshd-stop";

/// The startup deadline's states, in one atomic so the deadline and the
/// run's end cannot both claim it.
const DEADLINE_ARMED: u8 = 0;
const DEADLINE_FIRED: u8 = 1;
const DEADLINE_DISARMED: u8 = 2;

/// `dshd-run run NAME` as launchd's job: one run, then an exit, or an
/// `exec` of the `current` runner for the next run.
pub(super) fn run(name: &str) -> u8 {
    // Before any thread exists, as on Linux: every thread inherits the
    // mask, and only the main thread's sigwait takes a stop. macOS drops
    // a SIGCHLD without a handler even while it is blocked, so dsh's end
    // and the runner's own wakes would never reach sigwait.
    if let Err(error) = sys::keep_for_sigwait(sys::SIGCHLD)
        .and_then(|()| sys::block_signals(signal::RUNNER_SIGNALS))
    {
        log_line(&error.to_string());
        return EXIT_CONFIG;
    }
    let host = match NativeHost::new() {
        Ok(host) => host,
        Err(error) => {
            log_line(&error.to_string());
            return EXIT_CONFIG;
        }
    };
    let begun = match Place::from_launchd(name).and_then(|place| begin(place, &host)) {
        Ok(begun) => begun,
        Err(error) => {
            // Before the run's log exists this reaches `launchd.log`.
            log_line(&error.to_string());
            return EXIT_CONFIG;
        }
    };
    let environment = runner_environment(std::env::vars_os(), &begun.invocation);
    let supervised = match Launch::prepare_at(
        begun.place.clone(),
        &environment,
        begun.invocation.clone(),
        || host.boot_id(),
    ) {
        Ok(launch) => supervise(&launch, &host),
        Err(error) => {
            log_line(&error.to_string());
            Supervised::refused()
        }
    };
    finish(&begun, &host, &supervised, START_LIMIT)
}

/// One run, once its own files are set up.
#[derive(Debug)]
struct Begun {
    place: Place,
    /// The run's invocation id.
    invocation: String,
}

/// Step 1: sets up the run's files at `place` (see the module
/// documentation). Fails before dsh exists, with nothing to complete.
fn begin(place: Place, host: &dyn Host) -> Result<Begun, Error> {
    let pid = host.pid();
    let start = host.process_start(pid);
    let continued = match runstate::read(&place.runtime) {
        Ok(prior) => continues(prior.as_ref(), pid, start),
        Err(error) => {
            log_line(&format!(
                "the previous run's state is unreadable, so this run starts afresh: {error}"
            ));
            false
        }
    };
    if !continued {
        runstate::clear_stale(&place.runtime)?;
    }
    let invocation = ids::invocation_id(host.random_16()?);
    let runs = place.dir.path().join(RUNS_DIR);
    let log = logs::create(&runs, &invocation)?;
    host.redirect_output(&log)?;
    drop(log);
    out::error_line(RUN_STARTED);
    if continued {
        log_line("this run continues the job after its runner restarted it");
    }
    prune_logs(&runs, &logs::run_log(&runs, &invocation));
    let now = host.now();
    if !continued {
        let first = StartHistory {
            starts: vec![Start {
                at: now,
                automatic: false,
            }],
        };
        write_history(&place.runtime, &first)?;
    }
    let state = RunState {
        invocation_id: invocation.clone(),
        pid,
        process_start: start.unwrap_or(0),
        phase: Phase::Starting,
        phase_since: now,
    };
    runstate::write(&place.runtime, &state)?;
    Ok(Begun { place, invocation })
}

/// Whether this process continues a job it restarted itself: the state
/// file names this very process (its pid, and its start time, 0 when
/// unknown) in phase `auto-restart`. An `exec` keeps both; a new job has
/// another pid, and a reused pid another start time.
fn continues(prior: Option<&RunState>, pid: u32, start: Option<u64>) -> bool {
    prior.is_some_and(|state| {
        state.phase == Phase::AutoRestart
            && state.pid == pid
            && state.process_start == start.unwrap_or(0)
    })
}

/// The environment dsh's composition reads as the runner's: this
/// process's, with the run's invocation id where systemd sets
/// `INVOCATION_ID`.
fn runner_environment(
    process: impl IntoIterator<Item = (OsString, OsString)>,
    invocation: &str,
) -> Vec<(OsString, OsString)> {
    let mut environment: Vec<(OsString, OsString)> = process
        .into_iter()
        .filter(|(name, _)| name != INVOCATION_ID)
        .collect();
    environment.push((OsString::from(INVOCATION_ID), OsString::from(invocation)));
    environment
}

/// What supervising one dsh came to.
#[derive(Debug)]
struct Supervised {
    /// The runner's exit status from the exit table, or 78.
    status: u8,
    /// The runner ended the start at the startup deadline.
    timed_out: bool,
    /// How dsh ended and the stop signal, when dsh was spawned.
    seen: Option<RunnerEnd>,
    /// dsh's descendants, as sampled.
    tracker: Tracker,
    /// What `wait4` reported dsh used, when it was reaped and the usage
    /// converted.
    usage: Option<EndUsage>,
}

impl Supervised {
    /// A start refused before dsh was spawned.
    fn refused() -> Self {
        Self {
            status: EXIT_CONFIG,
            timed_out: false,
            seen: None,
            tracker: Tracker::default(),
            usage: None,
        }
    }

    /// The stop signal the runner received, if any.
    fn stop_signal(&self) -> Option<i32> {
        self.seen.as_ref().and_then(|seen| seen.stop_signal)
    }

    /// dsh exited through its restart button.
    fn restart_button(&self) -> bool {
        self.seen
            .as_ref()
            .is_some_and(|seen| seen.child.exit_code == Some(DSH_RESTART))
    }
}

/// Step 2: spawns dsh and supervises it on the main thread, as
/// `Launch::run` does on Linux, with `run/state.json` as the notify
/// channel, the startup deadline, the stop deadline and the tree sampler.
fn supervise(launch: &Launch, host: &dyn Host) -> Supervised {
    let mut child = match launch.spawn() {
        Ok(child) => child,
        Err(error) => {
            log_line(&error.to_string());
            return Supervised::refused();
        }
    };
    let pid = child.id();
    log_line(&format!(
        "started dsh as pid {pid} from {}",
        launch.release.display()
    ));
    let sampler = Sampler::start(host.processes(), pid, SAMPLE_INTERVAL)
        .inspect_err(|error| log_line(&format!("{error}; dsh's descendants are not recorded")))
        .ok();
    let notify = Arc::new(Notify::state_file(launch.place.runtime.clone()));
    let ready = Arc::new(AtomicBool::new(false));
    let stop_asked = Arc::new(AtomicBool::new(false));
    let deadline = Deadline::arm(STARTUP_TIMEOUT, &ready, &stop_asked);
    let serving = launch.serve(&mut child, Some(Arc::clone(&notify)), &ready, &stop_asked);
    if let Err(error) = &serving {
        log_line(&error.to_string());
        terminate(pid);
    }
    let (end, stop_signal, usage) = watch(&mut child, &serving, &notify, &stop_asked);
    let timed_out = deadline.disarm();
    let tracker = sampler.map(Sampler::finish).unwrap_or_default();
    let served = serving.map(Serving::finish);
    notify.stopping();
    let status = if served.is_err() || timed_out || stop_asked.load(Ordering::SeqCst) {
        EXIT_CONFIG
    } else {
        exit::exit_status(
            launch.record.transport,
            ready.load(Ordering::SeqCst),
            stop_signal.is_some(),
            end,
        )
    };
    log_line(&ending(end, status, timed_out));
    Supervised {
        status,
        timed_out,
        seen: Some(RunnerEnd {
            child: end,
            stop_signal,
        }),
        tracker,
        usage,
    }
}

/// Supervises dsh until it ended and was reaped with `wait4`, which also
/// gives what it used. The first stop (a signal, or `stop_asked`) moves the
/// phase to `stopping`, arms the stop deadline and runs the transport's
/// stop; a dsh still there at the deadline is killed, from the main thread
/// like every other signal to dsh.
fn watch(
    child: &mut Child,
    serving: &Result<Serving, Error>,
    notify: &Notify,
    stop_asked: &AtomicBool,
) -> (ChildEnd, Option<i32>, Option<EndUsage>) {
    let pid = child.id();
    let kill_due = Arc::new(AtomicBool::new(false));
    let begin_stop = || {
        notify.stopping();
        arm_stop_deadline(&kill_due);
    };
    let mut killed = false;
    let enforce_stop_deadline = |_: &mut Child| {
        if kill_due.load(Ordering::SeqCst) && !killed {
            killed = true;
            log_line(&format!(
                "dsh did not stop within {} seconds; killing it",
                stop_deadline().as_secs()
            ));
            if let Err(error) = sys::kill(pid, libc::SIGKILL) {
                log_line(&format!("cannot kill dsh: {error}"));
            }
        }
    };
    if let Ok(Serving::Acp { relay }) = serving {
        let stop = || {
            begin_stop();
            if !relay.stop_child() {
                terminate(pid);
            }
        };
        signal::supervise_reaping(child, stop_asked, stop, enforce_stop_deadline)
    } else {
        let stop = || {
            begin_stop();
            terminate(pid);
        };
        signal::supervise_reaping(child, stop_asked, stop, enforce_stop_deadline)
    }
}

/// How long dsh gets once a stop began before the runner kills it:
/// launchd's `ExitTimeOut` less [`GRACE`], which leaves the runner the time
/// to stop what dsh left and record the run before launchd would kill it.
fn stop_deadline() -> Duration {
    Duration::from_secs(u64::from(EXIT_TIMEOUT_SECS)).saturating_sub(GRACE)
}

/// Sets `kill_due` after [`stop_deadline`] and wakes the main thread,
/// whose wake hook then kills dsh if it still runs.
fn arm_stop_deadline(kill_due: &Arc<AtomicBool>) {
    let due = Arc::clone(kill_due);
    let after = stop_deadline();
    let armed = spawn_thread(STOP_THREAD, move || {
        std::thread::sleep(after);
        due.store(true, Ordering::SeqCst);
        signal::wake();
    });
    if let Err(error) = armed {
        log_line(&format!(
            "{error}; a dsh that ignores the stop is not killed"
        ));
    }
}

/// The startup deadline: once [`STARTUP_TIMEOUT`] passed, a run that is
/// not ready is stopped as a timed-out start (systemd's
/// `TimeoutStartSec`).
#[derive(Debug)]
struct Deadline(Arc<AtomicU8>);

impl Deadline {
    fn arm(after: Duration, ready: &Arc<AtomicBool>, stop_asked: &Arc<AtomicBool>) -> Self {
        let state = Arc::new(AtomicU8::new(DEADLINE_ARMED));
        let (shared, ready, stop_asked) = (
            Arc::clone(&state),
            Arc::clone(ready),
            Arc::clone(stop_asked),
        );
        let armed = spawn_thread(STARTUP_THREAD, move || {
            std::thread::sleep(after);
            if ready.load(Ordering::SeqCst) {
                return;
            }
            let claimed = shared.compare_exchange(
                DEADLINE_ARMED,
                DEADLINE_FIRED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
            if claimed.is_ok() {
                log_line(&format!(
                    "dsh was not ready within {} seconds; stopping it",
                    after.as_secs()
                ));
                signal::ask_stop(&stop_asked);
            }
        });
        if let Err(error) = armed {
            log_line(&format!("{error}; the startup deadline is not enforced"));
        }
        Self(state)
    }

    /// Disarms the deadline at the run's end; true when it had fired.
    fn disarm(&self) -> bool {
        self.0
            .compare_exchange(
                DEADLINE_ARMED,
                DEADLINE_DISARMED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err_and(|state| state == DEADLINE_FIRED)
    }
}

/// The one line the runner logs when dsh ended, under launchd.
fn ending(end: ChildEnd, status: u8, timed_out: bool) -> String {
    let how = how_dsh_ended(end);
    if timed_out {
        format!(
            "{how}; it was not ready within {} seconds, so the start timed out; exiting {status}",
            STARTUP_TIMEOUT.as_secs()
        )
    } else if status == EXIT_TEMPFAIL && end.exit_code == Some(DSH_RESTART) {
        format!(
            "dsh asked to be restarted (status {DSH_RESTART}); status {status} asks for a restart"
        )
    } else if status == EXIT_TEMPFAIL {
        format!("{how} after it was ready; status {status} asks for a restart")
    } else {
        format!("{how}; exiting {status}")
    }
}

/// Steps 3 and 4: stops what dsh left behind, completes the run, then
/// exits or restarts. Returns the status to exit with.
fn finish(
    begun: &Begun,
    host: &dyn Host,
    supervised: &Supervised,
    limit: Option<StartLimit>,
) -> u8 {
    let place = &begun.place;
    let table = host.processes();
    let own = host.pid();
    let group = table
        .snapshot()
        .ok()
        .and_then(|snapshot| tree::led_group(&snapshot, own));
    tree::stop_leftovers(
        table.as_ref(),
        &supervised.tracker,
        own,
        group,
        LEFTOVER_GRACE,
        LEFTOVER_POLL,
    );
    let end_words = words::own_end(supervised.status, supervised.timed_out);
    let stop_intent = match record_end(begun, host, &end_words, supervised) {
        Ok(stop_intent) => stop_intent,
        Err(error) => {
            log_line(&format!("cannot record the run: {error}"));
            matches!(fsx::kind(&place.runtime.stop_intent()), Ok(Some(_)))
        }
    };
    if let Err(error) = release::collapse_booted(&place.dir) {
        log_line(&format!("cannot release the ended closure: {error}"));
    }
    for file in [
        place.runtime.web_url(),
        place.runtime.acp_socket(),
        place.runtime.stop_intent(),
    ] {
        if let Err(error) = completion::remove_if_present(&file) {
            log_line(&error.to_string());
        }
    }
    out::error_line(&format!("{RUN_ENDED}: {}", end_words.service_result));
    let status = supervised.status;
    match restart::after_exit(status, supervised.stop_signal().is_some(), stop_intent) {
        AfterExit::Exit(status) => status,
        AfterExit::Restart(delay) => restart_in_place(
            begun,
            host,
            status,
            delay,
            supervised.restart_button(),
            limit,
        ),
    }
}

/// Completes `last-run.json`, as `poststop` does on Linux, with the runner's
/// own end words. A start half from another invocation, or none, is
/// replaced by one whose end time stands in for the start. Returns whether
/// the CLI's stop intent was there.
fn record_end(
    begun: &Begun,
    host: &dyn Host,
    end_words: &EndWords,
    supervised: &Supervised,
) -> Result<bool, Error> {
    let place = &begun.place;
    let ended_at = host.now();
    let path = place.dir.last_run_json();
    let mut run = match completion::start_half(&path, &begun.invocation) {
        Some(start) => start,
        None => LastRun {
            id: instance::load(&place.root, &place.name)?.record.id,
            release_path: fsx::read_link(&place.dir.booted())?
                .map_or_else(|| OsText::from(""), OsText::new),
            boot_id: host.boot_id()?,
            started_at: ended_at,
            invocation_id: begun.invocation.clone(),
            end: None,
            unrecognized: serde_json::Map::new(),
        },
    };
    let stop_intent = fsx::kind(&place.runtime.stop_intent())?.is_some();
    // `wait4`'s usage of dsh and the descendants it waited for; a refused
    // start, or a usage that did not convert, is every total null.
    let resources = rusage::run_totals(supervised.usage);
    run.end = Some(run_end(
        end_words,
        ended_at,
        supervised.seen.as_ref(),
        stop_intent,
        resources,
    ));
    fsx::write_json_atomic(&path, &run, PRIVATE_MODE)?;
    Ok(stop_intent)
}

/// The end half of `last-run.json` from the runner's own view of the run.
fn run_end(
    end_words: &EndWords,
    ended_at: u64,
    seen: Option<&RunnerEnd>,
    stop_intent: bool,
    resources: RunResources,
) -> RunEnd {
    RunEnd {
        ended_at,
        service_result: end_words.service_result.clone(),
        exit_code: end_words.exit_code.clone(),
        exit_status: end_words.exit_status.clone(),
        child: seen.map(|seen| seen.child),
        stop_intent,
        stop_signal: seen.and_then(|seen| seen.stop_signal),
        resources: Some(resources),
        unrecognized: serde_json::Map::new(),
    }
}

/// Step 4's restart, in this process: phase `auto-restart`, a pause of
/// `delay` that a stop signal ends, the instance lock, no stop intent, a
/// start `limit` admits, recorded in the history (automatic unless dsh's
/// restart `button` asked), then `exec` of the `current` runner. Returns
/// the status to exit with when it does not restart.
fn restart_in_place(
    begun: &Begun,
    host: &dyn Host,
    status: u8,
    delay: Duration,
    button: bool,
    limit: Option<StartLimit>,
) -> u8 {
    let place = &begun.place;
    if let Err(error) = runstate::set_phase(&place.runtime, Phase::AutoRestart, host.now()) {
        log_line(&format!("cannot record the restart pause: {error}"));
    }
    log_line(&format!("restarting dsh in {} seconds", delay.as_secs()));
    if let Paused::Stopped(signal) = host.pause(delay) {
        log_line(&format!(
            "stop requested by signal {signal} during the restart pause; dsh is not restarted"
        ));
        return status;
    }
    let lock = match restart_lock(place, host) {
        Ok(Some(lock)) => lock,
        Ok(None) => return status,
        Err(error) => {
            log_line(&format!(
                "cannot take the instance lock, so dsh is not restarted: {error}"
            ));
            return status;
        }
    };
    match fsx::kind(&place.runtime.stop_intent()) {
        Ok(None) => {}
        Ok(Some(_)) => {
            log_line("a stop was asked during the restart pause; dsh is not restarted");
            return status;
        }
        Err(error) => {
            log_line(&format!("{error}; dsh is not restarted"));
            return status;
        }
    }
    let now = host.now();
    let mut history = read_history(&place.runtime);
    if !restart::admits(&history, now, limit) {
        refuse_by_start_limit(place);
        return status;
    }
    history.starts.push(Start {
        at: now,
        automatic: !button,
    });
    if let Err(error) = write_history(&place.runtime, &history) {
        log_line(&format!("{error}; dsh is not restarted"));
        return status;
    }
    let program = place.dir.current().join(RELEASE_RUNNER);
    let args = [OsString::from("run"), OsString::from(place.name.as_str())];
    // On success the lock's descriptor closes at the exec (close-on-exec),
    // so a CLI waiting for it then sees the new run.
    let error = host.exec(&program, &args);
    drop(lock);
    log_line(&format!("cannot restart the runner: {error}"));
    status
}

/// The instance lock for a restart, waited for without blocking a stop:
/// tried again every [`LOCK_RETRY`] in a pause, so a CLI that holds it
/// while it boots the job out gets the stop through. `None` when a stop
/// signal came first.
fn restart_lock(place: &Place, host: &dyn Host) -> Result<Option<Lock>, Error> {
    loop {
        if let Some(lock) = fsx::try_lock_exclusive(&place.dir.lock())? {
            return Ok(Some(lock));
        }
        if let Paused::Stopped(signal) = host.pause(LOCK_RETRY) {
            log_line(&format!(
                "stop requested by signal {signal} while the restart waited for the instance lock; dsh is not restarted"
            ));
            return Ok(None);
        }
    }
}

/// The start limit refused the restart: the ended run's end words become
/// `start-limit-hit`, as systemd records the unit's result then.
fn refuse_by_start_limit(place: &Place) {
    log_line("the start limit refuses another start; dsh is not restarted");
    let path = place.dir.last_run_json();
    let rewritten = fsx::read_json::<LastRun>(&path).and_then(|run| {
        let Some(mut run) = run else {
            return Ok(());
        };
        if let Some(end) = run.end.as_mut() {
            let refused = words::start_limit_end();
            end.service_result = refused.service_result;
            end.exit_code = refused.exit_code;
            end.exit_status = refused.exit_status;
        }
        fsx::write_json_atomic(&path, &run, PRIVATE_MODE)
    });
    if let Err(error) = rewritten {
        log_line(&format!("cannot record the start limit: {error}"));
    }
}

/// `run/starts.json`.
fn history_path(run_dir: &RuntimeDir) -> PathBuf {
    run_dir.path().join(restart::STARTS_FILE)
}

/// The start history; an empty one when it is missing or unreadable (the
/// latter is logged).
fn read_history(run_dir: &RuntimeDir) -> StartHistory {
    match fsx::read_json(&history_path(run_dir)) {
        Ok(history) => history.unwrap_or_default(),
        Err(error) => {
            log_line(&format!(
                "the start history is unreadable and begins again: {error}"
            ));
            StartHistory::default()
        }
    }
}

fn write_history(run_dir: &RuntimeDir, history: &StartHistory) -> Result<(), Error> {
    fsx::write_json_atomic(&history_path(run_dir), history, PRIVATE_MODE)
}

/// Removes the run logs `retention::select` gives up, keeping `current`.
/// Failures are logged: an old log never stops a start.
fn prune_logs(runs: &Path, current: &Path) {
    let files = match log_files(runs) {
        Ok(files) => files,
        Err(error) => {
            log_line(&format!("cannot list the run logs to prune them: {error}"));
            return;
        }
    };
    for path in retention::select(&files, current) {
        if let Err(error) = completion::remove_if_present(&path) {
            log_line(&error.to_string());
        }
    }
}

/// The regular files of the runs directory, with their age and size.
fn log_files(runs: &Path) -> Result<Vec<LogFile>, Error> {
    let mut files = Vec::new();
    for (name, kind) in fsx::list_dir(runs)? {
        if !kind.is_file() {
            continue;
        }
        let path = runs.join(name);
        let Some(metadata) = fsx::metadata(&path)? else {
            continue;
        };
        files.push(LogFile {
            path,
            modified: u64::try_from(metadata.mtime()).unwrap_or(0),
            bytes: metadata.len(),
        });
    }
    Ok(files)
}

#[cfg(test)]
#[path = "launchd/unit_tests.rs"]
mod tests;
