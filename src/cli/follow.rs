//! Following one run: `journalctl --user _SYSTEMD_INVOCATION_ID=ID -f`
//! read on a thread, printed or not, until the runner's last line or a
//! deadline; then one look at the unit decides whether the run ended,
//! and `last-run.json` says how. `run`, `wait` and `read --follow` share it.

use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use super::lifecycle;
use super::status;
use crate::cli::Context;
use crate::cli::report::{OutcomeSlug, RunExit};
use crate::error::{CHECK_IN, Error};
use crate::instance::{self, Instance};
use crate::name::InstanceName;
use crate::out;
use crate::record::LastRun;
use crate::runner::RUN_ENDED;
use crate::systemctl::UnitFacts;

/// How a followed run concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Concluded {
    /// `ended` or `still-running`.
    pub(super) outcome: OutcomeSlug,
    /// The exit code and why.
    pub(super) exit: RunExit,
}

/// Which lines of the journal to follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Since {
    /// The whole run, from its first line.
    Start,
    /// Lines written from now on.
    Now,
}

/// Follows the instance's current run, printing its lines when `print`,
/// until the run ends or `timeout` seconds pass, and concludes. A run that
/// is no longer up when `print` is off concludes at once.
pub(super) fn until_end(
    ctx: &Context,
    instance: &Instance,
    print: bool,
    timeout: u64,
) -> Result<Concluded, Error> {
    let name = &instance.name;
    let facts = ctx.systemctl.show(&name.unit())?;
    let Some(followed) = invocation(&facts, instance)? else {
        return Err(Error::NotRunning(name.to_string()));
    };
    let finished = if print || status::unit_up(&facts) {
        let deadline = Instant::now() + Duration::from_secs(timeout);
        journal(
            ctx,
            instance,
            &followed,
            print,
            Since::Start,
            Some(deadline),
        )?
    } else {
        JournalEnd::Recorded
    };
    let facts = lifecycle::settle(ctx, &name.unit())?;
    if status::unit_up(&facts) {
        let reason = check_in_reason(name, &followed, &facts, timeout, finished);
        return Ok(Concluded {
            outcome: OutcomeSlug::StillRunning,
            exit: RunExit {
                exit_code: CHECK_IN,
                signal: None,
                reason,
            },
        });
    }
    let Some(run) = instance::read_last_run(&instance.dir)? else {
        return Err(Error::NotRunning(name.to_string()));
    };
    Ok(Concluded {
        outcome: OutcomeSlug::Ended,
        exit: run_exit(name, &run),
    })
}

/// Why a journal follower stopped, independently of the instance's current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JournalEnd {
    /// The matching end was recorded, or the manager observes the unit down.
    Recorded,
    /// Another invocation replaced the run being followed.
    Replaced,
    /// The actual deadline passed.
    Timeout,
    /// The journal process closed its output before an authenticated end.
    Closed,
}

fn check_in_reason(
    name: &InstanceName,
    followed: &str,
    facts: &UnitFacts,
    timeout: u64,
    end: JournalEnd,
) -> String {
    if !facts.invocation_id.is_empty() && facts.invocation_id != followed {
        return format!(
            "the followed run ended and systemd restarted {name} as invocation {} (restart {}); the instance continues",
            facts.invocation_id, facts.restarts
        );
    }
    match end {
        JournalEnd::Timeout => format!("still running after {timeout} s; the instance continues"),
        JournalEnd::Closed => {
            "the journal follow ended while the instance is still running; inspect its journal"
                .to_owned()
        }
        JournalEnd::Recorded | JournalEnd::Replaced => {
            "the followed run ended; the instance is changing state".to_owned()
        }
    }
}

/// The invocation a verb reads: the unit's current one, else the one the
/// last run recorded. `None` before the first run.
pub(super) fn invocation(facts: &UnitFacts, instance: &Instance) -> Result<Option<String>, Error> {
    if !facts.invocation_id.is_empty() {
        return Ok(Some(facts.invocation_id.clone()));
    }
    let recorded = instance::read_last_run(&instance.dir)?.map(|run| run.invocation_id);
    Ok(recorded.filter(|id| !id.is_empty()))
}

