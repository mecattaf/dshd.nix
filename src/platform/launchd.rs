//! The launchd backend: one `LaunchAgent` job per instance in the user's GUI
//! login domain `gui/<uid>`, rendered by dshd from a closed key set and
//! driven only with `launchctl bootstrap`, `bootout`, `print` and
//! `print-disabled`. Never `kickstart`, never `enable` or `disable`, never
//! a schedule.
//!
//! Naming, fixed here and nowhere else:
//! - label `dshd.NAME`, service target `gui/<uid>/dshd.NAME` (the `unit`
//!   field of `get` and `ls`);
//! - instances under `~/.local/state/dshd/NAME`, the home taken from the
//!   password database and `XDG_STATE_HOME` ignored, so every shell and
//!   agent finds the same instances;
//! - the job's plist at `<instance>/launchd.plist`, its early output at
//!   `<instance>/launchd.log`, the run's files under `<instance>/run`, the
//!   per-run logs under `<instance>/runs`;
//! - an enabled instance's copy at `~/Library/LaunchAgents/dshd.NAME.plist`.
//!
//! The module compiles on every target. Its verbs are tested against
//! `fake::FakeLaunchctl`; only live use needs a Mac. A piece that is not
//! built yet answers [`not_built`], which is exit 7 (`systemd-unavailable`)
//! with a message that says so.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use crate::error::Error;
use crate::name::{InstanceDir, InstanceName, RELEASE_RUNNER, RuntimeDir, StateRoot};
use crate::platform::{Action, RunLogs, ServiceManager};
use crate::systemctl::{ActiveState, Resources, UnitFacts};
use crate::tool::Tool;

pub mod command;
pub mod completion;
pub mod enable;
pub mod exit;
pub mod facts;
#[cfg(test)]
pub(crate) mod fake;
pub mod follow;
pub mod ids;
pub mod logs;
pub mod plist;
pub mod print;
pub mod query;
pub mod restart;
pub mod retention;
pub mod runstate;
pub mod rusage;
pub mod start;
pub mod stop;
#[cfg(test)]
pub(crate) mod testing;

use self::command::CommandRunner;
use self::print::JobFacts;

/// The fixed product prefix of every label dshd writes. No owner login,
/// host name or organisation ever appears in a label.
pub const LABEL_PREFIX: &str = "dshd.";
/// The rendered job, inside the instance directory.
pub const PLIST_FILE: &str = "launchd.plist";
/// The job's stdout and stderr before the runner opens its run log.
pub const LAUNCHD_LOG_FILE: &str = "launchd.log";
/// The run's files (`state.json`, `acp.sock`, `web-url`, `stop-intent`),
/// in place of systemd's `RuntimeDirectory=`.
pub const RUN_DIR: &str = "run";
/// The per-run logs, `<invocationId>.log` and the `latest.log` link.
pub const RUNS_DIR: &str = "runs";
/// Where an enabled instance's copy lives, under the home.
pub const AGENTS_DIR: &str = "Library/LaunchAgents";
/// launchd's grace between SIGTERM and SIGKILL, as `TimeoutStopSec=30s`.
pub const EXIT_TIMEOUT_SECS: u32 = 30;
/// The startup deadline the runner enforces, as the NixOS option's default.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(150);
/// How long `start` waits beyond the startup deadline for the runner's own
/// verdict, and `stop` beyond launchd's exit timeout, before each settles
/// on what it sees.
pub const GRACE: Duration = Duration::from_secs(5);
/// How often a job that is still moving is read again.
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The start time (unix seconds) of a process, by pid; `None` when it is
/// unknown or the process is gone.
pub type ProcessStart = fn(u32) -> Option<u64>;

/// The live counters of the process tree under a pid (the job's runner).
pub type TreeResources = fn(u32) -> Resources;

/// The error of a piece of the macOS backend that is not built yet: exit 7,
/// as every call that needs the manager returns until it lands.
#[must_use]
pub fn not_built(what: &str) -> Error {
    Error::SystemdUnavailable(format!("launchd support is not built yet: {what}"))
}

