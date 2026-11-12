//! The one error type. Every failure the CLI or runner can report is a
//! variant here, with a stable kind slug, an exit code and an optional
//! remedy. Human and JSON renderings both come from these three accessors,
//! so the two surfaces cannot drift.

use std::fmt;
use std::io;
use std::path::PathBuf;

use crate::name::GRAMMAR;

/// The exit code of `run` and `wait` when their deadline passed while the
/// instance still runs: the operation continues, and nothing failed.
pub const CHECK_IN: u8 = 124;

/// The remedy for a user manager that stops with the last login session.
pub const LINGER_REMEDY: &str = "set users.users.<you>.linger = true in the NixOS configuration (or run loginctl enable-linger)";

/// What went wrong. Variants carry what the message needs and nothing else;
/// large payloads are boxed so the type stays small.
#[derive(Debug)]
pub enum Error {
    /// The instance name broke the grammar.
    InvalidName {
        /// The name as given.
        name: String,
        /// The first rule it broke.
        reason: &'static str,
    },
    /// A flag, value or verb was not understood.
    InvalidArgument(String),
    /// A verb that acts on one instance was given no NAME.
    NeedsTarget {
        /// The verb.
        verb: &'static str,
        /// The instances that exist, when they could be listed.
        candidates: Option<Vec<String>>,
    },
    /// `start -- ARGV`: creating an instance is `run`'s; the argv as shown.
    CreateWithRun(String),
    /// `update` on an instance created from a release store path.
    PathPinned(String),
    /// `run --patch PATH` names no readable regular file of at most 1 MiB.
    PatchUnreadable {
        /// The path as given.
        path: String,
        /// Why it cannot be snapshotted.
        reason: String,
    },
    /// `run --stdin FILE` names no readable regular file (or stdin) of at
    /// most 1 MiB.
    StdinUnreadable {
        /// The path as given, `-` for dshd's own stdin.
        path: String,
        /// Why it cannot be snapshotted.
        reason: String,
    },
    /// No such instance.
    NotFound(String),
    /// No such release in the registry.
    ReleaseNotFound(String),
    /// The release package is missing a required path or its metadata is bad.
    ReleaseInvalid {
        /// The release path or name.
        release: String,
        /// Why.
        reason: String,
    },
    /// `run --name N` with a recipe that differs from the stored one.
    ExistsDifferentRecipe(Box<RecipeDifference>),
    /// The verb needs the instance stopped.
    InstanceRunning(String),
    /// A headless instance whose task already ran would resubmit its prompt.
    TaskAlreadyRan(String),
    /// The user masked the instance's unit, so systemd refuses to start
    /// it. Holds the unit.
    UnitMasked(String),
    /// The verb needs the instance running.
    NotRunning(String),
    /// `stop --invocation ID`: the unit runs another invocation, or it is
    /// stopped and its last run was another one or none.
    OtherInvocation(Box<OtherInvocation>),
    /// Another controller holds the ACP relay.
    AttachedElsewhere(String),
    /// The instance's transport does not support the requested connection.
    UnsupportedTransport {
        /// The instance.
        name: String,
        /// The transport it has.
        transport: &'static str,
    },
    /// The profile's app is not one dshd can hold in a unit.
    UnsupportedProfile {
        /// The profile.
        profile: String,
        /// What kind of app it has.
        app: UnheldApp,
    },
    /// `enable` on an instance whose unit cannot be wanted by the target.
    AutostartNotAllowed(String),
    /// On-disk state contradicts itself (unknown record keys, dangling links).
    InconsistentState {
        /// The instance.
        name: String,
        /// What was found.
        reason: String,
    },
    /// systemd's user manager did not answer.
    SystemdUnavailable(String),
    /// The user manager did not answer, and `XDG_RUNTIME_DIR` is unset or
    /// not this user's: the shell came from `su`, `sudo -u` or `runuser`.
    ForeignSession(String),
    /// A tool dshd needs is not on `PATH`.
    ToolMissing(&'static str),
    /// The unit template is not installed in the user manager.
    TemplateMissing,
    /// A tool ran and failed.
    Tool(Box<ToolFailure>),
    /// The unit did not reach `active` within the timeout.
    StartFailed(Box<StartFailure>),
    /// The unit did not stop.
    StopFailed {
        /// The instance.
        name: String,
        /// What systemd said.
        detail: String,
    },
    /// A file operation failed.
    Io {
        /// What was being done.
        action: &'static str,
        /// On which path.
        path: PathBuf,
        /// The OS error.
        source: io::Error,
    },
    /// Writing to stdout or stderr failed. `BrokenPipe` exits 0 quietly.
    Output(io::Error),
}

/// The app of a profile dshd refuses to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnheldApp {
    /// An SDK host, which speaks JSON-RPC on stdio to the client that
    /// starts it.
    Sdk,
    /// No app dshd can hold: no manifest, no bundles, or a custom app.
    Other,
}

