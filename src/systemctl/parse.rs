//! Parsing what `systemctl --user` prints: the values of one `show`
//! block, the manager's `UnitPath`, `list-units` lines and
//! `show-environment` assignments. Pure text in, values out; the calls and
//! the block splitting are the parent's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{ActiveState, LoadState, Resources, UnitFacts, UnitResult, invalid_show};
use crate::error::Error;
use crate::name::InstanceName;

/// Typed facts from a complete block; unknown state values stay unknown.
pub(super) fn unit_facts(block: &BTreeMap<&str, &str>) -> Result<UnitFacts, Error> {
    let required = |key| {
        block
            .get(key)
            .copied()
            .ok_or_else(|| invalid_show(&format!("missing property {key}")))
    };
    let text = |key| block.get(key).copied().unwrap_or_default();
    let load = load_state(required("LoadState")?);
    let active = active_state(required("ActiveState")?);
    let service = load == LoadState::Loaded;
    if service {
        for key in [
            "Result",
            "MainPID",
            "ExecMainStatus",
            "NRestarts",
            "ActiveEnterTimestamp",
            "InactiveEnterTimestamp",
            "InvocationID",
        ] {
            required(key)?;
        }
    }
    let number = |key| -> Result<&str, Error> {
        match block.get(key).copied() {
            Some(value) => Ok(value),
            None if !service => Ok("0"),
            None => Err(invalid_show(&format!("missing property {key}"))),
        }
    };
    let counter =
        |key| counter(text(key)).map_err(|()| invalid_show(&format!("invalid counter {key}")));
    let main_pid = number("MainPID")?
        .parse()
        .map_err(|_| invalid_show("invalid MainPID"))?;
    let exec_main_status = number("ExecMainStatus")?
        .parse()
        .map_err(|_| invalid_show("invalid ExecMainStatus"))?;
    let restarts = number("NRestarts")?
        .parse()
        .map_err(|_| invalid_show("invalid NRestarts"))?;
    let invocation_id = text("InvocationID");
    if !invocation_id.is_empty()
        && (invocation_id.len() != 32
            || !invocation_id.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(invalid_show("invalid InvocationID"));
    }
    if active == ActiveState::Active && (main_pid == 0 || invocation_id.is_empty()) {
        return Err(invalid_show("an active instance has no process identity"));
    }
    Ok(UnitFacts {
        unit: required("Id")?.to_owned(),
        load_state: load,
        active_state: active,
        sub_state: required("SubState")?.to_owned(),
        result: unit_result(text("Result")),
        main_pid,
        exec_main_status,
        restarts,
        active_enter: timestamp(text("ActiveEnterTimestamp"))?,
        inactive_enter: timestamp(text("InactiveEnterTimestamp"))?,
        invocation_id: invocation_id.to_owned(),
        wanted_by: required("WantedBy")?
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        resources: Resources {
            cpu_usage_nsec: counter("CPUUsageNSec")?,
            memory_current: counter("MemoryCurrent")?,
            memory_peak: counter("MemoryPeak")?,
            tasks_current: counter("TasksCurrent")?,
            io_read_bytes: counter("IOReadBytes")?,
            io_write_bytes: counter("IOWriteBytes")?,
        },
    })
}

/// systemd's unavailable values are distinct from a measured zero.
fn counter(value: &str) -> Result<Option<u64>, ()> {
    match value {
        "" | "[not set]" | "n/a" | "infinity" => Ok(None),
        value => value
            .parse::<u64>()
            .map(|value| (value != u64::MAX).then_some(value))
            .map_err(|_| ()),
    }
}

/// `--timestamp=unix` as seconds; fractional precision is retained only to seconds.
fn timestamp(value: &str) -> Result<Option<u64>, Error> {
    if value.is_empty() || value == "n/a" {
        return Ok(None);
    }
    let value = value
        .strip_prefix('@')
        .ok_or_else(|| invalid_show("invalid unix timestamp"))?;
    let (seconds, fraction) = value.split_once('.').unwrap_or((value, ""));
    if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_show("invalid timestamp fraction"));
    }
    seconds
        .parse::<u64>()
        .map(Some)
        .map_err(|_| invalid_show("invalid timestamp seconds"))
}

fn load_state(value: &str) -> LoadState {
    match value {
        "loaded" => LoadState::Loaded,
        "not-found" => LoadState::NotFound,
        "masked" => LoadState::Masked,
        other => LoadState::Unknown(other.to_owned()),
    }
}

fn active_state(value: &str) -> ActiveState {
    match value {
        "active" => ActiveState::Active,
        "activating" => ActiveState::Activating,
        "deactivating" => ActiveState::Deactivating,
        "inactive" => ActiveState::Inactive,
        "failed" => ActiveState::Failed,
        other => ActiveState::Unknown(other.to_owned()),
    }
}

fn unit_result(value: &str) -> UnitResult {
    match value {
        "success" => UnitResult::Success,
        "exit-code" => UnitResult::ExitCode,
        "signal" => UnitResult::Signal,
        "timeout" => UnitResult::Timeout,
        "core-dump" => UnitResult::CoreDump,
        "start-limit-hit" => UnitResult::StartLimitHit,
        other => UnitResult::Unknown(other.to_owned()),
    }
}