/// `launchctl`, one user's GUI domain and that user's instance root.
#[derive(Debug)]
pub struct LaunchdManager {
    launchctl: Box<dyn CommandRunner>,
    uid: u32,
    home: PathBuf,
    state_root: StateRoot,
    startup_wait: Duration,
    stop_wait: Duration,
    poll_interval: Duration,
    process_start: ProcessStart,
    tree_resources: TreeResources,
}

impl LaunchdManager {
    /// `launchctl` from `PATH`, for the invoking user from the password
    /// database.
    pub fn from_env() -> Result<Self, Error> {
        let user = crate::sys::current_user()?;
        Ok(Self::new(
            Box::new(Tool::named("launchctl")),
            user.uid,
            user.home,
        ))
    }

    /// A manager over a given runner, uid and password-database home.
    #[must_use]
    pub fn new(launchctl: Box<dyn CommandRunner>, uid: u32, home: PathBuf) -> Self {
        let state_root = StateRoot::new(home.join(".local").join("state"));
        Self {
            launchctl,
            uid,
            home,
            state_root,
            startup_wait: STARTUP_TIMEOUT + GRACE,
            stop_wait: Duration::from_secs(u64::from(EXIT_TIMEOUT_SECS)) + GRACE,
            poll_interval: POLL_INTERVAL,
            process_start: native_process_start,
            tree_resources: native_tree_resources,
        }
    }

    /// The same manager with other waits (tests shorten them): how long
    /// `start` waits for readiness, how long `stop` waits for launchd to
    /// let the job go, and how often each reads again.
    #[must_use]
    pub fn with_timing(
        mut self,
        startup_wait: Duration,
        stop_wait: Duration,
        poll: Duration,
    ) -> Self {
        self.startup_wait = startup_wait;
        self.stop_wait = stop_wait;
        self.poll_interval = poll;
        self
    }

    /// The same manager with another process start-time source (tests).
    #[must_use]
    pub fn with_process_start(mut self, process_start: ProcessStart) -> Self {
        self.process_start = process_start;
        self
    }

    /// The same manager with another source of live counters (tests).
    #[must_use]
    pub fn with_tree_resources(mut self, tree_resources: TreeResources) -> Self {
        self.tree_resources = tree_resources;
        self
    }

    /// `facts` with the live counters of the job's process tree, while a
    /// run is up: `facts::unit_facts` leaves every counter unknown, and
    /// the tree is sampled here, from the runner's pid down. A run that is
    /// not up keeps them unknown, as systemd reports no counters then.
    #[must_use]
    pub fn with_live_resources(&self, mut facts: UnitFacts) -> UnitFacts {
        let up = matches!(
            facts.active_state,
            ActiveState::Active | ActiveState::Activating | ActiveState::Deactivating
        );
        if up && facts.main_pid != 0 {
            facts.resources = (self.tree_resources)(facts.main_pid);
        }
        facts
    }

    /// The `launchctl` runner.
    pub fn launchctl(&self) -> &dyn CommandRunner {
        self.launchctl.as_ref()
    }

    /// The user's uid.
    pub fn uid(&self) -> u32 {
        self.uid
    }

    /// How long `start` waits for readiness: the startup deadline the
    /// runner enforces plus [`GRACE`].
    pub fn startup_wait(&self) -> Duration {
        self.startup_wait
    }

    /// How long `stop` waits for launchd to let the job go: its exit
    /// timeout plus [`GRACE`].
    pub fn stop_wait(&self) -> Duration {
        self.stop_wait
    }

    /// The interval between two reads of a moving job.
    pub fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    /// The start time of process `pid`.
    pub fn process_start(&self, pid: u32) -> Option<u64> {
        (self.process_start)(pid)
    }

    /// `gui/<uid>`.
    pub fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    /// `dshd.NAME`.
    pub fn label(name: &InstanceName) -> String {
        format!("{LABEL_PREFIX}{name}")
    }

    /// The instance a label names, or `None` for labels that are not dshd's.
    pub fn name_of_label(label: &str) -> Option<InstanceName> {
        InstanceName::parse(label.strip_prefix(LABEL_PREFIX)?).ok()
    }

