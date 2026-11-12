//! `dshd run ... -- DSH ARGUMENTS`: resolve the recipe (release, cwd, the
//! argv as typed and the profile it selects, environment capture, the
//! transport the live profile selects), create the instance or match the
//! stored one, and start it. A web or acp instance, or any with `--bg`, is
//! left running; a headless one runs in the foreground until its task ends
//! or the timeout passes.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;

use super::follow;
use super::lifecycle;
use super::status::{self, Shown};
use crate::cli::report::OutcomeSlug;
use crate::cli::{Asked, Context, RunArgs};
use crate::error::Error;
use crate::fsx::{self, Lock};
use crate::instance::{self, CreateRequest, Instance};
use crate::launch;
use crate::name;
use crate::out;
use crate::profile;
use crate::record::{OsText, ProfileRef, Transport};
use crate::release;
use crate::tool::Tool;

/// Runs the verb. The exit code is 0, or for a foreground headless run in
/// text form dsh's own status or 124 at the check-in.
pub(super) fn run(ctx: &Context, args: &RunArgs) -> Result<u8, Error> {
    lifecycle::require_template(ctx)?;
    let (bg, timeout) = (args.bg, args.timeout);
    let (lock, instance, created, several_apps) = create(ctx, args)?;
    let mut shown = lifecycle::bring_up(ctx, &instance, false, created)?;
    drop(lock);
    shown.warnings.extend(several_apps);
    if bg || instance.record.transport != Transport::None {
        status::print_outcome(ctx, shown)?;
        return Ok(0);
    }
    foreground(ctx, &instance, shown, timeout)
}

/// Follows a headless run. A foreground run is a text verb: dsh's lines go
/// to stdout and dshd's own to stderr, and the exit code is the run's,
/// also when stdout is piped. Only `--json` prints the outcome instead,
/// which says how the run ended; the exit code is then 0.
fn foreground(ctx: &Context, instance: &Instance, shown: Shown, timeout: u64) -> Result<u8, Error> {
    let json = ctx.asked == Asked::Json;
    if !json {
        for line in &shown.before {
            out::error_line(&format!("[dshd] {line}"));
        }
        for warning in &shown.warnings {
            out::error_line(&format!("dshd: warning: {warning}"));
        }
    }
    let concluded = follow::until_end(ctx, instance, !json, timeout)?;
    if json {
        let report = status::report(ctx, &instance.name, false)?;
        let shown = Shown {
            warnings: shown.warnings,
            exit: Some(concluded.exit),
            ..Shown::new(concluded.outcome, report)
        };
        status::print_outcome(ctx, shown)?;
        return Ok(0);
    }
    if concluded.outcome == OutcomeSlug::StillRunning {
        let name = &instance.name;
        out::error_line(&format!(
            "[dshd] still running as {name}: dshd wait {name}, dshd read {name}"
        ));
    }
    Ok(concluded.exit.exit_code)
}

/// Resolves the recipe from argv and creates the instance, or matches the
/// stored recipe of an existing one, whose labels are then set. Returns
/// the instance lock, the instance as loaded under it, whether it was
/// created now, and the warning for a profile that lists several apps.
/// The root lock covers dsh materialising a missing profile, the name
/// choice and the creation, never a systemctl call.
fn create(ctx: &Context, args: &RunArgs) -> Result<(Lock, Instance, bool, Option<String>), Error> {
    let release_given = args.release.is_some();
    let release = release::resolve(Path::new(name::RELEASES_DIR), args.release.as_deref())?;
    let cwd = match &args.cwd {
        Some(dir) => fsx::canonical_dir(&ctx.cwd.join(dir))?,
        None => fsx::canonical_dir(&ctx.cwd)?,
    };
    let client: BTreeMap<OsString, OsString> = ctx.environment.iter().cloned().collect();
    let dsh_home = launch::resolve_dsh_home(&client, &ctx.home, &cwd);
    let selected = launch::selected_profile(&args.argv)?;
    let dsh = Tool::at("dsh", release.path.join(name::RELEASE_DSH));
    let loaded = if args.name.is_none() {
        ctx.systemctl.list_loaded_instances()?
    } else {
        Vec::new()
    };
    let root_lock = instance::lock_root(&ctx.state_root)?;
    let mut materialise = dsh.command();
    materialise
        .env(launch::DSH_HOME, &dsh_home)
        .current_dir(&cwd);
    let detected = profile::transport(&dsh, materialise, &dsh_home, &selected)?;
    let several_apps = several_apps_warning(&selected, &detected);
    let transport = detected.transport;
    let name = match &args.name {
        Some(name) => name.clone(),
        None => instance::choose_name(&ctx.state_root, &loaded, &selected)?,
    };
    let created = fsx::kind(ctx.state_root.instance(&name).path())?.is_none();
    let environment =
        launch::capture_environment(&ctx.environment, &release.file.unset_environment);
    let labels = args
        .labels
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let request = CreateRequest {
        name: name.clone(),
        release,
        release_given,
        cwd,
        args: args.argv.clone(),
        environment,
        profile: ProfileRef {
            name: selected,
            dsh_home: OsText::new(dsh_home.into_os_string()),
            unrecognized: serde_json::Map::new(),
        },
        transport,
        labels,
    };
    instance::create(&ctx.state_root, &ctx.nix_store, request)?;
    let (lock, mut instance) = instance::lock_loaded(&ctx.state_root, &name)?;
    drop(root_lock);
    if !created && instance::set_labels(&instance, &args.labels)? {
        instance = instance::load_strict(&ctx.state_root, &name)?;
    }
    Ok((lock, instance, created, several_apps))
}

/// The warning for a profile that lists more than one app bundle. DSH
/// composes every bundle a profile lists into one tree, so the apps are
/// mounted together and each layer's rows apply to the others; dshd holds
/// the instance by the transport the first one selects.
fn several_apps_warning(profile: &str, detected: &profile::Detected) -> Option<String> {
    (detected.apps.len() > 1).then(|| {
        format!(
            "profile {profile} lists several apps ({}): DSH mounts every bundle a profile lists \
             in one process, and dshd holds the instance as transport {}; give each app its own \
             profile",
            detected.apps.join(", "),
            detected.transport.slug(),
        )
    })
}
