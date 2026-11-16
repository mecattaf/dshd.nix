//! Tests of the plist renderer and the key allow-list. The expected plists
//! are golden files under `tests/fixtures/launchd/plist/`: they are the
//! source of truth, written by hand from Apple's PLIST 1.0 format.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use super::*;
use crate::name::InstanceName;
use crate::platform::launchd::LaunchdManager;
use crate::platform::launchd::fake::FakeLaunchctl;

fn golden(case: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/launchd/plist")
        .join(format!("{case}.plist"));
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Fails with the first line that differs, so a mismatch is readable.
fn assert_golden(case: &str, actual: &str) {
    let expected = golden(case);
    if actual == expected {
        return;
    }
    let mismatch = expected
        .lines()
        .zip(actual.lines())
        .enumerate()
        .find(|(_, (want, got))| want != got);
    match mismatch {
        Some((index, (want, got))) => panic!(
            "{case}.plist, line {}: expected {want:?}, rendered {got:?}",
            index + 1
        ),
        None => panic!(
            "{case}.plist: same lines up to the shorter one; expected {} bytes, rendered {} bytes (check the final newline)",
            expected.len(),
            actual.len()
        ),
    }
}

fn spec(home: &str, name: &str) -> JobSpec {
    let manager = LaunchdManager::new(Box::new(FakeLaunchctl::default()), 501, PathBuf::from(home));
    manager.job_spec(&InstanceName::parse(name).unwrap())
}

// catches: the allow-list and the never-written list sharing a key, so a schedule or keep-alive key could be written
#[test]
fn allow_list_and_never_written_are_disjoint() {
    for key in NEVER_WRITTEN {
        assert!(!ALLOWED_KEYS.contains(key), "{key} is in both lists");
    }
    assert_eq!(ALLOWED_KEYS.len(), 10);
}

// catches: the job spec pointing the runner anywhere but the instance's current link, or logs outside the instance
#[test]
fn job_spec_runs_the_current_runner_in_the_instance() {
    let spec = spec("/Users/u", "web-1");
    assert_eq!(spec.label, "dshd.web-1");
    assert_eq!(
        spec.program,
        PathBuf::from("/Users/u/.local/state/dshd/web-1/current/bin/dshd-run")
    );
    assert_eq!(spec.arguments, ["run", "web-1"]);
    assert_eq!(
        spec.working_directory,
        PathBuf::from("/Users/u/.local/state/dshd/web-1")
    );
    assert_eq!(
        spec.log_path,
        PathBuf::from("/Users/u/.local/state/dshd/web-1/launchd.log")
    );
    assert_eq!(spec.exit_timeout, 30);
}

// catches: a rendered plist that differs in any byte from the golden file (keys, order, indentation, newline)
#[test]
#[ignore = "packet D01: plist renderer"]
fn renders_the_golden_web_instance() {
    assert_golden("web-1", &render(&spec("/Users/u", "web-1")).unwrap());
}

// catches: a path with XML metacharacters producing a plist launchd cannot parse
#[test]
#[ignore = "packet D01: plist renderer"]
fn escapes_xml_metacharacters_in_paths() {
    assert_golden(
        "escaped-home",
        &render(&spec("/Users/R&D <lab>", "chat")).unwrap(),
    );
}

// catches: the longest instance name truncated or rendered differently
#[test]
#[ignore = "packet D01: plist renderer"]
fn renders_the_longest_name() {
    assert_golden(
        "longest-name",
        &render(&spec("/Users/u", &"a".repeat(63))).unwrap(),
    );
}

// catches: a rendered plist whose top-level keys are not exactly the allow-list, in order
#[test]
#[ignore = "packet D01: plist renderer"]
fn rendered_keys_are_exactly_the_allow_list() {
    for name in ["web-1", "acp", "task-0001", &"z".repeat(63)] {
        let xml = render(&spec("/Users/u", name)).unwrap();
        assert_eq!(top_level_keys(&xml), ALLOWED_KEYS, "{name}");
        assert_eq!(check_keys(&xml), Ok(()), "{name}");
        for key in NEVER_WRITTEN {
            assert!(!xml.contains(&format!("<key>{key}</key>")), "{name}: {key}");
        }
    }
}

// catches: a schedule key or any key outside the allow-list passing the check
#[test]
#[ignore = "packet D01: plist renderer"]
fn check_keys_names_the_first_key_outside_the_list() {
    let with = |extra: &str| {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>dshd.x</string>\n{extra}</dict>\n</plist>\n"
        )
    };
    assert_eq!(check_keys(&with("")), Ok(()));
    assert_eq!(
        check_keys(&with(
            "\t<key>StartInterval</key>\n\t<integer>60</integer>\n"
        )),
        Err("StartInterval".to_owned())
    );
    assert_eq!(
        check_keys(&with(
            "\t<key>StartCalendarInterval</key>\n\t<dict>\n\t\t<key>Hour</key>\n\t\t<integer>3</integer>\n\t</dict>\n"
        )),
        Err("StartCalendarInterval".to_owned())
    );
    assert_eq!(
        check_keys(&with("\t<key>KeepAlive</key>\n\t<true/>\n")),
        Err("KeepAlive".to_owned())
    );
}

// catches: keys of a nested dictionary read as top-level keys, or top-level keys missed after a nested one
#[test]
#[ignore = "packet D01: plist renderer"]
fn nested_keys_are_not_top_level() {
    let xml = "<plist version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>x</string>\n\t<key>Outer</key>\n\t<dict>\n\t\t<key>Inner</key>\n\t\t<string>y</string>\n\t</dict>\n\t<key>RunAtLoad</key>\n\t<true/>\n</dict>\n</plist>\n";
    assert_eq!(top_level_keys(xml), ["Label", "Outer", "RunAtLoad"]);
}

// catches: a path that is not UTF-8 written lossily into the plist instead of refused
#[test]
#[ignore = "packet D01: plist renderer"]
fn a_path_that_is_not_utf8_is_refused() {
    let mut spec = spec("/Users/u", "web-1");
    spec.working_directory = PathBuf::from(OsString::from_vec(b"/Users/\xff".to_vec()));
    match render(&spec) {
        Err(Error::Io { action, .. }) => assert_eq!(action, "render"),
        other => panic!("{other:?}"),
    }
}
