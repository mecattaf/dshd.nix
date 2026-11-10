//! The complete error kind/exit contract and remedy regressions.

use super::*;

// catches: an error kind moved to another exit code, which scripts branch on
#[test]
fn exit_codes_follow_the_table() {
    let mut rows = usage_cases();
    rows.extend(state_cases());
    rows.extend(host_cases());
    for (error, kind, code) in rows {
        assert_eq!((error.kind(), error.exit_code()), (kind, code), "{error:?}");
        known_variant(&error);
    }
}

fn usage_cases() -> Vec<(Error, &'static str, u8)> {
    vec![
        (
            Error::InvalidName {
                name: "bad".into(),
                reason: "grammar",
            },
            "invalid-name",
            2,
        ),
        (Error::InvalidArgument("flag".into()), "invalid-argument", 2),
        (
            Error::NeedsTarget {
                verb: "stop",
                candidates: None,
            },
            "needs-target",
            2,
        ),
        (Error::CreateWithRun("web".into()), "invalid-argument", 2),
        (Error::PathPinned("x".into()), "invalid-argument", 2),
        (Error::NotFound("x".into()), "not-found", 3),
        (Error::ReleaseNotFound("x".into()), "release-not-found", 3),
    ]
}

fn state_cases() -> Vec<(Error, &'static str, u8)> {
    vec![
        (
            Error::ReleaseInvalid {
                release: "x".into(),
                reason: "layout".into(),
            },
            "release-invalid",
            1,
        ),
        (
            Error::ExistsDifferentRecipe(Box::new(RecipeDifference {
                name: "x".into(),
                fields: vec![],
            })),
            "exists-different-recipe",
            4,
        ),
        (Error::InstanceRunning("x".into()), "instance-running", 4),
        (Error::TaskAlreadyRan("x".into()), "task-already-ran", 4),
        (Error::UnitMasked("x".into()), "unit-masked", 4),
        (Error::NotRunning("x".into()), "not-running", 5),
        (
            Error::AttachedElsewhere("x".into()),
            "attached-elsewhere",
            4,
        ),
        (
            Error::UnsupportedTransport {
                name: "x".into(),
                transport: "none",
            },
            "unsupported-transport",
            6,
        ),
        (
            Error::UnsupportedProfile {
                profile: "x".into(),
                app: UnheldApp::Sdk,
            },
            "unsupported-profile",
            6,
        ),
        (
            Error::AutostartNotAllowed("x".into()),
            "autostart-not-allowed",
            6,
        ),
        (
            Error::InconsistentState {
                name: "x".into(),
                reason: "keys".into(),
            },
            "inconsistent-state",
            1,
        ),
    ]
}

fn host_cases() -> Vec<(Error, &'static str, u8)> {
    let failure = |tool| {
        Error::Tool(Box::new(ToolFailure {
            tool,
            args: vec![],
            status: Some(1),
            stderr: "cause".into(),
        }))
    };
    vec![
        (
            Error::SystemdUnavailable("bus".into()),
            "systemd-unavailable",
            7,
        ),
        (
            Error::ForeignSession("user".into()),
            "systemd-unavailable",
            7,
        ),
        (Error::ToolMissing("systemctl"), "tool-missing", 7),
        (Error::TemplateMissing, "template-missing", 7),
        (failure("systemctl"), "systemctl-failed", 1),
        (failure("nix-store"), "release-invalid", 1),
        (failure("journalctl"), "io", 1),
        (
            Error::StartFailed(Box::new(StartFailure {
                name: "x".into(),
                active_state: "failed".into(),
                result: "exit-code".into(),
                journal_tail: vec![],
            })),
            "start-failed",
            1,
        ),
        (
            Error::StopFailed {
                name: "x".into(),
                detail: "denied".into(),
            },
            "stop-failed",
            1,
        ),
        (
            Error::io(
                "read",
                "/record",
                io::Error::from(io::ErrorKind::PermissionDenied),
            ),
            "io",
            1,
        ),
        (
            Error::Output(io::Error::from(io::ErrorKind::BrokenPipe)),
            "io",
            0,
        ),
        (
            Error::Output(io::Error::from(io::ErrorKind::Other)),
            "io",
            1,
        ),
    ]
}

// An added variant requires an explicit contract-table audit.
fn known_variant(error: &Error) {
    match error {
        Error::InvalidName { .. }
        | Error::InvalidArgument(_)
        | Error::NeedsTarget { .. }
        | Error::CreateWithRun(_)
        | Error::PathPinned(_)
        | Error::NotFound(_)
        | Error::ReleaseNotFound(_)
        | Error::ReleaseInvalid { .. }
        | Error::ExistsDifferentRecipe(_)
        | Error::InstanceRunning(_)
        | Error::TaskAlreadyRan(_)
        | Error::UnitMasked(_)
        | Error::NotRunning(_)
        | Error::AttachedElsewhere(_)
        | Error::UnsupportedTransport { .. }
        | Error::UnsupportedProfile { .. }
        | Error::AutostartNotAllowed(_)
        | Error::InconsistentState { .. }
        | Error::SystemdUnavailable(_)
        | Error::ForeignSession(_)
        | Error::ToolMissing(_)
        | Error::TemplateMissing
        | Error::Tool(_)
        | Error::StartFailed(_)
        | Error::StopFailed { .. }
        | Error::Io { .. }
        | Error::Output(_) => {}
    }
}

// catches: a systemctl or nix-store failure reported under the generic io kind
#[test]
fn tool_kind_depends_on_tool() {
    let failure = |tool| {
        Error::Tool(Box::new(ToolFailure {
            tool,
            args: vec![],
            status: Some(1),
            stderr: String::new(),
        }))
    };
    assert_eq!(failure("systemctl").kind(), "systemctl-failed");
    assert_eq!(failure("nix-store").kind(), "release-invalid");
    assert_eq!(failure("journalctl").kind(), "io");
}

// catches: a remedy that sends the user to the wrong place for a missing bus or a headless instance
#[test]
fn cause_specific_hints() {
    let hint = |error: Error| error.hint().unwrap();
    assert!(hint(Error::SystemdUnavailable(String::new())).starts_with("set users.users."));
    assert!(hint(Error::ForeignSession(String::new())).starts_with("log in as this user"));
    let none = Error::UnsupportedTransport {
        name: "batch".into(),
        transport: "none",
    };
    assert_eq!(hint(none), "dshd read --follow batch");
    let web = Error::UnsupportedTransport {
        name: "web-1".into(),
        transport: "web",
    };
    assert_eq!(
        web.to_string(),
        "web-1 has transport web; --via names the other transport"
    );
    assert_eq!(hint(web), "dshd attach web-1");
    let needs = |candidates| {
        Error::NeedsTarget {
            verb: "stop",
            candidates,
        }
        .to_string()
    };
    assert_eq!(
        needs(Some(vec!["a".into(), "b".into()])),
        "stop: which instance? a, b"
    );
    assert_eq!(
        needs(Some(vec![])),
        "stop: no NAME given, and no instance exists"
    );
    assert_eq!(needs(None), "stop needs the NAME of an instance");
}

// catches: an error type grown past the clippy result_large_err threshold
#[test]
fn error_stays_small() {
    assert!(size_of::<Error>() <= 64, "{}", size_of::<Error>());
}
