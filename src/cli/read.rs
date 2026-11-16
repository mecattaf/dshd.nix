//! `dshd read NAME [-n LINES | --head LINES] [-f]`: the journal of the
//! instance's current run, or of its last one, by the run's invocation id,
//! as dsh wrote it, a line journald broke at `LineMax` joined again: its
//! newest lines, or with `--head` its oldest. Under launchd the run's log
//! file `<instance>/runs/<invocationId>.log` takes the journal's place
//! (`ServiceManager::run_logs`). A note on
//! stderr says when the lines are not the whole run: they do not reach back
//! to the runner's first line, or newer lines follow them; `-f` keeps
//! printing until the run ends.

use std::path::Path;

use super::follow::{self, Since};
use super::journal::{self, Entry, Joiner};
use super::status;
use crate::cli::report::ReadReport;
use crate::cli::{Context, ReadArgs};
use crate::error::Error;
use crate::instance;
use crate::name::InstanceName;
use crate::out;
use crate::platform::RunLogs;
use crate::platform::launchd::logs;
use crate::record::Transport;
use crate::runner::RUN_STARTED;

/// Runs the verb. Text unless `--json` was given.
pub(super) fn run(ctx: &Context, args: &ReadArgs) -> Result<(), Error> {
    let name = &args.name;
    let instance = instance::load(&ctx.state_root, name)?;
    let facts = ctx.manager.show(name)?;
    let Some(id) = follow::invocation(&facts, &instance)? else {
        return Err(Error::NotRunning(name.to_string()));
    };
    let kept = ctx.manager.run_logs(name);
    let Window {
        lines,
        from_start,
        newer,
    } = match &kept {
        RunLogs::Journal if args.head => oldest(ctx, &id, args.lines)?,
        RunLogs::Journal => newest(ctx, &id, args.lines)?,
        RunLogs::Files(dir) => from_file(&logs::run_log(dir, &id), args.head, args.lines)?,
    };
    let complete = from_start && !newer;
    let acp = instance.record.transport == Transport::Acp;
    if ctx.json {
        let streams = if acp {
            vec!["stderr"]
        } else {
            vec!["stdout", "stderr"]
        };
        return out::json(&ReadReport {
            name: name.to_string(),
            invocation_id: id,
            complete,
            streams,
            lines,
        });
    }
    if acp {
        out::error_line("[dshd] acp: stdout is the ACP channel; this is dsh's stderr");
    }
    if !from_start {
        out::error_line(&older_missing(&kept, name, &id));
    }
    if newer {
        out::error_line(&format!(
            "[dshd] incomplete: newer output follows; the newest lines: dshd read {name}"
        ));
    }
    if !lines.is_empty() {
        out::line(&lines.join("\n"))?;
    }
    if args.follow && status::unit_up(&facts) {
        follow::journal(ctx, &instance, &id, true, Since::Now, None)?;
    }
    Ok(())
}

/// The note for lines that do not reach back to the run's first line,
/// naming where the whole run is.
fn older_missing(kept: &RunLogs, name: &InstanceName, id: &str) -> String {
    match kept {
        RunLogs::Journal => format!(
            "[dshd] incomplete: older output may be missing; the unit's whole journal: journalctl --user -u {}",
            name.unit()
        ),
        RunLogs::Files(dir) => format!(
            "[dshd] incomplete: older output may be missing; the run's whole log: {}",
            logs::run_log(dir, id).display()
        ),
    }
}

/// The lines `read` shows, and what they leave out.
#[derive(Debug, PartialEq, Eq)]
struct Window {
    lines: Vec<String>,
    /// The first line is the run's `dshd-run: run started` line.
    from_start: bool,
    /// The run has lines after the last one shown.
    newer: bool,
}

/// journalctl reads `-n` as a C int, so an ask stops here.
const MOST_ENTRIES: u32 = i32::MAX.unsigned_abs();

