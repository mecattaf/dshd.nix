//! The launchd runner completes its own run (macOS), as `poststop` does
//! after a unit on Linux: the end it saw and who asked for the stop, the
//! child's end and what it used, the run's files, and `booted` moved to
//! `current`, all under the invocation id the runner chose.

use dshd::platform::launchd::RUNS_DIR;
use dshd::sys::SIGTERM;

use super::*;

/// The run files the launchd runner keeps after a run: its state, which
/// the next run of the same job reads, and the start history.
const KEPT: [&str; 2] = ["state.json", "starts.json"];

fn web() -> Spec<'static> {
    Spec {
        profile: "web",
        args: &[],
        env: &[],
    }
}

// catches: dshd's stop intent ignored, another stop attributed to dshd, or an intent reused by a later
// run rather than consumed with the observed stop signal (P2.5), under launchd
#[test]
fn the_runner_records_who_requested_its_stop() -> TestResult {
    for (name, intent) in [("intended", true), ("unintended", false)] {
        let instance = Instance::create(name, &web())?;
        let running = instance.start()?;
        let url_file = instance.runtime().web_url();
        wait_for("the web announcement", || Ok(url_file.exists()))?;
        let intent_file = instance.runtime().stop_intent();
        if intent {
            std::fs::write(&intent_file, "")?;
        }
        let ended = running.end(true)?;
        assert_eq!(ended.status, Some(0), "{name}:\n{}", ended.journal);
        let (run, end) = recorded(&instance)?;
        assert_eq!(run.invocation_id, invocation(&ended), "{name}");
        assert_eq!(
            (end.stop_intent, end.stop_signal),
            (intent, Some(SIGTERM)),
            "{name}: {end:?}"
        );
        assert_eq!(
            (end.service_result.as_str(), end.exit_status.as_str()),
            ("success", "0"),
            "{name}: {end:?}"
        );
        assert!(
            !intent_file.exists(),
            "{name}: the stop intent is used once"
        );
        assert!(!url_file.exists(), "{name}: the URL goes with the run");
    }
    Ok(())
}

// catches: a run completed under another invocation, the child's exit or its usage lost (wait4's
// totals never reaching last-run.json), byte counts invented where rusage has none, the run's files
// left behind, or a run log that does not open and close the run
#[test]
fn the_runner_records_the_child_end_and_clears_the_run() -> TestResult {
    let instance = Instance::create("recorded", &headless(&[]))?;
    let ended = instance.start()?.end(false)?;
    assert_eq!(ended.status, Some(0), "{}", ended.journal);
    assert_eq!(ended.invocations.len(), 1, "{}", ended.journal);
    assert!(ended.journal.starts_with(RUN_STARTED), "{}", ended.journal);
    assert_eq!(
        ended.journal.lines().last(),
        Some(format!("{RUN_ENDED}: success").as_str()),
        "the run's last line:\n{}",
        ended.journal
    );
    let (run, end) = recorded(&instance)?;
    assert_eq!(run.invocation_id, invocation(&ended));
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
        run.release_path.as_os_str(),
        instance.path("release").as_os_str()
    );
    assert!(
        run.started_at > 0 && run.started_at <= end.ended_at,
        "{run:?} {end:?}"
    );
    let resources = end.resources.ok_or("the run recorded no resources")?;
    assert!(
        resources.cpu_usage_nsec.is_some() && resources.memory_peak.is_some(),
        "wait4's totals: {resources:?}"
    );
    assert_eq!(
        (resources.io_read_bytes, resources.io_write_bytes),
        (None, None),
        "rusage has no byte counts"
    );
    assert_eq!(
        std::fs::metadata(instance.dir().last_run_json())?.mode() & 0o777,
        0o600
    );
    let left: Vec<_> = entries(instance.runtime().path())?
        .into_iter()
        .filter(|name| !KEPT.iter().any(|kept| name == kept))
        .collect();
    assert!(left.is_empty(), "run files left behind: {left:?}");
    let logs = entries(&instance.dir().path().join(RUNS_DIR))?;
    assert!(
        logs.iter()
            .any(|name| *name == format!("{}.log", invocation(&ended)).as_str()),
        "{logs:?}"
    );
    Ok(())
}

// catches: the runner keeping the ended closure booted after an update instead of releasing it through
// `current`, or recording the release it was not started from
#[test]
fn the_ended_release_is_recorded_and_booted_moves_to_current() -> TestResult {
    let instance = Instance::create("collapse", &headless(&[("DSH_STUB_DELAY", "2")]))?;
    let running = instance.start()?;
    wait_for("the original release to spawn dsh", || {
        Ok(instance.stub_log()?.contains("invocation "))
    })?;
    let next = instance.path("release-next");
    std::os::unix::fs::symlink(instance.path("release"), &next)?;
    dshd::fsx::symlink_replace(&next, &instance.dir().current())?;
    let ended = running.end(false)?;
    assert_eq!(ended.status, Some(0), "{}", ended.journal);
    let (run, _) = recorded(&instance)?;
    assert_eq!(
        run.release_path.as_os_str(),
        instance.path("release").as_os_str()
    );
    assert_eq!(std::fs::read_link(instance.dir().booted())?, next);
    Ok(())
}
