//! Supervision on the runner's main thread. `run` blocks [`RUNNER_SIGNALS`]
//! before any thread exists, so every thread inherits the mask and only the
//! main thread's `sigwait` takes them: SIGCHLD reaps dsh, and the first stop
//! signal runs the transport's stop action once. The main thread is the
//! only one that signals or reaps dsh, so a reaped, possibly reused pid is
//! never signalled; a thread that needs dsh stopped asks the main thread
//! (`ask_stop`). `KillMode=mixed` sends SIGTERM to the runner only; the
//! runner decides what the child receives.
//!
//! A blocked SIGCHLD stays pending for `sigwait` under its default
//! disposition, and the child starts with an empty mask
//! (`sys::unmask_on_exec`).

use std::io;
use std::os::unix::process::ExitStatusExt;
use std::process::{self, Child, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::record::ChildEnd;
use crate::runner::log_line;
use crate::sys::{self, SIGCHLD, SIGHUP, SIGINT, SIGTERM};

/// Signals the runner takes with `sigwait`: the three stop signals and
/// the child's end.
pub const RUNNER_SIGNALS: &[i32] = &[SIGTERM, SIGINT, SIGHUP, SIGCHLD];

/// Waits on the main thread until dsh has ended and reaps it. The first
/// stop signal is logged and runs `on_stop` once; so does `stop_asked`,
/// found set after a wake from `ask_stop`. Returns how dsh ended and the
/// stop signal, when one arrived.
pub(crate) fn supervise(
    child: &mut Child,
    stop_asked: &AtomicBool,
    on_stop: impl FnOnce(),
) -> (ChildEnd, Option<i32>) {
    let mut on_stop = Some(on_stop);
    let mut stop_signal = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return (end_of(status), stop_signal),
            Ok(None) => {}
            Err(error) => return (unknown_end(&error), stop_signal),
        }
        if stop_asked.load(Ordering::SeqCst)
            && let Some(stop) = on_stop.take()
        {
            stop();
        }
        match sys::wait_signal(RUNNER_SIGNALS) {
            // dsh ended, stopped or continued, or a thread asked for a stop: look again.
            Ok(SIGCHLD) => {}
            Ok(signal) => {
                if let Some(stop) = on_stop.take() {
                    log_line(&format!("stop requested by signal {signal}"));
                    stop_signal = Some(signal);
                    stop();
                }
            }
            Err(error) => {
                log_line(&format!(
                    "stop signals cannot be received, so the run ends when dsh does: {error}"
                ));
                let end = child
                    .wait()
                    .map_or_else(|error| unknown_end(&error), end_of);
                return (end, stop_signal);
            }
        }
    }
}

/// Asks the main thread to run the stop action, from any other thread: sets
/// `stop_asked`, then wakes `supervise` with SIGCHLD to the runner's own
/// pid, which every thread blocks, so it stays pending for the main
/// thread's `sigwait`.
pub(crate) fn ask_stop(stop_asked: &AtomicBool) {
    stop_asked.store(true, Ordering::SeqCst);
    if let Err(error) = sys::kill(process::id(), SIGCHLD) {
        log_line(&format!("cannot wake the runner's main thread: {error}"));
    }
}

fn end_of(status: ExitStatus) -> ChildEnd {
    ChildEnd {
        exit_code: status.code(),
        signal: status.signal(),
    }
}

/// A failed wait leaves an end with neither code nor signal, which the exit
/// table treats as a failure.
fn unknown_end(error: &io::Error) -> ChildEnd {
    log_line(&format!("cannot wait for dsh: {error}"));
    ChildEnd {
        exit_code: None,
        signal: None,
    }
}