/// The newest `count` lines; one more call reads the run's first line, to
/// tell whether they reach back to it. One entry more than `count` is
/// asked for: when journald broke it at `LineMax`, the entries after it up
/// to the end of that line are left out. A window with fewer whole lines
/// than `count` asks for twice the entries, until it reaches the run's
/// first entry.
fn newest(ctx: &Context, id: &str, count: u32) -> Result<Window, Error> {
    let shown = usize::try_from(count).unwrap_or(usize::MAX);
    let mut ask = count.saturating_add(1).min(MOST_ENTRIES);
    let mut lines = loop {
        let entries = capture(ctx, id, &ask.to_string())?;
        let whole = entries.len() < asked(ask);
        let lines = after_first(entries, whole);
        if lines.len() >= shown || whole || ask == MOST_ENTRIES {
            break lines;
        }
        ask = ask.saturating_mul(2).min(MOST_ENTRIES);
    };
    lines.drain(..lines.len().saturating_sub(shown));
    let first = journal::lines(capture(ctx, id, "+1")?);
    let from_start = first
        .first()
        .is_some_and(|line| line.starts_with(RUN_STARTED) && lines.first() == Some(line));
    Ok(Window {
        lines,
        from_start,
        newer: false,
    })
}

/// The lines of the newest entries of a run. Unless they are `whole`, from
/// the run's first entry, the oldest entry only tells whether the next one
/// starts a line: it is left out, and when journald broke the line there,
/// so are the rest of that line's pieces.
fn after_first(entries: Vec<Entry>, whole: bool) -> Vec<String> {
    let mut entries = entries.into_iter();
    if !whole && entries.next().is_some_and(|first| first.broken()) {
        for piece in entries.by_ref() {
            if !piece.broken() {
                break;
            }
        }
    }
    journal::lines(entries)
}

/// The oldest `count` lines, asking for one entry more, to tell whether
/// newer ones follow. When journald broke lines at `LineMax`, the entries
/// may end inside a line or hold fewer lines than that: the ask doubles
/// until it shows a line after the `count`th, or reaches the run's end.
fn oldest(ctx: &Context, id: &str, count: u32) -> Result<Window, Error> {
    let shown = usize::try_from(count).unwrap_or(usize::MAX);
    let mut ask = count.saturating_add(1).min(MOST_ENTRIES);
    let mut lines = loop {
        let entries = capture(ctx, id, &format!("+{ask}"))?;
        let whole = entries.len() < asked(ask);
        let mut joiner = Joiner::default();
        let mut lines: Vec<String> = entries
            .into_iter()
            .flat_map(|entry| joiner.push(entry))
            .collect();
        let ended = lines.len();
        lines.extend(joiner.finish());
        if whole || ended > shown || ask == MOST_ENTRIES {
            break lines;
        }
        ask = ask.saturating_mul(2).min(MOST_ENTRIES);
    };
    let newer = lines.len() > shown;
    // Before the cut, so an empty window (`--head 0`) still says whether
    // it begins at the run's first line.
    let from_start = lines
        .first()
        .is_some_and(|line| line.starts_with(RUN_STARTED));
    lines.truncate(shown);
    Ok(Window {
        lines,
        from_start,
        newer,
    })
}

/// The window of a run's log file (launchd), which holds whole lines, so
/// nothing is joined. `--head` reads one line more than it shows, to tell
/// whether newer lines follow and, for `--head 0`, whether the run's first
/// line is there. A missing file (a log retention removed) is no lines,
/// not from the start.
fn from_file(path: &Path, head: bool, count: u32) -> Result<Window, Error> {
    let window = if head {
        logs::Window::Oldest(count.saturating_add(1))
    } else {
        logs::Window::Newest(count)
    };
    Ok(file_window(logs::read_window(path, window)?, head, count))
}

/// What [`from_file`] read, as the window `read` shows: the newest lines
/// reach back to the run's first line when the file holds no more and the
/// first is the runner's; the oldest are followed by newer ones when the
/// file holds more than `count`.
fn file_window(read: logs::Lines, head: bool, count: u32) -> Window {
    let shown = usize::try_from(count).unwrap_or(usize::MAX);
    let logs::Lines {
        mut lines,
        truncated,
    } = read;
    let starts = lines
        .first()
        .is_some_and(|line| line.starts_with(RUN_STARTED));
    if !head {
        return Window {
            lines,
            from_start: starts && !truncated,
            newer: false,
        };
    }
    let newer = truncated || lines.len() > shown;
    lines.truncate(shown);
    Window {
        lines,
        from_start: starts,
        newer,
    }
}