/// Two recipes that differ, field by field, for `exists-different-recipe`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeDifference {
    /// The instance.
    pub name: String,
    /// `(field, stored, requested)` triples in display form.
    pub fields: Vec<(&'static str, String, String)>,
}

/// What `stop --invocation` found instead of the run it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherInvocation {
    /// The instance.
    pub name: String,
    /// The invocation `--invocation` named.
    pub expected: String,
    /// The unit's current invocation while it is up, else its last one;
    /// `None` when there is none to name.
    pub found: Option<String>,
    /// The unit is up, so `found` is the run it holds now.
    pub up: bool,
}

/// A tool ran and exited non-zero.
#[derive(Debug)]
pub struct ToolFailure {
    /// The tool's name (`systemctl`, `nix-store`, `journalctl`, `dsh`...).
    pub tool: &'static str,
    /// Its argv after the program, for the message.
    pub args: Vec<String>,
    /// Exit status, or `None` when it died by signal.
    pub status: Option<i32>,
    /// Its stderr, trimmed.
    pub stderr: String,
}

/// The unit did not become active.
#[derive(Debug)]
pub struct StartFailure {
    /// The instance.
    pub name: String,
    /// What `systemctl show` said when the start settled.
    pub active_state: String,
    /// `Result=` of the unit.
    pub result: String,
    /// The last journal lines of the unit, when they could be read.
    pub journal_tail: Vec<String>,
}

