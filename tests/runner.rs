//! The runner outside systemd, with the stub dsh: the web announcement and
//! its pinned port, readiness that is never reported for a start that
//! cannot be attached, the ACP relay's handshake and probe, the exit status
//! table end to end, the refusals before anything is spawned, `poststop`
//! and the stop it records, and the argv, environment and signal mask dsh
//! receives.

mod common;

#[path = "runner/launch.rs"]
mod launch;
#[path = "runner/poststop.rs"]
mod poststop;

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::path::Path;
use std::time::Duration;

use common::{Instance, Spec, TestResult, wait_for};
use dshd::record::LastRun;
use dshd::runner::{EXIT_CONFIG, RUN_ENDED, RUN_STARTED};

/// What the stub's web mode prints as its token.
const STUB_TOKEN: &str = "token=stub";
/// A bound on one read from the relay, so a silent relay fails the test.
const READ_BOUND: Duration = Duration::from_secs(20);
/// A recorded variable whose value must reach dsh and nothing else.
const CANARY_NAME: &str = "PLANTED_CANARY";
const CANARY: &str = "canary-4f1d";

fn headless<'a>(env: &'a [(&'a str, &'a str)]) -> Spec<'a> {
    Spec {
        profile: "headless",
        args: &[],
        env,
    }
}

/// The ports the stub announced, in order (`web PID PORT` lines).
fn announced_ports(log: &str) -> Vec<String> {
    log.lines()
        .filter_map(|line| {
            line.strip_prefix("web ")?
                .split(' ')
                .nth(1)
                .map(str::to_owned)
        })
        .collect()
}

/// The argv of the stub's first invocation, from its NUL-separated record.
fn first_argv(log: &str) -> TestResult<Vec<String>> {
    let (_, rest) = log
        .split_once("\nargc ")
        .ok_or("the stub recorded no argc")?;
    let (count, rest) = rest
        .split_once('\n')
        .ok_or("the stub's argc line has no end")?;
    let rest = rest
        .strip_prefix("argv ")
        .ok_or("the stub recorded no argv")?;
    let count: usize = count.parse()?;
    let argv: Vec<String> = rest.split('\0').take(count).map(str::to_owned).collect();
    assert_eq!(argv.len(), count, "the record holds every argument");
    Ok(argv)
}

/// `dshd-run poststop NAME` with what systemd hands `ExecStopPost` after
/// the run ended `SERVICE_RESULT`/`EXIT_CODE`/`EXIT_STATUS`.
fn poststop(instance: &Instance, ended: [&str; 3]) -> io::Result<std::process::Output> {
    let [result, code, status] = ended;
    instance
        .runner("poststop")
        .env("SERVICE_RESULT", result)
        .env("EXIT_CODE", code)
        .env("EXIT_STATUS", status)
        .output()
}

/// `last-run.json` with its end, as poststop completed it.
fn recorded(instance: &Instance) -> TestResult<(LastRun, dshd::record::RunEnd)> {
    let mut run = dshd::instance::read_last_run(&instance.dir())?.ok_or("no last-run.json")?;
    let end = run.end.take().ok_or("last-run.json has no end")?;
    Ok((run, end))
}

/// The names of the files left in a directory.
fn entries(dir: &Path) -> TestResult<Vec<std::ffi::OsString>> {
    Ok(std::fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<_, _>>()?)
}

/// The `env NAME=VALUE` lines of the stub's record, as `NAME=VALUE`.
fn stub_env(log: &str) -> Vec<&str> {
    log.lines()
        .filter_map(|line| line.strip_prefix("env "))
        .collect()
}

