//! Building a new instance: a staging directory beside the final one, the
//! profile copy and record inside it, the exposed profile link, the rename
//! into place and the roots registered at their final paths. A failure at
//! any step removes what was built.

use std::path::{Path, PathBuf};

use super::{CreateRequest, Instance, RECORD_MODE, new_id, remove};
use crate::error::Error;
use crate::fsx;
use crate::name::{InstanceDir, InstanceName, StateRoot};
use crate::profile;
use crate::record::{InstanceRecord, OsText};
use crate::release;
use crate::tool::Tool;

/// Prefix of the directory an instance is built in before it is renamed
/// into place. Instance names cannot start with a dot.
const STAGING_PREFIX: &str = ".tmp-";

/// Builds a new instance under `.tmp-<name>`, exposes its profile, renames
/// it into place and roots its release. The staging name is fixed per
/// instance name, so a leftover from an interrupted creation is cleared by
/// the next one (the root lock serialises them).
pub(super) fn build(
    root: &StateRoot,
    nix_store: &Tool,
    name: InstanceName,
    request: CreateRequest,
) -> Result<Instance, Error> {
    let dir = root.instance(&name);
    let dsh_home = PathBuf::from(request.profile.dsh_home.as_os_str());
    let exposed = name.exposed_profile(&request.profile.name);
    profile::verify_link(&dsh_home, &exposed, &dir.profile())?;
    let staging = InstanceDir::new(root.instances_dir().join(format!("{STAGING_PREFIX}{name}")));
    fsx::remove_tree(staging.path())?;
    let release = request.release.clone();
    let record = match stage(&staging, nix_store, &name, request) {
        Ok(record) => record,
        Err(error) => return Err(abandon(&name, error, fsx::remove_tree(staging.path()))),
    };
    if let Err(error) = profile::expose(&dsh_home, &exposed, &dir.profile()) {
        return Err(abandon(&name, error, fsx::remove_tree(staging.path())));
    }
    if let Err(error) = fsx::rename(staging.path(), dir.path()) {
        let cleanup = profile::unlink_exact(&dsh_home, &exposed, &dir.profile())
            .and_then(|()| fsx::remove_tree(staging.path()));
        return Err(abandon(&name, error, cleanup));
    }
    let instance = Instance { name, dir, record };
    let rooted = release::add_root(nix_store, &release.path, &instance.dir.current())
        .and_then(|()| release::add_root(nix_store, &release.path, &instance.dir.booted()))
        .and_then(|()| refresh_gcroots(&instance.dir, nix_store));
    match rooted {
        Ok(()) => Ok(instance),
        Err(error) => Err(abandon(&instance.name, error, remove(&instance))),
    }
}

/// Fills the staging directory: the profile copy (with its store roots),
/// the composition stamp and `instance.json` with a fresh id.
fn stage(
    staging: &InstanceDir,
    nix_store: &Tool,
    name: &InstanceName,
    request: CreateRequest,
) -> Result<InstanceRecord, Error> {
    let CreateRequest {
        release,
        cwd,
        args,
        environment,
        profile: source,
        transport,
        labels,
        patches,
        ..
    } = request;
    fsx::create_dir_private(staging.path())?;
    let source_dir = profile::resolve_source(Path::new(source.dsh_home.as_os_str()), &source.name)?;
    profile::copy_profile(
        &source_dir,
        &staging.profile(),
        &staging.gcroots(),
        nix_store,
    )?;
    let patches: Vec<PathBuf> = patches.iter().map(|patch| cwd.join(patch)).collect();
    let record = InstanceRecord {
        id: new_id()?,
        name: name.as_str().to_owned(),
        release: release.name,
        composition: profile::composition_stamp(&staging.profile(), &patches)?,
        cwd: OsText::new(cwd),
        args: args.into_iter().map(OsText::from).collect(),
        environment,
        profile: source,
        transport,
        labels,
        created_at: fsx::unix_now(),
        unrecognized: serde_json::Map::new(),
    };
    fsx::write_json_atomic(&staging.instance_json(), &record, RECORD_MODE)?;
    Ok(record)
}

/// Re-registers every `gcroots/<k>` link at its own path, so the roots
/// name the instance directory as it is now.
pub(super) fn refresh_gcroots(dir: &InstanceDir, nix_store: &Tool) -> Result<(), Error> {
    let gcroots = dir.gcroots();
    for (entry, _) in &fsx::list_dir(&gcroots)? {
        let link = gcroots.join(entry);
        if let Some(target) = fsx::read_link(&link)? {
            release::add_root(nix_store, &target, &link)?;
        }
    }
    if fsx::kind(&gcroots)?.is_some() {
        fsx::sync_directory(&gcroots)?;
    }
    Ok(())
}

/// Keeps the cause of a failed creation. When the cleanup failed too,
/// something is left behind, so the result is `InconsistentState` naming
/// both.
fn abandon(name: &InstanceName, error: Error, cleanup: Result<(), Error>) -> Error {
    match cleanup {
        Ok(()) => error,
        Err(cleanup) => Error::InconsistentState {
            name: name.to_string(),
            reason: format!("{error}; removing the partial instance failed too: {cleanup}"),
        },
    }
}
