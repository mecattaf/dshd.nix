//! The systemd user manager behind [`ServiceManager`]: today's
//! [`Systemctl`] calls, unit names, paths, echoes and remedies, unchanged.
//! Nothing here decides anything new; every method delegates.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::error::Error;
use crate::name::{InstanceName, RuntimeDir, StateRoot, TEMPLATE_FILE};
use crate::platform::{Action, RunLogs, ServiceManager};
use crate::systemctl::{self, Systemctl, UnitFacts};
use crate::tool::Tool;

/// `systemctl --user` and `journalctl --user`, with the state and runtime
/// roots resolved once per process.
#[derive(Debug)]
pub struct SystemdManager {
    systemctl: Systemctl,
    journalctl: Tool,
    state_root: StateRoot,
    runtime_root: PathBuf,
}

impl SystemdManager {
    /// The manager on `PATH`. Resolves `%S` first and `XDG_RUNTIME_DIR`
    /// second, as the CLI always has, so the calls the golden cases log
    /// keep their order. A manager that does not answer is
    /// `SystemdUnavailable` or `ForeignSession`.
    pub fn from_env() -> Result<Self, Error> {
        let systemctl = Systemctl::from_env();
        let state_root = systemctl.state_root()?;
        let runtime_root = systemctl.runtime_root()?;
        Ok(Self::with_roots(
            systemctl,
            Tool::named("journalctl"),
            state_root,
            runtime_root,
        ))
    }

    /// A manager over given tools and already-resolved roots (tests).
    #[must_use]
    pub fn with_roots(
        systemctl: Systemctl,
        journalctl: Tool,
        state_root: StateRoot,
        runtime_root: PathBuf,
    ) -> Self {
        Self {
            systemctl,
            journalctl,
            state_root,
            runtime_root,
        }
    }
}

impl ServiceManager for SystemdManager {
    fn tool_name(&self) -> &'static str {
        "systemctl"
    }

    fn state_root(&self) -> &StateRoot {
        &self.state_root
    }

    fn runtime_dir(&self, name: &InstanceName) -> RuntimeDir {
        RuntimeDir::under(&self.runtime_root, name)
    }

    fn relay_client_env(&self) -> Vec<(OsString, OsString)> {
        vec![(
            OsString::from("XDG_RUNTIME_DIR"),
            self.runtime_root.clone().into_os_string(),
        )]
    }

    fn unit_id(&self, name: &InstanceName) -> String {
        name.unit()
    }

    fn show(&self, name: &InstanceName) -> Result<UnitFacts, Error> {
        self.systemctl.show(&name.unit())
    }

    fn show_many(&self, names: &[InstanceName]) -> Result<Vec<UnitFacts>, Error> {
        let units: Vec<String> = names.iter().map(InstanceName::unit).collect();
        self.systemctl.show_many(&units)
    }

    fn list_loaded_instances(&self) -> Result<Vec<InstanceName>, Error> {
        self.systemctl.list_loaded_instances()
    }

    fn template_installed(&self) -> Result<bool, Error> {
        self.systemctl.template_installed()
    }

    fn start(&self, name: &InstanceName) -> Result<(), Error> {
        self.systemctl.start(&name.unit())
    }

    fn stop(&self, name: &InstanceName) -> Result<(), Error> {
        self.systemctl.stop(&name.unit())
    }

    fn restart(&self, name: &InstanceName) -> Result<(), Error> {
        self.systemctl.restart(&name.unit())
    }

    fn reset_failed(&self, name: &InstanceName) -> Result<(), Error> {
        self.systemctl.reset_failed(&name.unit())
    }

    fn enable(&self, name: &InstanceName) -> Result<Vec<String>, Error> {
        let link = self.systemctl.add_wants(&name.unit())?;
        Ok(vec![
            format!("ln -sfn {TEMPLATE_FILE} {}", link.display()),
            self.systemctl.echo(&["daemon-reload"]),
        ])
    }

    fn disable(&self, name: &InstanceName) -> Result<(), Error> {
        self.systemctl.disable(&name.unit())
    }

    fn failure_tail(&self, name: &InstanceName, lines: u32) -> Result<Vec<String>, Error> {
        self.systemctl
            .journal_tail(&self.journalctl, &name.unit(), lines)
    }

    fn run_logs(&self, _name: &InstanceName) -> RunLogs {
        RunLogs::Journal
    }

    fn echo(&self, action: Action, name: &InstanceName) -> String {
        let verb = match action {
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Restart => "restart",
            Action::Disable => "disable",
        };
        self.systemctl.echo(&[verb, &name.unit()])
    }

    fn masked_remedy(&self, name: &InstanceName) -> String {
        format!("systemctl --user unmask {}", name.unit())
    }

    fn unknown_remedy(&self, name: &InstanceName) -> String {
        format!("systemctl --user status {}", name.unit())
    }

    fn boot_id(&self) -> Result<String, Error> {
        systemctl::boot_id()
    }

    fn linger(&self, user: &str) -> bool {
        systemctl::linger_enabled(user)
    }
}
