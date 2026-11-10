//! `dshd update [-R] NAME`: follows the recorded release name, strips newly
//! excluded environment names and repoints `current`. A running instance
//! keeps its booted release until it ends; a stopped one releases it now.

use std::path::Path;

use super::lifecycle::{self, Launch};
use super::status::{self, Shown};
use crate::cli::report::OutcomeSlug;
use crate::cli::{Context, UpdateArgs};
use crate::error::Error;
use crate::instance;
use crate::name;
use crate::record::Transport;
use crate::release::{self, Release};
use crate::systemctl::ActiveState;

#[derive(Default)]
struct Summary {
    before: Vec<String>,
    notes: Vec<String>,
    warnings: Vec<String>,
}

/// Runs the verb.
pub(super) fn run(ctx: &Context, args: &UpdateArgs) -> Result<(), Error> {
    let name = &args.name;
    let (lock, instance) = instance::lock_loaded(&ctx.state_root, name)?;
    if instance.record.release.starts_with('/') {
        return Err(Error::PathPinned(name.to_string()));
    }
    let release = release::resolve(
        Path::new(name::RELEASES_DIR),
        Some(instance.record.release.as_str()),
    )?;
    let removed = instance::strip_environment(&instance, &release.file.unset_environment)?;
    let (old, _) = release::links(&instance.dir)?;
    let changed = instance::update_current(&instance, &ctx.nix_store, &release)?;
    let mut summary = describe(ctx, &instance, &release, old.as_deref(), changed, &removed);
    let active = ctx.systemctl.show(&name.unit())?.active_state;
    if matches!(active, ActiveState::Inactive | ActiveState::Failed) {
        release::collapse_booted(&instance.dir)?;
    }
    let (current, booted) = release::links(&instance.dir)?;
    if args.restart && active == ActiveState::Active && booted != current {
        restart(ctx, &instance, &mut summary)?;
    }
    drop(lock);
    let report = status::report(ctx, name, false)?;
    let outcome = if changed || !removed.is_empty() {
        OutcomeSlug::Updated
    } else {
        OutcomeSlug::Unchanged
    };
    status::print_outcome(
        ctx,
        Shown {
            before: summary.before,
            warnings: summary.warnings,
            notes: summary.notes,
            ..Shown::new(outcome, report)
        },
    )
}

fn describe(
    ctx: &Context,
    instance: &instance::Instance,
    release: &Release,
    old: Option<&Path>,
    changed: bool,
    removed: &[String],
) -> Summary {
    let mut summary = Summary::default();
    let name = &instance.name;
    let label = release::version_label(&release.path);
    if !removed.is_empty() {
        let note = format!(
            "Removed captured environment names: {}.",
            removed.join(", ")
        );
        summary.before.push(note.clone());
        summary.notes.push(note);
    }
    if changed {
        let from = old.map_or_else(|| "a missing release".to_owned(), release::version_label);
        summary.before.push(format!(
            "Updated dshd instance {name} from {from} to {label}."
        ));
        if let Some(old) = old {
            match release::closure_difference(&ctx.nix, old, &release.path) {
                Ok(lines) => {
                    summary.notes.extend(lines.iter().cloned());
                    summary.before.extend(lines);
                }
                Err(error) => summary
                    .warnings
                    .push(format!("cannot compare release closures: {error}")),
            }
        }
    } else {
        summary
            .before
            .push(format!("dshd instance {name} is already on {label}."));
    }
    summary
}

fn restart(
    ctx: &Context,
    instance: &instance::Instance,
    summary: &mut Summary,
) -> Result<(), Error> {
    let name = &instance.name;
    if instance.record.transport == Transport::None {
        let note = format!(
            "{name} is headless, so it is not restarted; the update applies at its next dshd start --rerun {name}"
        );
        summary.before.push(note.clone());
        summary.notes.push(note);
    } else {
        lifecycle::record_stop_intent(ctx, name)?;
        summary
            .before
            .push(lifecycle::launch(ctx, instance, Launch::Restart)?);
    }
    Ok(())
}
