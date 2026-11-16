//! An in-process model of launchd's `gui/<uid>` domain for the unit tests:
//! a [`CommandRunner`] that records every `launchctl` argv and answers
//! `bootstrap`, `bootout`, `print` and `print-disabled` the way launchctl
//! does, in the text shape of `tests/fixtures/launchd/print/`. Any other
//! verb (`kickstart`, `enable`, `disable`, `load`...) is recorded and
//! answered with success, so a test can assert dshd never used it.
//!
//! The model: `bootstrap` reads the plist dshd wrote, refuses a label it
//! already holds (exit 5), spawns one process (a fresh pid) and calls the
//! test's `on_bootstrap` hook, which stands in for the runner (it may
//! write `run/state.json`). `bootout` of a held job removes it at once, or
//! after `teardown_prints` more `print` calls show it still there.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::error::{Error, ToolFailure};
use crate::platform::launchd::command::CommandRunner;
use crate::platform::launchd::exit::{
    DOMAIN_REFUSES, IO_ERROR, NO_DOMAIN, NO_SERVICE, NO_SUCH_PROCESS,
};
use crate::tool::Captured;

/// The first pid the fake hands out.
const FIRST_PID: u32 = 4242;

/// One job the fake launchd holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FakeJob {
    /// The plist it was bootstrapped from.
    pub(crate) plist: PathBuf,
    /// Its process, while one runs.
    pub(crate) pid: Option<u32>,
    /// How many times it was spawned.
    pub(crate) runs: u32,
    /// The last exit status, once a process ended.
    pub(crate) last_exit: Option<i32>,
    /// After `bootout`: how many more `print` calls still show it.
    pub(crate) tearing_down: Option<u32>,
}

/// What a hook is told.
#[derive(Debug)]
pub(crate) struct HookEvent<'a> {
    /// The job's label.
    pub(crate) label: &'a str,
    /// The process the fake spawned, or the one `bootout` ends.
    pub(crate) pid: Option<u32>,
}

/// A test's stand-in for what the runner does when launchd acts.
pub(crate) type Hook = Arc<dyn Fn(&HookEvent<'_>) + Send + Sync>;

/// The fake's whole state; tests read and preset it through
/// [`FakeLaunchctl::state`].
#[derive(Default)]
pub(crate) struct FakeState {
    /// Every argv, in order, failing calls included.
    pub(crate) calls: Vec<Vec<String>>,
    /// Jobs by label.
    pub(crate) jobs: BTreeMap<String, FakeJob>,
    /// `print-disabled`: label to disabled.
    pub(crate) disabled: BTreeMap<String, bool>,
    /// No GUI login session: every domain call fails as launchctl does.
    pub(crate) no_gui_domain: bool,
    /// The text of every plist `bootstrap` loaded, in order.
    pub(crate) received: Vec<String>,
    /// How many `print` calls a booted-out job stays visible for.
    pub(crate) teardown_prints: u32,
    /// The next `bootstrap` fails with this exit status and stderr.
    pub(crate) bootstrap_failure: Option<(i32, String)>,
    /// A spawned process ends at once with this status (a runner that
    /// failed before readiness).
    pub(crate) exits_at_once: Option<i32>,
    /// Called after a successful `bootstrap`.
    pub(crate) on_bootstrap: Option<Hook>,
    /// Called after a successful `bootout`.
    pub(crate) on_bootout: Option<Hook>,
    next_pid: u32,
}

impl fmt::Debug for FakeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeState")
            .field("calls", &self.calls)
            .field("jobs", &self.jobs)
            .field("disabled", &self.disabled)
            .field("no_gui_domain", &self.no_gui_domain)
            .finish_non_exhaustive()
    }
}

/// A shared handle: clones see the same launchd, so a test keeps one
/// while the manager owns another.
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeLaunchctl {
    state: Arc<Mutex<FakeState>>,
}

