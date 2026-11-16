//! The one `LaunchAgent` property list dshd writes per instance, from a
//! closed key set. Scheduling is not dshd's job: whatever schedules work
//! starts instances from outside with `dshd start`, and some hosts refuse
//! any job that carries a schedule key. So the renderer writes
//! exactly [`ALLOWED_KEYS`], in that order, and nothing else; no plist
//! ever holds an environment value, a token or a credential.

use std::path::PathBuf;

use crate::error::Error;
use crate::platform::launchd::not_built;

/// The keys dshd writes, in the order it writes them. Adding a key is a
/// reviewed change to this list.
pub const ALLOWED_KEYS: &[&str] = &[
    "Label",
    "ProgramArguments",
    "WorkingDirectory",
    "RunAtLoad",
    "LimitLoadToSessionType",
    "ProcessType",
    "ExitTimeOut",
    "AbandonProcessGroup",
    "StandardOutPath",
    "StandardErrorPath",
];

/// Keys dshd never writes: restart policy lives in the runner, the
/// runner composes dsh's environment, and nothing is ever scheduled.
pub const NEVER_WRITTEN: &[&str] = &[
    "KeepAlive",
    "StartInterval",
    "StartCalendarInterval",
    "WatchPaths",
    "QueueDirectories",
    "StartOnMount",
    "Sockets",
    "Disabled",
    "EnvironmentVariables",
];

/// Mode of every plist dshd writes: owned by the user, never group- or
/// world-writable, and holding nothing secret.
pub const PLIST_MODE: u32 = 0o644;

/// What one instance's job says. Everything else is fixed by the renderer:
/// `RunAtLoad` true, `LimitLoadToSessionType` `Aqua`, `ProcessType`
/// `Standard`, `AbandonProcessGroup` false.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpec {
    /// `Label`: `dshd.NAME`.
    pub label: String,
    /// The first entry of `ProgramArguments`: `<instance>/current/bin/dshd-run`.
    pub program: PathBuf,
    /// The rest of `ProgramArguments`: `run NAME`.
    pub arguments: Vec<String>,
    /// `WorkingDirectory`: the instance directory.
    pub working_directory: PathBuf,
    /// `StandardOutPath` and `StandardErrorPath`: `<instance>/launchd.log`.
    pub log_path: PathBuf,
    /// `ExitTimeOut` in seconds.
    pub exit_timeout: u32,
}

/// Renders `spec` as an XML property list: the XML declaration, Apple's
/// PLIST 1.0 doctype, one `<dict>` holding [`ALLOWED_KEYS`] in order, one
/// tab of indentation per level, `&`, `<` and `>` escaped in every string,
/// and a final newline. A path that is not UTF-8 is an `Io` error with
/// action `render`, since a plist string must be text.
pub fn render(spec: &JobSpec) -> Result<String, Error> {
    let _ = spec;
    Err(not_built("the plist renderer"))
}

/// The keys of the top-level `<dict>` of a plist dshd rendered, in order.
/// Keys of nested dictionaries are not top-level keys.
pub fn top_level_keys(xml: &str) -> Vec<String> {
    let _ = xml;
    Vec::new()
}

/// `Ok` when every top-level key is in [`ALLOWED_KEYS`]; otherwise the
/// first key that is not.
pub fn check_keys(xml: &str) -> Result<(), String> {
    let _ = xml;
    Err("the plist key check is not built yet".to_owned())
}

#[cfg(test)]
#[path = "plist_tests.rs"]
mod tests;
