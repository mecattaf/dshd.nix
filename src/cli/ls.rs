//! `dshd ls`, and `dshd` alone: union of directories and loaded units, one
//! batched `systemctl show`, one entry per instance. A unit the manager
//! holds without a directory is listed as orphaned, never skipped; a
//! directory that is not an instance is a warning, and the listing goes on.

use super::{render, status};
use crate::cli::report::ListReport;
use crate::cli::{Context, LsArgs};
use crate::error::Error;
use crate::instance;
use crate::name::InstanceName;
use crate::out;
use crate::systemctl;

/// Where a person at a terminal looks next, after the bare list.
const NEXT: &str = "[dshd] verbs: dshd --help; agents: dshd --skill";

/// Runs the verb.
pub(super) fn run(ctx: &Context, args: LsArgs) -> Result<(), Error> {
    let on_disk = instance::discover(&ctx.state_root)?;
    let loaded = ctx.systemctl.list_loaded_instances()?;
    let mut names: Vec<InstanceName> = on_disk.names.iter().chain(&loaded).cloned().collect();
    names.sort();
    names.dedup();
    let units: Vec<String> = names.iter().map(InstanceName::unit).collect();
    let facts = ctx.systemctl.show_many(&units)?;
    let boot = systemctl::boot_id()?;
    let mut instances = Vec::with_capacity(names.len());
    for (name, facts) in names.iter().zip(&facts) {
        match status::gather(ctx, name, facts, &boot, false, loaded.contains(name)) {
            Ok(gathered) => instances.push(render::entry_of(&gathered.report)),
            // Removed between the directory scan and now: it no longer exists.
            Err(Error::NotFound(_)) => {}
            Err(other) => return Err(other),
        }
    }
    let report = ListReport {
        instances,
        linger: systemctl::linger_enabled(&ctx.user),
        warnings: on_disk.invalid,
    };
    if ctx.json {
        return out::json(&report);
    }
    if !report.instances.is_empty() {
        let lines: Vec<String> = report.instances.iter().map(render::line).collect();
        out::line(&lines.join("\n"))?;
    }
    for warning in &report.warnings {
        out::error_line(&format!("dshd: warning: {warning}"));
    }
    if args.bare && out::stdout_is_terminal() {
        out::error_line(NEXT);
    }
    Ok(())
}