    /// `gui/<uid>/dshd.NAME`.
    pub fn service_target(&self, name: &InstanceName) -> String {
        format!("{}/{}", self.domain(), Self::label(name))
    }

    /// The instance directory.
    pub fn instance_dir(&self, name: &InstanceName) -> InstanceDir {
        self.state_root.instance(name)
    }

    /// `<instance>/launchd.plist`.
    pub fn plist_path(&self, name: &InstanceName) -> PathBuf {
        self.instance_dir(name).path().join(PLIST_FILE)
    }

    /// `~/Library/LaunchAgents/dshd.NAME.plist`.
    pub fn agent_copy(&self, name: &InstanceName) -> PathBuf {
        self.home
            .join(AGENTS_DIR)
            .join(format!("{}.plist", Self::label(name)))
    }

    /// `<instance>/launchd.log`.
    pub fn launchd_log(&self, name: &InstanceName) -> PathBuf {
        self.instance_dir(name).path().join(LAUNCHD_LOG_FILE)
    }

    /// `<instance>/runs`.
    pub fn runs_dir(&self, name: &InstanceName) -> PathBuf {
        self.instance_dir(name).path().join(RUNS_DIR)
    }

    /// The job dshd renders for the instance: the runner through the
    /// `current` link, as `ExecStart` runs it on Linux.
    pub fn job_spec(&self, name: &InstanceName) -> plist::JobSpec {
        let dir = self.instance_dir(name);
        plist::JobSpec {
            label: Self::label(name),
            program: dir.current().join(RELEASE_RUNNER),
            arguments: vec!["run".to_owned(), name.to_string()],
            working_directory: dir.path().to_path_buf(),
            log_path: self.launchd_log(name),
            exit_timeout: EXIT_TIMEOUT_SECS,
        }
    }

    /// Runs `launchctl ARGS`.
    pub fn call(&self, args: &[String]) -> Result<crate::tool::Captured, Error> {
        self.launchctl.run(args)
    }

    /// `launchctl print gui/<uid>/dshd.NAME`, parsed; `None` when launchd
    /// holds no such job. A missing GUI domain is `SystemdUnavailable`.
    pub fn job(&self, name: &InstanceName) -> Result<Option<JobFacts>, Error> {
        let args = ["print".to_owned(), self.service_target(name)];
        match self.call(&args) {
            Ok(captured) => print::parse_job(&captured.stdout_text()).map(Some),
            Err(error) => match exit::classify(error) {
                exit::Failure::NoSuchJob => Ok(None),
                exit::Failure::Other(error) => Err(error),
            },
        }
    }
}

