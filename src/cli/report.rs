//! The JSON objects the CLI prints: the status object, the list, the
//! outcome of a mutating verb, the journal of a run, and the error shape.
//! `tests/contract/dshd.schema.json` describes each; fields are only ever
//! added.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::error::Error;
use crate::record::{ChildEnd, Transport};
use crate::relay::protocol::ProbeLine;

/// The `state` vocabulary of `get` and `ls`, closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StateSlug {
    /// The manager reports an unrecognized active state; inspect its raw value.
    Unknown,
    /// Unit active, runner ready.
    Running,
    /// Unit activating, or in auto-restart.
    Starting,
    /// Unit deactivating.
    Stopping,
    /// Unit failed, or its last run ended without `success`.
    Failed,
    /// Unit inactive after its last run ended.
    Stopped,
    /// No end recorded: the host or the user manager went down mid-run.
    Interrupted,
    /// Unit inactive, last run belongs to an earlier boot.
    PreviousBoot,
    /// The user masked the unit, so systemd will not start it.
    Masked,
    /// Unit loaded but no record on disk.
    Orphaned,
    /// Record on disk contradicts itself.
    Inconsistent,
}

/// How the last run ended, closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ending {
    /// `dshd stop` or `dshd restart` stopped it.
    Stopped,
    /// Something other than dshd stopped it: `systemctl stop`, a logout
    /// without linger, a shutdown.
    StoppedWithoutIntent,
    /// dsh exited by itself, and the unit's result was `success`.
    Exited,
    /// The unit's result was not `success`.
    Failed,
    /// No end recorded: the host or the user manager went down mid-run.
    Interrupted,
}

/// The `release` block of a status object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseReport {
    /// The recorded registry name.
    pub name: String,
    /// `current` link target, or `null` when dangling.
    pub current: Option<String>,
    /// `booted` link target, or `null` before the first start.
    pub booted: Option<String>,
    /// What the registry points at now, or `null` when the name is gone.
    pub registry: Option<String>,
    /// `current`, `stale`, `outdated` or `missing`.
    pub state: &'static str,
    /// Short label for the human line, from the store name.
    pub label: String,
}

/// The `profile` block of a status object: the profile dsh runs, live
/// from its DSH home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileReport {
    /// The profile name.
    pub name: String,
    /// The DSH home the profile lives in.
    pub dsh_home: String,
}

/// The `web` block of a status object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebReport {
    /// Pinned port, once known.
    pub port: Option<u16>,
    /// Announced URL with the token redacted, while running.
    pub url: Option<String>,
}

/// The `lastRun` block of a status object. The end fields are `null`
/// until the run's end is recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastRunReport {
    /// Unix seconds.
    pub started_at: u64,
    /// The systemd invocation the run belonged to.
    pub invocation_id: String,
    /// How the run ended; `null` while it is the run in progress.
    pub ending: Option<Ending>,
    /// Unix seconds.
    pub ended_at: Option<u64>,
    /// `SERVICE_RESULT`.
    pub service_result: Option<String>,
    /// `EXIT_CODE`.
    pub exit_code: Option<String>,
    /// `EXIT_STATUS`.
    pub exit_status: Option<String>,
    /// The `dsh` child's own end.
    pub child: Option<ChildEnd>,
    /// The run happened under the current boot.
    pub this_boot: bool,
}

/// The status object: `get`, and the body of every mutating verb's JSON
/// output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusReport {
    /// Instance name.
    pub name: String,
    /// Instance id.
    pub id: Option<String>,
    /// `dshd@<name>.service`.
    pub unit: String,
    /// Closed vocabulary.
    pub state: StateSlug,
    /// systemd's `ActiveState`.
    pub active_state: String,
    /// systemd's `SubState`.
    pub sub_state: String,
    /// systemd's `Result`.
    pub result: String,
    /// Main PID while running.
    pub main_pid: Option<u32>,
    /// `NRestarts`.
    pub restarts: u32,
    /// Native resource counters, null where accounting is unavailable.
    pub resources: crate::systemctl::Resources,
    /// Current `InvocationID`, while running.
    pub invocation_id: Option<String>,
    /// Unix seconds the unit entered its current state.
    pub since: Option<u64>,
    /// `enabled`: wanted by `dshd.target`.
    pub enabled: bool,
    /// Linger is on, so the instance outlives the last login session.
    pub linger: bool,
    /// Recorded transport.
    pub transport: Option<Transport>,
    /// Recorded cwd.
    pub cwd: Option<String>,
    /// Recorded native argv after rewrite and injection.
    pub args: Vec<String>,
    /// Names only of the captured environment.
    pub environment_names: Vec<String>,
    /// Labels.
    pub labels: BTreeMap<String, String>,
    /// Release block.
    pub release: Option<ReleaseReport>,
    /// Profile block.
    pub profile: Option<ProfileReport>,
    /// Web block, for web instances.
    pub web: Option<WebReport>,
    /// Relay block, for acp instances, when the probe answered.
    pub acp: Option<ProbeLine>,
    /// Last run block.
    pub last_run: Option<LastRunReport>,
    /// The remedy command, when an action is needed.
    pub remedy: Option<String>,
}

