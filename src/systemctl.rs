//! The user manager as dshd sees it: `systemctl --user` calls, the parsed
//! facts of a unit, and the derivation of one instance state from those
//! facts, the record and the last run. No D-Bus and no unit files: the one
//! file dshd writes for the manager is the wants link of an enabled instance.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::cli::report::{Ending, StateSlug};
use crate::error::{Error, ToolFailure};
use crate::fsx;
use crate::name::{InstanceName, StateRoot, TARGET, TEMPLATE_FILE};
use crate::record::{InstanceRecord, LastRun};
use crate::sys;
use crate::tool::{Captured, Tool};
use serde::Serialize;

mod parse;

use self::parse::{parse_environment, parse_list_units, persistent_unit_dir, unit_facts};

/// The kernel's id of the running boot.
const BOOT_ID_FILE: &str = "/proc/sys/kernel/random/boot_id";
/// Where logind keeps one empty file per user that has linger enabled.
const LINGER_DIR: &str = "/var/lib/systemd/linger";
/// The `list-units` pattern that matches every instance of the template.
const INSTANCE_PATTERN: &str = "dshd@*.service";
/// A read-only template instance outside the public instance-name grammar.
/// Loading it expands `%S` without spawning a process or adding a listed instance.
const STATE_QUERY_UNIT: &str = "dshd@-.service";
/// The `SERVICE_RESULT` of a run that ended cleanly.
const RESULT_SUCCESS: &str = "success";

/// `systemctl --user`. Every call passes `--user` first; `list-units`
/// adds `--plain --no-legend` and `show` adds `--timestamp=unix`. The pager
/// and colours are off through the environment [`Tool::command`] sets.
#[derive(Debug, Clone)]
pub struct Systemctl {
    tool: Tool,
    paths: OnceLock<ManagerPaths>,
}

/// The manager variables dshd derives paths from, read with one
/// `show-environment` per process. Only absolute values count, as the XDG
/// base directory rules say.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagerPaths {
    runtime_dir: Option<PathBuf>,
}

/// Properties of one unit from `systemctl show`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitFacts {
    /// `Id`.
    pub unit: String,
    /// `LoadState`.
    pub load_state: LoadState,
    /// `ActiveState`.
    pub active_state: ActiveState,
    /// `SubState`, verbatim.
    pub sub_state: String,
    /// `Result`.
    pub result: UnitResult,
    /// `MainPID`, 0 when none.
    pub main_pid: u32,
    /// `ExecMainStatus`.
    pub exec_main_status: i32,
    /// `NRestarts`.
    pub restarts: u32,
    /// `ActiveEnterTimestamp` as unix seconds, when set.
    pub active_enter: Option<u64>,
    /// `InactiveEnterTimestamp` as unix seconds, when set.
    pub inactive_enter: Option<u64>,
    /// `InvocationID`, empty when none.
    pub invocation_id: String,
    /// `WantedBy`: the units whose `.wants` links name this one.
    pub wanted_by: Vec<String>,
    /// Native counters; unavailable accounting is null, never invented zero.
    pub resources: Resources,
}

impl UnitFacts {
    /// Whether the manager has loaded this unit as wanted by `dshd.target`.
    pub fn enabled(&self) -> bool {
        self.wanted_by.iter().any(|unit| unit == TARGET)
    }
}

/// Resource counters supplied by the user manager for the current or last run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resources {
    /// CPU time in nanoseconds, when CPU accounting is available.
    #[serde(rename = "cpuUsageNSec")]
    pub cpu_usage_nsec: Option<u64>,
    /// Current memory use in bytes.
    pub memory_current: Option<u64>,
    /// Peak memory use in bytes.
    pub memory_peak: Option<u64>,
    /// Current task count.
    pub tasks_current: Option<u64>,
    /// Bytes read; unavailable when the user manager has no io controller.
    pub io_read_bytes: Option<u64>,
    /// Bytes written; unavailable when the user manager has no io controller.
    pub io_write_bytes: Option<u64>,
}

