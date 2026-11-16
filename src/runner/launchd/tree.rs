//! dsh's descendants under launchd, which has no cgroup to hold them. The
//! runner samples the process table every [`SAMPLE_INTERVAL`] while dsh
//! runs and records every descendant of dsh with its start stamp
//! ([`Tracker`]). When dsh has ended it stops what is left
//! ([`stop_leftovers`]): the recorded descendants that still run as the
//! same process, and the members of the runner's own process group when
//! the runner leads it (launchd makes each job a group of its own), never
//! the runner itself or its direct children. SIGTERM first, SIGKILL to
//! whatever is still there after [`LEFTOVER_GRACE`].
//!
//! A process that leaves both the process group and dsh's tree between two
//! samples (a double fork into a new session within a second) is not seen
//! and can survive; on Linux the unit's cgroup holds it.
//!
//! The table goes through [`ProcessTable`]. On macOS it is `/bin/ps`, one
//! call per snapshot ([`PsTable`]); the tests use a fake and, on Linux,
//! `/proc`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::error::Error;
use crate::runner::log_line;
use crate::sys::{self, SIGTERM};
use crate::tool::Tool;

/// How often dsh's tree is sampled while dsh runs.
pub(crate) const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// How long what dsh left behind gets between SIGTERM and SIGKILL.
pub(crate) const LEFTOVER_GRACE: Duration = Duration::from_secs(2);
/// How often the leftovers are looked at again within the grace.
pub(crate) const LEFTOVER_POLL: Duration = Duration::from_millis(100);
/// Name of the sampling thread.
const SAMPLER_THREAD: &str = "dshd-tree";
/// The process table every macOS has, outside any package manager.
const PS_PROGRAM: &str = "/bin/ps";
/// One `-o` per column, so no header text can swallow the next keyword;
/// `lstart` last, since it holds spaces.
const PS_ARGS: [&str; 11] = [
    "-A", "-o", "pid=", "-o", "ppid=", "-o", "pgid=", "-o", "state=", "-o", "lstart=",
];
/// Month abbreviations of `lstart` in the C locale.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// One process in a snapshot of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessEntry {
    /// Its pid.
    pub(crate) pid: u32,
    /// Its parent's pid.
    pub(crate) ppid: u32,
    /// Its process group.
    pub(crate) pgid: u32,
    /// When it started, in a unit the table chooses: compared only for
    /// equality, so a reused pid reads as another process.
    pub(crate) start: u64,
}

/// The processes of the machine, as the runner reads and signals them.
pub(crate) trait ProcessTable: Send + Sync {
    /// Every live process (zombies left out).
    fn snapshot(&self) -> Result<Vec<ProcessEntry>, Error>;

    /// Sends `signal` to `pid`; `Ok(false)` when it is gone.
    fn signal(&self, pid: u32, signal: i32) -> Result<bool, Error>;
}

/// The table through `/bin/ps -A`.
#[derive(Debug, Clone)]
pub(crate) struct PsTable {
    ps: Tool,
}

impl PsTable {
    /// `/bin/ps`.
    pub(crate) fn new() -> Self {
        Self {
            ps: Tool::at("ps", PS_PROGRAM),
        }
    }
}

impl ProcessTable for PsTable {
    fn snapshot(&self) -> Result<Vec<ProcessEntry>, Error> {
        Ok(parse_ps(&self.ps.run(PS_ARGS)?.stdout_text()))
    }

    fn signal(&self, pid: u32, signal: i32) -> Result<bool, Error> {
        sys::kill(pid, signal)
    }
}

/// The rows of `ps -A -o pid= -o ppid= -o pgid= -o state= -o lstart=` in
/// the C locale. A zombie (state `Z...`) is left out, and so is a row that
/// does not parse.
pub(crate) fn parse_ps(text: &str) -> Vec<ProcessEntry> {
    text.lines().filter_map(parse_ps_row).collect()
}

