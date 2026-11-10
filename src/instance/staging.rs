//! Building a new instance: the record in a staging directory beside the
//! final one, the rename into place and the roots registered at their
//! final paths. A failure at any step removes what was built.

use super::{CreateRequest, Instance, RECORD_MODE, new_id, remove};
use crate::error::Error;
use crate::fsx;
use crate::name::{InstanceDir, InstanceName, StateRoot};
use crate::record::{InstanceRecord, OsText};
use crate::release;
use crate::tool::Tool;

/// Prefix of the directory an instance is built in before it is renamed
/// into place. Instance names cannot start with a dot.
const STAGING_PREFIX: &str = ".tmp-";

/// Builds a new instance under `.tmp-<name>`, renames it into place and
/// roots its release. The staging name is fixed per instance name, so a
/// leftover from an interrupted creation is cleared by the next one (the
/// root lock serialises them).
pub(super) fn build(
    root: &StateRoot,
    nix_store: &Tool,
    name: InstanceName,
    request: CreateRequest,
) -> Result<Instance, Error> {
    let dir = root.instance(&name);
    let staging = InstanceDir::new(root.instances_dir().join(format!("{STAGING_PREFIX}{name}")));
    fsx::remove_tree(staging.path())?;
    let release = request.release.path.clone();
    let staged = stage(&staging, &name, request).and_then(|record| {
        fsx::rename(staging.path(), dir.path())?;
        Ok(record)
    });
    let record = match staged {
        Ok(record) => record,
        Err(error) => return Err(abandon(&name, error, remove(&staging))),
    };
    let instance = Instance { name, dir, record };
    let rooted = release::add_root(nix_store, &release, &instance.dir.current())
        .and_then(|()| release::add_root(nix_store, &release, &instance.dir.booted()));
    match rooted {
        Ok(()) => Ok(instance),
        Err(error) => Err(abandon(&instance.name, error, remove(&instance.dir))),
    }
}

/// Fills the staging directory: `instance.json` with a fresh id.
fn stage(
    staging: &InstanceDir,
    name: &InstanceName,
    request: CreateRequest,
) -> Result<InstanceRecord, Error> {
    let CreateRequest {
        release,
        cwd,
        args,
        environment,
        profile,
        transport,
        labels,
        ..
    } = request;
    fsx::create_dir_private(staging.path())?;
    let record = InstanceRecord {
        id: new_id()?,
        name: name.as_str().to_owned(),
        release: release.name,
        cwd: OsText::new(cwd),
        args: args.into_iter().map(OsText::from).collect(),
        environment,
        profile,
        transport,
        labels,
        created_at: fsx::unix_now(),
        unrecognized: serde_json::Map::new(),
    };
    fsx::write_json_atomic(&staging.instance_json(), &record, RECORD_MODE)?;
    Ok(record)
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