/// `list-units --plain --no-legend` lines: the unit is the first column
/// that names an instance (a status bullet, when systemd prints one, is
/// skipped).
pub(super) fn parse_list_units(text: &str) -> Vec<InstanceName> {
    text.lines()
        .filter_map(|line| {
            line.split(char::is_whitespace)
                .find_map(InstanceName::from_unit)
        })
        .collect()
}

/// `show-environment` output: one `NAME=VALUE` per line.
pub(super) fn parse_environment(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_owned(), unquote(value)))
        .collect()
}

/// systemd prints a value that needs shell quoting as `$'...'` with
/// backslash escapes (`\\`, `\'`, `\n`, `\xHH`...). Anything else is the
/// value itself. A quoted value that does not decode to UTF-8 is kept as
/// printed.
fn unquote(value: &str) -> String {
    let Some(inner) = value
        .strip_prefix("$'")
        .and_then(|rest| rest.strip_suffix('\''))
    else {
        return value.to_owned();
    };
    let mut bytes = Vec::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        let decoded = match c {
            '\\' => match chars.next() {
                Some('a') => Some(0x07),
                Some('b') => Some(0x08),
                Some('f') => Some(0x0c),
                Some('n') => Some(b'\n'),
                Some('r') => Some(b'\r'),
                Some('t') => Some(b'\t'),
                Some('v') => Some(0x0b),
                Some('x') => {
                    let hex: String = chars.by_ref().take(2).collect();
                    u8::from_str_radix(&hex, 16).ok()
                }
                Some(escaped) => {
                    push_char(&mut bytes, escaped);
                    None
                }
                None => Some(b'\\'),
            },
            plain => {
                push_char(&mut bytes, plain);
                None
            }
        };
        bytes.extend(decoded);
    }
    String::from_utf8(bytes).unwrap_or_else(|_| value.to_owned())
}

/// The persistent unit directory in a user manager's `UnitPath`: the
/// search path starts with that directory's `.control` sibling, which
/// `systemctl set-property` writes to (systemd.unit(5), the user load path).
pub(super) fn persistent_unit_dir(unit_path: &str) -> Option<PathBuf> {
    let paths = words(unit_path);
    let dir = paths.first()?.strip_suffix(".control")?;
    let listed = paths.iter().any(|path| path == dir);
    (listed && dir.ends_with("/systemd/user") && Path::new(dir).is_absolute())
        .then(|| PathBuf::from(dir))
}

/// A string array as `systemctl show` prints it: words separated by
/// spaces, a word that needs shell quoting in double quotes with `"`, `\`,
/// `` ` `` and `$` backslash-escaped.
fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quoted = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                word.get_or_insert_with(String::new);
            }
            '\\' if quoted => word.get_or_insert_with(String::new).extend(chars.next()),
            ' ' if !quoted => words.extend(word.take()),
            plain => word.get_or_insert_with(String::new).push(plain),
        }
    }
    words.extend(word);
    words
}

fn push_char(bytes: &mut Vec<u8>, c: char) {
    let mut buffer = [0; 4];
    bytes.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    // catches: a show-environment value kept in its $'...' quoting, or a value with '=' cut short
    #[test]
    fn environment_values_are_unquoted() {
        let environment = parse_environment(
            "A=plain\nB=$'with space'\nC=$'it\\'s\\ttab\\x41'\nD=a=b\nnot a pair\n",
        );
        assert_eq!(environment.get("A").map(String::as_str), Some("plain"));
        assert_eq!(environment.get("B").map(String::as_str), Some("with space"));
        assert_eq!(environment.get("C").map(String::as_str), Some("it's\ttabA"));
        assert_eq!(environment.get("D").map(String::as_str), Some("a=b"));
        assert_eq!(environment.len(), 4);
    }

    // catches: the wants link written to the runtime control directory or to a
    // directory the manager does not search, or a quoted entry kept in its quotes
    #[test]
    fn persistent_unit_dir_is_the_first_control_sibling() {
        let unit_path = "/srv/alice/.config/systemd/user.control /run/user/1/systemd/user.control \
                         /run/user/1/systemd/transient /srv/alice/.config/systemd/user \
                         /run/user/1/systemd/user /etc/systemd/user";
        assert_eq!(
            persistent_unit_dir(unit_path),
            Some(PathBuf::from("/srv/alice/.config/systemd/user"))
        );
        let quoted = r#""/srv/a \"b\"/systemd/user.control" /run/user/1/systemd/user.control "/srv/a \"b\"/systemd/user""#;
        assert_eq!(
            persistent_unit_dir(quoted),
            Some(PathBuf::from("/srv/a \"b\"/systemd/user"))
        );
        for refused in [
            "/etc/systemd/user /srv/alice/.config/systemd/user",
            "/srv/alice/.config/systemd/user.control /etc/systemd/user",
            "/srv/alice/.config/other.control /srv/alice/.config/other",
            "relative/systemd/user.control relative/systemd/user",
            "",
        ] {
            assert_eq!(persistent_unit_dir(refused), None, "{refused}");
        }
    }

    // catches: the target or the status bullet read as an instance name
    #[test]
    fn list_units_names_instances_only() {
        let text = "dshd@web-1.service loaded active running dshd instance web-1\n\
                    \u{25cf} dshd@acp-2.service loaded failed failed dshd instance acp-2\n\
                    dshd.target loaded active active dshd instances\n";
        let names: Vec<String> = parse_list_units(text)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(names, ["web-1", "acp-2"]);
    }
}