/// `ActiveState` values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveState {
    /// active
    Active,
    /// activating (includes `auto-restart`)
    Activating,
    /// deactivating
    Deactivating,
    /// inactive
    Inactive,
    /// failed
    Failed,
    /// anything else, kept verbatim
    Unknown(String),
}

/// `Result` values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitResult {
    /// success
    Success,
    /// exit-code
    ExitCode,
    /// signal
    Signal,
    /// timeout
    Timeout,
    /// core-dump
    CoreDump,
    /// start-limit-hit
    StartLimitHit,
    /// anything else, kept verbatim
    Unknown(String),
}

/// `LoadState` values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState {
    /// loaded
    Loaded,
    /// not-found
    NotFound,
    /// masked
    Masked,
    /// anything else, kept verbatim
    Unknown(String),
}

/// The properties `show` requests, in order.
const SHOW_PROPERTIES: &[&str] = &[
    "Id",
    "LoadState",
    "ActiveState",
    "SubState",
    "Result",
    "MainPID",
    "ExecMainStatus",
    "NRestarts",
    "ActiveEnterTimestamp",
    "InactiveEnterTimestamp",
    "InvocationID",
    "WantedBy",
    "CPUUsageNSec",
    "MemoryCurrent",
    "MemoryPeak",
    "TasksCurrent",
    "IOReadBytes",
    "IOWriteBytes",
];