// catches: a web URL or token that leaks into the journal, a port not pinned on the first announcement, or a restart that asks for a new port (P3.1)
#[test]
fn web_port_pinned_across_restart_and_token_never_in_journal() -> TestResult {
    let instance = Instance::create(
        "web-pin",
        &Spec {
            profile: "web",
            args: &[],
            env: &[],
        },
    )?;
    let dir = instance.dir();
    let url_file = instance.runtime().web_url();

    let first = instance.start()?;
    wait_for("the web announcement", || {
        Ok(url_file.exists() && dshd::instance::read_web_port(&dir)?.is_some())
    })?;
    let url = std::fs::read_to_string(&url_file)?;
    let port = dshd::instance::read_web_port(&dir)?.ok_or("no pinned port")?;
    assert!(
        url.trim()
            .starts_with(&format!("http://127.0.0.1:{port}/?{STUB_TOKEN}")),
        "{url}"
    );
    assert_eq!(
        std::fs::metadata(&url_file)?.mode() & 0o777,
        0o600,
        "the URL file carries the token"
    );
    let ended = first.end(true)?;
    assert_eq!(
        ended.status,
        Some(0),
        "a stopped web instance exits 0:\n{}",
        ended.journal
    );

    let second = instance.start()?;
    wait_for("the second announcement", || {
        Ok(announced_ports(&instance.stub_log()?).len() == 2)
    })?;
    wait_for("this process's fresh URL", || {
        Ok(std::fs::read_to_string(&url_file)? != url)
    })?;
    let fresh_url = std::fs::read_to_string(&url_file)?;
    assert_ne!(
        fresh_url, url,
        "a restart supplies this process's fresh token"
    );
    assert!(fresh_url.contains(STUB_TOKEN), "{fresh_url}");
    let ended = second.end(true)?;
    assert_eq!(ended.status, Some(0));
    assert_eq!(
        announced_ports(&instance.stub_log()?),
        [port.to_string(), port.to_string()]
    );
    assert!(
        ended.journal.contains("token=REDACTED"),
        "the announcement reaches the journal redacted:\n{}",
        ended.journal
    );
    assert!(
        !ended.journal.contains(STUB_TOKEN),
        "the token never reaches the journal:\n{}",
        ended.journal
    );
    Ok(())
}

// catches: a crash after readiness that does not ask for a restart, a child status 75 passed through as a
// restart request, or a failure reported as success (P1.7)
#[test]
fn crash_and_failure_exit_statuses_end_to_end() -> TestResult {
    let cases: [(&str, &str, Option<&str>, i32); 5] = [
        ("exit-three", "headless", Some("exit-3"), 3),
        ("exit-tempfail", "headless", Some("exit-75"), 1),
        ("web-failed", "web", Some("exit-1"), 1),
        (
            "web-crashed",
            "web",
            Some("crash-after-ready"),
            i32::from(dshd::runner::EXIT_TEMPFAIL),
        ),
        ("clean", "headless", None, 0),
    ];
    for (name, profile, mode, expected) in cases {
        let env: Vec<(&str, &str)> = mode
            .map(|mode| ("DSH_STUB_MODE", mode))
            .into_iter()
            .collect();
        let instance = Instance::create(
            name,
            &Spec {
                profile,
                args: &[],
                env: &env,
            },
        )?;
        let ended = instance.start()?.end(false)?;
        assert_eq!(ended.status, Some(expected), "{name}:\n{}", ended.journal);
        assert!(
            instance.stub_log()?.contains("invocation "),
            "{name}: dsh was spawned"
        );
        if mode == Some("crash-after-ready") {
            // catches: dsh's last output dropped or logged after the runner's account of its end
            let said = ended
                .journal
                .find("stub: crashing")
                .ok_or("dsh's last line is missing")?;
            let ending = ended
                .journal
                .find("dshd-run: dsh exited with status 1")
                .ok_or("the runner logged no end")?;
            assert!(
                said < ending,
                "dsh's last line comes before the runner's account:\n{}",
                ended.journal
            );
        }
    }
    Ok(())
}

// catches: a runner that starts dsh in a cwd that is gone, or asks systemd to retry a configuration error (P1.7)
#[test]
fn missing_cwd_is_78_without_spawning_dsh() -> TestResult {
    let instance = Instance::create("no-cwd", &headless(&[]))?;
    std::fs::remove_dir(instance.path("work"))?;
    let ended = instance.start()?.end(false)?;
    assert_eq!(
        ended.status,
        Some(i32::from(EXIT_CONFIG)),
        "{}",
        ended.journal
    );
    assert!(
        ended.journal.starts_with(RUN_STARTED),
        "every run's lines follow its first line:\n{}",
        ended.journal
    );
    assert!(
        ended.journal.contains("work"),
        "the journal names the missing directory:\n{}",
        ended.journal
    );
    assert_eq!(instance.stub_log()?, "", "dsh was never spawned");
    Ok(())
}

