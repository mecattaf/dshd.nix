//! Native argv, environment and signal-mask fidelity, including secret containment.

use super::*;

/// Every retained run file and the poststop output must omit captured secrets.
fn assert_secret_absent(instance: &Instance, journal: &str) -> TestResult {
    // catches: a recorded environment value that reaches the journal, last-run.json or a runtime file
    assert!(
        !journal.contains(CANARY),
        "an environment value reached the journal:\n{journal}"
    );
    let runtime = instance.runtime();
    let mut written = vec![(
        instance.dir().last_run_json(),
        std::fs::read(instance.dir().last_run_json())?,
    )];
    for name in entries(runtime.path())? {
        let file = runtime.path().join(name);
        written.push((file.clone(), std::fs::read(file)?));
    }
    assert!(
        written.len() > 1,
        "the run left its per-run files for poststop"
    );
    let after = poststop(instance, ["success", "exited", "0"])?;
    assert_eq!(
        after.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&after.stderr)
    );
    written.push((
        instance.dir().last_run_json(),
        std::fs::read(instance.dir().last_run_json())?,
    ));
    written.push((instance.path("poststop stderr"), after.stderr));
    for (file, bytes) in written {
        assert!(
            !String::from_utf8_lossy(&bytes).contains(CANARY),
            "an environment value reached {}",
            file.display()
        );
    }

    Ok(())
}

/// The captured, refreshed and runner-only groups reach the child with their documented meanings.
fn assert_environment(instance: &Instance, log: &str, excluded: &[(&str, &str)]) {
    let env = stub_env(log);
    let has = |line: String| env.contains(&line.as_str());
    assert!(
        has(format!("DSH_HOME={}", instance.path("dsh").display())),
        "{env:?}"
    );
    assert!(
        has(format!("PWD={}", instance.path("work").display())),
        "{env:?}"
    );
    assert!(
        dshd::launch::SESSION_REFRESHED.contains(&"XDG_RUNTIME_DIR")
            && dshd::launch::RUNNER_PASSTHROUGH.contains(&"INVOCATION_ID")
    );
    assert!(
        has(format!(
            "XDG_RUNTIME_DIR={}",
            instance.path("run").display()
        )),
        "refreshed from the manager: {env:?}"
    );
    assert!(
        has(format!("INVOCATION_ID={}", common::INVOCATION_ID)),
        "passed through from the runner: {env:?}"
    );
    assert!(
        has("DSH_PERMISSION_MODE=danger-full-access".to_owned()),
        "the release fills what the record lacks: {env:?}"
    );
    assert!(
        has(format!("{CANARY_NAME}={CANARY}")),
        "a recorded variable reaches dsh: {env:?}"
    );
    for &(name, _) in excluded {
        assert!(
            dshd::launch::CAPTURE_EXCLUDED.contains(&name),
            "{name} is on the closed list"
        );
        assert!(
            !env.iter().any(|line| line.starts_with(&format!("{name}="))),
            "{name} reached dsh: {env:?}"
        );
    }
    assert!(
        !env.iter()
            .any(|line| line.starts_with("RUNTIME_DIRECTORY=")),
        "the runner's own variables stay with it: {env:?}"
    );
}

// catches: an argument re-quoted, split or expanded on its way to dsh, a closed-list variable captured, or
// the runner's own identity not passed through (P1.2, P3.4)
#[test]
fn argv_and_environment_reach_dsh_verbatim_without_closed_list_names() -> TestResult {
    let prompt = "say \"$PATH\" 100% 'quoted' `tick`\nand a second line";
    let excluded = [
        ("SSH_CONNECTION", "10.0.0.1 22 10.0.0.2 22"),
        ("NOTIFY_SOCKET", "/nonexistent/notify"),
        ("XDG_SESSION_ID", "7"),
        ("HC_SCOPE", "scope-planted"),
        ("DSH_PROFILE", "web"),
    ];
    let mut planted = excluded.to_vec();
    planted.push((CANARY_NAME, CANARY));
    let instance = Instance::create(
        "verbatim",
        &Spec {
            profile: "headless",
            args: &["-p", prompt],
            env: &planted,
        },
    )?;
    let ended = instance.start()?.end(false)?;
    assert_eq!(ended.status, Some(0), "{}", ended.journal);
    assert_secret_absent(&instance, &ended.journal)?;

    let log = instance.stub_log()?;
    assert_eq!(first_argv(&log)?, ["headless", "-p", prompt]);
    assert!(
        log.contains(&format!("\ncwd {}\n", instance.path("work").display())),
        "dsh runs in the recorded cwd:\n{log}"
    );
    assert_environment(&instance, &log, &excluded);
    Ok(())
}

