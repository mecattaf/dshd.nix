//! Tests of the `launchctl print` parsers against the fixtures in
//! `tests/fixtures/launchd/print/`.

use std::path::{Path, PathBuf};

use super::*;

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/launchd/print")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn job(name: &str) -> JobFacts {
    parse_job(&fixture(name)).unwrap_or_else(|error| panic!("{name}: {error}"))
}

// catches: a running job read without its pid, or a pid taken from a nested block
#[test]
fn running_job_has_its_pid_and_state() {
    let facts = job("job-running.txt");
    assert_eq!(facts.state, JobState::Running);
    assert_eq!(facts.pid, Some(4242));
    assert_eq!(facts.runs, Some(1));
    assert_eq!(facts.last_exit, Some(LastExit::Never));
    assert_eq!(
        facts.path,
        Some(PathBuf::from(
            "/Users/u/.local/state/dshd/web-1/launchd.plist"
        ))
    );
}

// catches: a stopped job reported with a pid, or its exit status lost or misread with trailing text
#[test]
fn ended_jobs_keep_their_last_exit() {
    let clean = job("job-exited-0.txt");
    assert_eq!(clean.state, JobState::NotRunning);
    assert_eq!(clean.pid, None);
    assert_eq!(clean.last_exit, Some(LastExit::Code(0)));
    let config = job("job-exited-78.txt");
    assert_eq!(config.last_exit, Some(LastExit::Code(78)));
    assert_eq!(config.runs, Some(2));
    let killed = job("job-killed.txt");
    assert_eq!(killed.last_exit, Some(LastExit::Signal(9)));
    assert_eq!(killed.pid, None);
}

// catches: a job loaded but never spawned taken for a stopped or running one
#[test]
fn never_spawned_job_is_spawn_scheduled() {
    let facts = job("job-never-ran.txt");
    assert_eq!(facts.state, JobState::SpawnScheduled);
    assert_eq!(facts.runs, Some(0));
    assert_eq!(facts.pid, None);
}

// catches: an unknown state word mapped onto a known state instead of kept verbatim
#[test]
fn unknown_state_word_is_kept() {
    let facts = job("job-odd-state.txt");
    assert_eq!(facts.state, JobState::Other("penguin".to_owned()));
    assert_eq!(facts.pid, Some(4243));
}

// catches: text that is no job block accepted as a job with every fact unknown
#[test]
fn text_that_is_no_job_is_a_malformed_answer() {
    for text in ["", "Usage: launchctl print <service-target>\n", "garbage\n"] {
        match parse_job(text) {
            Err(Error::Tool(failure)) => {
                assert_eq!(failure.tool, "launchctl", "{text:?}");
                assert_eq!(failure.status, Some(0), "{text:?}");
            }
            other => panic!("{text:?}: {other:?}"),
        }
    }
}

// catches: a value that is not a number taken as zero instead of unknown
#[test]
fn unreadable_values_are_unknown() {
    let text = "gui/501/dshd.x = {\n\tstate = running\n\tpid = many\n\truns = -\n\tlast exit code = soon\n}\n";
    let facts = parse_job(text).unwrap();
    assert_eq!(facts.state, JobState::Running);
    assert_eq!(facts.pid, None);
    assert_eq!(facts.runs, None);
    assert_eq!(facts.last_exit, None);
    assert_eq!(facts.path, None);
}

// catches: services outside the services block listed, a pid of 0 read as a process, or a label with spaces cut
#[test]
#[ignore = "packet D03: launchctl domain and disabled parsers"]
fn domain_services_are_read_from_their_block_only() {
    let services = parse_domain_services(&fixture("domain.txt"));
    let labels: Vec<&str> = services.iter().map(|entry| entry.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "com.apple.cloudphotod",
            "com.apple.Finder",
            "dshd.web-1",
            "dshd.broken",
            "dshd.done",
            "dshd.Not_A-Name",
            "dshd.acp",
            "com.example.dshd.lookalike",
        ]
    );
    let entry = |label: &str| services.iter().find(|entry| entry.label == label).unwrap();
    assert_eq!(entry("dshd.web-1").pid, Some(4242));
    assert_eq!(entry("dshd.web-1").last_exit, None);
    assert_eq!(entry("dshd.broken").pid, None);
    assert_eq!(entry("dshd.broken").last_exit, Some(78));
    assert_eq!(entry("dshd.done").last_exit, Some(0));
}

// catches: an empty or unrelated domain text producing entries
#[test]
#[ignore = "packet D03: launchctl domain and disabled parsers"]
fn a_domain_without_services_has_none() {
    assert!(parse_domain_services("gui/501 = {\n\ttype = gui\n}\n").is_empty());
    assert!(parse_domain_services("").is_empty());
}

// catches: the disabled list read from the login-item block, or the old true/false words misread
#[test]
#[ignore = "packet D03: launchctl domain and disabled parsers"]
fn disabled_list_reads_both_vocabularies() {
    for name in ["disabled.txt", "disabled-legacy.txt"] {
        let disabled = parse_disabled(&fixture(name));
        assert_eq!(disabled.get("dshd.masked"), Some(&true), "{name}");
        assert_eq!(disabled.get("dshd.web-1"), Some(&false), "{name}");
        assert_eq!(
            disabled.get("com.apple.ScreenReaderUIServer"),
            Some(&true),
            "{name}"
        );
        assert_eq!(disabled.get("dshd.not-a-service"), None, "{name}");
    }
    assert!(parse_disabled("").is_empty());
}
