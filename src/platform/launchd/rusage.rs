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
    let Some(usage) = usage else {
        return RunResources::default();
    };
    let cpu_usage_nsec = usage
        .user_usec
        .checked_add(usage.system_usec)
        .and_then(|usec| usec.checked_mul(1_000));
    RunResources {
        cpu_usage_nsec,
        memory_peak: Some(usage.max_rss_bytes),
        ..RunResources::default()
    }
}

/// `get.resources` summed over the live runner's process tree. Any
/// sample that is `None` (its call failed) makes every summed field
/// `None`; no timebase, or a zero denominator, makes the CPU `None`; an
/// overflowing sum is `None`; `memoryPeak` is always `None` (there is no
/// peak for the whole tree). An empty tree is every field `None`.
#[must_use]
pub fn live_counters(samples: &[Option<TaskSample>], timebase: Option<Timebase>) -> Resources {
    if samples.is_empty() {
        return Resources::default();
    }
    let mut ticks: Option<u128> = Some(0);
    let mut memory: Option<u64> = Some(0);
    let mut tasks: Option<u64> = Some(0);
    let mut io_read: Option<u64> = Some(0);
    let mut io_written: Option<u64> = Some(0);
    for sample in samples {
        let Some(sample) = sample else {
            return Resources::default();
        };
        ticks = ticks.and_then(|sum| {
            sum.checked_add(u128::from(sample.user_ticks))
                .and_then(|sum| sum.checked_add(u128::from(sample.system_ticks)))
        });
        memory = memory.and_then(|sum| sum.checked_add(sample.phys_footprint));
        tasks = tasks.and_then(|sum| sum.checked_add(sample.threads));
        io_read = io_read.and_then(|sum| sum.checked_add(sample.disk_read));
        io_written = io_written.and_then(|sum| sum.checked_add(sample.disk_written));
    }
    let cpu_usage_nsec = match timebase {
        Some(timebase) if timebase.denominator != 0 => ticks
            .and_then(|sum| {
                sum.checked_mul(u128::from(timebase.numerator))
                    .map(|scaled| scaled / u128::from(timebase.denominator))
            })
            .and_then(|nsec| u64::try_from(nsec).ok()),
        _ => None,
    };
    Resources {
        cpu_usage_nsec,
        memory_current: memory,
        memory_peak: None,
        tasks_current: tasks,
        io_read_bytes: io_read,
        io_write_bytes: io_written,
    }
}

#[cfg(test)]
#[path = "rusage_tests.rs"]
mod tests;