// catches: a runner that acts on a record written by a newer schema, dropping keys it does not know (P1.7)
#[test]
fn unknown_record_key_is_refused_before_spawn() -> TestResult {
    let instance = Instance::create("newer", &headless(&[]))?;
    let record = instance.dir().instance_json();
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&record)?)?;
    value
        .as_object_mut()
        .ok_or("the record is not an object")?
        .insert("colour".to_owned(), "blue".into());
    std::fs::write(&record, serde_json::to_vec(&value)?)?;
    let ended = instance.start()?.end(false)?;
    assert_eq!(
        ended.status,
        Some(i32::from(EXIT_CONFIG)),
        "{}",
        ended.journal
    );
    assert!(
        ended.journal.contains("colour"),
        "the journal names the key:\n{}",
        ended.journal
    );
    assert_eq!(instance.stub_log()?, "", "dsh was never spawned");
    Ok(())
}

// catches: a start reported ready that cannot be attached (Q14)
#[test]
fn web_start_without_its_announcement_is_never_ready() -> TestResult {
    let instance = Instance::create(
        "unannounced",
        &Spec {
            profile: "web",
            args: &[],
            env: &[],
        },
    )?;
    // A directory where the URL file goes: the announcement cannot be recorded.
    std::fs::create_dir(instance.runtime().web_url())?;
    let notify = UnixDatagram::bind(instance.path("notify.sock"))?;
    notify.set_nonblocking(true)?;
    let mut command = instance.runner("run");
    command.env("NOTIFY_SOCKET", instance.path("notify.sock"));
    let ended = instance.launch(command)?.end(false)?;
    assert_eq!(
        ended.status,
        Some(i32::from(EXIT_CONFIG)),
        "{}",
        ended.journal
    );
    assert!(
        ended
            .journal
            .contains("the start cannot be attached, so dsh is stopped"),
        "{}",
        ended.journal
    );
    let mut messages = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        match notify.recv(&mut buffer) {
            Ok(size) => messages
                .push(String::from_utf8_lossy(buffer.get(..size).unwrap_or_default()).into_owned()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) => return Err(error.into()),
        }
    }
    assert!(
        messages.iter().any(|message| message == "STOPPING=1"),
        "{messages:?}"
    );
    assert!(
        !messages.iter().any(|message| message.contains("READY=1")),
        "readiness was reported: {messages:?}"
    );
    Ok(())
}

// catches: an ACP runner that is not reachable once ready, a probe that reports a controller or session that
// does not exist, or a garbage first line taken for a handshake (P3.3)
#[test]
fn acp_runner_answers_probe_and_refuses_bad_handshake() -> TestResult {
    let instance = Instance::create(
        "acp-probe",
        &Spec {
            profile: "acp",
            args: &[],
            env: &[],
        },
    )?;
    let socket = instance.runtime().acp_socket();
    let running = instance.start()?;
    wait_for("the relay socket", || Ok(socket.exists()))?;

    let probe = instance.runner("probe").output()?;
    assert_eq!(
        probe.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&probe.stderr)
    );
    let line: dshd::relay::protocol::ProbeLine = serde_json::from_slice(&probe.stdout)?;
    let idle = dshd::relay::protocol::ProbeLine {
        attached: false,
        attached_since: None,
        initialized: false,
        held_sessions: Vec::new(),
        pending_permissions: 0,
    };
    assert_eq!(line, idle);
    assert_eq!(std::fs::metadata(&socket)?.mode() & 0o777, 0o600);

    let mut stream = UnixStream::connect(&socket)?;
    stream.set_read_timeout(Some(READ_BOUND))?;
    stream.write_all(b"hello\n")?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    let refused = dshd::relay::protocol::HandshakeReply::Refused {
        reason: dshd::relay::protocol::Refusal::BadHandshake,
    };
    assert_eq!(
        serde_json::from_str::<dshd::relay::protocol::HandshakeReply>(&reply)?,
        refused
    );

    let ended = running.end(true)?;
    assert_eq!(
        ended.status,
        Some(0),
        "closing dsh's stdin stops an ACP instance cleanly:\n{}",
        ended.journal
    );
    Ok(())
}
