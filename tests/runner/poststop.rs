//! Poststop records only observed ends and removes per-run state even on write failure.

use super::*;

// catches: dshd's stop intent ignored, another stop attributed to dshd, or intent reused by a later run
// rather than consumed with the observed stop signal (P2.5)
#[test]
fn poststop_records_who_requested_stop() -> TestResult {
    let ended_by = ["success", "exited", "0"];
    for (name, intent) in [("intended", true), ("unintended", false)] {
        let instance = Instance::create(
            name,
            &Spec {
                profile: "web",
                args: &[],
                env: &[],
            },
        )?;
        let running = instance.start()?;
        let url_file = instance.runtime().web_url();
        wait_for("the web announcement", || Ok(url_file.exists()))?;
        let intent_file = instance.runtime().path().join(dshd::name::STOP_INTENT_FILE);
        if intent {
            std::fs::write(&intent_file, "")?;
        }
        let ended = running.end(true)?;
        assert_eq!(ended.status, Some(0), "{name}:\n{}", ended.journal);
        let after = poststop(&instance, ended_by)?;
        assert_eq!(
            after.status.code(),
            Some(0),
            "{name}: {}",
            String::from_utf8_lossy(&after.stderr)
        );
        let (_, end) = recorded(&instance)?;
        assert_eq!(
            (end.stop_intent, end.stop_signal),
            (intent, Some(dshd::sys::SIGTERM)),
            "{name}: {end:?}"
        );
        assert!(
            !intent_file.exists(),
            "{name}: the stop intent is used once"
        );
    }

    Ok(())
}

// catches: poststop completing the wrong invocation, losing the child's exit or leaving runtime files
#[test]
fn poststop_records_child_end_and_cleans_runtime() -> TestResult {
    let ended_by = ["success", "exited", "0"];
    let instance = Instance::create("recorded", &headless(&[]))?;
    let ended = instance.start()?.end(false)?;
    assert_eq!(ended.status, Some(0), "{}", ended.journal);
    let runtime = instance.runtime();
    assert!(
        runtime.path().join("child-end.json").exists(),
        "the runner leaves the child's end for poststop"
    );
    let after = poststop(&instance, ended_by)?;
    let stderr = String::from_utf8_lossy(&after.stderr);
    assert_eq!(after.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr.lines().last(),
        Some(format!("{RUN_ENDED}: success").as_str()),
        "the run's last line: {stderr}"
    );
    let (run, end) = recorded(&instance)?;
    assert_eq!(
        (
            end.service_result.as_str(),
            end.exit_code.as_str(),
            end.exit_status.as_str()
        ),
        ("success", "exited", "0")
    );
    assert_eq!(
        end.child.map(|child| (child.exit_code, child.signal)),
        Some((Some(0), None))
    );
    assert_eq!((end.stop_intent, end.stop_signal), (false, None), "{end:?}");
    assert_eq!(
        run.invocation_id,
        common::INVOCATION_ID,
        "the start half of this invocation was completed"
    );
    assert_eq!(
        run.release_path.as_os_str(),
        instance.path("release").as_os_str()
    );
    assert!(
        run.started_at > 0 && run.started_at <= end.ended_at,
        "{run:?} {end:?}"
    );
    assert_eq!(
        std::fs::metadata(instance.dir().last_run_json())?.mode() & 0o777,
        0o600
    );
    let left = entries(runtime.path())?;
    assert!(left.is_empty(), "per-run files left behind: {left:?}");
    Ok(())
}