fn parse_ps_row(line: &str) -> Option<ProcessEntry> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let [id, parent, group, state, _weekday, month, day, time, year] = fields.as_slice() else {
        return None;
    };
    if state.starts_with('Z') {
        return None;
    }
    Some(ProcessEntry {
        pid: id.parse().ok()?,
        ppid: parent.parse().ok()?,
        pgid: group.parse().ok()?,
        start: lstart_stamp(month, day, time, year)?,
    })
}

/// `Mon DD HH:MM:SS YYYY` as the number `YYYYMMDDHHMMSS`: one-second
/// resolution, in local time, only ever compared for equality.
fn lstart_stamp(month: &str, day: &str, time: &str, year: &str) -> Option<u64> {
    let month = MONTHS.iter().position(|name| *name == month)?;
    let month = u64::try_from(month).ok()? + 1;
    let day: u64 = day.parse().ok()?;
    let mut clock = time.split(':').map(str::parse::<u64>);
    let (Some(Ok(hours)), Some(Ok(minutes)), Some(Ok(seconds)), None) =
        (clock.next(), clock.next(), clock.next(), clock.next())
    else {
        return None;
    };
    let year: u64 = year.parse().ok()?;
    if day > 31 || hours > 23 || minutes > 59 || seconds > 60 || year > 9999 {
        return None;
    }
    Some(((((year * 100 + month) * 100 + day) * 100 + hours) * 100 + minutes) * 100 + seconds)
}

/// What the runner knows of dsh's descendants.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Tracker {
    /// dsh itself, `(pid, start)`, from the first snapshot that held it.
    root: Option<(u32, u64)>,
    /// Every descendant seen and still alive at the last snapshot: pid to
    /// start.
    seen: BTreeMap<u32, u64>,
}

impl Tracker {
    /// Takes in one snapshot: forgets the recorded processes that ended
    /// (their pid is absent or names a later process), then, while dsh
    /// (`root_pid`) still runs as the process first seen, records every
    /// process below it.
    pub(crate) fn observe(&mut self, snapshot: &[ProcessEntry], root_pid: u32) {
        let by_pid: BTreeMap<u32, &ProcessEntry> =
            snapshot.iter().map(|entry| (entry.pid, entry)).collect();
        self.seen
            .retain(|pid, start| by_pid.get(pid).is_some_and(|entry| entry.start == *start));
        let root = if let Some(root) = self.root {
            root
        } else {
            let Some(entry) = by_pid.get(&root_pid) else {
                return;
            };
            *self.root.insert((root_pid, entry.start))
        };
        if by_pid.get(&root.0).map(|entry| entry.start) != Some(root.1) {
            return;
        }
        let mut children: BTreeMap<u32, Vec<&ProcessEntry>> = BTreeMap::new();
        for entry in snapshot {
            children.entry(entry.ppid).or_default().push(entry);
        }
        let mut visited = BTreeSet::from([root.0]);
        let mut queue = vec![root.0];
        while let Some(parent) = queue.pop() {
            for child in children.get(&parent).into_iter().flatten() {
                if visited.insert(child.pid) {
                    self.seen.insert(child.pid, child.start);
                    queue.push(child.pid);
                }
            }
        }
    }

    /// The recorded descendants, pid to start.
    #[cfg(test)]
    pub(crate) fn recorded(&self) -> &BTreeMap<u32, u64> {
        &self.seen
    }

    /// The processes to stop now: the recorded descendants that still run
    /// as the process recorded, and the members of `group` when given;
    /// never `own` (the runner) nor a direct child of it. Sorted by pid.
    pub(crate) fn targets(
        &self,
        snapshot: &[ProcessEntry],
        own: u32,
        group: Option<u32>,
    ) -> Vec<ProcessEntry> {
        let mut targets: BTreeMap<u32, ProcessEntry> = BTreeMap::new();
        for entry in snapshot {
            if entry.pid == own || entry.ppid == own {
                continue;
            }
            let recorded = self.seen.get(&entry.pid) == Some(&entry.start);
            if recorded || group == Some(entry.pgid) {
                targets.insert(entry.pid, *entry);
            }
        }
        targets.into_values().collect()
    }
}

