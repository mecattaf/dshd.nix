//! Unit tests for runner.

use super::*;

// catches: a runner that acts on another instance's state, or on a working directory that is no instance's (P1.7)
#[test]
fn state_root_follows_the_working_directory() {
    let name = InstanceName::parse("web-1").unwrap();
    let root = state_root_of(Path::new("/srv/state/dshd/web-1"), &name).unwrap();
    assert_eq!(root.path(), Path::new("/srv/state"));
    assert!(state_root_of(Path::new("/srv/state/dshd/web-2"), &name).is_none());
    assert!(state_root_of(Path::new("/srv/state/other/web-1"), &name).is_none());
    assert!(state_root_of(Path::new("/web-1"), &name).is_none());
}

// catches: DSH's restart button logged as a crash, a crash logged as a planned restart, or a status 42 that
// was passed through called a restart
#[test]
fn ending_explains_a_restart() {
    let asked = ChildEnd {
        exit_code: Some(42),
        signal: None,
    };
    assert_eq!(
        ending(asked, EXIT_TEMPFAIL),
        "dsh asked to be restarted (status 42); exiting 75 so systemd restarts the instance"
    );
    assert_eq!(ending(asked, 42), "dsh exited with status 42; exiting 42");
    let crashed = ChildEnd {
        exit_code: Some(1),
        signal: None,
    };
    assert_eq!(
        ending(crashed, EXIT_TEMPFAIL),
        "dsh exited with status 1 after it was ready; exiting 75 so systemd restarts the instance"
    );
    let killed = ChildEnd {
        exit_code: None,
        signal: Some(9),
    };
    assert_eq!(
        ending(killed, 137),
        "dsh was killed by signal 9; exiting 137"
    );
}

// catches: a start that hands dsh a snapshot that is gone, whose content changed since creation, or whose recorded
// name reaches outside the instance's patches directory; or the snapshot not inserted after the profile
#[test]
fn check_patches_refuses_a_changed_or_missing_snapshot() {
    use std::fs;

    use crate::fsx::testing::Scratch;

    let scratch = Scratch::new("runner-patches");
    let dir = InstanceDir::new(scratch.path().to_path_buf());
    let name = InstanceName::parse("web-1").unwrap();
    let mut record: InstanceRecord =
        serde_json::from_str(include_str!("../../tests/fixtures/instance.json")).unwrap();
    let patch = RunPatch::of(0, b"a: 1\n", OsText::from("a.yml"));
    record.patches = vec![patch.clone()];
    fs::create_dir_all(dir.patches()).unwrap();
    let refused = |record: &InstanceRecord| {
        matches!(
            check_patches(&name, &dir, record),
            Err(Error::InconsistentState { .. })
        )
    };
    assert!(refused(&record), "a missing snapshot");

    fs::write(dir.patch(&patch.file), b"a: 1\n").unwrap();
    check_patches(&name, &dir, &record).unwrap();
    let (args, _) = launch_args(&dir, &record).unwrap();
    let snapshot = dir.patch(&patch.file).into_os_string();
    let want: Vec<OsString> = vec![
        "web".into(),
        "--patch".into(),
        snapshot,
        "--port".into(),
        "4100".into(),
        "--no-open".into(),
    ];
    assert_eq!(args, want);

    fs::write(dir.patch(&patch.file), b"a: 2\n").unwrap();
    assert!(refused(&record), "a snapshot whose content changed");

    fs::write(scratch.path().join("outside.yml"), b"a: 1\n").unwrap();
    for entry in &mut record.patches {
        entry.file = "../outside.yml".to_owned();
    }
    assert!(refused(&record), "a name outside the patches directory");
}
