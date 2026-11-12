//! The runner: the main process of `dshd@<name>.service`. `run` loads the
//! record, checks cwd and release, composes the environment, checks that
//! the live profile still selects the recorded transport, spawns `dsh`
//! with pipes, reports readiness over `sd_notify`, supervises the
//! child on its main thread (stop signals and reaping, see `signal`) and
//! picks one exit status. `poststop` completes `last-run.json` from what
//! systemd hands `ExecStopPost` and the totals of the unit's cgroup.
//!
//! The unit's working directory is the instance directory and systemd
//! names the runtime directory in `$RUNTIME_DIRECTORY`; the runner derives
//! nothing else. Besides `last-run.json` and the `booted` link it reads
//! two per-run files in the runtime directory, which dies with the run:
//! `child-end.json`, which the runner leaves for `poststop`, and the CLI's
//! `stop-intent`. Every line of a run in the journal comes after
//! `RUN_STARTED`; the last one is `poststop`'s `RUN_ENDED`.

mod cgroup;
mod completion;

pub mod exit;
pub mod notify;
pub mod signal;
pub mod web;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::name::{InstanceDir, InstanceName, RELEASE_DSH, RuntimeDir, StateRoot};
use crate::record::{ChildEnd, InstanceRecord, LastRun, OsText, Transport};
use crate::relay::Relay;
use crate::tool::Tool;
use crate::{fsx, instance, launch, out, profile, release, sys, systemctl};

use self::notify::Notify;

/// Exit status that asks systemd to restart (`RestartForceExitStatus=75`).
pub const EXIT_TEMPFAIL: u8 = 75;
/// Exit status for a configuration or record error: reported, never retried.
pub const EXIT_CONFIG: u8 = 78;
/// The first line of every run in the journal, before anything else the
/// runner or dsh writes; output that does not start with it is incomplete.
pub const RUN_STARTED: &str = "dshd-run: run started";
/// The start of the last line of every run in the journal, which `poststop`
/// writes as `RUN_ENDED: <SERVICE_RESULT>` after it recorded the run or
/// reported why it could not.
pub const RUN_ENDED: &str = "dshd-run: run ended";

/// The variable systemd sets to the unit's `RuntimeDirectory`.
const RUNTIME_DIRECTORY: &str = "RUNTIME_DIRECTORY";
/// The variable systemd sets to the unit's invocation id, for `run` and
/// `poststop` alike.
const INVOCATION_ID: &str = "INVOCATION_ID";
/// What systemd hands `ExecStopPost` about the run's end.
const SERVICE_RESULT: &str = "SERVICE_RESULT";
const EXIT_CODE: &str = "EXIT_CODE";
const EXIT_STATUS: &str = "EXIT_STATUS";
/// Per-run file: what the runner saw of the run's end.
const CHILD_END_FILE: &str = "child-end.json";
/// Mode of every file the runner writes: records may hold environment values.
const PRIVATE_MODE: u32 = 0o600;
/// Name of the thread that pumps a web instance's stdout.
const WEB_THREAD: &str = "dshd-web";
/// How long the runner waits, once `dsh` has exited, for the stdout pump to
/// finish. The pipe normally closes with `dsh`; a process `dsh` started can
/// keep it open, and must not keep the unit from ending.
const OUTPUT_DRAIN: Duration = Duration::from_secs(1);
/// The status DSH's restart button exits with.
const DSH_RESTART: i32 = 42;

/// `dshd-run run NAME`: the unit's main process until `dsh` ends.
#[must_use]
pub fn run(name: &str) -> u8 {
    out::error_line(RUN_STARTED);
    // Before any thread exists, so every thread inherits the mask and only
    // the main thread's sigwait receives a stop or the child's end.
    if let Err(error) = sys::block_signals(signal::RUNNER_SIGNALS) {
        log_line(&error.to_string());
        return EXIT_CONFIG;
    }
    match Launch::prepare(name) {
        Ok(launch) => launch.run(),
        Err(error) => {
            log_line(&error.to_string());
            EXIT_CONFIG
        }
    }
}

/// `dshd-run poststop NAME`: completes `last-run.json` from
/// `$SERVICE_RESULT`, `$EXIT_CODE`, `$EXIT_STATUS`, the child's end the
/// runner left, the CLI's stop intent and the run's cgroup totals, clears
/// the per-run files, then
/// logs [`RUN_ENDED`]. Exits non-zero when the run cannot be recorded.
#[must_use]
pub fn poststop(name: &str) -> u8 {
    let status = match Place::from_process(name) {
        Ok(place) => completion::record_and_clear(&place),
        Err(error) => {
            log_line(&error.to_string());
            EXIT_CONFIG
        }
    };
    out::error_line(&format!("{RUN_ENDED}: {}", unit_value(SERVICE_RESULT)));
    status
}

