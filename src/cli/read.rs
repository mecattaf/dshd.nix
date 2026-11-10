//! `dshd read NAME [-n LINES] [-f]`: the journal of the instance's current
//! run, or of its last one, by the run's invocation id, as dsh wrote it.
//! A note on stderr says when the lines do not reach back to the runner's
//! first line; `-f` keeps printing until the run ends.

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
    let lines = capture(ctx, &id, &args.lines.to_string())?;
    let first = capture(ctx, &id, "+1")?;
    let complete = first
        .first()
        .is_some_and(|line| line.starts_with(RUN_STARTED) && lines.first() == Some(line));
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
    if !complete {
        out::error_line(&format!(
            "[dshd] incomplete: older output may be missing; the unit's whole journal: journalctl --user -u {}",
            name.unit()
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

/// `journalctl --user _SYSTEMD_INVOCATION_ID=ID -q -o cat -n LINES`: the
/// newest LINES lines, or with `+N` the oldest N.
fn capture(ctx: &Context, id: &str, lines: &str) -> Result<Vec<String>, Error> {
    let filter = format!("_SYSTEMD_INVOCATION_ID={id}");
    let captured =
        ctx.journalctl
            .run(["--user", filter.as_str(), "-q", "-o", "cat", "-n", lines])?;
    Ok(captured.stdout_text().lines().map(str::to_owned).collect())
}
