//! The restart policy the macOS runner keeps itself, since launchd's
//! `KeepAlive` cannot say "restart on 75 only": after the exit table
//! (`runner::exit`) gives 75 and no stop was asked, the runner records the
//! run, waits [`RESTART_DELAY`] and `exec`s the `current` runner, so the
//! job keeps its pid. A runner ended by a signal is never restarted.
//!
//! A start limit is optional: `None` means none. The Linux template's
//! `StartLimitBurst`/`StartLimitIntervalSec` are not copied here, because
//! that limit is not ratified. The start history in `run/starts.json`
//! still records every start, so `get` can report `restarts`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The exit status that asks for a restart.
pub const RESTART_STATUS: u8 = 75;
/// The pause before a restart, as `RestartSec=5` on Linux.
pub const RESTART_DELAY: Duration = Duration::from_secs(5);
/// The start history's file name inside the run directory.
pub const STARTS_FILE: &str = "starts.json";

/// At most `burst` starts within `interval_secs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartLimit {
    /// Starts allowed in the interval.
    pub burst: u32,
    /// The interval, in seconds.
    pub interval_secs: u64,
}

/// One start in the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Start {
    /// Unix seconds.
    pub at: u64,
    /// The runner restarted itself (exit 75) rather than being started by
    /// a bootstrap or by DSH's restart button.
    pub automatic: bool,
}

/// `run/starts.json`: this job's starts, oldest first. `start` and
/// `restart` bootstrap a new job and begin a new history.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartHistory {
    /// The starts.
    pub starts: Vec<Start>,
}

/// What the runner does once a run has ended and been recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterExit {
    /// Exit with the status: launchd keeps the job loaded, not running.
    Exit(u8),
    /// Wait this long, check again for a stop intent under the instance
    /// lock, then `exec` the `current` runner.
    Restart(Duration),
}

/// The decision after a run ended with runner exit status `status`.
/// `signalled` is true when the runner itself is ending on a signal it
/// was sent (it then never restarts); `stop_intent` when a stop was asked.
#[must_use]
pub fn after_exit(status: u8, signalled: bool, stop_intent: bool) -> AfterExit {
    let _ = (signalled, stop_intent);
    AfterExit::Exit(status)
}

/// Whether one more start at `now` is within `limit`; always true when
/// `limit` is `None`. Starts older than the interval do not count.
#[must_use]
pub fn admits(history: &StartHistory, now: u64, limit: Option<StartLimit>) -> bool {
    let _ = (history, now, limit);
    true
}

/// The number of automatic restarts in the history, as `NRestarts`.
#[must_use]
pub fn restarts(history: &StartHistory) -> u32 {
    let _ = history;
    0
}

#[cfg(test)]
#[path = "restart_tests.rs"]
mod tests;