impl Systemctl {
    /// `systemctl` from `PATH`.
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(Tool::named("systemctl"))
    }

    /// `systemctl` from a given tool (checks substitute a fake).
    #[must_use]
    pub fn new(tool: Tool) -> Self {
        Self {
            tool,
            paths: OnceLock::new(),
        }
    }

    /// `show-environment`, parsed. A failure to reach the manager is
    /// `ForeignSession` when this process has no user session of its own,
    /// else `SystemdUnavailable`.
    pub fn manager_environment(&self) -> Result<BTreeMap<String, String>, Error> {
        match self.call(["show-environment"]) {
            Ok(captured) => Ok(parse_environment(self.text(&captured, "show-environment")?)),
            Err(Error::Tool(failure)) => Err(unreachable_manager(Error::Tool(failure).to_string())),
            Err(other) => Err(other),
        }
    }

    /// Resolves the template's `%S` through the manager, independently of
    /// environment.d or a client's imported `XDG_STATE_HOME`.
    pub fn state_root(&self) -> Result<StateRoot, Error> {
        let captured = match self.call(["show", "-p", "WorkingDirectory", "--", STATE_QUERY_UNIT]) {
            Err(Error::Tool(failure)) => {
                return Err(unreachable_manager(Error::Tool(failure).to_string()));
            }
            other => other?,
        };
        let path = self.text(&captured, "show")?.trim_end();
        let directory = path.strip_prefix("WorkingDirectory=").unwrap_or_default();
        if directory.is_empty() {
            return Err(Error::TemplateMissing);
        }
        let directory = Path::new(directory);
        let root = directory
            .parent()
            .filter(|path| path.file_name().is_some_and(|name| name == "dshd"))
            .and_then(Path::parent)
            .filter(|_| directory.file_name().is_some_and(|name| name == "-"))
            .ok_or_else(|| {
                self.malformed(
                    "show",
                    "WorkingDirectory does not name the queried template instance",
                )
            })?;
        if !root.is_absolute() {
            return Err(self.malformed("show", "WorkingDirectory is not absolute"));
        }
        Ok(StateRoot::new(root.to_path_buf()))
    }

    /// `XDG_RUNTIME_DIR` of the manager.
    pub fn runtime_root(&self) -> Result<PathBuf, Error> {
        self.paths()?.runtime_dir.clone().ok_or_else(|| {
            Error::SystemdUnavailable(
                "the user manager's environment has no absolute XDG_RUNTIME_DIR".to_owned(),
            )
        })
    }

    /// `show -p <SHOW_PROPERTIES> UNIT`.
    pub fn show(&self, unit: &str) -> Result<UnitFacts, Error> {
        let mut facts = self.show_many(&[unit.to_owned()])?;
        facts
            .pop()
            .ok_or_else(|| self.malformed("show", "no unit properties in the output"))
    }

    /// One `show` call for many units, in the order given.
    pub fn show_many(&self, units: &[String]) -> Result<Vec<UnitFacts>, Error> {
        if units.is_empty() {
            return Ok(Vec::new());
        }
        let properties = SHOW_PROPERTIES.join(",");
        let args = ["show", "--timestamp=unix", "-p", properties.as_str()]
            .into_iter()
            .chain(std::iter::once("--"))
            .chain(units.iter().map(String::as_str));
        let captured = self.call(args)?;
        let facts = parse_show(self.text(&captured, "show")?)?;
        if facts.len() == units.len()
            && facts
                .iter()
                .zip(units)
                .all(|(fact, unit)| fact.unit == *unit)
        {
            Ok(facts)
        } else {
            Err(self.malformed("show", "the output does not hold one block per unit"))
        }
    }

    /// `list-units --all 'dshd@*.service'`, parsed back to names.
    pub fn list_loaded_instances(&self) -> Result<Vec<InstanceName>, Error> {
        let captured = self.call([
            "list-units",
            "--all",
            "--plain",
            "--no-legend",
            INSTANCE_PATTERN,
        ])?;
        Ok(parse_list_units(self.text(&captured, "list-units")?))
    }

    /// Whether `dshd@.service` is loaded (`TemplateMissing` is raised by
    /// callers that need it).
    ///
    /// systemd refuses to `show` a template name, which has no instance, so
    /// the answer is the load state of `dshd.target`: the module installs
    /// the template and the target together, and neither without the other.
    pub fn template_installed(&self) -> Result<bool, Error> {
        let captured = self.call(["show", "-p", "LoadState", "--", TARGET])?;
        Ok(self.text(&captured, "show")?.trim() == "LoadState=loaded")
    }

    /// `start UNIT`.
    pub fn start(&self, unit: &str) -> Result<(), Error> {
        self.act(["start", unit])
    }

    /// `stop UNIT`.
    pub fn stop(&self, unit: &str) -> Result<(), Error> {
        self.act(["stop", unit])
    }

    /// `restart UNIT`.
    pub fn restart(&self, unit: &str) -> Result<(), Error> {
        self.act(["restart", unit])
    }

    /// `reset-failed UNIT`; a unit that is not failed is fine.
    pub fn reset_failed(&self, unit: &str) -> Result<(), Error> {
        match self.act(["reset-failed", unit]) {
            // systemd answers "Unit NAME not loaded." for a unit it does not hold.
            Err(Error::Tool(failure)) if failure.stderr.contains("not loaded") => Ok(()),
            other => other,
        }
    }

    /// Makes `unit` wanted by `dshd.target`: links it to the template in
    /// `dshd.target.wants` of the manager's persistent unit directory, runs
    /// `daemon-reload` and returns the link. `systemctl add-wants` is not
    /// used, since the template and target resolve to store paths outside
    /// the unit search path, so it would also link both by name to those
    /// paths there. Such a copy shadows the template of every later switch,
    /// is no GC root, and hides `dshd.target` from the switch that starts
    /// it. A link the manager has not loaded after the reload is removed,
    /// since a later reload would enable the instance unannounced.
    pub fn add_wants(&self, unit: &str) -> Result<PathBuf, Error> {
        let link = self
            .persistent_unit_dir()?
            .join(format!("{TARGET}.wants"))
            .join(unit);
        let template = Path::new(TEMPLATE_FILE);
        if let Some(directory) = link.parent() {
            fsx::create_dir_private(directory)?;
        }
        fsx::symlink_replace(template, &link)?;
        let loaded = self.act(["daemon-reload"]).and_then(|()| self.show(unit));
        if loaded.as_ref().is_ok_and(UnitFacts::enabled) {
            return Ok(link);
        }
        let removed = fsx::unlink_if_target(&link, template);
        loaded?;
        removed?;
        Err(self.malformed(
            "daemon-reload",
            &format!("the user manager did not load {}", link.display()),
        ))
    }

    /// The manager's persistent unit directory,
    /// `$XDG_CONFIG_HOME/systemd/user` as the manager resolved it at its
    /// start, which no environment imported since can move.
    fn persistent_unit_dir(&self) -> Result<PathBuf, Error> {
        let captured = self.call(["show", "-p", "UnitPath"])?;
        let text = self.text(&captured, "show")?.trim_end();
        text.strip_prefix("UnitPath=")
            .and_then(persistent_unit_dir)
            .ok_or_else(|| {
                self.malformed(
                    "show",
                    "UnitPath does not start with the persistent control directory",
                )
            })
    }

    /// `disable UNIT`.
    pub fn disable(&self, unit: &str) -> Result<(), Error> {
        self.act(["disable", unit])
    }

    /// The last `n` journal lines of `unit` through `journalctl --user`.
    pub fn journal_tail(
        &self,
        journalctl: &Tool,
        unit: &str,
        n: u32,
    ) -> Result<Vec<String>, Error> {
        let lines = n.to_string();
        let captured = journalctl.run([
            "--user",
            "-u",
            unit,
            "-n",
            lines.as_str(),
            "--no-pager",
            "-o",
            "cat",
        ])?;
        Ok(captured.stdout_text().lines().map(str::to_owned).collect())
    }

    /// The systemctl command line a mutating verb echoes, for the human
    /// output (`systemctl --user start dshd@web-1.service`).
    #[must_use]
    pub fn echo(&self, args: &[&str]) -> String {
        format!("{} --user {}", self.tool.name, args.join(" "))
    }

    /// Runs `systemctl --user ARGS`.
    fn call<'a>(&self, args: impl IntoIterator<Item = &'a str>) -> Result<Captured, Error> {
        self.tool.run(std::iter::once("--user").chain(args))
    }

    /// Runs a mutating verb, whose output dshd does not read.
    fn act<'a>(&self, args: impl IntoIterator<Item = &'a str>) -> Result<(), Error> {
        self.call(args)?;
        Ok(())
    }

    /// The manager's path variables, read once per process.
    fn paths(&self) -> Result<&ManagerPaths, Error> {
        if let Some(paths) = self.paths.get() {
            return Ok(paths);
        }
        let paths = ManagerPaths::from_environment(&self.manager_environment()?);
        Ok(self.paths.get_or_init(|| paths))
    }

    /// Machine facts must never be changed by a lossy decoder.
    fn text<'a>(&self, captured: &'a Captured, verb: &str) -> Result<&'a str, Error> {
        std::str::from_utf8(&captured.stdout)
            .map_err(|_| self.malformed(verb, "output is not UTF-8"))
    }

    /// A `systemctl` answer that exited 0 but cannot be what was asked for.
    fn malformed(&self, verb: &str, detail: &str) -> Error {
        Error::Tool(Box::new(ToolFailure {
            tool: self.tool.name,
            args: vec!["--user".to_owned(), verb.to_owned()],
            status: Some(0),
            stderr: detail.to_owned(),
        }))
    }
}