// catches: a run that cannot be recorded reported as recorded, so `dshd get` shows the run before it; or per-run
// files kept because the record failed (Q14)
#[test]
fn poststop_that_cannot_record_fails() -> TestResult {
    let instance = Instance::create("unrecorded", &headless(&[]))?;
    let ended = instance.start()?.end(false)?;
    assert_eq!(ended.status, Some(0), "{}", ended.journal);
    let dir = instance.dir();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500))?;
    let after = poststop(&instance, ["success", "exited", "0"]);
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
    let after = after?;
    let stderr = String::from_utf8_lossy(&after.stderr);
    assert_eq!(
        after.status.code(),
        Some(i32::from(EXIT_CONFIG)),
        "{stderr}"
    );
    assert!(stderr.contains("cannot record the run"), "{stderr}");
    assert!(
        stderr
            .lines()
            .last()
            .is_some_and(|line| line.starts_with(RUN_ENDED)),
        "the run's last line: {stderr}"
    );
    let left = entries(instance.runtime().path())?;
    assert!(left.is_empty(), "per-run files left behind: {left:?}");
    Ok(())
}

// catches: a foreign invocation completing the previous run's start half or attributing its child end
#[test]
fn poststop_of_another_invocation_records_its_own_end() -> TestResult {
    let instance = Instance::create("other-run", &headless(&[]))?;
    assert_eq!(instance.start()?.end(false)?.status, Some(0));
    assert!(
        poststop(&instance, ["success", "exited", "0"])?
            .status
            .success()
    );
    let after = instance
        .runner("poststop")
        .env("INVOCATION_ID", "other")
        .env("SERVICE_RESULT", "exit-code")
        .env("EXIT_CODE", "exited")
        .env("EXIT_STATUS", "78")
        .output()?;
    assert!(
        after.status.success(),
        "{}",
        String::from_utf8_lossy(&after.stderr)
    );
    let (run, end) = recorded(&instance)?;
    assert_eq!(run.invocation_id, "other");
    assert_eq!(run.started_at, end.ended_at);
    assert!(end.child.is_none());
    Ok(())
}

// catches: a killed runner reported as a clean child end instead of an observed unit failure
#[test]
fn poststop_after_runner_killed_has_no_invented_child_end() -> TestResult {
    let instance = Instance::create(
        "killed-runner",
        &Spec {
            profile: "web",
            args: &[],
            env: &[],
        },
    )?;
    let running = instance.start()?;
    wait_for("the web announcement", || {
        Ok(instance.runtime().web_url().exists())
    })?;
    let child: u32 = instance
        .stub_log()?
        .lines()
        .find_map(|line| line.strip_prefix("invocation "))
        .ok_or("no child pid")?
        .parse()?;
    let status = std::fs::read_to_string(format!("/proc/{child}/status"))?;
    let runner: u32 = status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))
        .ok_or("no runner pid")?
        .trim()
        .parse()?;
    dshd::sys::kill(runner, 9)?;
    assert_eq!(running.end(false)?.status, None);
    dshd::sys::kill(child, dshd::sys::SIGTERM)?;
    assert!(
        poststop(&instance, ["signal", "killed", "KILL"])?
            .status
            .success()
    );
    let (run, end) = recorded(&instance)?;
    assert!(end.child.is_none());
    assert_eq!(end.service_result, "signal");
    let mut completed = run;
    completed.end = Some(end);
    assert_eq!(
        dshd::systemctl::ending(&completed),
        dshd::cli::report::Ending::Failed
    );
    Ok(())
}

// catches: poststop retaining the ended closure after an update instead of releasing it through the root
#[test]
fn poststop_keeps_the_ended_release_fact_but_moves_booted_to_current() -> TestResult {
    let instance = Instance::create("collapse", &headless(&[]))?;
    let running = instance.start()?;
    wait_for("the original release to spawn dsh", || {
        Ok(instance.stub_log()?.contains("invocation "))
    })?;
    let next = instance.path("release-next");
    std::os::unix::fs::symlink(instance.path("release"), &next)?;
    dshd::fsx::symlink_replace(&next, &instance.dir().current())?;
    assert_eq!(running.end(false)?.status, Some(0));
    assert!(
        poststop(&instance, ["success", "exited", "0"])?
            .status
            .success()
    );
    let (run, _) = recorded(&instance)?;
    assert_eq!(
        run.release_path.as_os_str(),
        instance.path("release").as_os_str()
    );
    assert_eq!(std::fs::read_link(instance.dir().booted())?, next);
    Ok(())
}
