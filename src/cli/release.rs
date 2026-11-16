//! `dshd release add|ls|rm|prune`: the releases a user adds without Nix,
//! as names in the user registry and entries of the directory store
//! ([`crate::release::user`]). Each verb does one thing when it is typed;
//! dshd never prunes, adds or fetches a release on its own, and never
//! changes the DSH an entry links.

use std::collections::BTreeSet;
use std::path::Path;

use crate::cli::report::{
    ReleaseChange, ReleaseEntry, ReleaseList, ReleaseName, ReleaseOutcome, ReleasePrune,
};
use crate::cli::{Context, ReleaseAddArgs, ReleaseArgs};
use crate::error::Error;
use crate::fsx;
use crate::instance;
use crate::name::InstanceName;
use crate::out;
use crate::release::{self, Registries, user};

/// Runs the verb.
pub(super) fn run(ctx: &Context, args: &ReleaseArgs) -> Result<(), Error> {
    match args {
        ReleaseArgs::Add(args) => add(ctx, args),
        ReleaseArgs::Ls => ls(ctx),
        ReleaseArgs::Rm(name) => rm(ctx, name),
        ReleaseArgs::Prune => prune(ctx),
    }
}

/// `release add`: the entry, then the name, under the root lock, so a
/// `prune` cannot remove the entry between the two.
fn add(ctx: &Context, args: &ReleaseAddArgs) -> Result<(), Error> {
    let registries = ctx.registries();
    let runner = user::own_runner()?;
    let dsh = ctx.cwd.join(&args.dsh);
    let lock = instance::lock_root(&ctx.state_root)?;
    let added = user::add(&registries, &args.name, &dsh, &runner, &args.environment)?;
    drop(lock);
    let moved = added.previous.as_deref() != Some(added.entry.as_path());
    let mut warnings = Vec::new();
    if fsx::kind(&registries.system.join(args.name.as_str()))?.is_some() {
        warnings.push(format!(
            "{} holds {} too, and wins: dshd resolves {} there, never here",
            registries.system.display(),
            args.name,
            args.name
        ));
    }
    let recording = recorded_by(ctx, &args.name)?;
    if moved && !recording.is_empty() {
        warnings.push(format!(
            "{} record release {}; they stay on their current entry until dshd update",
            recording.join(", "),
            args.name
        ));
    }
    let report = ReleaseChange {
        outcome: if moved {
            ReleaseOutcome::Added
        } else {
            ReleaseOutcome::Unchanged
        },
        name: args.name.to_string(),
        entry: Some(text(&added.entry)),
        previous: added.previous.as_deref().map(text),
        created: added.created,
        warnings,
    };
    if ctx.json {
        return out::json(&report);
    }
    let line = if moved {
        format!("Added release {}: {}", args.name, text(&added.entry))
    } else {
        format!(
            "Release {} already points at {}",
            args.name,
            text(&added.entry)
        )
    };
    out::line(&line)?;
    warn(&report.warnings);
    Ok(())
}

/// `release rm NAME`: the user registry name only; the entry stays until
/// `prune`.
fn rm(ctx: &Context, name: &InstanceName) -> Result<(), Error> {
    let registries = ctx.registries();
    let lock = instance::lock_root(&ctx.state_root)?;
    let previous = user::remove_name(&registries, name)?;
    drop(lock);
    let recording = recorded_by(ctx, name)?;
    let mut warnings = Vec::new();
    if !recording.is_empty() {
        warnings.push(format!(
            "{} record release {name}; they keep their entry, and get shows the release missing",
            recording.join(", ")
        ));
    }
    let report = ReleaseChange {
        outcome: ReleaseOutcome::Removed,
        name: name.to_string(),
        entry: None,
        previous: previous.as_deref().map(text),
        created: false,
        warnings,
    };
    if ctx.json {
        return out::json(&report);
    }
    out::line(&format!(
        "Removed release name {name}; dshd release prune removes the entries nothing holds"
    ))?;
    warn(&report.warnings);
    Ok(())
}

