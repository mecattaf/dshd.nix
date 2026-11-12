//! `dshd read NAME [-n LINES | --head LINES] [-f]`: the journal of the
//! instance's current run, or of its last one, by the run's invocation id,
//! as dsh wrote it, a line journald broke at `LineMax` joined again: its
//! newest lines, or with `--head` its oldest. A note on
//! stderr says when the lines are not the whole run: they do not reach back
//! to the runner's first line, or newer lines follow them; `-f` keeps
//! printing until the run ends.

use super::follow::{self, Since};
use super::journal::{self, Entry, Joiner};
use super::status;
use crate::cli::report::ReadReport;
use crate::cli::{Context, ReadArgs};
use crate::error::Error;
use crate::instance;
use crate::out;
use crate::record::Transport;
use crate::runner::RUN_STARTED;

/// Runs the verb. Text unless `--json` was given.
pub(super) fn run(ctx: &Context, args: &ReadArgs) -> Result<(), Error> {
    let name = &args.name;
    let instance = instance::load(&ctx.state_root, name)?;
    let facts = ctx.systemctl.show(&name.unit())?;
    let Some(id) = follow::invocation(&facts, &instance)? else {
        return Err(Error::NotRunning(name.to_string()));
    };
    let Window {
        lines,
        from_start,
        newer,
    } = if args.head {
        oldest(ctx, &id, args.lines)?
    } else {
        newest(ctx, &id, args.lines)?
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
        out::error_line(&format!(
            "[dshd] incomplete: older output may be missing; the unit's whole journal: journalctl --user -u {}",
            name.unit()
        ));
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

/// The lines `read` shows, and what they leave out.
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
    lines.truncate(shown);
    let from_start = lines
        .first()
        .is_some_and(|line| line.starts_with(RUN_STARTED));
    Ok(Window {
        lines,
        from_start,
        newer,
    })
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
