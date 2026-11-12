// SPDX-License-Identifier: Apache-2.0
// Adapted from herdr-core crates/herdr-core/tests/common/mod.rs @ ce81486; changes: keyed on the instance's HOME, bounded poll with POLL, SIGTERM through dshd::sys, no leak log file, assert instead of panic.
//! Shared by `tests/runner.rs` and `tests/relay.rs`: one instance laid out
//! on disk the way `dshd start` leaves it, under a private root, with the
//! stub dsh as the release's `bin/dsh` and the built `dshd-run` as its
//! runner; and the runner started outside systemd, from the release links,
//! in the working directory and with the variables the unit template gives
//! it. Every process a test starts for an instance carries that instance's
//! `HOME`, which is how dropping the instance finds the ones left behind.

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::{self, Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use dshd::name::{
    self, InstanceDir, InstanceName, RELEASE_DSH, RELEASE_FILE, RELEASE_RUNNER, RuntimeDir,
    StateRoot,
};
use dshd::record::{InstanceRecord, OsText, ProfileRef};
use dshd::tool::Tool;
use dshd::{fsx, instance, launch, profile, release, sys};
use serde_json::json;

/// What every test and helper returns.
pub(crate) type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// The stub dsh every release here ships as `bin/dsh`.
const STUB_DSH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/stub/dsh.sh");
/// The release metadata every release here ships.
const RELEASE_JSON: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/release.json");
/// The runner under test, as cargo built it.
const BUILT_RUNNER: &str = env!("CARGO_BIN_EXE_dshd-run");
/// The registry name the records carry; the runner never resolves it.
const RELEASE_NAME: &str = "default";
/// Mode of the instance record, as `dshd start` writes it.
const RECORD_MODE: u32 = 0o600;
/// What the runner is handed as `$INVOCATION_ID`, as systemd would.
pub(crate) const INVOCATION_ID: &str = "runner-test";
/// One wait for a state change polls this often...
const POLL: Duration = Duration::from_millis(20);
/// ...this many times (twenty seconds) before the test fails.
const POLLS: u32 = 1000;
/// How many polls (two seconds) a process an instance started gets to exit
/// on its own before it counts as left behind.
const LEAK_POLLS: u32 = 100;
/// Numbers the scratch roots of one test binary, so two instances of one
/// name never share a root.
static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

/// Directories and files under an instance's root.
const WORK: &str = "work";
const HOME: &str = "home";
const DSH_HOME: &str = "dsh";
const STATE: &str = "state";
const RUN: &str = "run";
const RELEASE: &str = "release";
const STUB_LOG: &str = "stub.log";
const JOURNAL: &str = "journal";
/// The file the stub waits for on a prompt that mentions "gate".
pub(crate) const GATE: &str = "gate";

/// The instance a test asks for: the profile word (`web`, `acp` or
/// `headless`), the app arguments after it as a user types them, and
/// variables recorded besides `PATH`, `HOME` and the stub's own.
pub(crate) struct Spec<'a> {
    pub(crate) profile: &'a str,
    pub(crate) args: &'a [&'a str],
    pub(crate) env: &'a [(&'a str, &'a str)],
}

/// One instance under its own root: the state root `state`, the runtime
/// root `run`, the DSH home `dsh`, the working directory `work`, the
/// release `release` and the stub's record `stub.log`. Dropping it removes
/// the root and fails the test when a process it started is still running;
/// a test binds its `Instance` before the `Running` and clients that use
/// it, so those are dropped, and their processes ended, first.
pub(crate) struct Instance {
    pub(crate) name: InstanceName,
    root: PathBuf,
}

impl Instance {
    /// Lays the instance out as `dshd run` does before it starts the unit:
    /// the profile in the DSH home, the release links and the record, built
    /// with the same launch and profile functions.
    pub(crate) fn create(name: &str, spec: &Spec<'_>) -> TestResult<Self> {
        let name = InstanceName::parse(name)?;
        let seq = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let scratch = std::env::temp_dir().join(format!(
            "dshd-{}-{name}-{}-{seq}",
            env!("CARGO_CRATE_NAME"),
            process::id()
        ));
        if scratch.exists() {
            fs::remove_dir_all(&scratch)?;
        }
        fs::create_dir_all(&scratch)?;
        // The runner finds its state root from its working directory, which
        // the kernel reports with every link resolved.
        let instance = Self {
            name,
            root: scratch.canonicalize()?,
        };
        instance.lay_out(spec)?;
        Ok(instance)
    }

    /// A path under the instance's root.
    pub(crate) fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// `<root>/state/dshd/<name>`.
    pub(crate) fn dir(&self) -> InstanceDir {
        StateRoot::new(self.path(STATE)).instance(&self.name)
    }