/// Follows the journal of invocation `id` until the runner's last line,
/// the end of journalctl's output, or `deadline`. Lines are printed on
/// stdout when `print`. journalctl is killed before this returns.
pub(super) fn journal(
    ctx: &Context,
    instance: &Instance,
    id: &str,
    print: bool,
    since: Since,
    deadline: Option<Instant>,
) -> Result<JournalEnd, Error> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Ok(JournalEnd::Timeout);
    }
    let filter = format!("_SYSTEMD_INVOCATION_ID={id}");
    let mut command = ctx.journalctl.command();
    command.args(["--user", filter.as_str(), "-f"]);
    match since {
        Since::Start => command.arg("--no-tail"),
        Since::Now => command.args(["-n", "0"]),
    };
    command
        .args(["-o", "cat"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = command
        .spawn()
        .map_err(|error| ctx.journalctl.spawn_error(error))?;
    let lines = match child.stdout.take() {
        Some(stdout) => read_lines(stdout),
        None => Err(Error::io(
            "read",
            "the journal",
            std::io::Error::from(std::io::ErrorKind::BrokenPipe),
        )),
    };
    let pumped =
        lines.and_then(|lines| pump(&lines, print, deadline, || recorded_end(ctx, instance, id)));
    // journalctl -f never ends by itself; whether it ended already does not matter.
    drop(child.kill());
    drop(child.wait());
    pumped
}

/// The marker is only a wake-up: child output can contain the same text.
/// Poststop writes the matching end before its own marker; after a restart
/// the manager's invocation changes even if that record was overwritten.
fn recorded_end(ctx: &Context, instance: &Instance, id: &str) -> Result<Option<JournalEnd>, Error> {
    if instance::read_last_run(&instance.dir)?
        .is_some_and(|run| run.invocation_id == id && run.end.is_some())
    {
        return Ok(Some(JournalEnd::Recorded));
    }
    let facts = ctx.systemctl.show(&instance.name.unit())?;
    if !facts.invocation_id.is_empty() && facts.invocation_id != id {
        Ok(Some(JournalEnd::Replaced))
    } else {
        Ok((!status::unit_up(&facts)).then_some(JournalEnd::Recorded))
    }
}

/// Reads `stdout` line by line on a thread. Bytes that are not UTF-8 are
/// replaced, as the journal shows them.
fn read_lines(stdout: impl std::io::Read + Send + 'static) -> Result<Receiver<String>, Error> {
    let (sender, receiver) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("journal".to_owned())
        .spawn(move || {
            for line in BufReader::new(stdout).split(b'\n') {
                let Ok(line) = line else {
                    break;
                };
                if sender
                    .send(String::from_utf8_lossy(&line).into_owned())
                    .is_err()
                {
                    break;
                }
            }
        });
    match spawned {
        Ok(_) => Ok(receiver),
        Err(error) => Err(Error::io("start a thread for", "the journal", error)),
    }
}

/// Prints lines until the runner's last line, the end of the output, or
/// the deadline.
fn pump(
    lines: &Receiver<String>,
    print: bool,
    deadline: Option<Instant>,
    mut ended: impl FnMut() -> Result<Option<JournalEnd>, Error>,
) -> Result<JournalEnd, Error> {
    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(JournalEnd::Timeout);
        }
        let received = match deadline {
            Some(deadline) => {
                lines.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            }
            None => lines.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let line = match received {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => return Ok(JournalEnd::Timeout),
            Err(RecvTimeoutError::Disconnected) => return Ok(JournalEnd::Closed),
        };
        if print {
            out::line(&line)?;
        }
        if line.starts_with(RUN_ENDED)
            && let Some(end) = ended()?
        {
            return Ok(end);
        }
    }
}