impl Error {
    /// The stable slug for JSON output and the documentation table.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::InvalidName { .. } => "invalid-name",
            Self::InvalidArgument(_)
            | Self::CreateWithRun(_)
            | Self::PathPinned(_)
            | Self::PatchUnreadable { .. }
            | Self::StdinUnreadable { .. } => "invalid-argument",
            Self::NeedsTarget { .. } => "needs-target",
            Self::NotFound(_) => "not-found",
            Self::ReleaseNotFound(_) => "release-not-found",
            Self::ReleaseInvalid { .. } => "release-invalid",
            Self::ExistsDifferentRecipe(_) => "exists-different-recipe",
            Self::InstanceRunning(_) => "instance-running",
            Self::TaskAlreadyRan(_) => "task-already-ran",
            Self::UnitMasked(_) => "unit-masked",
            Self::NotRunning(_) | Self::OtherInvocation(_) => "not-running",
            Self::AttachedElsewhere(_) => "attached-elsewhere",
            Self::UnsupportedTransport { .. } => "unsupported-transport",
            Self::UnsupportedProfile { .. } => "unsupported-profile",
            Self::AutostartNotAllowed(_) => "autostart-not-allowed",
            Self::InconsistentState { .. } => "inconsistent-state",
            Self::SystemdUnavailable(_) | Self::ForeignSession(_) => "systemd-unavailable",
            Self::ToolMissing(_) => "tool-missing",
            Self::TemplateMissing => "template-missing",
            Self::Tool(failure) => match failure.tool {
                "systemctl" => "systemctl-failed",
                "nix-store" => "release-invalid",
                _ => "io",
            },
            Self::StartFailed(_) => "start-failed",
            Self::StopFailed { .. } => "stop-failed",
            Self::Io { .. } | Self::Output(_) => "io",
        }
    }

    /// The process exit code for this error. Codes are stable once
    /// announced; a new kind joins the row of its nearest meaning.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Output(e) if e.kind() == io::ErrorKind::BrokenPipe => 0,
            Self::InvalidName { .. }
            | Self::InvalidArgument(_)
            | Self::NeedsTarget { .. }
            | Self::CreateWithRun(_)
            | Self::PathPinned(_)
            | Self::PatchUnreadable { .. }
            | Self::StdinUnreadable { .. } => 2,
            Self::NotFound(_) | Self::ReleaseNotFound(_) => 3,
            Self::ExistsDifferentRecipe(_)
            | Self::InstanceRunning(_)
            | Self::TaskAlreadyRan(_)
            | Self::UnitMasked(_)
            | Self::AttachedElsewhere(_) => 4,
            Self::NotRunning(_) | Self::OtherInvocation(_) => 5,
            Self::UnsupportedTransport { .. }
            | Self::UnsupportedProfile { .. }
            | Self::AutostartNotAllowed(_) => 6,
            Self::SystemdUnavailable(_)
            | Self::ForeignSession(_)
            | Self::ToolMissing(_)
            | Self::TemplateMissing => 7,
            Self::ReleaseInvalid { .. }
            | Self::InconsistentState { .. }
            | Self::Tool(_)
            | Self::StartFailed(_)
            | Self::StopFailed { .. }
            | Self::Io { .. }
            | Self::Output(_) => 1,
        }
    }

    /// The remedy printed after the message, when there is one: the next
    /// command to run, or a plain pointer where no command fits.
    pub fn hint(&self) -> Option<String> {
        match self {
            Self::InvalidName { .. } => Some(format!("names match {GRAMMAR}")),
            Self::InvalidArgument(_) => Some("dshd help".into()),
            Self::NeedsTarget { .. } | Self::NotFound(_) => Some("dshd ls".into()),
            Self::CreateWithRun(argv) => Some(format!("dshd run --bg -- {argv}")),
            Self::PatchUnreadable { path, .. } => Some(format!(
                "--patch takes a readable regular file of at most 1 MiB; check {path}"
            )),
            Self::StdinUnreadable { path, .. } => Some(format!(
                "--stdin takes a readable regular file of at most 1 MiB, or - for dshd's own stdin; check {path}"
            )),
            Self::ReleaseNotFound(_) => Some("ls /etc/dshd/releases".into()),
            Self::PathPinned(name) | Self::InconsistentState { name, .. } => Some(format!("dshd rm {name}")),
            Self::ExistsDifferentRecipe(diff) => Some(format!("dshd rm {}", diff.name)),
            Self::InstanceRunning(name) => Some(format!("dshd stop {name}")),
            Self::TaskAlreadyRan(name) => Some(format!("dshd start --rerun {name}")),
            Self::UnitMasked(unit) => Some(format!("systemctl --user unmask {unit}")),
            Self::NotRunning(name) | Self::AutostartNotAllowed(name) => Some(format!("dshd start {name}")),
            Self::OtherInvocation(other) => Some(format!("dshd get {}", other.name)),
            Self::AttachedElsewhere(name) => Some(format!("dshd attach --takeover {name}")),
            Self::UnsupportedTransport { name, transport: "none" } => Some(format!("dshd read --follow {name}")),
            Self::UnsupportedTransport { name, .. } => Some(format!("dshd attach {name}")),
            Self::UnsupportedProfile { profile, app: UnheldApp::Sdk } => Some(format!("run dsh {profile} under that client")),
            Self::UnsupportedProfile { profile, app: UnheldApp::Other } => {
                Some(format!("run `dsh {profile}` in a terminal, or in a terminal multiplexer pane"))
            }
            Self::SystemdUnavailable(_) => Some(LINGER_REMEDY.into()),
            Self::ForeignSession(_) => {
                Some("log in as this user (ssh, or machinectl shell USER@), so XDG_RUNTIME_DIR and the user bus exist".into())
            }
            Self::ToolMissing(tool) => Some(format!("put {tool} on PATH")),
            Self::TemplateMissing => Some("set dshd.enable = true and switch".into()),
            Self::StartFailed(failure) => Some(format!("dshd read {}", failure.name)),
            Self::StopFailed { name, .. } => Some(format!("dshd get {name}")),
            Self::ReleaseInvalid { .. } | Self::Tool(_) | Self::Io { .. } | Self::Output(_) => None,
        }
    }

    /// Builds an `Io` error.
    pub fn io(action: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.into(),
            source,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName { name, reason } => {
                write!(f, "invalid instance name {name:?}: {reason}")
            }
            Self::InvalidArgument(msg) => f.write_str(msg),
            Self::NeedsTarget { verb, candidates } => {
                format_needs_target(f, verb, candidates.as_deref())
            }
            Self::CreateWithRun(_) => {
                f.write_str("start starts an existing instance by NAME; run creates one")
            }
            Self::PathPinned(name) => write!(
                f,
                "{name} is pinned to a release path; remove it, then run it again with --release RELEASE"
            ),
            Self::PatchUnreadable { path, reason } => write!(f, "--patch {path}: {reason}"),
            Self::StdinUnreadable { path, reason } => write!(f, "--stdin {path}: {reason}"),
            Self::NotFound(name) => write!(f, "no instance named {name}"),
            Self::ReleaseNotFound(name) => write!(f, "no release named {name}"),
            Self::ReleaseInvalid { release, reason } => {
                write!(f, "release {release} is not usable: {reason}")
            }
            Self::ExistsDifferentRecipe(diff) => write!(f, "{diff}"),
            Self::InstanceRunning(name) => write!(f, "{name} is running"),
            Self::TaskAlreadyRan(name) => {
                write!(
                    f,
                    "{name}'s headless task already ran; starting it again resubmits the recorded prompt"
                )
            }
            Self::UnitMasked(unit) => write!(f, "{unit} is masked, so systemd will not start it"),
            Self::NotRunning(name) => write!(f, "{name} is not running"),
            Self::OtherInvocation(other) => write!(f, "{other}"),
            Self::AttachedElsewhere(name) => write!(f, "{name} already has a controller attached"),
            Self::UnsupportedTransport {
                name,
                transport: "none",
            } => {
                write!(f, "{name} has transport none; it has nothing to attach to")
            }
            Self::UnsupportedTransport { name, transport } => {
                write!(
                    f,
                    "{name} has transport {transport}; --via names the other transport"
                )
            }
            Self::UnsupportedProfile { profile, app } => {
                write!(
                    f,
                    "profile {profile} cannot be held by dshd: {}",
                    app.reason()
                )
            }
            Self::AutostartNotAllowed(name) => write!(
                f,
                "{name} has transport none, so it cannot be enabled: its task would run again at every start of the user manager"
            ),
            Self::InconsistentState { name, reason } => {
                write!(f, "{name} state is inconsistent: {reason}")
            }
            Self::SystemdUnavailable(detail) | Self::ForeignSession(detail) => {
                write!(f, "systemd user manager unavailable: {detail}")
            }
            Self::ToolMissing(tool) => write!(f, "{tool} not found on PATH"),
            Self::TemplateMissing => write!(
                f,
                "{} is not installed in the user manager",
                crate::name::UNIT_TEMPLATE
            ),
            Self::Tool(failure) => write!(f, "{failure}"),
            Self::StartFailed(failure) => write!(f, "{failure}"),
            Self::StopFailed { name, detail } => write!(f, "{name} did not stop: {detail}"),
            Self::Io {
                action,
                path,
                source,
            } => write!(f, "cannot {action} {}: {source}", path.display()),
            Self::Output(source) => write!(f, "cannot write output: {source}"),
        }
    }
}

