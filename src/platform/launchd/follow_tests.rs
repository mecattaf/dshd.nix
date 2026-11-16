//! The run log follower.

use std::io::Write;

use super::*;
use crate::fsx::testing::Scratch;

fn append(path: &Path, bytes: &[u8]) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

fn owned(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| (*line).to_owned()).collect()
}

// catches: `wait` or `run --json` re-reading lines already there when following from now
#[test]
fn from_now_skips_what_is_there() {
    let scratch = Scratch::new("ld-follow-now");
    let path = scratch.path().join("run.log");
    append(&path, b"old 1\nold 2\n");
    let mut follower = Follower::open(&path, Begin::Now).unwrap();
    assert!(follower.poll().unwrap().is_empty());
    append(&path, b"new\n");
    assert_eq!(follower.poll().unwrap(), owned(&["new"]));
}

// catches: `-f` from the start losing the first lines, or returning a line twice across polls
#[test]
fn from_start_reads_everything_once() {
    let scratch = Scratch::new("ld-follow-start");
    let path = scratch.path().join("run.log");
    append(&path, b"a\nb\n");
    let mut follower = Follower::open(&path, Begin::Start).unwrap();
    assert_eq!(follower.poll().unwrap(), owned(&["a", "b"]));
    assert!(follower.poll().unwrap().is_empty());
    append(&path, b"c\n");
    assert_eq!(follower.poll().unwrap(), owned(&["c"]));
}

// catches: a line handed out before its newline (split in two), or bad bytes failing the follow
#[test]
fn partial_lines_wait_for_their_newline() {
    let scratch = Scratch::new("ld-follow-partial");
    let path = scratch.path().join("run.log");
    let mut follower = Follower::open(&path, Begin::Start).unwrap();
    assert!(follower.poll().unwrap().is_empty(), "missing file");
    append(&path, b"abc");
    assert!(follower.poll().unwrap().is_empty());
    assert_eq!(follower.offset(), 3);
    append(&path, b"def \xff\nnext");
    assert_eq!(follower.poll().unwrap(), owned(&["abcdef \u{fffd}"]));
    append(&path, b"\n");
    assert_eq!(follower.poll().unwrap(), owned(&["next"]));
}

// catches: a follower stuck past the end of a replaced (shorter) file
#[test]
fn a_shrunk_file_is_read_again() {
    let scratch = Scratch::new("ld-follow-shrink");
    let path = scratch.path().join("run.log");
    append(&path, b"one long line\n");
    let mut follower = Follower::open(&path, Begin::Start).unwrap();
    assert_eq!(follower.poll().unwrap().len(), 1);
    std::fs::write(&path, b"x\n").unwrap();
    assert_eq!(follower.poll().unwrap(), owned(&["x"]));
}
