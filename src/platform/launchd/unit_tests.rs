//! Naming and echo lines of the launchd manager: built, so not ignored.

use super::*;
use crate::platform::launchd::testing::{Bench, argv};

// catches: a label, target or path that differs from the names the docs, the installer and the Mac test use
#[test]
fn names_are_fixed() {
    let bench = Bench::new("ld-names", "web-1");
    let manager = &bench.manager;
    let name = &bench.name;
    let home = bench.home();
    let instance = home.join(".local/state/dshd/web-1");
    assert_eq!(LaunchdManager::label(name), "dshd.web-1");
    assert_eq!(manager.domain(), "gui/501");
    assert_eq!(manager.service_target(name), "gui/501/dshd.web-1");
    assert_eq!(manager.unit_id(name), "gui/501/dshd.web-1");
    assert_eq!(manager.instance_dir(name).path(), instance);
    assert_eq!(manager.plist_path(name), instance.join("launchd.plist"));
    assert_eq!(manager.launchd_log(name), instance.join("launchd.log"));
    assert_eq!(manager.runs_dir(name), instance.join("runs"));
    assert_eq!(
        manager.run_logs(name),
        RunLogs::Files(instance.join("runs"))
    );
    assert_eq!(manager.runtime_dir(name).path(), instance.join("run"));
    assert_eq!(
        manager.agent_copy(name),
        home.join("Library/LaunchAgents/dshd.web-1.plist")
    );
    assert_eq!(
        LaunchdManager::name_of_label("dshd.web-1").map(|name| name.to_string()),
        Some("web-1".to_owned())
    );
    assert_eq!(LaunchdManager::name_of_label("com.apple.Finder"), None);
    assert_eq!(LaunchdManager::name_of_label("dshd."), None);
}

// catches: --dry-run echo lines naming a verb dshd does not run (kickstart, load) or the wrong target
#[test]
fn echo_lines_name_the_verbs_dshd_runs() {
    let bench = Bench::new("ld-echo", "web-1");
    let plist = bench.plist();
    let target = bench.target();
    let manager = &bench.manager;
    assert_eq!(
        manager.echo(Action::Start, &bench.name),
        format!("launchctl bootstrap gui/501 {plist}")
    );
    assert_eq!(
        manager.echo(Action::Stop, &bench.name),
        format!("launchctl bootout {target}")
    );
    assert_eq!(
        manager.echo(Action::Restart, &bench.name),
        format!("launchctl bootout {target} && launchctl bootstrap gui/501 {plist}")
    );
    assert_eq!(
        manager.masked_remedy(&bench.name),
        format!("launchctl enable {target}")
    );
    assert_eq!(manager.tool_name(), "launchctl");
    assert!(manager.relay_client_env().is_empty());
    assert_eq!(
        manager.relay_client_dir(&bench.name),
        Some(manager.instance_dir(&bench.name).path().to_path_buf()),
        "the relay client locates its instance from its working directory"
    );
    assert!(!manager.linger("anyone"));
}

// catches: the job spec running something other than the current release's runner, or a log outside the instance
#[test]
fn the_job_runs_the_current_runner() {
    let bench = Bench::new("ld-spec", "web-1");
    let spec = bench.manager.job_spec(&bench.name);
    let instance = bench.manager.instance_dir(&bench.name);
    assert_eq!(spec.label, "dshd.web-1");
    assert_eq!(spec.program, instance.current().join("bin/dshd-run"));
    assert_eq!(spec.arguments, argv(&["run", "web-1"]));
    assert_eq!(spec.working_directory, instance.path());
    assert_eq!(spec.log_path, bench.manager.launchd_log(&bench.name));
    assert_eq!(spec.exit_timeout, 30);
}

// catches: a job lookup that turns "no such job" into an error, or a missing domain into "no job"
#[test]
fn job_lookup_classifies_failures() {
    let bench = Bench::new("ld-job", "web-1");
    assert_eq!(bench.manager.job(&bench.name).unwrap(), None);
    bench.fake.state().no_gui_domain = true;
    assert_eq!(bench.manager.job(&bench.name).unwrap_err().exit_code(), 7);
    assert_eq!(
        bench.fake.calls().first(),
        Some(&argv(&["print", "gui/501/dshd.web-1"]))
    );
}

// catches: live counters left unknown for a run that is up, sampled from pid 0, or sampled for a
// run that is not up (systemd reports none then), or invented off macOS
#[test]
fn live_counters_come_from_the_job_tree_while_up() {
    use crate::platform::launchd::fake::FakeLaunchctl;
    use crate::systemctl::{ActiveState, Resources};
    let home = PathBuf::from("/Users/u");
    let counted = |pid: u32| Resources {
        tasks_current: Some(u64::from(pid)),
        ..Resources::default()
    };
    let manager = LaunchdManager::new(Box::new(FakeLaunchctl::default()), 501, home.clone())
        .with_tree_resources(counted);
    let mut facts =
        crate::systemctl::parse_show(include_str!("../../../tests/fixtures/show/running.txt"))
            .unwrap()
            .pop()
            .unwrap();
    facts.resources = Resources::default();
    facts.main_pid = 42;
    for state in [
        ActiveState::Active,
        ActiveState::Activating,
        ActiveState::Deactivating,
    ] {
        facts.active_state = state;
        let live = manager.with_live_resources(facts.clone());
        assert_eq!(live.resources.tasks_current, Some(42));
    }
    facts.active_state = ActiveState::Inactive;
    assert_eq!(
        manager.with_live_resources(facts.clone()).resources,
        Resources::default()
    );
    facts.active_state = ActiveState::Active;
    facts.main_pid = 0;
    assert_eq!(
        manager.with_live_resources(facts.clone()).resources,
        Resources::default()
    );
    facts.main_pid = 42;
    let native = LaunchdManager::new(Box::new(FakeLaunchctl::default()), 501, home);
    if cfg!(not(target_os = "macos")) {
        assert_eq!(
            native.with_live_resources(facts).resources,
            Resources::default()
        );
    }
}