/// One line to the journal (stderr of the unit), prefixed `dshd-run: `.
pub fn log_line(text: &str) {
    out::error_line(&format!("dshd-run: {text}"));
}

/// Reports readiness: `READY=1` and the status line to the service manager
/// when there is one, then the flag the exit table reads.
fn mark_ready(notify: Option<&Notify>, ready: &AtomicBool, status: Option<&str>) {
    if let Some(notify) = notify {
        notify.ready();
        if let Some(status) = status {
            notify.status(status);
        }
    }
    ready.store(true, Ordering::SeqCst);
}

/// Spawns a detached, named helper thread.
fn spawn_thread(name: &'static str, body: impl FnOnce() + Send + 'static) -> Result<(), Error> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(body)
        .map(|_detached| ())
        .map_err(|source| Error::io("spawn thread", name, source))
}

/// Where one instance lives, as the unit hands it to the runner.
struct Place {
    name: InstanceName,
    root: StateRoot,
    dir: InstanceDir,
    runtime: RuntimeDir,
}

impl Place {
    /// The working directory must be `<state root>/dshd/<name>`
    /// (`WorkingDirectory=%S/dshd/%i`), and `$RUNTIME_DIRECTORY` absolute.
    fn from_process(raw: &str) -> Result<Self, Error> {
        let name = InstanceName::parse(raw)?;
        let cwd = std::env::current_dir()
            .map_err(|source| Error::io("read", "the working directory", source))?;
        let root = state_root_of(&cwd, &name).ok_or_else(|| {
            Error::InvalidArgument(format!(
                "the working directory {} is not the state directory of {name}",
                cwd.display()
            ))
        })?;
        let runtime = std::env::var_os(RUNTIME_DIRECTORY)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute());
        let runtime = runtime.ok_or_else(|| {
            Error::InvalidArgument(format!(
                "{RUNTIME_DIRECTORY} is not set; dshd-run runs inside {}",
                name.unit()
            ))
        })?;
        Ok(Self {
            dir: root.instance(&name),
            root,
            name,
            runtime: RuntimeDir::new(runtime),
        })
    }

    fn runtime_file(&self, file: &str) -> PathBuf {
        self.runtime.path().join(file)
    }
}

/// The state root whose instance directory for `name` is `cwd`.
fn state_root_of(cwd: &Path, name: &InstanceName) -> Option<StateRoot> {
    let root = StateRoot::new(cwd.parent()?.parent()?.to_path_buf());
    (root.instance(name).path() == cwd).then_some(root)
}

/// Everything resolved and checked before `dsh` is spawned.
struct Launch {
    place: Place,
    record: InstanceRecord,
    /// The release store path `current` names and `booted` now names.
    release: PathBuf,
    /// The recorded cwd, verified to be a directory.
    cwd: PathBuf,
    /// The composed environment `dsh` gets, and nothing else.
    environment: Vec<(OsString, OsString)>,
    /// The recorded argv, with the pinned web port applied.
    args: Vec<OsString>,
    /// A web instance with no pinned port yet: pin the announced one.
    pin_port: bool,
}

impl Launch {
    /// Checks the record, cwd, release and the live profile's transport,
    /// refusing before `dsh` is spawned; then repoints `booted` and writes
    /// the start half of `last-run.json` (`end` null), which is what a
    /// crash leaves behind for `interrupted`.
    fn prepare(raw: &str) -> Result<Self, Error> {
        let place = Place::from_process(raw)?;
        let started_at = fsx::unix_now();
        let record = instance::load_strict(&place.root, &place.name)?.record;
        let cwd = fsx::canonical_dir(Path::new(record.cwd.as_os_str()))?;
        let release = fsx::read_link(&place.dir.current())?
            .ok_or_else(|| inconsistent(&place.name, "the current release link is missing"))?;
        let release_file = release::validate(&release)?;
        let runner_environment: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        let environment = launch::compose_environment(
            &record.environment,
            &runner_environment,
            &release_file,
            Path::new(record.profile.dsh_home.as_os_str()),
            &cwd,
            &runner_environment,
        );
        let dsh = release.join(RELEASE_DSH);
        check_transport(
            &place.name,
            &record,
            &dsh,
            dsh_command(&dsh, &environment, &cwd),
        )?;
        let (args, pin_port) = launch_args(&place.dir, &record)?;
        fsx::symlink_replace(&release, &place.dir.booted())?;
        let start = LastRun {
            id: record.id.clone(),
            release_path: OsText::new(release.clone()),
            boot_id: systemctl::boot_id()?,
            started_at,
            invocation_id: unit_value(INVOCATION_ID),
            end: None,
            unrecognized: serde_json::Map::new(),
        };
        fsx::write_json_atomic(&place.dir.last_run_json(), &start, PRIVATE_MODE)?;
        Ok(Self {
            place,
            record,
            release,
            cwd,
            environment,
            args,
            pin_port,
        })
    }

