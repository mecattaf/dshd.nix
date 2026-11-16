//! Log retention selection.

use std::path::{Path, PathBuf};

use super::*;

fn log(index: u64, bytes: u64) -> LogFile {
    LogFile {
        path: PathBuf::from(format!("/runs/{index:032x}.log")),
        modified: 1_000 + index,
        bytes,
    }
}

// catches: retention that keeps more or fewer than 20 runs, or removes newer runs before older ones
#[test]
#[ignore = "packet D10: log retention"]
fn keeps_the_twenty_newest_runs() {
    let files: Vec<LogFile> = (0..25).map(|index| log(index, 10)).collect();
    let current = files.last().unwrap().path.clone();
    let removed = select(&files, &current);
    let expected: Vec<PathBuf> = (0..5).map(|index| log(index, 10).path).collect();
    assert_eq!(removed, expected);
}

// catches: the size bound ignored, applied to the running run, or removing newer ended runs first
#[test]
#[ignore = "packet D10: log retention"]
fn ended_runs_past_the_byte_bound_go_oldest_first() {
    let big = KEEP_BYTES / 2;
    let files = vec![
        log(1, big),
        log(2, big),
        log(3, big),
        log(4, KEEP_BYTES * 2),
    ];
    let removed = select(&files, &log(4, KEEP_BYTES * 2).path);
    assert_eq!(removed, [log(1, big).path]);
}

// catches: the current run's log, latest.log or a foreign file removed
#[test]
#[ignore = "packet D10: log retention"]
fn never_removes_current_latest_or_foreign_files() {
    let mut files: Vec<LogFile> = (0..30).map(|index| log(index, 10)).collect();
    let current = files.first().unwrap().path.clone();
    files.push(LogFile {
        path: PathBuf::from("/runs/latest.log"),
        modified: 1,
        bytes: 1,
    });
    files.push(LogFile {
        path: PathBuf::from("/runs/notes.txt"),
        modified: 1,
        bytes: 1,
    });
    let removed = select(&files, &current);
    assert!(!removed.contains(&current));
    assert!(
        !removed
            .iter()
            .any(|path| path == Path::new("/runs/latest.log"))
    );
    assert!(
        !removed
            .iter()
            .any(|path| path == Path::new("/runs/notes.txt"))
    );
    assert_eq!(removed.len(), 10, "{removed:?}");
}
