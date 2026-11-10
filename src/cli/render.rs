//! Human renderings: the `NAME: state(release-label), remedy: CMD` line
//! shared by `ls` and the mutating verbs, and the `get` page. JSON never
//! comes through here.

use std::fmt::Display;

use crate::cli::report::{
    Ending, LastRunReport, ListEntry, ProfileReport, ReleaseReport, StateSlug, StatusReport,
};
use crate::out::{self, Style};
use crate::relay::protocol::ProbeLine;

/// Seconds in a civil day.
const SECS_PER_DAY: i64 = 86_400;

/// The one-line form shared by `ls` and the mutating verbs. A release
/// that is not `current` shows its state beside the label.
pub(super) fn line(entry: &ListEntry) -> String {
    let style = match entry.state {
        StateSlug::Running | StateSlug::Starting | StateSlug::Stopping => Style::Active,
        StateSlug::Unknown
        | StateSlug::Failed
        | StateSlug::Masked
        | StateSlug::Orphaned
        | StateSlug::Inconsistent => Style::Failed,
        StateSlug::Stopped | StateSlug::Interrupted | StateSlug::PreviousBoot => Style::Inactive,
    };
    let release = match (&entry.release, entry.release_state) {
        (Some(label), Some(state)) if state != "current" => format!("({label}, {state})"),
        (Some(label), _) => format!("({label})"),
        (None, _) => String::new(),
    };
    let remedy = match &entry.remedy {
        Some(command) => format!(", remedy: {}", out::styled(Style::Command, command)),
        None => String::new(),
    };
    format!(
        "{}: {}{release}{remedy}",
        out::styled(style, &entry.name),
        state_word(entry.state)
    )
}

/// The multi-line status page: the line, then one indented `key: value`
/// per known fact.
pub(super) fn status_page(report: &StatusReport) -> String {
    let mut page = Page(vec![line(&entry_of(report))]);
    page.optional("id", report.id.as_ref());
    page.field(
        "unit",
        &format!(
            "{} ({}/{}, {})",
            report.unit, report.active_state, report.sub_state, report.result
        ),
    );
    page.optional("since", report.since.map(utc));
    page.optional("main pid", report.main_pid);
    page.optional("invocation", report.invocation_id.as_ref());
    page.field("restarts", &report.restarts);
    for (name, value) in [
        ("cpu ns", report.resources.cpu_usage_nsec),
        ("memory bytes", report.resources.memory_current),
        ("memory peak bytes", report.resources.memory_peak),
        ("tasks", report.resources.tasks_current),
        ("io read bytes", report.resources.io_read_bytes),
        ("io write bytes", report.resources.io_write_bytes),
    ] {
        page.optional(name, value);
    }
    page.field("enabled", &yes_no(report.enabled));
    if let Some(transport) = report.transport {
        page.field("transport", &transport.slug());
        page.optional("cwd", report.cwd.as_ref());
        let shown: Vec<String> = report.args.iter().map(|arg| quoted(arg)).collect();
        page.field("args", &shown.join(" "));
        page.field("environment", &report.environment_names.join(" "));
    }
    for (key, value) in &report.labels {
        page.field(&format!("label {key}"), value);
    }
    if let Some(release) = &report.release {
        release_lines(&mut page, release);
    }
    if let Some(profile) = &report.profile {
        page.field("profile", &profile_text(profile));
    }
    if let Some(web) = &report.web {
        let url = web
            .url
            .as_deref()
            .map_or_else(|| "no address announced".to_owned(), str::to_owned);
        let port = web.port.map_or_else(
            || "port not pinned yet".to_owned(),
            |port| format!("port {port}"),
        );
        page.field("web", &format!("{url} ({port})"));
    }
    if let Some(acp) = &report.acp {
        acp_lines(&mut page, acp);
    }
    if let Some(run) = &report.last_run {
        page.field("last run", &last_run_text(run));
    }
    page.0.join("\n")
}

/// The `linger:` line of the `get` page, last on it.
pub(super) fn linger_line(linger: bool) -> String {
    format!("  linger: {}", yes_no(linger))
}

/// A list entry from a full report, for the line after a mutating verb.
pub(super) fn entry_of(report: &StatusReport) -> ListEntry {
    ListEntry {
        name: report.name.clone(),
        state: report.state,
        transport: report.transport,
        release: report.release.as_ref().map(|release| release.label.clone()),
        release_state: report.release.as_ref().map(|release| release.state),
        enabled: report.enabled,
        since: report.since,
        remedy: report.remedy.clone(),
    }
}

/// The lines of a status page under construction.
struct Page(Vec<String>);