/// `release ls`: every entry with what holds it, and every user registry
/// name.
fn ls(ctx: &Context) -> Result<(), Error> {
    let registries = ctx.registries();
    let report = list(ctx, &registries)?;
    if ctx.json {
        return out::json(&report);
    }
    let mut lines = Vec::new();
    for entry in &report.entries {
        lines.push(format!("{}  {}", entry.label, entry.path));
        let mut held: Vec<String> = Vec::new();
        held.extend(entry.names.iter().map(|name| format!("name {name}")));
        held.extend(
            entry
                .current
                .iter()
                .map(|name| format!("current link of {name}")),
        );
        held.extend(
            entry
                .booted
                .iter()
                .map(|name| format!("booted link of {name}")),
        );
        if held.is_empty() {
            lines.push("  held by nothing: dshd release prune removes it".to_owned());
        } else {
            lines.push(format!("  held by {}", held.join(", ")));
        }
    }
    for name in &report.names {
        let is_entry = report
            .entries
            .iter()
            .any(|entry| Some(&entry.path) == name.target.as_ref());
        if !is_entry {
            let target = name.target.as_deref().unwrap_or("nothing (it dangles)");
            lines.push(format!("name {} points at {target}", name.name));
        }
    }
    if lines.is_empty() {
        lines.push(format!("No releases in {}", report.store));
    }
    out::line(&lines.join("\n"))?;
    warn(&report.warnings);
    Ok(())
}

/// `release prune`, under the root lock, so no instance is created on an
/// entry while it goes.
fn prune(ctx: &Context) -> Result<(), Error> {
    let registries = ctx.registries();
    let lock = instance::lock_root(&ctx.state_root)?;
    let held: BTreeSet<_> = user::holders(&registries, &ctx.state_root)?
        .into_keys()
        .collect();
    let pruned = user::prune(&registries, &held)?;
    let kept = user::entries(&registries)?;
    drop(lock);
    let report = ReleasePrune {
        outcome: if pruned.is_empty() {
            ReleaseOutcome::Unchanged
        } else {
            ReleaseOutcome::Pruned
        },
        pruned: pruned.iter().map(|path| text(path)).collect(),
        kept: kept.iter().map(|path| text(path)).collect(),
        warnings: Vec::new(),
    };
    if ctx.json {
        return out::json(&report);
    }
    let mut lines: Vec<String> = report
        .pruned
        .iter()
        .map(|path| format!("Removed {path}"))
        .collect();
    if lines.is_empty() {
        lines.push("Nothing to prune: a name or an instance holds every entry".to_owned());
    }
    out::line(&lines.join("\n"))
}

/// The `release ls` object.
fn list(ctx: &Context, registries: &Registries) -> Result<ReleaseList, Error> {
    let mut holders = user::holders(registries, &ctx.state_root)?;
    let entries = user::entries(registries)?
        .into_iter()
        .map(|path| {
            let held = holders.remove(&path).unwrap_or_default();
            ReleaseEntry {
                path: text(&path),
                label: release::version_label(&path),
                names: held.names,
                current: held.current,
                booted: held.booted,
            }
        })
        .collect();
    let names = user::names(registries)?
        .into_iter()
        .map(|name| ReleaseName {
            name: name.name,
            target: name.target.as_deref().map(text),
        })
        .collect();
    let warnings = if registries.user.is_none() {
        vec![
            "the service manager names no configuration root, so there is no user release registry"
                .to_owned(),
        ]
    } else {
        Vec::new()
    };
    Ok(ReleaseList {
        store: text(&registries.store),
        registry: registries.user.as_deref().map(text),
        entries,
        names,
        warnings,
    })
}

/// The instances whose record names release `name`, sorted; an instance
/// whose record cannot be read is left out.
fn recorded_by(ctx: &Context, name: &InstanceName) -> Result<Vec<String>, Error> {
    Ok(instance::discover(&ctx.state_root)?
        .names
        .iter()
        .filter_map(|instance| instance::load(&ctx.state_root, instance).ok())
        .filter(|instance| instance.record.release == name.as_str())
        .map(|instance| instance.name.to_string())
        .collect())
}

/// Prints each warning on stderr.
fn warn(warnings: &[String]) {
    for warning in warnings {
        out::error_line(&format!("dshd: warning: {warning}"));
    }
}

fn text(path: &Path) -> String {
    path.display().to_string()
}