/// The exit a run's recorded end maps to: dsh's own status, 128 + the
/// signal that ended it, or 1 when dsh never ran or no end was recorded.
fn run_exit(name: &InstanceName, run: &LastRun) -> RunExit {
    let Some(end) = &run.end else {
        let reason =
            format!("{name} has no end recorded: the host or the user manager went down mid-run");
        return RunExit {
            exit_code: 1,
            signal: None,
            reason,
        };
    };
    let Some(child) = end.child else {
        let reason = format!(
            "the run ended ({}) without dsh having run",
            end.service_result
        );
        return RunExit {
            exit_code: 1,
            signal: None,
            reason,
        };
    };
    let (signal, reason) = match (child.exit_code, child.signal) {
        (Some(status), _) => (None, format!("dsh exited with status {status}")),
        (None, Some(signal)) => (Some(signal), format!("dsh was ended by signal {signal}")),
        (None, None) => (None, "dsh ended without a status".to_owned()),
    };
    RunExit {
        exit_code: child.shell_status(),
        signal,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(end: &serde_json::Value) -> LastRun {
        let run = serde_json::json!({
            "id": "4b2d6a5e-1f3c-4e7a-9c0d-2a1b3c4d5e6f",
            "releasePath": "/nix/store/0000000000000000000000000000000a-dshd-release-dsh-0.2.2-rc.1",
            "bootId": "7f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b",
            "startedAt": 1_794_009_610,
            "invocationId": "0123456789abcdef0123456789abcdef",
            "end": end,
        });
        serde_json::from_value(run).unwrap()
    }

    fn ended(child: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "endedAt": 1_794_013_210,
            "serviceResult": "exit-code",
            "exitCode": "exited",
            "exitStatus": "3",
            "child": child,
            "stopIntent": false,
            "stopSignal": null,
        })
    }

    // catches: run or wait exiting 0 for a failed task, a signal death not mapped to 128+S, or an
    // unrecorded end reported as success
    #[test]
    fn exits_mirror_dsh() {
        let name = InstanceName::parse("batch").unwrap();
        let exit = |end: &serde_json::Value| run_exit(&name, &run(end));
        assert_eq!(
            exit(&ended(&serde_json::json!({ "exitCode": 3 }))).exit_code,
            3
        );
        let killed = exit(&ended(&serde_json::json!({ "signal": 9 })));
        assert_eq!((killed.exit_code, killed.signal), (137, Some(9)));
        assert_eq!(
            exit(&ended(&serde_json::Value::Null)).exit_code,
            1,
            "dsh never ran"
        );
        let cut = exit(&serde_json::Value::Null);
        assert_eq!(cut.exit_code, 1);
        assert!(cut.reason.contains("no end recorded"), "{}", cut.reason);
    }

    // catches: following past the runner's last line, which would block run and wait forever
    #[test]
    fn pump_stops_at_the_last_line() {
        let (sender, receiver) = mpsc::channel();
        for line in [
            "dshd-run: run started".to_owned(),
            "working".to_owned(),
            format!("{RUN_ENDED}: success"),
            "after".to_owned(),
        ] {
            sender.send(line).unwrap();
        }
        assert_eq!(
            pump(&receiver, false, None, || Ok(Some(JournalEnd::Recorded))).unwrap(),
            JournalEnd::Recorded
        );
        assert_eq!(
            receiver.try_recv().unwrap(),
            "after",
            "the line after the end stays unread"
        );
        drop(sender);
        assert_eq!(
            pump(&receiver, false, None, || Ok(None)).unwrap(),
            JournalEnd::Closed
        );
    }

    // catches: an EOF-only fake making a missing deadline look like a successful check-in
    #[test]
    fn a_live_follower_reaches_its_actual_deadline() {
        let (_sender, receiver) = mpsc::channel();
        let deadline = Instant::now() + Duration::from_millis(30);
        assert_eq!(
            pump(&receiver, false, Some(deadline), || Ok(None)).unwrap(),
            JournalEnd::Timeout
        );
    }

    // catches: dsh printing the marker causing a premature 124 while its own run remains active
    #[test]
    fn a_spoofed_end_marker_keeps_following_until_the_deadline() {
        let (sender, receiver) = mpsc::channel();
        sender.send(format!("{RUN_ENDED}: success")).unwrap();
        sender.send("the task is still running".to_owned()).unwrap();
        let deadline = Instant::now() + Duration::from_millis(30);
        assert_eq!(
            pump(&receiver, false, Some(deadline), || Ok(None)).unwrap(),
            JournalEnd::Timeout
        );
        assert!(
            receiver.try_recv().is_err(),
            "output after the spoofed marker was still read"
        );
    }

    // catches: a crash and restart described as an uneventful timeout, or EOF described as a passed deadline
    #[test]
    fn check_in_reasons_distinguish_restarts_deadlines_and_eof() {
        let name = InstanceName::parse("site").unwrap();
        let mut facts =
            crate::systemctl::parse_show(include_str!("../../tests/fixtures/show/running.txt"))
                .unwrap()
                .pop()
                .unwrap();
        facts.invocation_id = "new".to_owned();
        facts.restarts = 2;
        let restarted = check_in_reason(&name, "old", &facts, 900, JournalEnd::Recorded);
        assert!(
            restarted.contains("restarted site as invocation new (restart 2)"),
            "{restarted}"
        );
        assert!(!restarted.contains("after 900"), "{restarted}");
        facts.invocation_id = "old".to_owned();
        let closed = check_in_reason(&name, "old", &facts, 900, JournalEnd::Closed);
        assert!(
            closed.contains("journal follow ended") && !closed.contains("after 900"),
            "{closed}"
        );
        assert!(
            check_in_reason(&name, "old", &facts, 900, JournalEnd::Timeout).contains("after 900")
        );
    }
}