impl Page {
    fn field(&mut self, key: &str, value: &dyn Display) {
        self.0.push(format!("  {key}: {value}"));
    }

    fn optional<T: Display>(&mut self, key: &str, value: Option<T>) {
        if let Some(value) = value {
            self.field(key, &value);
        }
    }
}

/// The state word, as the JSON `state` slug spells it.
fn state_word(state: StateSlug) -> &'static str {
    match state {
        StateSlug::Unknown => "unknown",
        StateSlug::Running => "running",
        StateSlug::Starting => "starting",
        StateSlug::Stopping => "stopping",
        StateSlug::Failed => "failed",
        StateSlug::Stopped => "stopped",
        StateSlug::Interrupted => "interrupted",
        StateSlug::PreviousBoot => "previous-boot",
        StateSlug::Masked => "masked",
        StateSlug::Orphaned => "orphaned",
        StateSlug::Inconsistent => "inconsistent",
    }
}

fn release_lines(page: &mut Page, release: &ReleaseReport) {
    page.field(
        "release",
        &format!("{}, {} ({})", release.name, release.label, release.state),
    );
    let link = |target: Option<&String>| target.map_or_else(|| "none".to_owned(), String::clone);
    page.field("  current", &link(release.current.as_ref()));
    page.field("  booted", &link(release.booted.as_ref()));
    page.field("  registry", &link(release.registry.as_ref()));
}

fn profile_text(profile: &ProfileReport) -> String {
    format!("{} in {}", profile.name, profile.dsh_home)
}

fn acp_lines(page: &mut Page, acp: &ProbeLine) {
    let controller = match acp.attached_since {
        Some(since) => format!("controller attached since {}", utc(since)),
        None if acp.attached => "controller attached".to_owned(),
        None => "no controller attached".to_owned(),
    };
    let initialized = if acp.initialized {
        "initialized"
    } else {
        "not initialized"
    };
    page.field(
        "acp",
        &format!(
            "{controller}, {initialized}, {} held sessions, {} pending permissions",
            acp.held_sessions.len(),
            acp.pending_permissions
        ),
    );
    for held in &acp.held_sessions {
        let bound = if held.bound { "bound" } else { "unbound" };
        page.field(
            "  session",
            &format!(
                "{} in {} ({bound}, {} buffered, {} dropped)",
                held.session_id, held.cwd, held.buffered, held.dropped
            ),
        );
    }
}

