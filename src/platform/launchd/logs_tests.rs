//! The per-run log window.

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::fsx::testing::Scratch;

const ID: &str = "0123456789abcdef0123456789abcdef";

fn log_with(scratch: &Scratch, text: &[u8]) -> PathBuf {
    let path = scratch.path().join("run.log");
    std::fs::write(&path, text).unwrap();
    path
}

fn owned(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| (*line).to_owned()).collect()
}

// catches: a run log readable by others, a missing latest link, or a second create clobbering a live run's log
#[test]
fn create_makes_a_private_log_and_points_latest_at_it() {
    let scratch = Scratch::new("ld-logs-create");
    let runs = scratch.path().join("runs");
    let mut file = create(&runs, ID).unwrap();
    std::io::Write::write_all(&mut file, b"dshd-run: run started\n").unwrap();
    let path = run_log(&runs, ID);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let dir_mode = std::fs::metadata(&runs).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700);
    assert_eq!(
        std::fs::read_link(runs.join(LATEST)).unwrap().file_name(),
        path.file_name()
    );
    assert!(
        create(&runs, ID).is_err(),
        "a second create of the same run"
    );
    let other = "fedcba9876543210fedcba9876543210";
    drop(create(&runs, other).unwrap());
    assert_eq!(
        std::fs::read_link(runs.join(LATEST)).unwrap().file_name(),
        run_log(&runs, other).file_name()
    );
}

// catches: -n and --head windows that are off by one, reorder lines, or report complete when lines were cut
#[test]
fn windows_read_newest_and_oldest() {
    let scratch = Scratch::new("ld-logs-window");
    let path = log_with(&scratch, b"a\nb\nc\nd\ne\n");
    let newest = read_window(&path, Window::Newest(3)).unwrap();
    assert_eq!(
        newest,
        Lines {
            lines: owned(&["c", "d", "e"]),
            truncated: true
        }
    );
    let oldest = read_window(&path, Window::Oldest(3)).unwrap();
    assert_eq!(
        oldest,
        Lines {
            lines: owned(&["a", "b", "c"]),
            truncated: true
        }
    );
    let none = read_window(&path, Window::Oldest(0)).unwrap();
    assert_eq!(
        none,
        Lines {
            lines: Vec::new(),
            truncated: true
        }
    );
    let all = read_window(&path, Window::Newest(10)).unwrap();
    assert_eq!(
        all,
        Lines {
            lines: owned(&["a", "b", "c", "d", "e"]),
            truncated: false
        }
    );
}

// catches: a half-written last line shown (it changes under the reader), or invalid UTF-8 failing the read
#[test]
fn a_partial_last_line_is_left_out_and_bad_bytes_replaced() {
    let scratch = Scratch::new("ld-logs-partial");
    let path = log_with(&scratch, b"one\ntw\xffo\nthr");
    assert_eq!(
        read_window(&path, Window::Newest(5)).unwrap().lines,
        owned(&["one", "tw\u{fffd}o"])
    );
    assert_eq!(
        read_window(&path, Window::Oldest(5)).unwrap().lines,
        owned(&["one", "tw\u{fffd}o"])
    );
}

// catches: a removed run log read as an error instead of no lines (as for a run journald rotated away)
#[test]
fn a_missing_log_is_no_lines() {
    let scratch = Scratch::new("ld-logs-missing");
    let lines = read_window(&scratch.path().join("gone.log"), Window::Newest(5)).unwrap();
    assert_eq!(
        lines,
        Lines {
            lines: Vec::new(),
            truncated: false
        }
    );
}

// catches: a newest window over a large file that reads it all forwards (it must read from the end)
#[test]
fn newest_reads_a_large_file() {
    let scratch = Scratch::new("ld-logs-large");
    let mut text = String::new();
    for number in 0..200_000 {
        writeln!(text, "line {number}").unwrap();
    }
    let path = log_with(&scratch, text.as_bytes());
    let lines = read_window(&path, Window::Newest(2)).unwrap();
    assert_eq!(lines.lines, owned(&["line 199998", "line 199999"]));
}

// catches: a start-failed tail empty when the runner never opened a run log, or taken from an older run
#[test]
fn failure_tail_prefers_the_newest_run_then_launchd_log() {
    let scratch = Scratch::new("ld-logs-tail");
    let runs = scratch.path().join("runs");
    let launchd = scratch.path().join("launchd.log");
    assert!(failure_tail(&runs, &launchd, 5).unwrap().is_empty());
    std::fs::write(&launchd, "dyld: missing library\n").unwrap();
    assert_eq!(
        failure_tail(&runs, &launchd, 5).unwrap(),
        owned(&["dyld: missing library"])
    );
    let mut file = create(&runs, ID).unwrap();
    std::io::Write::write_all(&mut file, b"dshd-run: run started\nboom\n").unwrap();
    assert_eq!(failure_tail(&runs, &launchd, 1).unwrap(), owned(&["boom"]));
}