impl ManagerPaths {
    fn from_environment(environment: &BTreeMap<String, String>) -> Self {
        let absolute = |key: &str| {
            environment
                .get(key)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        };
        Self {
            runtime_dir: absolute("XDG_RUNTIME_DIR"),
        }
    }
}

/// Parses queried `show` properties, refusing incomplete, duplicate or malformed facts.
pub fn parse_show(text: &str) -> Result<Vec<UnitFacts>, Error> {
    let mut blocks = Vec::new();
    let mut block: BTreeMap<&str, &str> = BTreeMap::new();
    for line in text.lines() {
        if line.is_empty() {
            if !block.is_empty() {
                blocks.push(unit_facts(&block)?);
                block.clear();
            }
        } else {
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| invalid_show("a property line has no '='"))?;
            if !SHOW_PROPERTIES.contains(&key) {
                return Err(invalid_show(&format!("unexpected property {key}")));
            }
            if block.insert(key, value).is_some() {
                return Err(invalid_show(&format!("duplicate property {key}")));
            }
        }
    }
    if !block.is_empty() {
        blocks.push(unit_facts(&block)?);
    }
    Ok(blocks)
}

fn invalid_show(detail: &str) -> Error {
    Error::Tool(Box::new(ToolFailure {
        tool: "systemctl",
        args: vec!["--user".to_owned(), "show".to_owned()],
        status: Some(0),
        stderr: format!("invalid property output: {detail}"),
    }))
}

