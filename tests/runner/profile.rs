//! The live profile at every start: the runner writes nothing in the DSH
//! home, dsh materialises a profile that is gone, and a profile that now
//! selects another transport is refused before dsh runs.

use std::path::PathBuf;

use super::tree::tree;
use super::*;

/// A web profile's bundles, and an ACP and an SDK one's.
const WEB_MANIFEST: &str = r#"{"name":"web","dsh":{"profile":{"bundles":["@deepseek-ai/dsh-base","@deepseek-ai/dsh-web-app"]}}}"#;
const ACP_MANIFEST: &str =
    r#"{"dsh":{"profile":{"bundles":["@deepseek-ai/dsh-base","@deepseek-ai/dsh-acp-app"]}}}"#;
const SDK_MANIFEST: &str = r#"{"dsh":{"profile":{"bundles":["@deepseek-ai/dsh-sdk-minimal"]}}}"#;

fn web() -> Spec<'static> {
    Spec {
        profile: "web",
        args: &[],
        env: &[],
    }
}

/// The stub's records, one per invocation of dsh, each split at the
/// start of its `invocation PID` line.
fn invocations(log: &str) -> Vec<&str> {
    log.strip_prefix("invocation ")
        .map(|records| records.split("\ninvocation ").collect())
        .unwrap_or_default()
}

/// A start that was refused before dsh ran: the configuration status, the
/// journal naming `reason`, no dsh, and no run recorded or booted.
fn refused(instance: &Instance, reason: &str) -> TestResult {
    let ended = instance.start()?.end(false)?;
    assert_eq!(
        ended.status,
        Some(i32::from(EXIT_CONFIG)),
        "{}",
        ended.journal
    );
    assert!(
        ended.journal.contains(reason),
        "the journal says {reason:?}:\n{}",
        ended.journal
    );
    assert_eq!(instance.stub_log()?, "", "dsh was never spawned");
    assert!(
        std::fs::symlink_metadata(instance.dir().booted()).is_err()
            && dshd::instance::read_last_run(&instance.dir())?.is_none(),
        "a refused start boots nothing and records no run"
    );
    Ok(())
}

// catches: the runner writing in the DSH home at a start, a stop or a restart, such as a link, a copy or a stamp
#[test]
fn the_runner_writes_nothing_in_the_dsh_home() -> TestResult {
    let instance = Instance::create("untouched", &web())?;
    let home = instance.path("dsh");
    let before = tree(&home)?;
    assert!(
        before.contains_key(&home.join("profiles/web/package.json")),
        "{before:?}"
    );
    for run in 1..=2 {
        let running = instance.start()?;
        wait_for("the web announcement", || {
            Ok(announced_ports(&instance.stub_log()?).len() == run)
        })?;
        let ended = running.end(true)?;
        assert_eq!(ended.status, Some(0), "{}", ended.journal);
        assert_eq!(tree(&home)?, before, "the DSH home is as it was");
    }
    assert_eq!(announced_ports(&instance.stub_log()?).len(), 2);
    Ok(())
}

// catches: a start that boots a profile dshd could not read, or that materialises it other than through dsh, with
// the instance's home, directory and environment (P2.6)
#[test]
fn a_missing_profile_is_materialised_by_dsh_before_the_start() -> TestResult {
    let instance = Instance::create("fresh", &web())?;
    let profiles = instance.path("dsh/profiles");
    std::fs::remove_dir_all(profiles.join("web"))?;
    let running = instance.start()?;
    wait_for("the web announcement", || {
        Ok(!announced_ports(&instance.stub_log()?).is_empty())
    })?;
    let ended = running.end(true)?;
    assert_eq!(ended.status, Some(0), "{}", ended.journal);

    let log = instance.stub_log()?;
    let runs = invocations(&log);
    let &[materialise, boot] = runs.as_slice() else {
        return Err(format!("dsh ran {} times, not twice:\n{log}", runs.len()).into());
    };
    assert_eq!(
        first_argv(materialise)?,
        ["--profile", "web", "--dump-default-config"]
    );
    assert!(
        materialise.contains(&format!("\ncwd {}\n", instance.path("work").display())),
        "{materialise}"
    );
    let env = stub_env(materialise);
    for expected in [
        format!("DSH_HOME={}", instance.path("dsh").display()),
        format!("INVOCATION_ID={}", common::INVOCATION_ID),
        "DSH_PERMISSION_MODE=danger-full-access".to_owned(),
    ] {
        assert!(env.contains(&expected.as_str()), "{expected}: {env:?}");
    }
    assert!(
        !materialise.contains("\nmode "),
        "materialising boots nothing"
    );
    assert_eq!(first_argv(boot)?, ["web", "--no-open", "--port", "0"]);

    let manifest = profiles.join("web/package.json");
    let made: Vec<PathBuf> = tree(&profiles)?.into_keys().collect();
    assert_eq!(
        made,
        [profiles.clone(), profiles.join("web"), manifest.clone()],
        "only what dsh made is new"
    );
    let written: serde_json::Value = serde_json::from_slice(&std::fs::read(manifest)?)?;
    assert_eq!(
        written,
        serde_json::from_str::<serde_json::Value>(WEB_MANIFEST)?
    );
    Ok(())
}

// catches: a start that serves the recorded transport for a profile that now selects another, holds a profile
// that became an SDK host, or spawns dsh before it refuses (P1.7)
#[test]
fn a_profile_now_of_another_transport_is_refused_before_dsh_runs() -> TestResult {
    let instance = Instance::create("drifted", &web())?;
    let manifest = instance.path("dsh/profiles/web/package.json");
    std::fs::write(&manifest, ACP_MANIFEST)?;
    refused(
        &instance,
        "now selects transport acp, and drifted was created for web",
    )?;
    std::fs::write(&manifest, SDK_MANIFEST)?;
    refused(&instance, "profile web cannot be held by dshd")?;
    Ok(())
}