// catches: a user-typed --port 0 replaced by an old pin, or a user port causing dshd to pin it
#[test]
fn explicit_ephemeral_port_is_preserved_with_and_without_an_old_pin() -> TestResult {
    for old_pin in [false, true] {
        let name = if old_pin { "old-pin" } else { "no-pin" };
        let instance = Instance::create(
            name,
            &Spec {
                profile: "web",
                args: &["--port", "0"],
                env: &[("DSH_STUB_PORT", "47777")],
            },
        )?;
        if old_pin {
            dshd::instance::write_web_port(&instance.dir(), 43333)?;
        }
        let running = instance.start()?;
        wait_for("the explicit ephemeral port announcement", || {
            Ok(instance.runtime().web_url().exists())
        })?;
        let ended = running.end(true)?;
        assert_eq!(ended.status, Some(0), "{}", ended.journal);
        let log = instance.stub_log()?;
        assert!(
            first_argv(&log)?
                .windows(2)
                .any(|args| args == ["--port", "0"]),
            "{log}"
        );
        assert_eq!(announced_ports(&log), ["47777"]);
        assert_eq!(
            dshd::instance::read_web_port(&instance.dir())?,
            old_pin.then_some(43333)
        );
    }
    Ok(())
}

// catches: dsh inheriting the runner's blocked stop signals, so it would sit out the SIGTERM a stop
// sends it until the final SIGKILL (sys::unmask_on_exec)
#[test]
fn dsh_starts_with_no_blocked_signals() -> TestResult {
    let instance = Instance::create("unmasked", &headless(&[]))?;
    let ended = instance.start()?.end(false)?;
    assert_eq!(ended.status, Some(0), "{}", ended.journal);
    let log = instance.stub_log()?;
    let mask = log
        .lines()
        .find_map(|line| line.strip_prefix("sigblk "))
        .ok_or("the stub recorded no signal mask")?;
    assert_eq!(u64::from_str_radix(mask, 16)?, 0, "{log}");
    Ok(())
}

// catches: a `run --stdin` snapshot that never reaches a headless dsh, which then reads /dev/null as its task,
// a task larger than one argv string cut short, a snapshot read again at a later start, or one whose content
// changed since creation handed to dsh instead of refused before it is spawned
#[test]
fn stdin_snapshot_is_headless_dsh_stdin_at_every_start() -> TestResult {
    use dshd::record::{InstanceRecord, RunStdin};
    // Larger than one argv string may be (MAX_ARG_STRLEN, 128 KiB).
    let task = format!("{}\nand a last line\n", "a".repeat(140_000));
    let instance = Instance::create(
        "stdin",
        &Spec {
            profile: "headless",
            args: &["-"],
            env: &[],
        },
    )?;
    let dir = instance.dir();
    let mut record: InstanceRecord =
        serde_json::from_slice(&std::fs::read(dir.instance_json())?)?;
    let snapshot = RunStdin::of(task.as_bytes(), dshd::record::OsText::from("task.txt"));
    let digest = snapshot.sha256.clone();
    record.stdin = Some(snapshot);
    std::fs::write(dir.stdin(), &task)?;
    dshd::fsx::write_json_atomic(&dir.instance_json(), &record, 0o600)?;

    for _ in 0..2 {
        let ended = instance.start()?.end(false)?;
        assert_eq!(ended.status, Some(0), "{}", ended.journal);
    }
    let log = instance.stub_log()?;
    let read: Vec<&str> = log
        .lines()
        .filter_map(|line| line.strip_prefix("stdin ")?.split(' ').nth(1))
        .collect();
    assert_eq!(read, [digest.as_str(), digest.as_str()], "{log}");

    std::fs::write(dir.stdin(), "another task\n")?;
    let before = instance.stub_log()?;
    let ended = instance.start()?.end(false)?;
    assert_eq!(
        ended.status,
        Some(i32::from(EXIT_CONFIG)),
        "{}",
        ended.journal
    );
    assert!(
        ended.journal.contains("--stdin snapshot"),
        "the journal names the snapshot:\n{}",
        ended.journal
    );
    assert_eq!(instance.stub_log()?, before, "dsh was never spawned");
    Ok(())
}
