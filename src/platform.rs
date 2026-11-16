//! The platform seam: the one interface the CLI drives a service manager
//! through. Linux implements it with the systemd user manager
//! ([`systemd::SystemdManager`], today's code behind the trait, byte for
//! byte); macOS implements it with launchd ([`launchd::LaunchdManager`]).
//! [`native`] picks the implementation at compile time by `target_os`.
//!
//! The launchd module compiles on every target, so its pure logic (the
//! plist renderer, the `launchctl print` parsers, the log windows) and its
//! verbs against a fake `launchctl` are tested on Linux too. Only its live
//! use needs a Mac.
//!
//! What stays shared and platform-neutral: the state vocabulary
//! (`systemctl::derive_state`, `ending`, `remedy`), the records, the launch
//! recipe and the runner's exit table. [`UnitFacts`] is the common shape of
//! what a manager knows about one instance; launchd fills the systemd words
//! from its own facts and the run records.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use crate::error::Error;
use crate::name::{InstanceName, RuntimeDir, StateRoot};
use crate::systemctl::UnitFacts;

pub mod launchd;
pub mod systemd;

/// A mutating verb whose manager command line is echoed on the human page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The instance is started.
    Start,
    /// The instance is stopped.
    Stop,
    /// The instance is restarted.
    Restart,
    /// The instance no longer starts with the login session.
    Disable,
}

/// Where the output of an instance's runs is kept, for `read`, `wait` and
/// `run` to read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunLogs {
    /// journald: every entry of a run carries its
    /// `_SYSTEMD_INVOCATION_ID` (systemd).
    Journal,
    /// One file per run, `<dir>/<invocationId>.log`, read with
    /// `launchd::logs::read_window` and followed with
    /// `launchd::follow::Follower` (launchd).
    Files(PathBuf),
}

/// The service manager that owns instance processes, as the CLI sees it.
///
/// Every method takes an instance name, never a manager-specific unit
/// string, so the same verb body works on both platforms. Errors keep the
/// existing kinds (`systemd-unavailable`, `systemctl-failed`,
/// `unit-masked`, `template-missing`) on every platform: the contract
/// only grows.
pub trait ServiceManager: fmt::Debug {
    /// The tool the manager is driven with (`systemctl`, `launchctl`). A
    /// tool failure that names it is the manager's own failure.
    fn tool_name(&self) -> &'static str;

    /// Where instance directories live: systemd's `%S`, or
    /// `~/.local/state` from the password database on macOS.
    fn state_root(&self) -> &StateRoot;

    /// The user's configuration root, which holds the user release
    /// registry (`<config root>/dshd/releases`): the user manager's `%E`
    /// (`XDG_CONFIG_HOME`, else `~/.config`) on Linux, `~/.config` from the
    /// password database on macOS.
    fn config_root(&self) -> Result<PathBuf, Error>;

    /// The runtime directory of one instance's run: `%t/dshd/NAME` on
    /// Linux, `<instance>/run` on macOS.
    fn runtime_dir(&self, name: &InstanceName) -> RuntimeDir;

    /// Variables the booted runner's relay client (`attach`, `probe`)
    /// needs to find the relay of a run.
    fn relay_client_env(&self) -> Vec<(OsString, OsString)>;

    /// How the manager names the instance: the `unit` field of `get` and
    /// `ls` (`dshd@NAME.service`, or `gui/UID/dshd.NAME`).
    fn unit_id(&self, name: &InstanceName) -> String;

    /// What the manager knows about one instance, loaded or not.
    fn show(&self, name: &InstanceName) -> Result<UnitFacts, Error>;

    /// [`ServiceManager::show`] for many instances, in the order given,
    /// with a number of manager calls that does not grow with `names`.
    fn show_many(&self, names: &[InstanceName]) -> Result<Vec<UnitFacts>, Error>;

    /// The instances the manager holds, with or without a directory.
    fn list_loaded_instances(&self) -> Result<Vec<InstanceName>, Error>;

    /// Whether the manager can run instances at all (systemd: the unit
    /// template is installed).
    fn template_installed(&self) -> Result<bool, Error>;

    /// Starts the instance; at most one runner is spawned per call.
    fn start(&self, name: &InstanceName) -> Result<(), Error>;

    /// Stops the instance and returns once the manager let it go.
    fn stop(&self, name: &InstanceName) -> Result<(), Error>;

    /// Stops and starts the instance.
    fn restart(&self, name: &InstanceName) -> Result<(), Error>;

    /// Clears a failed state; an instance that is not failed is fine.
    fn reset_failed(&self, name: &InstanceName) -> Result<(), Error>;

    /// Makes the instance start with the login session (or the user
    /// manager). Returns the command lines the human page echoes, in
    /// order, unstyled.
    fn enable(&self, name: &InstanceName) -> Result<Vec<String>, Error>;

    /// Undoes [`ServiceManager::enable`]; nothing stops.
    fn disable(&self, name: &InstanceName) -> Result<(), Error>;

    /// The last `lines` lines of the instance's output, for the tail a
    /// `start-failed` error carries.
    fn failure_tail(&self, name: &InstanceName, lines: u32) -> Result<Vec<String>, Error>;

    /// Where the output of the instance's runs is kept.
    fn run_logs(&self, name: &InstanceName) -> RunLogs;

    /// The manager command line a mutating verb echoes.
    fn echo(&self, action: Action, name: &InstanceName) -> String;

    /// The remedy `get` prints for a masked instance: only the user
    /// unmasks, never dshd.
    fn masked_remedy(&self, name: &InstanceName) -> String;

    /// The remedy `get` prints for a state outside the vocabulary.
    fn unknown_remedy(&self, name: &InstanceName) -> String;

    /// The id of the running boot (or boot session): only compared for
    /// equality, to tell `previous-boot` from `stopped`.
    fn boot_id(&self) -> Result<String, Error>;

    /// Whether instances outlive the user's last login session. Always
    /// false on macOS: a `LaunchAgent` ends at logout.
    fn linger(&self, user: &str) -> bool;
}

/// The service manager of this platform.
#[cfg(not(target_os = "macos"))]
pub fn native() -> Result<Box<dyn ServiceManager>, Error> {
    Ok(Box::new(systemd::SystemdManager::from_env()?))
}

/// The service manager of this platform.
#[cfg(target_os = "macos")]
pub fn native() -> Result<Box<dyn ServiceManager>, Error> {
    Ok(Box::new(launchd::LaunchdManager::from_env()?))
}

/// The boot id of this platform, as [`ServiceManager::boot_id`] gives it,
/// without building a manager: the kernel's boot id on Linux. The runner
/// stamps every run with it.
#[cfg(not(target_os = "macos"))]
pub fn native_boot_id() -> Result<String, Error> {
    crate::systemctl::boot_id()
}

/// The boot id of this platform, as [`ServiceManager::boot_id`] gives it,
/// without building a manager: `kern.bootsessionuuid` on macOS, since
/// there is no `/proc`. The runner stamps every run with it.
#[cfg(target_os = "macos")]
pub fn native_boot_id() -> Result<String, Error> {
    crate::sys::boot_session_uuid()
}
