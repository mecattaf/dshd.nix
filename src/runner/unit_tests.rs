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