/// The kernel boot id, from `/proc/sys/kernel/random/boot_id`.
pub fn boot_id() -> Result<String, Error> {
    let path = Path::new(BOOT_ID_FILE);
    match fsx::read_text(path)? {
        Some(text) => Ok(text.trim().to_owned()),
        None => Err(Error::io(
            "read",
            path,
            io::Error::from(io::ErrorKind::NotFound),
        )),
    }
}

/// Whether `loginctl enable-linger` is on for the user (the file under
/// `/var/lib/systemd/linger`).
#[must_use]
pub fn linger_enabled(user: &str) -> bool {
    if user.is_empty() || user.contains('/') {
        return false;
    }
    matches!(Path::new(LINGER_DIR).join(user).try_exists(), Ok(true))
}

/// The error for a manager that did not answer. A shell from `su`,
/// `sudo -u` or `runuser` has no `XDG_RUNTIME_DIR`, or one of the user it
/// came from; it reaches no user manager whatever linger says.
fn unreachable_manager(detail: String) -> Error {
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty());
    let owner = runtime_dir
        .as_ref()
        .and_then(|dir| fs::metadata(dir).ok())
        .map(|metadata| metadata.uid());
    let uid = sys::current_user().ok().map(|user| user.uid);
    match foreign_session(runtime_dir.is_some(), owner, uid) {
        Some(why) => Error::ForeignSession(format!("{detail}; {why}")),
        None => Error::SystemdUnavailable(detail),
    }
}

/// Why this process has no user session of its own, when it has none:
/// `XDG_RUNTIME_DIR` is unset, names nothing, or belongs to another uid.
fn foreign_session(set: bool, owner: Option<u32>, uid: Option<u32>) -> Option<&'static str> {
    match (set, owner, uid) {
        (false, _, _) => Some("XDG_RUNTIME_DIR is unset, so this shell has no user session"),
        (true, None, _) => Some("XDG_RUNTIME_DIR names no directory"),
        (true, Some(owner), Some(uid)) if owner != uid => {
            Some("XDG_RUNTIME_DIR belongs to another user")
        }
        (true, Some(_), _) => None,
    }
}

