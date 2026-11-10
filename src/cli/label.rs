//! `dshd label NAME KEY=VALUE...`: sets or removes labels in the record
//! under the instance lock. No systemctl call: labels are metadata, and
//! dsh never sees them.

use super::status::{self, Shown};
use crate::cli::report::OutcomeSlug;
use crate::cli::{Context, LabelArgs};
use crate::error::Error;
use crate::instance;

/// Runs the verb.
pub(super) fn run(ctx: &Context, args: &LabelArgs) -> Result<(), Error> {
    let (lock, instance) = instance::lock_loaded(&ctx.state_root, &args.name)?;
    let changed = instance::set_labels(&instance, &args.labels)?;
    drop(lock);
    let report = status::report(ctx, &args.name, false)?;
    let outcome = if changed {
        OutcomeSlug::Labelled
    } else {
        OutcomeSlug::Unchanged
    };
    status::print_outcome(ctx, Shown::new(outcome, report))
}
