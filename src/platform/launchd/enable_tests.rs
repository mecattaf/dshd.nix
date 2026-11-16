//! `enable` and `disable` against the fake launchd.

use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::platform::launchd::plist;
use crate::platform::launchd::testing::Bench;

fn copy(bench: &Bench) -> std::path::PathBuf {
    bench.manager.agent_copy(&bench.name)
}

// catches: an enable that loads or starts the job, writes a symlink or a group-writable file, or a copy that differs
#[test]
#[ignore = "packet D06: launchd enable and disable"]
fn enable_writes_the_copy_and_calls_no_launchctl() {
    let bench = Bench::new("ld-enable", "web-1");
    let echoes = enable(&bench.manager, &bench.name).unwrap();
    let copy = copy(&bench);
    assert_eq!(
        copy,
        bench.home().join("Library/LaunchAgents/dshd.web-1.plist")
    );
    let metadata = std::fs::symlink_metadata(&copy).unwrap();
    assert!(metadata.file_type().is_file(), "not a regular file");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o644);
    let rendered = plist::render(&bench.manager.job_spec(&bench.name)).unwrap();
    assert_eq!(std::fs::read_to_string(&copy).unwrap(), rendered);
    assert_eq!(std::fs::read_to_string(bench.plist()).unwrap(), rendered);
    assert_eq!(
        echoes,
        [format!(
            "install -m 0644 {} {}",
            bench.plist(),
            copy.display()
        )]
    );
    assert!(bench.fake.calls().is_empty(), "{:?}", bench.fake.calls());
    assert_eq!(
        copy_state(&bench.manager, &bench.name).unwrap(),
        CopyState::Current
    );
}

// catches: a stale or edited copy left in place by enable, or reported as current
#[test]
#[ignore = "packet D06: launchd enable and disable"]
fn enable_rewrites_a_copy_that_differs() {
    let bench = Bench::new("ld-enable-differs", "web-1");
    let copy = copy(&bench);
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::write(&copy, "<plist>edited by hand</plist>\n").unwrap();
    assert_eq!(
        copy_state(&bench.manager, &bench.name).unwrap(),
        CopyState::Differs
    );
    enable(&bench.manager, &bench.name).unwrap();
    assert_eq!(
        copy_state(&bench.manager, &bench.name).unwrap(),
        CopyState::Current
    );
}

// catches: a symlink at the copy's path taken for a current copy
#[test]
#[ignore = "packet D06: launchd enable and disable"]
fn a_symlinked_copy_differs() {
    let bench = Bench::new("ld-enable-link", "web-1");
    enable(&bench.manager, &bench.name).unwrap();
    let copy = copy(&bench);
    std::fs::remove_file(&copy).unwrap();
    std::os::unix::fs::symlink(bench.plist(), &copy).unwrap();
    assert_eq!(
        copy_state(&bench.manager, &bench.name).unwrap(),
        CopyState::Differs
    );
}

// catches: a disable that unloads or stops the job, leaves the copy, or fails when there is none
#[test]
#[ignore = "packet D06: launchd enable and disable"]
fn disable_removes_the_copy_and_calls_no_launchctl() {
    let bench = Bench::new("ld-disable", "web-1");
    assert_eq!(
        copy_state(&bench.manager, &bench.name).unwrap(),
        CopyState::Absent
    );
    disable(&bench.manager, &bench.name).unwrap();
    enable(&bench.manager, &bench.name).unwrap();
    disable(&bench.manager, &bench.name).unwrap();
    assert!(!copy(&bench).exists());
    assert_eq!(
        copy_state(&bench.manager, &bench.name).unwrap(),
        CopyState::Absent
    );
    assert!(bench.fake.calls().is_empty(), "{:?}", bench.fake.calls());
}