    /// Spawns `dsh`, serves its transport, supervises it and returns the
    /// runner's exit status. A transport that cannot be served, at once or
    /// when the web announcement cannot be recorded, stops dsh and is a
    /// configuration error, whatever dsh's own status.
    fn run(self) -> u8 {
        let mut child = match self.spawn() {
            Ok(child) => child,
            Err(error) => {
                log_line(&error.to_string());
                return EXIT_CONFIG;
            }
        };
        let pid = child.id();
        log_line(&format!(
            "started dsh as pid {pid} from {}",
            self.release.display()
        ));
        let notify = Notify::from_env().map(Arc::new);
        let ready = Arc::new(AtomicBool::new(false));
        let stop_asked = Arc::new(AtomicBool::new(false));
        let serving = self.serve(&mut child, notify.clone(), &ready, &stop_asked);
        if let Err(error) = &serving {
            log_line(&error.to_string());
            terminate(pid);
        }
        let (end, stop_signal) = if let Ok(Serving::Acp { relay }) = &serving {
            signal::supervise(&mut child, &stop_asked, || {
                if !relay.stop_child() {
                    terminate(pid);
                }
            })
        } else {
            signal::supervise(&mut child, &stop_asked, || terminate(pid))
        };
        let seen = RunnerEnd {
            child: end,
            stop_signal,
        };
        if let Err(error) = fsx::write_json_atomic(
            &self.place.runtime_file(CHILD_END_FILE),
            &seen,
            PRIVATE_MODE,
        ) {
            log_line(&error.to_string());
        }
        let served = serving.map(Serving::finish);
        if let Some(notify) = &notify {
            notify.stopping();
        }
        let status = if served.is_err() || stop_asked.load(Ordering::SeqCst) {
            EXIT_CONFIG
        } else {
            exit::exit_status(
                self.record.transport,
                ready.load(Ordering::SeqCst),
                stop_signal.is_some(),
                end,
            )
        };
        log_line(&ending(end, status));
        status
    }

    /// `<release>/bin/dsh` with the recorded argv, stdio per transport.
    fn spawn(&self) -> Result<Child, Error> {
        let dsh = self.release.join(RELEASE_DSH);
        let (stdin, stdout) = match self.record.transport {
            Transport::Web => (Stdio::null(), Stdio::piped()),
            Transport::Acp => (Stdio::piped(), Stdio::piped()),
            Transport::None => (Stdio::null(), Stdio::inherit()),
        };
        let mut command = dsh_command(&dsh, &self.environment, &self.cwd);
        command.args(&self.args);
        command.stdin(stdin).stdout(stdout).stderr(Stdio::inherit());
        command
            .spawn()
            .map_err(|source| Error::io("start", dsh, source))
    }

    /// Starts what the transport needs beside the child and reports
    /// readiness where it is reached at once (acp: the socket is bound;
    /// none: `dsh` is spawned). Web readiness waits for the announcement,
    /// and a web pump that cannot record it sets `stop_asked`.
    fn serve(
        &self,
        child: &mut Child,
        notify: Option<Arc<Notify>>,
        ready: &Arc<AtomicBool>,
        stop_asked: &Arc<AtomicBool>,
    ) -> Result<Serving, Error> {
        match self.record.transport {
            Transport::Web => {
                let stdout = pipe(child.stdout.take(), "stdout")?;
                let pump = web::Pump {
                    runtime: self.place.runtime.clone(),
                    dir: self.place.dir.clone(),
                    pin_port: self.pin_port,
                    notify,
                    ready: Arc::clone(ready),
                    stop_asked: Arc::clone(stop_asked),
                };
                let (done, pump_done) = mpsc::channel::<()>();
                spawn_thread(WEB_THREAD, move || {
                    pump.run(stdout);
                    drop(done);
                })?;
                Ok(Serving::Web { pump_done })
            }
            Transport::Acp => {
                let stdin = pipe(child.stdin.take(), "stdin")?;
                let stdout = pipe(child.stdout.take(), "stdout")?;
                let socket = self.place.runtime.acp_socket();
                let relay = Relay::start(&socket, stdin, stdout)?;
                let status = format!("acp {}", socket.display());
                mark_ready(notify.as_deref(), ready, Some(status.as_str()));
                Ok(Serving::Acp { relay })
            }
            Transport::None => {
                mark_ready(notify.as_deref(), ready, None);
                Ok(Serving::Plain)
            }
        }
    }
}

/// What a transport keeps running beside the child until it ends.
enum Serving {
    /// The stdout pump, which drops its sender when `dsh`'s stdout closes.
    Web { pump_done: Receiver<()> },
    /// The relay. A stop signal stops dsh through it (`Relay::stop_child`);
    /// it is wound down only after dsh was reaped, so dsh's last frames
    /// still reach the controller.
    Acp { relay: Relay },
    /// Nothing: `dsh` writes to the journal itself.
    Plain,
}