/// One instance in `ls`: the status object's summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListEntry {
    /// Instance name.
    pub name: String,
    /// Closed vocabulary.
    pub state: StateSlug,
    /// Recorded transport.
    pub transport: Option<Transport>,
    /// Release label.
    pub release: Option<String>,
    /// Release state.
    pub release_state: Option<&'static str>,
    /// Wanted by `dshd.target`.
    pub enabled: bool,
    /// Unix seconds the unit entered its current state.
    pub since: Option<u64>,
    /// Remedy command, when an action is needed.
    pub remedy: Option<String>,
}

/// `ls`: every instance, one object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListReport {
    /// The instances, by name.
    pub instances: Vec<ListEntry>,
    /// Linger is on.
    pub linger: bool,
    /// Entries of the state directory that are not instances.
    pub warnings: Vec<String>,
}

/// The `outcome` vocabulary of the mutating verbs, closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutcomeSlug {
    /// `run` made a new instance and started it.
    Created,
    /// The instance was started.
    Started,
    /// The instance ran already.
    AlreadyRunning,
    /// The instance was stopped.
    Stopped,
    /// The instance was not running.
    AlreadyStopped,
    /// The instance was restarted.
    Restarted,
    /// `current` now names another release.
    Updated,
    /// Nothing changed.
    Unchanged,
    /// The instance is wanted by `dshd.target` now.
    Enabled,
    /// It was wanted already.
    AlreadyEnabled,
    /// It is no longer wanted.
    Disabled,
    /// It was not wanted.
    AlreadyDisabled,
    /// The instance and its state are gone.
    Removed,
    /// The labels changed.
    Labelled,
    /// The run followed by `run` or `wait` ended.
    Ended,
    /// The run followed by `run` or `wait` still runs at the check-in.
    StillRunning,
}

/// How a run that `run` or `wait` followed ended, or that it still runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunExit {
    /// The exit code the text form uses: dsh's own code, 128 + the signal
    /// that ended it, or 124 at a check-in.
    pub exit_code: u8,
    /// The signal that ended dsh.
    pub signal: Option<i32>,
    /// What happened, in words.
    pub reason: String,
}

/// The JSON output of a mutating verb.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    /// Closed vocabulary.
    pub outcome: OutcomeSlug,
    /// What the text form says beside the status line.
    pub warnings: Vec<String>,
    /// For `run` and `wait` once they followed a run.
    #[serde(flatten)]
    pub exit: Option<RunExit>,
    /// The status observed after the action (before deletion for `removed`).
    #[serde(flatten)]
    pub status: StatusReport,
}

/// `read --json`: the journal of one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadReport {
    /// Instance name.
    pub name: String,
    /// The invocation read.
    pub invocation_id: String,
    /// The lines start at the run's first line.
    pub complete: bool,
    /// Which of dsh's streams the journal holds.
    pub streams: Vec<&'static str>,
    /// The lines.
    pub lines: Vec<String>,
}

/// The JSON shape of every error on stderr.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorJson<'a> {
    /// The error object.
    pub error: ErrorBody<'a>,
}

/// Body of [`ErrorJson`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorBody<'a> {
    /// Stable slug.
    pub kind: &'a str,
    /// Message text.
    pub message: String,
    /// Remedy, or `null`.
    pub hint: Option<String>,
}

impl<'a> ErrorJson<'a> {
    /// Builds the JSON view of an error.
    pub fn new(error: &'a Error) -> Self {
        Self {
            error: ErrorBody {
                kind: error.kind(),
                message: error.to_string(),
                hint: error.hint(),
            },
        }
    }
}