impl ServiceManager for LaunchdManager {
    fn tool_name(&self) -> &'static str {
        self.launchctl.name()
    }

    fn state_root(&self) -> &StateRoot {
        &self.state_root
    }

    fn config_root(&self) -> Result<PathBuf, Error> {
        Ok(self.home.join(".config"))
    }

    fn runtime_dir(&self, name: &InstanceName) -> RuntimeDir {
        RuntimeDir::new(self.instance_dir(name).path().join(RUN_DIR))
    }

    fn relay_client_env(&self) -> Vec<(OsString, OsString)> {
        Vec::new()
    }

    fn relay_client_dir(&self, name: &InstanceName) -> Option<PathBuf> {
        Some(self.instance_dir(name).path().to_path_buf())
    }

    fn unit_id(&self, name: &InstanceName) -> String {
        self.service_target(name)
    }

    fn show(&self, name: &InstanceName) -> Result<UnitFacts, Error> {
        query::show(self, name).map(|facts| self.with_live_resources(facts))
    }

    fn show_many(&self, names: &[InstanceName]) -> Result<Vec<UnitFacts>, Error> {
        let all = query::show_many(self, names)?;
        Ok(all
            .into_iter()
            .map(|facts| self.with_live_resources(facts))
            .collect())
    }

    fn list_loaded_instances(&self) -> Result<Vec<InstanceName>, Error> {
        query::list_loaded(self)
    }

    fn template_installed(&self) -> Result<bool, Error> {
        // launchd has no template: dshd renders each job itself.
        Ok(true)
    }

    fn start(&self, name: &InstanceName) -> Result<(), Error> {
        start::start(self, name)
    }

    fn stop(&self, name: &InstanceName) -> Result<(), Error> {
        stop::stop(self, name)
    }

    fn restart(&self, name: &InstanceName) -> Result<(), Error> {
        start::restart(self, name)
    }

    fn reset_failed(&self, _name: &InstanceName) -> Result<(), Error> {
        // A failed run is a record, not a manager state to clear.
        Ok(())
    }

    fn forget(&self, name: &InstanceName, _facts: &UnitFacts) -> Result<(), Error> {
        // launchd keeps a job whose run ended (a crash, a killed runner, a
        // finished task) loaded until it is booted out, and its plist is
        // about to go with the directory. A job it does not hold is fine.
        stop::stop(self, name)
    }

    fn enable(&self, name: &InstanceName) -> Result<Vec<String>, Error> {
        enable::enable(self, name)
    }

    fn disable(&self, name: &InstanceName) -> Result<(), Error> {
        enable::disable(self, name)
    }

    fn failure_tail(&self, name: &InstanceName, lines: u32) -> Result<Vec<String>, Error> {
        logs::failure_tail(&self.runs_dir(name), &self.launchd_log(name), lines)
    }

    fn run_logs(&self, name: &InstanceName) -> RunLogs {
        RunLogs::Files(self.runs_dir(name))
    }

    fn echo(&self, action: Action, name: &InstanceName) -> String {
        let target = self.service_target(name);
        let plist = self.plist_path(name);
        match action {
            Action::Start => format!("launchctl bootstrap {} {}", self.domain(), plist.display()),
            Action::Stop => format!("launchctl bootout {target}"),
            Action::Restart => format!(
                "launchctl bootout {target} && launchctl bootstrap {} {}",
                self.domain(),
                plist.display()
            ),
            Action::Disable => format!("rm {}", self.agent_copy(name).display()),
        }
    }

    fn masked_remedy(&self, name: &InstanceName) -> String {
        format!("launchctl enable {}", self.service_target(name))
    }

    fn unknown_remedy(&self, name: &InstanceName) -> String {
        format!("launchctl print {}", self.service_target(name))
    }

    fn boot_id(&self) -> Result<String, Error> {
        boot_session_id()
    }

    fn linger(&self, _user: &str) -> bool {
        false
    }
}

/// The start time of a process on macOS: `proc_pidinfo`.
#[cfg(target_os = "macos")]
fn native_process_start(pid: u32) -> Option<u64> {
    crate::sys::process_start(pid)
}

/// Off macOS the backend runs only under test, where the bench supplies
/// start times: a real one is unknown, so no state file reads live.
#[cfg(not(target_os = "macos"))]
fn native_process_start(_pid: u32) -> Option<u64> {
    None
}

/// The live counters of the process tree under `root` on macOS: one
/// `proc_pid_rusage` sample per process that has not exited (a zombie
/// holds no memory and runs nothing), converted with the timebase.
#[cfg(target_os = "macos")]
fn native_tree_resources(root: u32) -> Resources {
    let samples: Vec<_> = crate::sys::process_tree(root)
        .iter()
        .filter(|process| !process.zombie)
        .map(|process| crate::sys::task_sample(process.pid))
        .collect();
    rusage::live_counters(&samples, crate::sys::timebase())
}

/// Off macOS there is no tree to sample: every counter is unknown.
#[cfg(not(target_os = "macos"))]
fn native_tree_resources(_root: u32) -> Resources {
    Resources::default()
}

/// `kern.bootsessionuuid`, lowercased: the boot identity on macOS.
#[cfg(target_os = "macos")]
fn boot_session_id() -> Result<String, Error> {
    crate::sys::boot_session_uuid()
}

/// Off macOS the launchd backend runs only under test, with no boot
/// session to read.
#[cfg(not(target_os = "macos"))]
fn boot_session_id() -> Result<String, Error> {
    Err(not_built("the boot session id outside macOS"))
}

#[cfg(test)]
#[path = "launchd/unit_tests.rs"]
mod tests;