/// The ending word, as the JSON `ending` slug spells it.
fn ending_word(ending: Ending) -> &'static str {
    match ending {
        Ending::Stopped => "stopped",
        Ending::StoppedWithoutIntent => "stopped-without-intent",
        Ending::Exited => "exited",
        Ending::Failed => "failed",
        Ending::Interrupted => "interrupted",
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// The last run, opening with how it ended. A run in progress, and one
/// whose end was never recorded, show their start only.
fn last_run_text(run: &LastRunReport) -> String {
    let boot = if run.this_boot {
        ""
    } else {
        ", an earlier boot"
    };
    let (Some(ending), Some(ended_at)) = (run.ending, run.ended_at) else {
        let progress = match run.ending {
            None => "in progress",
            Some(ending) => ending_word(ending),
        };
        return format!(
            "{progress}: started {}, no end recorded{boot}",
            utc(run.started_at)
        );
    };
    let child = match run.child.as_ref() {
        Some(end) => match (end.exit_code, end.signal) {
            (Some(code), _) => format!(", dsh exit {code}"),
            (None, Some(signal)) => format!(", dsh signal {signal}"),
            (None, None) => String::new(),
        },
        None => String::new(),
    };
    let word = |field: Option<&str>| field.unwrap_or_default().to_owned();
    format!(
        "{}: {} to {}, {} ({} {}){child}{boot}",
        ending_word(ending),
        utc(run.started_at),
        utc(ended_at),
        word(run.service_result.as_deref()),
        word(run.exit_code.as_deref()),
        word(run.exit_status.as_deref())
    )
}

/// One argument as a POSIX shell would need it typed.
pub(super) fn quoted(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@%+=:,./_-".contains(&b));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// Unix seconds as `YYYY-MM-DD HH:MM:SS UTC`.
fn utc(secs: u64) -> String {
    let Ok(secs) = i64::try_from(secs) else {
        return format!("@{secs}");
    };
    let (year, month, day) = civil_from_days(secs.div_euclid(SECS_PER_DAY));
    let in_day = secs.rem_euclid(SECS_PER_DAY);
    let (hour, minute, second) = (in_day / 3600, in_day % 3600 / 60, in_day % 60);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

/// Days since 1970-01-01 to a proleptic Gregorian `(year, month, day)`,
/// after Howard Hinnant's `civil_from_days`: the year is counted from
/// March so the leap day falls at its end.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * march_month + 2) / 5 + 1;
    let month = if march_month < 10 {
        march_month + 3
    } else {
        march_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Transport;

    /// `text` without colour escapes, whatever the test's stdout is.
    fn plain(text: &str) -> String {
        let mut kept = String::new();
        let mut escaped = false;
        for c in text.chars() {
            match (escaped, c) {
                (false, '\x1b') => escaped = true,
                (false, _) => kept.push(c),
                (true, 'm') => escaped = false,
                (true, _) => {}
            }
        }
        kept
    }

    fn entry(state: StateSlug, release_state: &'static str, remedy: Option<&str>) -> ListEntry {
        ListEntry {
            name: "web-1".to_owned(),
            state,
            transport: Some(Transport::Web),
            release: Some("dsh-0.2.2-rc.1".to_owned()),
            release_state: Some(release_state),
            enabled: false,
            since: None,
            remedy: remedy.map(str::to_owned),
        }
    }

    // catches: the list line losing its release label, its release state or its remedy
    #[test]
    fn line_forms() {
        assert_eq!(
            plain(&line(&entry(StateSlug::Running, "current", None))),
            "web-1: running(dsh-0.2.2-rc.1)"
        );
        assert_eq!(
            plain(&line(&entry(
                StateSlug::Stopped,
                "current",
                Some("dshd start web-1")
            ))),
            "web-1: stopped(dsh-0.2.2-rc.1), remedy: dshd start web-1"
        );
        assert_eq!(
            plain(&line(&entry(
                StateSlug::Running,
                "outdated",
                Some("dshd update -R web-1")
            ))),
            "web-1: running(dsh-0.2.2-rc.1, outdated), remedy: dshd update -R web-1"
        );
        let orphan = ListEntry {
            release: None,
            release_state: None,
            ..entry(StateSlug::Orphaned, "current", None)
        };
        assert_eq!(plain(&line(&orphan)), "web-1: orphaned");
        let boot = entry(StateSlug::PreviousBoot, "missing", None);
        assert_eq!(
            plain(&line(&boot)),
            "web-1: previous-boot(dsh-0.2.2-rc.1, missing)"
        );
    }

    // catches: a run cut off before its end shown as ending on 1970-01-01, or the ending left out of the line
    #[test]
    fn last_run_opens_with_its_ending() {
        let run = LastRunReport {
            started_at: 1_794_009_610,
            invocation_id: "0123456789abcdef0123456789abcdef".to_owned(),
            ending: None,
            ended_at: None,
            service_result: None,
            exit_code: None,
            exit_status: None,
            child: None,
            this_boot: true,
        };
        assert_eq!(
            last_run_text(&run),
            "in progress: started 2026-11-07 00:00:10 UTC, no end recorded"
        );
        let cut = LastRunReport {
            ending: Some(Ending::Interrupted),
            this_boot: false,
            ..run.clone()
        };
        assert_eq!(
            last_run_text(&cut),
            "interrupted: started 2026-11-07 00:00:10 UTC, no end recorded, an earlier boot"
        );
        let ended = LastRunReport {
            ending: Some(Ending::Exited),
            ended_at: Some(1_794_013_210),
            service_result: Some("success".into()),
            exit_code: Some("exited".into()),
            exit_status: Some("0".into()),
            ..run
        };
        assert_eq!(
            last_run_text(&ended),
            "exited: 2026-11-07 00:00:10 UTC to 2026-11-07 01:00:10 UTC, success (exited 0)"
        );
        assert_eq!(linger_line(false), "  linger: no");
    }

    // catches: an off-by-one day or month in the UTC rendering, leap days included
    #[test]
    fn utc_dates() {
        assert_eq!(utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(utc(1_794_009_610), "2026-11-07 00:00:10 UTC");
        assert_eq!(utc(1_794_013_210), "2026-11-07 01:00:10 UTC");
        assert_eq!(utc(u64::MAX), format!("@{}", u64::MAX));
    }

    // catches: arguments shown so that pasting them into a shell runs something else
    #[test]
    fn shell_quoting() {
        assert_eq!(quoted("--profile"), "--profile");
        assert_eq!(quoted("web@home"), "web@home");
        assert_eq!(quoted(""), "''");
        assert_eq!(quoted("two words"), "'two words'");
        assert_eq!(quoted("it's"), r"'it'\''s'");
    }
}
