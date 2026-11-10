//! Unit tests for status.

use super::*;
use crate::systemctl::parse_show;

fn facts(fixture: &str) -> UnitFacts {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/show")
        .join(fixture);
    parse_show(&std::fs::read_to_string(path).unwrap())
        .unwrap()
        .pop()
        .unwrap()
}

fn name(text: &str) -> InstanceName {
    InstanceName::parse(text).unwrap()
}

// catches: any name of an installed template reported as an existing instance
#[test]
fn unknown_template_names_are_not_instances() {
    assert!(known_to_manager(&facts("running.txt")));
    assert!(
        known_to_manager(&facts("inactive.txt")),
        "it was active before"
    );
    assert!(!known_to_manager(&facts("not-found.txt")));
    let fresh = UnitFacts {
        inactive_enter: None,
        ..facts("inactive.txt")
    };
    assert!(!known_to_manager(&fresh), "a template loads any name");
    assert!(unit_up(&facts("running.txt")));
    assert!(!unit_up(&facts("inactive.txt")));
}

// catches: unit facts misread into the report (pid 0 shown, since from the wrong timestamp)
#[test]
fn bare_report_reads_the_unit() {
    let report = bare_report(
        &name("web-1"),
        &facts("running.txt"),
        StateSlug::Running,
        true,
        false,
    );
    assert_eq!(report.unit, name("web-1").unit());
    assert_eq!(report.active_state, "active");
    assert_eq!(report.sub_state, "running");
    assert_eq!(report.result, "success");
    assert_eq!(report.main_pid, Some(4242));
    assert_eq!(
        report.invocation_id.as_deref(),
        Some("0123456789abcdef0123456789abcdef")
    );
    assert_eq!(report.since, Some(1_794_009_610));
    let stopped = bare_report(
        &name("web-1"),
        &facts("inactive.txt"),
        StateSlug::Stopped,
        false,
        true,
    );
    assert_eq!((stopped.main_pid, stopped.invocation_id), (None, None));
    assert_eq!(stopped.since, Some(1_794_013_210));
    assert!(
        !report.linger && stopped.linger,
        "linger is reported as given"
    );
    assert_eq!(
        result_word(&UnitResult::Unknown("oom-kill".into())),
        "oom-kill"
    );
    assert_eq!(
        active_word(&ActiveState::Unknown("reloading".into())),
        "reloading"
    );
}

// catches: an outdated or stale release with no remedy, or the wrong one for its state
#[test]
fn release_remedies() {
    let web = name("web-1");
    let remedy = |release, state| release_remedy(release, state, &web);
    assert_eq!(
        remedy(ReleaseState::Outdated, StateSlug::Running).as_deref(),
        Some("dshd update -R web-1")
    );
    assert_eq!(
        remedy(ReleaseState::Outdated, StateSlug::Starting).as_deref(),
        Some("dshd update web-1")
    );
    assert_eq!(
        remedy(ReleaseState::Stale, StateSlug::Running).as_deref(),
        Some("dshd restart web-1")
    );
    assert_eq!(remedy(ReleaseState::Stale, StateSlug::Starting), None);
    assert_eq!(remedy(ReleaseState::Current, StateSlug::Running), None);
    assert_eq!(remedy(ReleaseState::Missing, StateSlug::Running), None);
}