impl Serving {
    /// Winds the transport down after the child ended.
    fn finish(self) {
        match self {
            Self::Web { pump_done } => {
                if let Err(RecvTimeoutError::Timeout) = pump_done.recv_timeout(OUTPUT_DRAIN) {
                    log_line(
                        "dsh's stdout is still held open by a process it started; not waiting for it",
                    );
                }
            }
            Self::Acp { relay } => {
                relay.child_exited();
                relay.shutdown();
            }
            Self::Plain => {}
        }
    }
}

/// What the runner saw of the run's end, left in `child-end.json` for
/// `poststop`: how dsh ended and the stop signal that arrived, if any.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunnerEnd {
    child: ChildEnd,
    stop_signal: Option<i32>,
}

/// The web and headless stop: SIGTERM to dsh. Called on the main thread
/// only, before dsh is reaped.
fn terminate(pid: u32) {
    if let Err(error) = sys::kill(pid, sys::SIGTERM) {
        log_line(&format!("cannot stop dsh: {error}"));
    }
}

/// A pipe the spawn configured, taken out of the child.
fn pipe<T>(pipe: Option<T>, stream: &str) -> Result<T, Error> {
    pipe.ok_or_else(|| Error::InvalidArgument(format!("dsh was spawned without a {stream} pipe")))
}

/// The one line the runner logs when `dsh` ended.
fn ending(end: ChildEnd, status: u8) -> String {
    if status == EXIT_TEMPFAIL && end.exit_code == Some(DSH_RESTART) {
        return format!(
            "dsh asked to be restarted (status {DSH_RESTART}); exiting {status} so systemd restarts the instance"
        );
    }
    let how = match (end.exit_code, end.signal) {
        (Some(code), _) => format!("dsh exited with status {code}"),
        (None, Some(signal)) => format!("dsh was killed by signal {signal}"),
        (None, None) => "dsh ended without a status".to_owned(),
    };
    if status == EXIT_TEMPFAIL {
        format!("{how} after it was ready; exiting {status} so systemd restarts the instance")
    } else {
        format!("{how}; exiting {status}")
    }
}

/// The recorded argv, with the pinned port applied to a web instance, and
/// whether the first announcement should pin one.
fn launch_args(dir: &InstanceDir, record: &InstanceRecord) -> Result<(Vec<OsString>, bool), Error> {
    let mut args: Vec<OsString> = record
        .args
        .iter()
        .map(|arg| arg.as_os_str().to_owned())
        .collect();
    if record.transport != Transport::Web {
        return Ok((args, false));
    }
    let pinned = instance::read_web_port(dir)?;
    let injected = launch::inject_web_args(&mut args, pinned);
    Ok((args, injected && pinned.is_none()))
}

/// `dsh` with the composed environment and nothing inherited, in the
/// recorded cwd, starting with no blocked signals. The caller adds the
/// arguments and stdio.
fn dsh_command(dsh: &Path, environment: &[(OsString, OsString)], cwd: &Path) -> Command {
    let mut command = Command::new(dsh);
    command
        .env_clear()
        .envs(environment.iter().cloned())
        .current_dir(cwd);
    sys::unmask_on_exec(&mut command);
    command
}

/// The live profile must still select the recorded transport, since the
/// runner serves that one and dsh boots the profile as it is now. A
/// profile that is gone is materialised by dsh through `materialise` first,
/// as dsh's own boot would.
fn check_transport(
    name: &InstanceName,
    record: &InstanceRecord,
    dsh: &Path,
    materialise: Command,
) -> Result<(), Error> {
    let profile = &record.profile;
    let dsh_home = Path::new(profile.dsh_home.as_os_str());
    let live = profile::transport(&Tool::at("dsh", dsh), materialise, dsh_home, &profile.name)?;
    if live == record.transport {
        return Ok(());
    }
    Err(Error::InvalidArgument(format!(
        "profile {} in {} now selects transport {}, and {name} was created for {}: \
         restore the profile, or remove the instance with dshd rm {name} and run it again",
        profile.name,
        dsh_home.display(),
        live.slug(),
        record.transport.slug(),
    )))
}

fn inconsistent(name: &InstanceName, reason: &str) -> Error {
    Error::InconsistentState {
        name: name.to_string(),
        reason: reason.to_owned(),
    }
}

/// A value systemd hands the unit's processes; empty when it set none.
fn unit_value(name: &str) -> String {
    match std::env::var_os(name).map(OsString::into_string) {
        Some(Ok(value)) => value,
        Some(Err(_)) | None => String::new(),
    }
}

#[cfg(test)]
#[path = "runner/unit_tests.rs"]
mod tests;