impl FakeLaunchctl {
    /// The state, locked.
    pub(crate) fn state(&self) -> MutexGuard<'_, FakeState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Every argv so far.
    pub(crate) fn calls(&self) -> Vec<Vec<String>> {
        self.state().calls.clone()
    }

    /// The first word of every call so far.
    pub(crate) fn verbs(&self) -> Vec<String> {
        self.calls()
            .iter()
            .map(|call| call.first().cloned().unwrap_or_default())
            .collect()
    }

    /// Presets a job as `bootstrap` would have left it.
    pub(crate) fn hold(&self, label: &str, plist: &str, pid: Option<u32>, last_exit: Option<i32>) {
        self.state().jobs.insert(
            label.to_owned(),
            FakeJob {
                plist: PathBuf::from(plist),
                pid,
                runs: 1,
                last_exit,
                tearing_down: None,
            },
        );
    }

    fn answer(&self, args: &[String]) -> Result<String, (i32, String)> {
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        match words.as_slice() {
            ["bootstrap", domain, plist] => self.bootstrap(domain, plist),
            ["bootout", target] => self.bootout(target),
            ["print", target] if target.matches('/').count() == 2 => self.print_job(target),
            ["print", domain] => self.print_domain(domain),
            ["print-disabled", domain] => self.print_disabled(domain),
            [verb, ..] if !verb.starts_with('-') => Ok(String::new()),
            _ => Err((64, "Usage: launchctl <subcommand> ...".to_owned())),
        }
    }

    fn bootstrap(&self, domain: &str, plist: &str) -> Result<String, (i32, String)> {
        let text = std::fs::read_to_string(plist).map_err(|_| {
            (
                IO_ERROR,
                "Bootstrap failed: 5: Input/output error".to_owned(),
            )
        })?;
        let label = label_of(&text).ok_or((
            IO_ERROR,
            "Bootstrap failed: 5: Input/output error".to_owned(),
        ))?;
        let (hook, pid) = {
            let mut state = self.state();
            if state.no_gui_domain || !domain.starts_with("gui/") {
                return Err((
                    DOMAIN_REFUSES,
                    "Bootstrap failed: 125: Domain does not support specified action".to_owned(),
                ));
            }
            if let Some(failure) = state.bootstrap_failure.take() {
                return Err(failure);
            }
            if state.jobs.contains_key(&label) {
                return Err((
                    IO_ERROR,
                    "Bootstrap failed: 5: Input/output error".to_owned(),
                ));
            }
            if state.next_pid == 0 {
                state.next_pid = FIRST_PID;
            }
            let pid = state.next_pid;
            state.next_pid += 1;
            let exited = state.exits_at_once;
            state.jobs.insert(
                label.clone(),
                FakeJob {
                    plist: PathBuf::from(plist),
                    pid: if exited.is_some() { None } else { Some(pid) },
                    runs: 1,
                    last_exit: exited,
                    tearing_down: None,
                },
            );
            state.received.push(text);
            (state.on_bootstrap.clone(), pid)
        };
        if let Some(hook) = hook {
            hook(&HookEvent {
                label: &label,
                pid: Some(pid),
            });
        }
        Ok(String::new())
    }

    fn bootout(&self, target: &str) -> Result<String, (i32, String)> {
        let label = label_of_target(target);
        let (hook, pid) = {
            let mut state = self.state();
            if state.no_gui_domain {
                return Err(no_domain());
            }
            let teardown = state.teardown_prints;
            let Some(job) = state.jobs.get_mut(label) else {
                return Err((
                    NO_SUCH_PROCESS,
                    "Boot-out failed: 3: No such process".to_owned(),
                ));
            };
            let pid = job.pid;
            if teardown == 0 {
                state.jobs.remove(label);
            } else {
                job.tearing_down = Some(teardown);
            }
            (state.on_bootout.clone(), pid)
        };
        if let Some(hook) = hook {
            hook(&HookEvent { label, pid });
        }
        Ok(String::new())
    }

