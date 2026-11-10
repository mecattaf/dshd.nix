//! `dshd rm NAME`: under the root and instance locks, refuses a running instance,
//! reports the status observed just before deletion, clears the unit's
//! failed state and its `dshd.target` link, removes the profile link and
//! the tree.

use super::lifecycle;
use super::status::{self, Shown};
use crate::cli::report::OutcomeSlug;
use crate::cli::{Context, NameArgs};
use crate::error::Error;
use crate::fsx;
use crate::instance;
use crate::out;
use crate::systemctl::ActiveState;

/// Runs the verb.
pub(super) fn run(ctx: &Context, args: &NameArgs) -> Result<(), Error> {
    let name = &args.name;
    let root_lock = instance::lock_root(&ctx.state_root)?;
    let dir = ctx.state_root.instance(name);
    // A unit the manager holds without a directory has nothing to lock.
    let lock = match fsx::kind(dir.path())? {
        Some(_) => Some(instance::lock(&dir)?),
        None => None,
    };
    // A record a newer dshd wrote is removed too, since `rm` is the remedy
    // dshd prints for it: removal reads only the profile link, which it
    // unlinks only while it points at this instance's own copy.
    let loaded = instance::load(&ctx.state_root, name);
    let unit = name.unit();
    let facts = ctx.systemctl.show(&unit)?;
    if !matches!(
        facts.active_state,
        ActiveState::Inactive | ActiveState::Failed
    ) {
        return Err(Error::InstanceRunning(name.to_string()));
    }
    let report = status::report_from(ctx, name, &facts, false)?;
    if facts.active_state == ActiveState::Failed {
        ctx.systemctl.reset_failed(&unit)?;
    }
    let mut shown = Shown::new(OutcomeSlug::Removed, report);
    if shown.status.enabled {
        ctx.systemctl.disable(&unit)?;
        shown
            .before
            .push(lifecycle::echoed(ctx, &["disable", &unit]));
    }
    match loaded {
        Ok(instance) => instance::remove(&instance)?,
        Err(Error::NotFound(_)) => {}
        Err(Error::InconsistentState { reason, .. }) => {
            shown.warnings.push(format!(
                "{reason}; without its record dshd cannot tell which profile link is {name}'s, \
                 so any link to its copy stays in the DSH home"
            ));
            fsx::remove_tree(dir.path())?;
        }
        Err(other) => return Err(other),
    }
    drop(lock);
    drop(root_lock);
    if ctx.json {
        return status::print_outcome(ctx, shown);
    }
    shown.before.push(format!("Removed dshd instance {name}."));
    out::line(&shown.before.join("\n"))?;
    for warning in &shown.warnings {
        out::error_line(&format!("dshd: warning: {warning}"));
    }
    Ok(())
}