/// The process group the runner leads, from a snapshot: `own` when the
/// runner's group is its own pid, else `None` (the group then holds
/// processes that are not the run's, and none of them is signalled).
pub(crate) fn led_group(snapshot: &[ProcessEntry], own: u32) -> Option<u32> {
    snapshot
        .iter()
        .find(|entry| entry.pid == own)
        .and_then(|entry| (entry.pgid == own).then_some(own))
}

/// Stops what dsh left behind: SIGTERM to every target of `tracker` (see
/// [`Tracker::targets`]), then SIGKILL to those still running as the same
/// process after `grace`, read again every `poll`. Returns how many
/// processes were signalled. A table that cannot be read stops nothing
/// and is logged.
pub(crate) fn stop_leftovers(
    table: &dyn ProcessTable,
    tracker: &Tracker,
    own: u32,
    group: Option<u32>,
    grace: Duration,
    poll: Duration,
) -> usize {
    let snapshot = match table.snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            log_line(&format!(
                "cannot read the process table, so nothing dsh left behind is stopped: {error}"
            ));
            return 0;
        }
    };
    let targets = tracker.targets(&snapshot, own, group);
    if targets.is_empty() {
        return 0;
    }
    for target in &targets {
        send(table, target.pid, SIGTERM);
    }
    let deadline = Instant::now() + grace;
    let mut left = targets.clone();
    loop {
        match table.snapshot() {
            Ok(now) => left.retain(|target| still_runs(&now, target)),
            Err(error) => log_line(&format!("cannot read the process table: {error}")),
        }
        if left.is_empty() || Instant::now() >= deadline {
            break;
        }
        thread::sleep(poll);
    }
    for target in &left {
        send(table, target.pid, libc::SIGKILL);
    }
    let pids: Vec<String> = targets.iter().map(|t| t.pid.to_string()).collect();
    log_line(&format!(
        "stopped {} process(es) dsh left behind ({}); {} needed SIGKILL",
        targets.len(),
        pids.join(" "),
        left.len()
    ));
    targets.len()
}

/// Whether `target` is in `snapshot` as the same process; its parent may
/// have changed (an orphan is adopted).
fn still_runs(snapshot: &[ProcessEntry], target: &ProcessEntry) -> bool {
    snapshot
        .iter()
        .any(|entry| entry.pid == target.pid && entry.start == target.start)
}

fn send(table: &dyn ProcessTable, pid: u32, signal: i32) {
    if let Err(error) = table.signal(pid, signal) {
        log_line(&format!("cannot signal process {pid}: {error}"));
    }
}

/// Samples the table on its own thread while dsh runs.
#[derive(Debug)]
pub(crate) struct Sampler {
    stop: Sender<()>,
    thread: JoinHandle<()>,
    tracker: Arc<Mutex<Tracker>>,
}

impl Sampler {
    /// Starts sampling below `root` (dsh's pid) at once and then every
    /// `interval`. A snapshot that fails is skipped.
    pub(crate) fn start(
        table: Arc<dyn ProcessTable>,
        root: u32,
        interval: Duration,
    ) -> Result<Self, Error> {
        let tracker = Arc::new(Mutex::new(Tracker::default()));
        let (stop, stopped) = mpsc::channel::<()>();
        let shared = Arc::clone(&tracker);
        let thread = thread::Builder::new()
            .name(SAMPLER_THREAD.to_owned())
            .spawn(move || {
                loop {
                    if let Ok(snapshot) = table.snapshot() {
                        shared
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .observe(&snapshot, root);
                    }
                    match stopped.recv_timeout(interval) {
                        Err(RecvTimeoutError::Timeout) => {}
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .map_err(|source| Error::io("spawn thread", SAMPLER_THREAD, source))?;
        Ok(Self {
            stop,
            thread,
            tracker,
        })
    }

    /// Stops sampling and returns what was recorded.
    pub(crate) fn finish(self) -> Tracker {
        let Self {
            stop,
            thread,
            tracker,
        } = self;
        drop(stop);
        if thread.join().is_err() {
            log_line("the process tree sampler panicked; what it recorded is kept");
        }
        tracker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