    fn print_job(&self, target: &str) -> Result<String, (i32, String)> {
        let label = label_of_target(target);
        let mut state = self.state();
        if state.no_gui_domain {
            return Err(no_domain());
        }
        let missing = || {
            (
                NO_SERVICE,
                format!("Could not find service \"{label}\" in domain for user gui"),
            )
        };
        let Some(job) = state.jobs.get_mut(label) else {
            return Err(missing());
        };
        if let Some(left) = job.tearing_down {
            if left == 0 {
                state.jobs.remove(label);
                return Err(missing());
            }
            job.tearing_down = Some(left - 1);
        }
        Ok(job_text(target, job))
    }

    fn print_domain(&self, domain: &str) -> Result<String, (i32, String)> {
        let state = self.state();
        if state.no_gui_domain {
            return Err(no_domain());
        }
        let mut text = format!("{domain} = {{\n\ttype = gui\n\n\tservices = {{\n");
        for (label, job) in &state.jobs {
            let pid = job.pid.unwrap_or(0);
            let last = job
                .last_exit
                .map_or_else(|| "-".to_owned(), |code| code.to_string());
            writeln!(text, "\t\t{pid:>8} {last:>6}     \t{label}").unwrap();
        }
        text.push_str("\t}\n\n\tproperties = \n}\n");
        Ok(text)
    }

    fn print_disabled(&self, _domain: &str) -> Result<String, (i32, String)> {
        let state = self.state();
        if state.no_gui_domain {
            return Err(no_domain());
        }
        let mut text = "disabled services = {\n".to_owned();
        for (label, disabled) in &state.disabled {
            let word = if *disabled { "disabled" } else { "enabled" };
            writeln!(text, "\t\"{label}\" => {word}").unwrap();
        }
        text.push_str("}\n");
        Ok(text)
    }
}

impl CommandRunner for FakeLaunchctl {
    fn name(&self) -> &'static str {
        "launchctl"
    }

    fn run(&self, args: &[String]) -> Result<Captured, Error> {
        self.state().calls.push(args.to_vec());
        match self.answer(args) {
            Ok(stdout) => Ok(Captured {
                stdout: stdout.into_bytes(),
                stderr: Vec::new(),
            }),
            Err((status, stderr)) => Err(Error::Tool(Box::new(ToolFailure {
                tool: "launchctl",
                args: args.to_vec(),
                status: Some(status),
                stderr,
            }))),
        }
    }
}

fn no_domain() -> (i32, String) {
    (
        NO_DOMAIN,
        "Bad request.\nCould not find domain for port".to_owned(),
    )
}

/// The label of `gui/<uid>/LABEL`.
fn label_of_target(target: &str) -> &str {
    target.rsplit('/').next().unwrap_or(target)
}

/// The `Label` string of a plist, found as launchd would.
fn label_of(text: &str) -> Option<String> {
    let after = text.split("<key>Label</key>").nth(1)?;
    let value = after.split("<string>").nth(1)?.split("</string>").next()?;
    Some(value.to_owned())
}

/// A job in `launchctl print` shape.
fn job_text(target: &str, job: &FakeJob) -> String {
    let (active, state) = if job.pid.is_some() {
        (1, "running")
    } else {
        (0, "not running")
    };
    let mut text = format!(
        "{target} = {{\n\tactive count = {active}\n\tpath = {}\n\ttype = LaunchAgent\n\tstate = {state}\n\n\truns = {}\n",
        job.plist.display(),
        job.runs
    );
    if let Some(pid) = job.pid {
        writeln!(text, "\tpid = {pid}").unwrap();
    }
    match job.last_exit {
        Some(code) => writeln!(text, "\tlast exit code = {code}").unwrap(),
        None => text.push_str("\tlast exit code = (never exited)\n"),
    }
    text.push_str("}\n");
    text
}
