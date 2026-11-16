//! The rusage conversions.

use super::*;

fn sample(ticks: u64) -> TaskSample {
    TaskSample {
        user_ticks: ticks,
        system_ticks: ticks,
        phys_footprint: 1_000,
        threads: 2,
        disk_read: 10,
        disk_written: 20,
    }
}

// catches: run totals in microseconds instead of nanoseconds, or invented I/O byte counts
#[test]
fn run_totals_from_wait4() {
    let totals = run_totals(Some(EndUsage {
        user_usec: 1_500_000,
        system_usec: 500_000,
        max_rss_bytes: 104_857_600,
    }));
    assert_eq!(totals.cpu_usage_nsec, Some(2_000_000_000));
    assert_eq!(totals.memory_peak, Some(104_857_600));
    assert_eq!(totals.io_read_bytes, None);
    assert_eq!(totals.io_write_bytes, None);
    assert_eq!(run_totals(None), RunResources::default());
}

// catches: Apple Silicon mach ticks reported as nanoseconds, or counters not summed over the tree
#[test]
fn live_counters_sum_and_convert_ticks() {
    let timebase = Some(Timebase {
        numerator: 125,
        denominator: 3,
    });
    let counters = live_counters(&[Some(sample(300)), Some(sample(600))], timebase);
    assert_eq!(counters.cpu_usage_nsec, Some((600 + 1_200) * 125 / 3));
    assert_eq!(counters.memory_current, Some(2_000));
    assert_eq!(counters.tasks_current, Some(4));
    assert_eq!(counters.io_read_bytes, Some(20));
    assert_eq!(counters.io_write_bytes, Some(40));
    assert_eq!(counters.memory_peak, None);
}

// catches: a failed sample, a missing timebase or an overflow read as 0 instead of unknown
#[test]
fn unavailable_is_none_never_zero() {
    let timebase = Some(Timebase {
        numerator: 1,
        denominator: 1,
    });
    assert_eq!(
        live_counters(&[Some(sample(1)), None], timebase),
        Resources::default()
    );
    assert_eq!(live_counters(&[], timebase), Resources::default());
    let no_clock = live_counters(&[Some(sample(1))], None);
    assert_eq!(no_clock.cpu_usage_nsec, None);
    assert_eq!(no_clock.memory_current, Some(1_000));
    let zero = live_counters(
        &[Some(sample(1))],
        Some(Timebase {
            numerator: 1,
            denominator: 0,
        }),
    );
    assert_eq!(zero.cpu_usage_nsec, None);
    let huge = live_counters(&[Some(sample(u64::MAX)), Some(sample(u64::MAX))], timebase);
    assert_eq!(huge.cpu_usage_nsec, None);
}
