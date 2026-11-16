//! A bench for the launchd verb tests: a scratch home, the fake launchd
//! and a manager over both, with waits short enough for a test.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::fsx::testing::Scratch;
use crate::name::{InstanceName, RuntimeDir};
use crate::platform::ServiceManager;
use crate::platform::launchd::LaunchdManager;
use crate::platform::launchd::fake::{FakeLaunchctl, HookEvent};

/// The uid of the bench's GUI domain.
pub(crate) const UID: u32 = 501;
/// The start time the bench reports for every process the fake spawns.
pub(crate) const START: u64 = 1_700_000_000;

/// One instance under test.
pub(crate) struct Bench {
    /// Holds the home; removed on drop.
    pub(crate) scratch: Scratch,
    /// The fake launchd, shared with the manager.
    pub(crate) fake: FakeLaunchctl,
    /// The manager under test.
    pub(crate) manager: LaunchdManager,
    /// The instance.
    pub(crate) name: InstanceName,
}

impl Bench {
    /// A bench for instance `name`, its directory created.
    pub(crate) fn new(label: &str, name: &str) -> Self {
        let scratch = Scratch::new(label);
        let home = scratch.path().join("Users").join("u");
        let fake = FakeLaunchctl::default();
        let manager = LaunchdManager::new(Box::new(fake.clone()), UID, home)
            .with_timing(
                Duration::from_secs(2),
                Duration::from_secs(2),
                Duration::from_millis(5),
            )
            .with_process_start(|_pid| Some(START));
        let name = InstanceName::parse(name).unwrap();
        std::fs::create_dir_all(manager.instance_dir(&name).path()).unwrap();
        Self {
            scratch,
            fake,
            manager,
            name,
        }
    }

    /// The home the manager uses.
    pub(crate) fn home(&self) -> PathBuf {
        self.scratch.path().join("Users").join("u")
    }

    /// `gui/501/dshd.NAME`.
    pub(crate) fn target(&self) -> String {
        self.manager.service_target(&self.name)
    }

    /// The instance's plist path, as a string for argv comparisons.
    pub(crate) fn plist(&self) -> String {
        self.manager.plist_path(&self.name).display().to_string()
    }

    /// The instance's run directory, created.
    pub(crate) fn run_dir(&self) -> RuntimeDir {
        let run = self.manager.runtime_dir(&self.name);
        std::fs::create_dir_all(run.path()).unwrap();
        run
    }

    /// Makes every bootstrap act as a runner that becomes ready at once
    /// under `invocation`, by writing `run/state.json` after `delay`.
    pub(crate) fn ready_on_bootstrap(&self, invocation: &str, delay: Duration) {
        let run = self.run_dir();
        let invocation = invocation.to_owned();
        let label = LaunchdManager::label(&self.name);
        self.fake.state().on_bootstrap = Some(Arc::new(move |event: &HookEvent<'_>| {
            if event.label != label {
                return;
            }
            let path = run.path().join("state.json");
            let text = state_json(&invocation, event.pid.unwrap_or(0), "ready");
            if delay.is_zero() {
                std::fs::write(&path, text).unwrap();
            } else {
                std::thread::spawn(move || {
                    std::thread::sleep(delay);
                    std::fs::write(&path, text).unwrap();
                });
            }
        }));
    }
}

/// A `run/state.json` in the shape `runstate::RunState` reads.
pub(crate) fn state_json(invocation: &str, pid: u32, phase: &str) -> String {
    format!(
        r#"{{"invocationId":"{invocation}","pid":{pid},"processStart":{START},"phase":"{phase}","phaseSince":{}}}"#,
        START + 1
    )
}

/// `["a", "b"]` as owned strings, for argv comparisons.
pub(crate) fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}
