//! Tests of dsh's process tree under launchd: the `ps` parser, the
//! tracker, the stop of what dsh left behind against a fake table and, on
//! Linux, against real processes through `/proc`.

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

// catches: a ps row misread (a column shifted by lstart's spaces or a padded day), a zombie taken for a live process,
// or a malformed row turned into a process
#[test]
fn parses_ps_rows_in_both_layouts() {
    let text = "\
      1       0       1 Ss   Sun Nov  8 22:04:59 2026
  501     1   501 S    Mon Nov 16 06:12:01 2026
  777   501   501 R+   Mon Nov 16 06:12:02 2026
  778   777   501 Z    Mon Nov 16 06:12:03 2026
  779   777   501 S    Xyz Nov 16 06:12:03
  780   777   501 S    Mon Foo 16 06:12:03 2026
  781   777   501 S    Mon Nov 16 06:12 2026
garbage
";
    // The stamps read YYYYMMDDhhmmss.
    assert_eq!(
        parse_ps(text),
        [
            entry(1, 0, 1, 20_261_108_220_459),
            entry(501, 1, 501, 20_261_116_061_201),
            entry(777, 501, 501, 20_261_116_061_202),
        ]
    );
}

// catches: a ps call that would print a header, read a locale's month names, or swallow a keyword into a header
#[test]
fn ps_is_called_with_one_column_per_option() {
    let table = PsTable::new();
    assert_eq!(table.ps.program, std::ffi::OsString::from("/bin/ps"));
    assert_eq!(PS_ARGS.iter().filter(|arg| **arg == "-o").count(), 5);
    assert!(
        PS_ARGS
            .iter()
            .filter(|arg| **arg != "-o" && **arg != "-A")
            .all(|arg| arg.ends_with('=') && !arg.contains(','))
    );
    assert_eq!(PS_ARGS.last(), Some(&"lstart="));
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

/// Spawns `sh -c SCRIPT`, which prints `up` once its children exist, and
/// returns it after that line.
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
#[test]
fn real_orphans_are_stopped() {
    use procfs::ProcTable;

    let table = ProcTable;
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
    let orphans_alive = |table: &ProcTable| {
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
