//! Gathering facts through the fake launchd.

use super::*;
use crate::platform::launchd::testing::{Bench, START, argv, state_json};
use crate::systemctl::{ActiveState, LoadState};

const ID: &str = "0123456789abcdef0123456789abcdef";

// catches: a ready run read without its state file's invocation, or the LaunchAgents copy not making it enabled
#[test]
#[ignore = "packet D15: launchd fact gathering"]
fn show_reads_the_job_and_the_run_files() {
    let bench = Bench::new("ld-query-show", "web-1");
    bench
        .fake
        .hold("dshd.web-1", &bench.plist(), Some(4242), None);
    let run = bench.run_dir();
    std::fs::write(run.path().join("state.json"), state_json(ID, 4242, "ready")).unwrap();
    let copy = bench.manager.agent_copy(&bench.name);
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::write(&copy, "x").unwrap();
    let facts = show(&bench.manager, &bench.name).unwrap();
    assert_eq!(facts.unit, "gui/501/dshd.web-1");
    assert_eq!(facts.active_state, ActiveState::Active);
    assert_eq!(facts.invocation_id, ID);
    assert_eq!(facts.active_enter, Some(START + 1));
    assert!(facts.enabled());
}

// catches: a disabled label not read as masked, or a missing job read as an error
#[test]
#[ignore = "packet D15: launchd fact gathering"]
fn show_of_a_disabled_absent_job_is_masked() {
    let bench = Bench::new("ld-query-masked", "web-1");
    bench
        .fake
        .state()
        .disabled
        .insert("dshd.web-1".to_owned(), true);
    let facts = show(&bench.manager, &bench.name).unwrap();
    assert_eq!(facts.load_state, LoadState::Masked);
    assert_eq!(facts.active_state, ActiveState::Inactive);
}

// catches: `ls` paying one launchctl call per instance, or facts returned out of order
#[test]
#[ignore = "packet D15: launchd fact gathering"]
fn show_many_makes_two_calls_in_all() {
    let bench = Bench::new("ld-query-many", "web-1");
    let other = InstanceName::parse("api").unwrap();
    std::fs::create_dir_all(bench.manager.instance_dir(&other).path()).unwrap();
    bench.fake.hold("dshd.api", "/p", Some(7), None);
    let facts = show_many(&bench.manager, &[bench.name.clone(), other]).unwrap();
    let [first, second] = facts.as_slice() else {
        panic!("{facts:?}");
    };
    assert_eq!(first.unit, "gui/501/dshd.web-1");
    assert_eq!(first.load_state, LoadState::NotFound);
    assert_eq!(second.unit, "gui/501/dshd.api");
    assert_eq!(second.main_pid, 7);
    assert_eq!(
        bench.fake.calls(),
        [
            argv(&["print", "gui/501"]),
            argv(&["print-disabled", "gui/501"])
        ]
    );
}

// catches: other users' or apps' jobs, or labels that are not instance names, listed as instances
#[test]
#[ignore = "packet D15: launchd fact gathering"]
fn list_loaded_keeps_only_dshd_instances() {
    let bench = Bench::new("ld-query-list", "web-1");
    for label in [
        "dshd.web-1",
        "dshd.api",
        "com.apple.Finder",
        "dshd.Bad Name",
        "dshd.",
    ] {
        bench.fake.hold(label, "/p", None, Some(0));
    }
    let names = list_loaded(&bench.manager).unwrap();
    let names: Vec<String> = names.iter().map(ToString::to_string).collect();
    assert_eq!(names, ["api", "web-1"]);
}

// catches: no GUI login session reported as anything but exit 7
#[test]
#[ignore = "packet D15: launchd fact gathering"]
fn no_gui_session_exits_7() {
    let bench = Bench::new("ld-query-nogui", "web-1");
    bench.fake.state().no_gui_domain = true;
    for error in [
        show(&bench.manager, &bench.name).unwrap_err(),
        list_loaded(&bench.manager).unwrap_err(),
    ] {
        assert_eq!(error.exit_code(), 7, "{error}");
        assert!(
            error.to_string().contains("no GUI login session"),
            "{error}"
        );
    }
}