/// Derives the closed state vocabulary. `facts` is `None` when the unit is
/// not loaded; `record` is `None` when there is no directory; `last_run` is
/// `None` before the first run.
///
/// A unit whose `LoadState` is `not-found` counts as not loaded. Without a
/// record, a unit the manager holds is `orphaned` and anything else is
/// `inconsistent`. With a record, a unit that is up reads as the manager
/// says; one that is not up and masked is `masked`. Otherwise an inactive
/// unit is told apart by its last run: none is `stopped`; a run with no end
/// recorded is `interrupted` (the host or the manager went down mid-run); a
/// run whose `serviceResult` is not `success` is `failed`; a clean run is
/// `stopped` under this boot and `previous-boot` under an earlier one. Any
/// other `ActiveState` (`reloading`, `refreshing`, `maintenance`) is
/// reported as `unknown`, with its raw value preserved. Whether the record itself is
/// consistent is the caller's call (`instance::load_strict`).
#[must_use]
pub fn derive_state(
    facts: Option<&UnitFacts>,
    record: Option<&InstanceRecord>,
    last_run: Option<&LastRun>,
    boot_id: &str,
) -> StateSlug {
    let loaded = facts.filter(|facts| facts.load_state != LoadState::NotFound);
    if record.is_none() {
        return if loaded.is_some() {
            StateSlug::Orphaned
        } else {
            StateSlug::Inconsistent
        };
    }
    let masked = loaded.is_some_and(|facts| facts.load_state == LoadState::Masked);
    match loaded.map(|facts| &facts.active_state) {
        Some(ActiveState::Active) => StateSlug::Running,
        Some(ActiveState::Activating) => StateSlug::Starting,
        Some(ActiveState::Unknown(_)) => StateSlug::Unknown,
        Some(ActiveState::Deactivating) => StateSlug::Stopping,
        _ if masked => StateSlug::Masked,
        Some(ActiveState::Failed) => StateSlug::Failed,
        Some(ActiveState::Inactive) | None => match last_run {
            None => StateSlug::Stopped,
            Some(run) => match &run.end {
                None => StateSlug::Interrupted,
                Some(end) if end.service_result != RESULT_SUCCESS => StateSlug::Failed,
                Some(_) if run.boot_id == boot_id => StateSlug::Stopped,
                Some(_) => StateSlug::PreviousBoot,
            },
        },
    }
}

/// How a run ended, by precedence: no end recorded is `interrupted`, a
/// result other than `success` is `failed`, a stop dshd asked for is
/// `stopped`, a stop signal without that intent is
/// `stopped-without-intent`, and anything else is dsh having `exited`.
#[must_use]
pub fn ending(run: &LastRun) -> Ending {
    match &run.end {
        None => Ending::Interrupted,
        Some(end) if end.service_result != RESULT_SUCCESS => Ending::Failed,
        Some(end) if end.stop_intent => Ending::Stopped,
        Some(end) if end.stop_signal.is_some() => Ending::StoppedWithoutIntent,
        Some(_) => Ending::Exited,
    }
}

/// What a headless instance's task did, which changes its remedy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// The instance has a transport, so a start submits nothing.
    NotHeadless,
    /// A headless instance that never ran.
    NotRun,
    /// A headless instance whose last run ended so.
    Ended(Ending),
}

/// The remedy command for a state, when one applies. `up` says the unit
/// holds or moves a process. An orphan is stopped while up and removed
/// after; a masked unit is unmasked by the user, never by dshd; an
/// inconsistent instance is removed. A down instance is started again,
/// except a headless one whose task ran: none when dsh exited by itself,
/// and `start --rerun` otherwise, since a start submits the task again.
#[must_use]
pub fn remedy(state: StateSlug, name: &InstanceName, up: bool, task: Task) -> Option<String> {
    match state {
        StateSlug::Orphaned if up => Some(format!("dshd stop {name}")),
        StateSlug::Orphaned | StateSlug::Inconsistent => Some(format!("dshd rm {name}")),
        StateSlug::Masked => Some(format!("systemctl --user unmask {}", name.unit())),
        StateSlug::Unknown => Some(format!("systemctl --user status {}", name.unit())),
        StateSlug::Failed
        | StateSlug::Stopped
        | StateSlug::Interrupted
        | StateSlug::PreviousBoot => match task {
            Task::Ended(Ending::Exited) => None,
            Task::Ended(_) => Some(format!("dshd start --rerun {name}")),
            Task::NotHeadless | Task::NotRun => Some(format!("dshd start {name}")),
        },
        StateSlug::Running | StateSlug::Starting | StateSlug::Stopping => None,
    }
}

#[cfg(test)]
mod unit_tests;