/// An ask as a count of entries.
fn asked(ask: u32) -> usize {
    usize::try_from(ask).unwrap_or(usize::MAX)
}

/// `journalctl --user _SYSTEMD_INVOCATION_ID=ID -q -o json ... -n ENTRIES`:
/// the newest ENTRIES entries, or with `+N` the oldest N, oldest first.
fn capture(ctx: &Context, id: &str, entries: &str) -> Result<Vec<Entry>, Error> {
    let filter = format!("_SYSTEMD_INVOCATION_ID={id}");
    let captured = ctx.journalctl.run(
        ["--user", filter.as_str(), "-q"]
            .into_iter()
            .chain(journal::OUTPUT)
            .chain(["-n", entries]),
    )?;
    Ok(captured
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(Entry::parse)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsx::testing::Scratch;

    fn read(lines: &[&str], truncated: bool) -> logs::Lines {
        logs::Lines {
            lines: lines.iter().map(|line| (*line).to_owned()).collect(),
            truncated,
        }
    }

    fn window(lines: &[&str], from_start: bool, newer: bool) -> Window {
        Window {
            lines: lines.iter().map(|line| (*line).to_owned()).collect(),
            from_start,
            newer,
        }
    }

    // catches: a run log's newest lines called complete when the file holds older ones or the
    // first line is not the runner's, or said to be followed by newer lines
    #[test]
    fn newest_lines_of_a_run_log() {
        assert_eq!(
            file_window(read(&[RUN_STARTED, "a", "b"], false), false, 5),
            window(&[RUN_STARTED, "a", "b"], true, false)
        );
        assert_eq!(
            file_window(read(&["a", "b"], true), false, 2),
            window(&["a", "b"], false, false)
        );
        assert_eq!(
            file_window(read(&["a", "b"], false), false, 2),
            window(&["a", "b"], false, false),
            "a log whose first line is not the runner's is not the whole run"
        );
        assert_eq!(
            file_window(read(&[], false), false, 2),
            window(&[], false, false),
            "a removed log is not the whole run"
        );
    }

    // catches: --head showing the extra line it read, missing the newer lines that follow, or
    // losing whether the run's first line is there when it shows none (--head 0)
    #[test]
    fn oldest_lines_of_a_run_log() {
        assert_eq!(
            file_window(read(&[RUN_STARTED, "a", "b"], true), true, 2),
            window(&[RUN_STARTED, "a"], true, true)
        );
        assert_eq!(
            file_window(read(&[RUN_STARTED, "a"], false), true, 2),
            window(&[RUN_STARTED, "a"], true, false)
        );
        assert_eq!(
            file_window(read(&[RUN_STARTED], true), true, 0),
            window(&[], true, true)
        );
        assert_eq!(
            file_window(read(&[RUN_STARTED, "a", "b"], false), true, 2),
            window(&[RUN_STARTED, "a"], true, true),
            "the line read past the count is a newer one"
        );
    }

    // catches: the launchd window not read from the run's own log file, or a partial last line
    // shown
    #[test]
    fn a_run_log_file_is_read_whole_lines_only() {
        let scratch = Scratch::new("read-run-log");
        let path = scratch.path().join("0123456789abcdef0123456789abcdef.log");
        std::fs::write(&path, format!("{RUN_STARTED}\none\ntwo\npart")).unwrap();
        assert_eq!(
            from_file(&path, false, 2).unwrap(),
            window(&["one", "two"], false, false)
        );
        assert_eq!(
            from_file(&path, true, 1).unwrap(),
            window(&[RUN_STARTED], true, true)
        );
        assert_eq!(
            from_file(&path, false, 10).unwrap(),
            window(&[RUN_STARTED, "one", "two"], true, false)
        );
        let gone = scratch.path().join("gone.log");
        assert_eq!(
            from_file(&gone, false, 10).unwrap(),
            window(&[], false, false)
        );
    }
}