    /// `<root>/run/dshd/<name>`, the unit's runtime directory.
    pub(crate) fn runtime(&self) -> RuntimeDir {
        RuntimeDir::under(&self.path(RUN), &self.name)
    }

    /// `dshd-run VERB NAME` as the unit (`run` through `current`,
    /// `poststop` through `booted`) or the CLI (`attach` and `probe`
    /// through `booted`) invokes it, with the unit's working directory,
    /// runtime directory and invocation id, and nothing else inherited.
    pub(crate) fn runner(&self, verb: &str) -> Command {
        let dir = self.dir();
        let program = if verb == "run" {
            dir.current().join(RELEASE_RUNNER).into_os_string()
        } else {
            name::booted_runner(&dir)
        };
        let mut command = Command::new(program);
        command
            .args([verb, self.name.as_str()])
            .env_clear()
            .env("PATH", host_path())
            .env("HOME", self.path(HOME))
            .env("XDG_RUNTIME_DIR", self.path(RUN))
            .env("RUNTIME_DIRECTORY", self.runtime().path())
            .env("INVOCATION_ID", INVOCATION_ID)
            .current_dir(dir.path())
            .stdin(Stdio::null());
        command
    }

    /// Starts `dshd-run run NAME`, its stdout and stderr appended to the
    /// instance's journal file.
    pub(crate) fn start(&self) -> TestResult<Running> {
        self.launch(self.runner("run"))
    }

    /// Starts `command`, a `runner("run")` a test added to, as `start`
    /// does.
    pub(crate) fn launch(&self, mut command: Command) -> TestResult<Running> {
        let journal = self.path(JOURNAL);
        let file = File::options().create(true).append(true).open(&journal)?;
        let child = command.stdout(file.try_clone()?).stderr(file).spawn()?;
        Ok(Running {
            child: Some(child),
            journal,
        })
    }

    /// Everything the stub recorded so far.
    pub(crate) fn stub_log(&self) -> TestResult<String> {
        match fs::read_to_string(self.path(STUB_LOG)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            read => Ok(read?),
        }
    }

    fn lay_out(&self, spec: &Spec<'_>) -> TestResult {
        let release = self.path(RELEASE);
        for dir in [WORK, HOME, "release/bin", "release/share/dshd"] {
            fs::create_dir_all(self.path(dir))?;
        }
        fs::create_dir_all(self.runtime().path())?;
        symlink(STUB_DSH, release.join(RELEASE_DSH))?;
        symlink(BUILT_RUNNER, release.join(RELEASE_RUNNER))?;
        fs::copy(RELEASE_JSON, release.join(RELEASE_FILE))?;
        let release_file = release::validate(&release)?;

        let dsh_home = self.path(DSH_HOME);
        let profile_dir = dsh_home.join("profiles").join(spec.profile);
        fs::create_dir_all(&profile_dir)?;
        fs::write(profile_dir.join("package.json"), manifest(spec.profile)?)?;
        let dir = self.dir();
        fs::create_dir_all(dir.path())?;
        symlink(&release, dir.current())?;

        let mut typed = vec![OsString::from(spec.profile)];
        typed.extend(spec.args.iter().map(OsString::from));
        let selected = launch::selected_profile(&typed)?;
        let dsh = Tool::at("dsh", release.join(RELEASE_DSH));
        let transport = profile::transport(&dsh, dsh.command(), &dsh_home, &selected)?.transport;
        let mut variables = vec![
            (OsString::from("PATH"), host_path()),
            (OsString::from("HOME"), self.path(HOME).into_os_string()),
            (
                OsString::from("DSH_STUB_LOG"),
                self.path(STUB_LOG).into_os_string(),
            ),
            (
                OsString::from("DSH_STUB_GATE"),
                self.path(GATE).into_os_string(),
            ),
        ];
        variables.extend(
            spec.env
                .iter()
                .map(|(key, value)| (OsString::from(key), OsString::from(value))),
        );
        let record = InstanceRecord {
            id: instance::new_id()?,
            name: self.name.to_string(),
            release: RELEASE_NAME.to_owned(),
            cwd: OsText::new(self.path(WORK)),
            args: typed.into_iter().map(OsText::from).collect(),
            environment: launch::capture_environment(&variables, &release_file.unset_environment),
            profile: ProfileRef {
                name: selected,
                dsh_home: OsText::new(dsh_home),
                unrecognized: serde_json::Map::new(),
            },
            transport,
            labels: BTreeMap::new(),
            created_at: fsx::unix_now(),
            unrecognized: serde_json::Map::new(),
        };
        fsx::write_json_atomic(&dir.instance_json(), &record, RECORD_MODE)?;
        Ok(())
    }
}

