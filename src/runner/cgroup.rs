//! What a run consumed, from the unit's own cgroup. systemd starts
//! `ExecStopPost` in that cgroup once every process of the run is gone, so
//! the totals `poststop` reads there are the run's: the same files systemd
//! reads for `CPUUsageNSec`, `MemoryPeak`, `IOReadBytes` and `IOWriteBytes`
//! while the unit runs. Only the unified hierarchy is read; a counter whose
//! file is absent, because its controller is off, is null.

use std::fs;
use std::path::{Path, PathBuf};

use crate::record::RunResources;

/// The calling process's cgroup membership.
const OWN_CGROUP: &str = "/proc/self/cgroup";
/// Where the unified hierarchy is mounted.
const UNIFIED_ROOT: &str = "/sys/fs/cgroup";
/// The membership line of the unified hierarchy.
const UNIFIED_LINE: &str = "0::";
/// The sub-cgroup systemd puts control processes in when a unit delegates.
const CONTROL_GROUP: &str = ".control";
/// Nanoseconds per microsecond, from `usage_usec` to `CPUUsageNSec`.
const NSEC_PER_USEC: u64 = 1_000;

/// The totals of the cgroup this process runs in; every counter is null
/// when that cgroup cannot be found.
pub(super) fn consumed() -> RunResources {
    fs::read_to_string(OWN_CGROUP)
        .ok()
        .and_then(|membership| unit_group(Path::new(UNIFIED_ROOT), &membership))
        .map_or_else(RunResources::default, |dir| totals(&dir))
}

/// The unit's cgroup directory from `/proc/self/cgroup`: the unified line,
/// without the control sub-cgroup.
fn unit_group(root: &Path, membership: &str) -> Option<PathBuf> {
    let path = membership
        .lines()
        .find_map(|line| line.strip_prefix(UNIFIED_LINE))?;
    let mut dir = root.join(path.trim_start_matches('/'));
    if dir.file_name().is_some_and(|name| name == CONTROL_GROUP) {
        dir.pop();
    }
    Some(dir)
}

/// The counters under one cgroup directory.
fn totals(dir: &Path) -> RunResources {
    let read = |file: &str| fs::read_to_string(dir.join(file)).ok();
    let (io_read_bytes, io_write_bytes) = read("io.stat").map_or((None, None), |text| {
        (io_total(&text, "rbytes"), io_total(&text, "wbytes"))
    });
    RunResources {
        cpu_usage_nsec: read("cpu.stat")
            .and_then(|text| keyed(&text, "usage_usec"))
            .and_then(|usec| usec.checked_mul(NSEC_PER_USEC)),
        memory_peak: read("memory.peak").and_then(|text| text.trim().parse().ok()),
        io_read_bytes,
        io_write_bytes,
        unrecognized: serde_json::Map::new(),
    }
}

/// The value of `KEY VALUE` in a flat-keyed file such as `cpu.stat`.
fn keyed(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let (name, value) = line.split_once(' ')?;
        (name == key).then(|| value.trim().parse().ok()).flatten()
    })
}

/// The sum of `KEY=VALUE` over the device lines of `io.stat`; zero when no
/// device saw IO, null when a value does not parse.
fn io_total(text: &str, key: &str) -> Option<u64> {
    let mut total: u64 = 0;
    for field in text.split_whitespace() {
        if let Some(value) = field
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
        {
            total = total.checked_add(value.parse().ok()?)?;
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsx::testing::Scratch;

    // catches: the unit's cgroup taken from a v1 line, kept with its leading `/` (which `join` would make absolute),
    // or a delegated unit's control sub-cgroup read instead of the unit's
    #[test]
    fn unit_group_from_membership() {
        let root = Path::new("/sys/fs/cgroup");
        let unit = "/user.slice/user-1000.slice/user@1000.service/app.slice/dshd@a.service";
        let expected = Some(root.join(unit.trim_start_matches('/')));
        assert_eq!(unit_group(root, &format!("0::{unit}\n")), expected);
        assert_eq!(
            unit_group(root, &format!("12:pids:/elsewhere\n0::{unit}/.control\n")),
            expected
        );
        assert_eq!(unit_group(root, "12:pids:/elsewhere\n"), None);
    }

    // catches: CPU time reported in microseconds, IO of one device only, an empty io.stat read as unavailable, or a
    // controller that is off read as zero
    #[test]
    fn totals_from_the_cgroup_files() {
        let scratch = Scratch::new("cgroup-totals");
        let dir = scratch.path();
        assert_eq!(totals(dir), RunResources::default());
        fs::write(
            dir.join("cpu.stat"),
            "usage_usec 2500000\nuser_usec 2000000\nsystem_usec 500000\n",
        )
        .unwrap();
        fs::write(dir.join("memory.peak"), "73400320\n").unwrap();
        fs::write(dir.join("io.stat"), "").unwrap();
        let found = totals(dir);
        assert_eq!(
            (
                found.cpu_usage_nsec,
                found.memory_peak,
                found.io_read_bytes,
                found.io_write_bytes
            ),
            (Some(2_500_000_000), Some(73_400_320), Some(0), Some(0))
        );
        fs::write(
            dir.join("io.stat"),
            "259:0 rbytes=4096 wbytes=1024 rios=1 wios=1 dbytes=0 dios=0\n8:0 rbytes=100 wbytes=0 rios=1 wios=0 dbytes=0 dios=0\n",
        )
        .unwrap();
        let found = totals(dir);
        assert_eq!(
            (found.io_read_bytes, found.io_write_bytes),
            (Some(4196), Some(1024))
        );
    }
}
