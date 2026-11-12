//! `dshd read NAME [-n LINES | --head LINES] [-f]`: the journal of the
//! instance's current run, or of its last one, by the run's invocation id,
//! as dsh wrote it: its newest lines, or with `--head` its oldest. A note on
//! stderr says when the lines are not the whole run: they do not reach back
//! to the runner's first line, or newer lines follow them; `-f` keeps
//! printing until the run ends.

use super::follow::{self, Since};
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

/// The newest `count` lines; one more call reads the run's first line, to
/// tell whether they reach back to it.
fn newest(ctx: &Context, id: &str, count: u32) -> Result<Window, Error> {
    let lines = capture(ctx, id, &count.to_string())?;
    let first = capture(ctx, id, "+1")?;
    let from_start = first
        .first()
        .is_some_and(|line| line.starts_with(RUN_STARTED) && lines.first() == Some(line));
    Ok(Window {
        lines,
        from_start,
        newer: false,
    })
}

/// The oldest `count` lines, in one call that asks for one line more, to
/// tell whether newer ones follow. journalctl reads `-n` as a C int, so the
/// ask stops at [`i32::MAX`].
fn oldest(ctx: &Context, id: &str, count: u32) -> Result<Window, Error> {
    let ask = count.saturating_add(1).min(i32::MAX.unsigned_abs());
    let mut lines = capture(ctx, id, &format!("+{ask}"))?;
    let shown = usize::try_from(count).unwrap_or(usize::MAX);
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

/// `journalctl --user _SYSTEMD_INVOCATION_ID=ID -q -o cat -n LINES`: the
/// newest LINES lines, or with `+N` the oldest N.
fn capture(ctx: &Context, id: &str, lines: &str) -> Result<Vec<String>, Error> {
    let filter = format!("_SYSTEMD_INVOCATION_ID={id}");
    let captured =
        ctx.journalctl
            .run(["--user", filter.as_str(), "-q", "-o", "cat", "-n", lines])?;
    Ok(captured.stdout_text().lines().map(str::to_owned).collect())
}
