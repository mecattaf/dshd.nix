//! `dshd wait NAME [--timeout S]`: a run that ended is reported at once;
//! a run in progress is followed silently until it ends or the timeout
//! passes. As text the exit code mirrors dsh's, or is 124 at the check-in;
//! as JSON the outcome says so and the exit code is 0.

use super::follow;
use super::status::{self, Shown};
use crate::cli::report::OutcomeSlug;
use crate::cli::{Context, WaitArgs};
use crate::error::Error;
use crate::instance;
use crate::out;

/// Runs the verb.
pub(super) fn run(ctx: &Context, args: &WaitArgs) -> Result<u8, Error> {
    let name = &args.name;
    let instance = instance::load_strict(&ctx.state_root, name)?;
    let concluded = follow::until_end(ctx, &instance, false, args.timeout)?;
    if ctx.json {
        let report = status::report(ctx, name, false)?;
        let shown = Shown {
            exit: Some(concluded.exit),
            ..Shown::new(concluded.outcome, report)
        };
        status::print_outcome(ctx, shown)?;
        return Ok(0);
    }
    if concluded.outcome == OutcomeSlug::StillRunning {
        out::error_line(&format!(
            "[dshd] still running as {name}: dshd wait {name}, dshd read {name}"
        ));
    } else {
        out::line(&format!("{name}: {}", concluded.exit.reason))?;
    }
    Ok(concluded.exit.exit_code)
}
