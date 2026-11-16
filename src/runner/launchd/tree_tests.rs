//! Tests of dsh's process tree under launchd: the tracker, the stop of
//! what dsh left behind against a fake table and against real processes
//! through the native table (libproc on macOS, `/proc` on Linux).

use std::collections::BTreeSet;
use std::sync::Mutex;

use super::*;

fn entry(pid: u32, parent: u32, group: u32, start: u64) -> ProcessEntry {
    ProcessEntry {
        pid,
        ppid: parent,
        pgid: group,
        start,
    }
}

/// A process table in memory. SIGTERM ends every process but the stubborn
/// ones; SIGKILL ends any.
#[derive(Default)]
struct FakeTable {
    entries: Mutex<Vec<ProcessEntry>>,
    stubborn: BTreeSet<u32>,
    signals: Mutex<Vec<(u32, i32)>>,
}

impl FakeTable {
    fn with(entries: Vec<ProcessEntry>) -> Self {
        Self {
            entries: Mutex::new(entries),
            ..Self::default()
        }
    }

    fn signals(&self) -> Vec<(u32, i32)> {
        self.signals.lock().unwrap().clone()
    }

    fn pids(&self) -> Vec<u32> {
        self.entries.lock().unwrap().iter().map(|e| e.pid).collect()
    }
}

impl ProcessTable for FakeTable {
    fn snapshot(&self) -> Result<Vec<ProcessEntry>, Error> {
        Ok(self.entries.lock().unwrap().clone())
    }

    fn signal(&self, pid: u32, signal: i32) -> Result<bool, Error> {
        self.signals.lock().unwrap().push((pid, signal));
        let mut entries = self.entries.lock().unwrap();
        let present = entries.iter().any(|e| e.pid == pid);
        if signal == libc::SIGKILL || !self.stubborn.contains(&pid) {
            entries.retain(|e| e.pid != pid);
        }
        Ok(present)
    }
}

// catches: a grandchild missed, the runner or an unrelated process recorded, an ended process kept so its reused
// pid would be signalled, or a reused dsh pid whose new children get recorded
#[test]
fn the_tracker_records_dsh_s_tree_only() {
    let runner = entry(100, 1, 100, 1);
    let dsh = entry(200, 100, 100, 2);
    let child = entry(300, 200, 100, 3);
    let grandchild = entry(400, 300, 400, 4);
    let unrelated = entry(500, 1, 500, 5);
    let mut tracker = Tracker::default();
    tracker.observe(&[runner, dsh, child, grandchild, unrelated], 200);
    assert_eq!(
        tracker
            .recorded()
            .iter()
            .map(|(p, s)| (*p, *s))
            .collect::<Vec<_>>(),
        [(300, 3), (400, 4)]
    );

    // The child ends; its pid comes back as another process under init.
    let reused = entry(300, 1, 300, 9);
    tracker.observe(&[runner, dsh, reused, grandchild], 200);
    assert_eq!(
        tracker.recorded().keys().copied().collect::<Vec<_>>(),
        [400]
    );

    // dsh ends and its pid is reused: what the new process starts is not dsh's.
    let new_dsh = entry(200, 1, 200, 10);
    let its_child = entry(600, 200, 200, 11);
    tracker.observe(&[runner, new_dsh, its_child, grandchild], 200);
    assert_eq!(
        tracker.recorded().keys().copied().collect::<Vec<_>>(),
        [400]
    );
}

// catches: dsh's identity taken from a later snapshot than the first that held it, or a tracker that records
// before dsh appears
#[test]
fn the_root_is_fixed_at_first_sight() {
    let mut tracker = Tracker::default();
    tracker.observe(&[entry(7, 1, 7, 1)], 200);
    assert!(tracker.recorded().is_empty());
    assert_eq!(tracker.root, None);
    tracker.observe(&[entry(200, 100, 100, 2), entry(201, 200, 100, 3)], 200);
    assert_eq!(tracker.root, Some((200, 2)));
    assert_eq!(tracker.recorded().len(), 1);
}

