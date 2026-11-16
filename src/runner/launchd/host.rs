//! What the launchd runner needs from the platform beyond its files, behind
//! one trait: the pid and its start time, the clock, the boot id, random
//! bytes for the invocation id, the stdio redirect, the process table, the
//! pause before a restart and `exec`. The tests drive the runner's own
//! logic with a fake of it; [`NativeHost`] is the real one.

use std::ffi::OsString;
use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::error::Error;
use crate::fsx;
use crate::platform::ServiceManager;
use crate::platform::launchd::LaunchdManager;
use crate::runner::launchd::tree::{LibprocTable, ProcessTable};
use crate::runner::{log_line, signal, spawn_thread};
use crate::sys::{self, SIGCHLD};

/// Name of the thread that ends a pause.
const PAUSE_THREAD: &str = "dshd-pause";
/// How often a pause whose stop signals cannot be received looks at its
/// alarm.
const PAUSE_POLL: Duration = Duration::from_millis(50);

/// How a pause ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Paused {
    /// The whole duration passed.
    Elapsed,
    /// A stop signal arrived first.
    Stopped(i32),
}

/// The platform, as the launchd runner uses it.
pub(crate) trait Host {
    /// This process's pid, which is the job's.
    fn pid(&self) -> u32;
    /// Unix seconds.
    fn now(&self) -> u64;
    /// The start time of `pid` from the source the CLI compares
    /// (`LaunchdManager::process_start`); `None` when unknown.
    fn process_start(&self, pid: u32) -> Option<u64>;
    /// The boot (session) id runs are recorded under.
    fn boot_id(&self) -> Result<String, Error>;
    /// 16 random bytes.
    fn random_16(&self) -> Result<[u8; 16], Error>;
    /// Makes `log` this process's stdout and stderr.
    fn redirect_output(&self, log: &File) -> Result<(), Error>;
    /// The process table.
    fn processes(&self) -> Arc<dyn ProcessTable>;
    /// Waits `duration` on the main thread, unless a stop signal arrives
    /// first.
    fn pause(&self, duration: Duration) -> Paused;
    /// Replaces this process's image with `program ARGS`, keeping its pid;
    /// returns only when that failed.
    fn exec(&self, program: &Path, args: &[OsString]) -> Error;
}

/// The real platform: launchd's naming and process start source, the
/// libproc process table and the runner's signal mask.
#[derive(Debug)]
pub(crate) struct NativeHost {
    manager: LaunchdManager,
    table: Arc<LibprocTable>,
}

impl NativeHost {
    /// The invoking user's launchd manager and the libproc table.
    pub(crate) fn new() -> Result<Self, Error> {
        Ok(Self {
            manager: LaunchdManager::from_env()?,
            table: Arc::new(LibprocTable),
        })
    }
}

impl Host for NativeHost {
    fn pid(&self) -> u32 {
        std::process::id()
    }

    fn now(&self) -> u64 {
        fsx::unix_now()
    }

    fn process_start(&self, pid: u32) -> Option<u64> {
        self.manager.process_start(pid)
    }

    fn boot_id(&self) -> Result<String, Error> {
        ServiceManager::boot_id(&self.manager)
    }

    fn random_16(&self) -> Result<[u8; 16], Error> {
        sys::random_16()
    }

    fn redirect_output(&self, log: &File) -> Result<(), Error> {
        sys::redirect_output(log)
    }

    fn processes(&self) -> Arc<dyn ProcessTable> {
        self.table.clone()
    }

    /// An alarm thread sleeps `duration`, then wakes the main thread's
    /// `sigwait` (`signal::wake`); a stop signal wakes it first.
    fn pause(&self, duration: Duration) -> Paused {
        let fired = Arc::new(AtomicBool::new(false));
        let alarm = Arc::clone(&fired);
        let armed = spawn_thread(PAUSE_THREAD, move || {
            thread::sleep(duration);
            alarm.store(true, Ordering::SeqCst);
            signal::wake();
        });
        if let Err(error) = armed {
            log_line(&format!("{error}; pausing without listening for a stop"));
            thread::sleep(duration);
            return Paused::Elapsed;
        }
        loop {
            match sys::wait_signal(signal::RUNNER_SIGNALS) {
                Ok(SIGCHLD) => {
                    if fired.load(Ordering::SeqCst) {
                        return Paused::Elapsed;
                    }
                }
                Ok(stop) => return Paused::Stopped(stop),
                Err(error) => {
                    log_line(&format!(
                        "stop signals cannot be received during the pause: {error}"
                    ));
                    while !fired.load(Ordering::SeqCst) {
                        thread::sleep(PAUSE_POLL);
                    }
                    return Paused::Elapsed;
                }
            }
        }
    }

    fn exec(&self, program: &Path, args: &[OsString]) -> Error {
        // The runner's own image, not dsh: the process keeps its pid, so
        // launchd's job and `run/state.json` still name it.
        let source = Command::new(program).args(args).exec();
        Error::io("exec", program, source)
    }
}