/// Missing target details are shared by usage errors with and without a candidate list.
fn format_needs_target(
    f: &mut fmt::Formatter<'_>,
    verb: &str,
    candidates: Option<&[String]>,
) -> fmt::Result {
    match candidates {
        None => write!(f, "{verb} needs the NAME of an instance"),
        Some([]) => write!(f, "{verb}: no NAME given, and no instance exists"),
        Some(candidates) => write!(f, "{verb}: which instance? {}", candidates.join(", ")),
    }
}

impl UnheldApp {
    fn reason(self) -> &'static str {
        match self {
            Self::Sdk => "an SDK host speaks JSON-RPC on stdio to the client that starts it",
            Self::Other => "it has no app dshd can hold; it may need a terminal",
        }
    }
}

impl fmt::Display for RecipeDifference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} exists with a different recipe:", self.name)?;
        for (field, stored, requested) in &self.fields {
            write!(f, "\n  {field}: stored {stored}, requested {requested}")?;
        }
        Ok(())
    }
}

impl fmt::Display for OtherInvocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            name,
            expected,
            found,
            up,
        } = self;
        write!(f, "{name} is not running invocation {expected}: ")?;
        match (found, up) {
            (Some(found), true) => write!(f, "it runs {found}"),
            (None, true) => f.write_str("systemd names no invocation for what it runs"),
            (Some(found), false) => write!(f, "its last run was {found}"),
            (None, false) => f.write_str("it has no recorded run"),
        }
    }
}

impl fmt::Display for ToolFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} failed", self.tool, self.args.join(" "))?;
        match self.status {
            Some(code) => write!(f, " with status {code}")?,
            None => f.write_str(" by signal")?,
        }
        if !self.stderr.is_empty() {
            write!(f, ": {}", self.stderr)?;
        }
        Ok(())
    }
}

impl fmt::Display for StartFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} did not start ({}, {})",
            self.name, self.active_state, self.result
        )?;
        for line in &self.journal_tail {
            write!(f, "\n  {line}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
#[path = "error/unit_tests.rs"]
mod tests;