// catches: the runner or its direct children (a ps it ran) signalled, a recorded pid that now names another process
// signalled, or the group stopped when the runner does not lead it
#[test]
fn targets_are_the_run_s_processes_only() {
    let mut tracker = Tracker::default();
    let dsh = entry(200, 100, 100, 2);
    let escaped = entry(300, 200, 300, 3);
    let reused_later = entry(301, 200, 100, 4);
    tracker.observe(&[entry(100, 1, 100, 1), dsh, escaped, reused_later], 200);

    let runner = entry(100, 1, 100, 1);
    let ps = entry(150, 100, 100, 5);
    let orphan_in_group = entry(250, 1, 100, 6);
    let escaped_now = entry(300, 1, 300, 3);
    let other_process_same_pid = entry(301, 1, 301, 99);
    let stranger = entry(900, 1, 900, 7);
    let now = [
        runner,
        ps,
        orphan_in_group,
        escaped_now,
        other_process_same_pid,
        stranger,
    ];
    assert_eq!(
        tracker.targets(&now, 100, Some(100)),
        [orphan_in_group, escaped_now]
    );
    assert_eq!(tracker.targets(&now, 100, None), [escaped_now]);
    assert_eq!(led_group(&now, 100), Some(100));
    assert_eq!(led_group(&[entry(100, 1, 42, 1)], 100), None);
    assert_eq!(led_group(&[], 100), None);
}

// catches: leftovers never sent SIGTERM first, a stubborn one never killed, a process killed that ended at SIGTERM,
// or a signal sent when nothing was left
#[test]
fn leftovers_get_term_then_kill() {
    let runner = entry(100, 1, 100, 1);
    let mut tracker = Tracker::default();
    tracker.observe(
        &[
            runner,
            entry(200, 100, 100, 2),
            entry(300, 200, 300, 3),
            entry(301, 200, 301, 4),
        ],
        200,
    );
    let table = FakeTable {
        stubborn: BTreeSet::from([301]),
        ..FakeTable::with(vec![runner, entry(300, 1, 300, 3), entry(301, 1, 301, 4)])
    };
    let stopped = stop_leftovers(
        &table,
        &tracker,
        100,
        Some(100),
        Duration::from_millis(50),
        Duration::from_millis(10),
    );
    assert_eq!(stopped, 2);
    assert_eq!(
        table.signals(),
        [(300, SIGTERM), (301, SIGTERM), (301, libc::SIGKILL)]
    );
    assert_eq!(table.pids(), [100]);

    let quiet = FakeTable::with(vec![runner]);
    assert_eq!(
        stop_leftovers(
            &quiet,
            &Tracker::default(),
            100,
            Some(100),
            Duration::ZERO,
            Duration::ZERO
        ),
        0
    );
    assert!(quiet.signals().is_empty());
}