impl Drop for Instance {
    // catches: a runner that exits while dsh or a dsh child still runs, or an attach client or runner a test left behind
    fn drop(&mut self) {
        let mut needle = b"HOME=".to_vec();
        needle.extend_from_slice(self.path(HOME).as_os_str().as_bytes());
        needle.push(0);
        let mut leaked = survivors(&needle);
        let mut polls = 0;
        while leaked.as_ref().is_ok_and(|left| !left.is_empty()) && polls < LEAK_POLLS {
            thread::park_timeout(POLL);
            leaked = survivors(&needle);
            polls += 1;
        }
        let leaked = leaked.unwrap_or_else(|error| {
            vec![(
                0,
                format!("/proc cannot be read, so nothing was checked: {error}"),
            )]
        });
        for (pid, _) in &leaked {
            if *pid != 0 {
                // The stub and its sleep exit on SIGTERM.
                drop(sys::kill(*pid, sys::SIGTERM));
            }
        }
        // A root that cannot go now is removed by the next run of the test.
        drop(fs::remove_dir_all(&self.root));
        assert!(
            leaked.is_empty() || thread::panicking(),
            "processes outlived {}: {leaked:?}",
            self.name
        );
    }
}

/// The processes other than this one whose environment holds `needle`
/// (one `NAME=VALUE` entry with its NUL), with their command lines.
fn survivors(needle: &[u8]) -> io::Result<Vec<(u32, String)>> {
    let me = process::id();
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        // A process that exited meanwhile, or another user's, is no survivor of ours.
        let Ok(environ) = fs::read(entry.path().join("environ")) else {
            continue;
        };
        if pid != me && environ.windows(needle.len()).any(|window| window == needle) {
            let cmdline = fs::read(entry.path().join("cmdline")).unwrap_or_default();
            found.push((
                pid,
                String::from_utf8_lossy(&cmdline)
                    .replace('\0', " ")
                    .trim_end()
                    .to_owned(),
            ));
        }
    }
    Ok(found)
}

/// A started runner. Dropping one that was not ended stops it, and kills
/// it when it does not stop.
pub(crate) struct Running {
    child: Option<Child>,
    journal: PathBuf,
}

/// How a runner ended: its exit code (none when killed by a signal) and
/// everything it and dsh wrote, as the journal would hold it.
pub(crate) struct Ended {
    pub(crate) status: Option<i32>,
    pub(crate) journal: String,
}

impl Running {
    /// Waits for the runner to exit, after a SIGTERM (`systemctl stop`)
    /// when `stop` is set.
    pub(crate) fn end(mut self, stop: bool) -> TestResult<Ended> {
        let child = self.child.as_mut().ok_or("the runner has already ended")?;
        if stop {
            sys::kill(child.id(), sys::SIGTERM)?;
        }
        let status = exit_status(child)?;
        self.child = None;
        Ok(Ended {
            status: status.code(),
            journal: fs::read_to_string(&self.journal)?,
        })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        // Asked first, the runner takes dsh down with it.
        let stopped = sys::kill(child.id(), sys::SIGTERM).is_ok() && exit_status(child).is_ok();
        if !stopped && child.kill().is_ok() {
            drop(child.wait());
        }
    }
}

/// Polls `ready` until it holds, failing the test after twenty seconds.
pub(crate) fn wait_for(what: &str, mut ready: impl FnMut() -> TestResult<bool>) -> TestResult {
    for _ in 0..POLLS {
        if ready()? {
            return Ok(());
        }
        thread::park_timeout(POLL);
    }
    Err(format!("gave up waiting for {what}").into())
}

/// The exit status of a child, once it has exited.
fn exit_status(child: &mut Child) -> TestResult<ExitStatus> {
    let mut exited = None;
    wait_for("the runner to exit", || {
        exited = child.try_wait()?;
        Ok(exited.is_some())
    })?;
    exited.ok_or_else(|| "the runner did not exit".into())
}

/// The test process's own `PATH`, for the utilities the stub uses.
fn host_path() -> OsString {
    std::env::var_os("PATH").unwrap_or_default()
}

/// A profile's `package.json` with DSH's real bundles for it: the base, and
/// the app that gives the profile its transport.
fn manifest(profile: &str) -> TestResult<Vec<u8>> {
    let app = match profile {
        "web" => "@deepseek-ai/dsh-web-app",
        "acp" => "@deepseek-ai/dsh-acp-app",
        "headless" => "@deepseek-ai/dsh-headless",
        other => return Err(format!("no manifest for the profile {other}").into()),
    };
    let bundles = ["@deepseek-ai/dsh-base", app];
    Ok(serde_json::to_vec(
        &json!({ "name": profile, "dsh": { "profile": { "bundles": bundles } } }),
    )?)
}
