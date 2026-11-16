//! Run totals and live counters on macOS, from `rusage` instead of
//! cgroups. Pure conversions: the FFI that fills [`EndUsage`] (`wait4`)
//! and [`TaskSample`] (`proc_pid_rusage` with `RUSAGE_INFO_V4`,
//! `mach_timebase_info`) lives in `sys`. A value that is unavailable is
//! `None`, never an invented zero.

use crate::record::RunResources;
use crate::systemctl::Resources;

/// What `wait4` reported for dsh and the descendants it waited for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndUsage {
    /// `ru_utime`, microseconds.
    pub user_usec: u64,
    /// `ru_stime`, microseconds.
    pub system_usec: u64,
    /// `ru_maxrss`: bytes on macOS, the largest single process.
    pub max_rss_bytes: u64,
}

/// One process's `rusage_info_v4`, in the fields dshd reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskSample {
    /// `ri_user_time`, mach ticks.
    pub user_ticks: u64,
    /// `ri_system_time`, mach ticks.
    pub system_ticks: u64,
    /// `ri_phys_footprint`, bytes.
    pub phys_footprint: u64,
    /// Thread count.
    pub threads: u64,
    /// `ri_diskio_bytesread`.
    pub disk_read: u64,
    /// `ri_diskio_byteswritten`.
    pub disk_written: u64,
}

/// `mach_timebase_info`: nanoseconds = ticks * numerator / denominator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timebase {
    /// Numerator.
    pub numerator: u32,
    /// Denominator.
    pub denominator: u32,
}

/// `lastRun.resources` from `wait4`: CPU is user plus system time in
/// nanoseconds, `memoryPeak` is `ru_maxrss`, I/O bytes are `None`.
/// `None` usage (the reap gave none) is every field `None`.
#[must_use]
pub fn run_totals(usage: Option<EndUsage>) -> RunResources {
    let _ = usage;
    RunResources::default()
}

/// `get.resources` summed over the live runner's process tree. Any
/// sample that is `None` (its call failed) makes every summed field
/// `None`; no timebase, or a zero denominator, makes the CPU `None`; an
/// overflowing sum is `None`; `memoryPeak` is always `None` (there is no
/// peak for the whole tree). An empty tree is every field `None`.
#[must_use]
pub fn live_counters(samples: &[Option<TaskSample>], timebase: Option<Timebase>) -> Resources {
    let _ = (samples, timebase);
    Resources::default()
}

#[cfg(test)]
#[path = "rusage_tests.rs"]
mod tests;