// catches: a sampler that never samples before dsh's first second, keeps running after finish, or loses its records
#[test]
fn the_sampler_records_until_finished() {
    let table = Arc::new(FakeTable::with(vec![
        entry(200, 100, 100, 2),
        entry(300, 200, 100, 3),
    ]));
    let sampler = Sampler::start(table, 200, Duration::from_secs(3600)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let seen = sampler
            .tracker
            .lock()
            .unwrap()
            .recorded()
            .contains_key(&300);
        if seen || Instant::now() > deadline {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let tracker = sampler.finish();
    assert_eq!(
        tracker.recorded().keys().copied().collect::<Vec<_>>(),
        [300]
    );
}

/// The Linux process table, for tests with real processes.
#[cfg(target_os = "linux")]
mod procfs {
    use super::*;

    /// `/proc/<pid>/stat`, zombies left out; the start is in clock ticks
    /// since boot.
    pub(super) struct ProcTable;

    impl ProcessTable for ProcTable {
        fn snapshot(&self) -> Result<Vec<ProcessEntry>, Error> {
            let mut entries = Vec::new();
            for dir in std::fs::read_dir("/proc").unwrap() {
                let dir = dir.unwrap();
                let Some(pid) = dir.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                    continue;
                };
                let Ok(stat) = std::fs::read_to_string(dir.path().join("stat")) else {
                    continue;
                };
                // The command name may hold spaces and parentheses; the
                // fields after its closing parenthesis do not.
                let (_, after) = stat.rsplit_once(") ").unwrap();
                let fields: Vec<&str> = after.split(' ').collect();
                let field = |index: usize| *fields.get(index).unwrap();
                if matches!(field(0), "Z" | "X") {
                    continue;
                }
                entries.push(ProcessEntry {
                    pid,
                    ppid: field(1).parse().unwrap(),
                    pgid: field(2).parse().unwrap(),
                    start: field(19).parse().unwrap(),
                });
            }
            Ok(entries)
        }

        fn signal(&self, pid: u32, signal: i32) -> Result<bool, Error> {
            sys::kill(pid, signal)
        }
    }
}

/// The table of the platform the tests run on.
#[cfg(target_os = "linux")]
fn native_table() -> procfs::ProcTable {
    procfs::ProcTable
}

/// The table of the platform the tests run on.
#[cfg(target_os = "macos")]
fn native_table() -> LibprocTable {
    LibprocTable
}

/// Spawns `sh -c SCRIPT`, which prints `up` once its children exist, and
/// returns it after that line.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn spawn_tree(script: &str) -> std::process::Child {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    let mut child = Command::new("sh")
        .args(["-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line, "up\n");
    child
}

// catches: real orphans of a dead dsh (reparented, so no longer in its tree) surviving the run, a stubborn one
// surviving SIGTERM, or another process of the machine touched
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn real_orphans_are_stopped() {
    let table = native_table();
    let mut dsh = spawn_tree("sleep 300 & (trap '' TERM; exec sleep 301) & echo up; wait");
    let root = dsh.id();
    let mut tracker = Tracker::default();
    let deadline = Instant::now() + Duration::from_secs(10);
    while tracker.recorded().len() < 2 && Instant::now() < deadline {
        tracker.observe(&table.snapshot().unwrap(), root);
        thread::sleep(Duration::from_millis(10));
    }
    let recorded: Vec<(u32, u64)> = tracker.recorded().iter().map(|(p, s)| (*p, *s)).collect();
    assert_eq!(recorded.len(), 2, "{recorded:?}");

    dsh.kill().unwrap();
    dsh.wait().unwrap();
    let orphans_alive = |table: &dyn ProcessTable| {
        let now = table.snapshot().unwrap();
        recorded
            .iter()
            .filter(|(pid, start)| now.iter().any(|e| e.pid == *pid && e.start == *start))
            .count()
    };
    assert_eq!(orphans_alive(&table), 2, "the orphans outlive dsh");

    let stopped = stop_leftovers(
        &table,
        &tracker,
        std::process::id(),
        None,
        Duration::from_millis(300),
        Duration::from_millis(20),
    );
    assert_eq!(stopped, 2);
    let deadline = Instant::now() + Duration::from_secs(10);
    while orphans_alive(&table) > 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(orphans_alive(&table), 0, "an orphan survived");
}

// catches: the libproc table missing this process or its child, misreading the parent or the group,
// listing a zombie as live, or giving a start that changes between snapshots (so a recorded process
// would read as ended, or a reused pid as the same process)
#[cfg(target_os = "macos")]
#[test]
fn the_libproc_table_describes_live_processes() {
    use std::process::Command;

    let table = LibprocTable;
    let me = std::process::id();
    let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let first = table.snapshot().unwrap();
    let mine = *first.iter().find(|entry| entry.pid == me).unwrap();
    let theirs = *first.iter().find(|entry| entry.pid == child.id()).unwrap();
    assert_eq!(theirs.ppid, me);
    assert_eq!(
        theirs.pgid, mine.pgid,
        "a child stays in its parent's group"
    );
    let again = table.snapshot().unwrap();
    assert!(
        again.contains(&mine) && again.contains(&theirs),
        "{again:?}"
    );
    child.kill().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while table
        .snapshot()
        .unwrap()
        .iter()
        .any(|entry| entry.pid == theirs.pid && entry.start == theirs.start)
    {
        assert!(Instant::now() < deadline, "a zombie is listed as live");
        thread::sleep(Duration::from_millis(10));
    }
    child.wait().unwrap();
}
