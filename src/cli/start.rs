//! `dshd start [--rerun] NAME`: starts an existing instance from its
//! recorded recipe and current release, under the instance lock. Creating
//! an instance is `run`'s.

use super::lifecycle;
use super::status;
use crate::cli::{Context, StartArgs};
use crate::error::Error;
use crate::instance;

/// Runs the verb.
pub(super) fn run(ctx: &Context, args: &StartArgs) -> Result<(), Error> {
    lifecycle::require_template(ctx)?;
    let (lock, instance) = instance::lock_loaded(&ctx.state_root, &args.name)?;
    let shown = lifecycle::bring_up(ctx, &instance, args.rerun, false)?;
    drop(lock);
    status::print_outcome(ctx, shown)
}
