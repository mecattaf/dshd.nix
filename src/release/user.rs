//! The releases a user adds without Nix: `dshd release add`, `ls`, `rm`
//! and `prune` over the user registry and the directory store
//! ([`super::store`]).
//!
//! A user registry name is a symlink `<config root>/dshd/releases/<NAME>`
//! to an entry of the store `%S/dshd/.releases`. `add` writes the entry
//! (or reuses the identical one) and points the name at it; `rm` removes
//! only the name; `prune` removes the entries that no user registry name
//! and no instance's `current` or `booted` link points at. Nothing here
//! runs on its own: each step is one verb the user typed.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::Registries;
use super::store::{self, EntryInputs};
use crate::error::Error;
use crate::fsx;
use crate::name::{InstanceName, RELEASE_RUNNER, StateRoot};

/// The variable the permission mode of a release sets.
pub const PERMISSION_MODE: &str = "DSH_PERMISSION_MODE";
/// The variables the runner sets itself after the strip, so an
/// `unsetEnvironment` naming one would promise a strip that never happens
/// (`lib/release.nix` refuses the same).
pub const RUNNER_SET: [&str; 4] = ["DSH_HOME", "PWD", "INVOCATION_ID", "JOURNAL_STREAM"];
/// The runner's place relative to the directory of the running `dshd`, in
/// the order they are tried: the macOS package layout
/// (`bin/dshd`, `libexec/dshd/dshd-run`), then beside it.
const RUNNER_PLACES: [&str; 2] = ["../libexec/dshd/dshd-run", "dshd-run"];

/// What a release sets for DSH: the permission mode, always stated, the
/// other variables of `--env`, and the names `--unset` strips.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReleaseEnvironment {
    /// `--permission-mode MODE`: the value of `DSH_PERMISSION_MODE`.
    pub permission_mode: String,
    /// `--env NAME=VALUE`, without `DSH_PERMISSION_MODE`.
    pub environment: BTreeMap<String, String>,
    /// `--unset NAME`, in the order given.
    pub unset: Vec<String>,
}

impl ReleaseEnvironment {
    /// The `release.json` text, the shape `lib.mkRelease` writes:
    /// `{"environment":{...},"unsetEnvironment":[...]}` and a newline.
    /// Refuses what `lib.mkRelease` refuses, and a permission mode that
    /// is not stated.
    pub fn release_json(&self) -> Result<Vec<u8>, Error> {
        self.check()?;
        let mut environment = self.environment.clone();
        environment.insert(PERMISSION_MODE.to_owned(), self.permission_mode.clone());
        let file = ReleaseJson {
            environment: &environment,
            unset_environment: &self.unset,
        };
        let mut bytes = serde_json::to_vec(&file).map_err(|error| Error::ReleaseInvalid {
            release: "release.json".to_owned(),
            reason: error.to_string(),
        })?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// The refusals of `lib.mkRelease`, as `invalid-argument`.
    pub fn check(&self) -> Result<(), Error> {
        let refuse = |message: String| Err(Error::InvalidArgument(message));
        if !permission_mode_shaped(&self.permission_mode) {
            return refuse(format!(
                "--permission-mode takes a DSH permission mode such as workspace-write, \
                 1 to 64 characters of a-z, 0-9 and '-'; got {:?}",
                self.permission_mode
            ));
        }
        for name in self.environment.keys() {
            if !variable_shaped(name) {
                return refuse(format!(
                    "--env {name}: a variable name is not empty and has no '=' or NUL"
                ));
            }
            if name == PERMISSION_MODE {
                return refuse(format!(
                    "--env {PERMISSION_MODE}: state the permission mode with --permission-mode"
                ));
            }
        }
        let mut seen = BTreeSet::new();
        for name in &self.unset {
            if !variable_shaped(name) {
                return refuse(format!(
                    "--unset {name:?}: a variable name is not empty and has no '=' or NUL"
                ));
            }
            if name == PERMISSION_MODE || self.environment.contains_key(name) {
                return refuse(format!("--unset {name}: the release also sets {name}"));
            }
            if RUNNER_SET.contains(&name.as_str()) {
                return refuse(format!(
                    "--unset {name}: the runner always sets {name} itself"
                ));
            }
            if !seen.insert(name) {
                return refuse(format!("--unset {name} given twice"));
            }
        }
        Ok(())
    }
}

/// `release.json` as written.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseJson<'a> {
    environment: &'a BTreeMap<String, String>,
    unset_environment: &'a [String],
}

/// Whether `mode` can be a permission mode: 1 to 64 characters of
/// lowercase ASCII letters, digits and `-`. dshd passes it to DSH as it
/// is and knows no list of modes.
pub fn permission_mode_shaped(mode: &str) -> bool {
    !mode.is_empty()
        && mode.len() <= 64
        && mode
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Whether `name` can be an environment variable name: not empty, no `=`
/// and no NUL.
fn variable_shaped(name: &str) -> bool {
    !name.is_empty() && !name.contains(['=', '\0'])
}

/// The runner of the running `dshd`: `../libexec/dshd/dshd-run` from its
/// directory, else `dshd-run` beside it; the first that is an executable
/// file.
pub fn own_runner() -> Result<PathBuf, Error> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| Error::io("resolve", "the running dshd", error))?;
    let dir = exe.parent().unwrap_or_else(|| Path::new("/"));
    let candidates: Vec<PathBuf> = RUNNER_PLACES.iter().map(|place| dir.join(place)).collect();
    candidates
        .iter()
        .find(|candidate| executable_file(candidate))
        .cloned()
        .ok_or_else(|| Error::ReleaseInvalid {
            release: RELEASE_RUNNER.to_owned(),
            reason: format!(
                "no executable dshd-run at {}",
                candidates
                    .iter()
                    .map(|candidate| candidate.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
        })
}

/// Whether `path` resolves to a regular file with an execute bit.
fn executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// What `add` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    /// The entry the name points at now.
    pub entry: PathBuf,
    /// What the name pointed at before, canonical; `None` for a new name.
    pub previous: Option<PathBuf>,
    /// Whether this call wrote the entry; `false` when identical inputs
    /// had written it before.
    pub created: bool,
}

/// `dshd release add NAME --dsh DSH ...`: writes the entry for `dsh`, the
/// runner `runner` and `environment` (or reuses the identical one) and
/// points the user registry name `name` at it.
pub fn add(
    registries: &Registries,
    name: &InstanceName,
    dsh: &Path,
    runner: &Path,
    environment: &ReleaseEnvironment,
) -> Result<Added, Error> {
    let registry = user_registry(registries)?;
    let dsh_kind = fsx::metadata(dsh)?;
    if !dsh_kind
        .is_some_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    {
        return Err(Error::ReleaseInvalid {
            release: name.to_string(),
            reason: format!("--dsh {} is not an executable file", dsh.display()),
        });
    }
    let release_json = environment.release_json()?;
    let inputs = EntryInputs {
        label: name.as_str(),
        dsh,
        runner,
        release_json: &release_json,
    };
    let expected = registries.store.join(format!(
        "{}{}{}",
        store::entry_hash(&inputs)?,
        store::ENTRY_INFIX,
        name.as_str()
    ));
    let existed = fsx::kind(&expected)?.is_some();
    let written = store::add_entry(&registries.store, &inputs)?;
    let created = !existed && written == expected;
    let entry = fsx::canonical_path(&written)?.unwrap_or(written);
    let previous = fsx::canonical_path(&registry.join(name.as_str()))?;
    store::register(&registry, name.as_str(), &entry)?;
    Ok(Added {
        entry,
        previous,
        created,
    })
}

/// `dshd release rm NAME`: removes the user registry name and returns what
/// it pointed at, canonical (`None` when it dangled). The entry stays
/// until `prune`. A name the user registry does not hold is
/// `release-not-found`.
pub fn remove_name(registries: &Registries, name: &InstanceName) -> Result<Option<PathBuf>, Error> {
    let registry = user_registry(registries)?;
    let link = registry.join(name.as_str());
    if !fsx::kind(&link)?.is_some_and(|kind| kind.is_symlink()) {
        return Err(Error::ReleaseNotFound(name.to_string()));
    }
    let target = fsx::canonical_path(&link)?;
    std::fs::remove_file(&link).map_err(|error| Error::io("remove", &link, error))?;
    Ok(target)
}

/// One user registry name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    /// The name.
    pub name: String,
    /// What it points at, canonical; `None` when it dangles.
    pub target: Option<PathBuf>,
}

/// The names of the user registry, sorted; entries that are not symlinks
/// with a name in the grammar are left out.
pub fn names(registries: &Registries) -> Result<Vec<Name>, Error> {
    let Some(registry) = &registries.user else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    for (entry, kind) in fsx::list_dir(registry)? {
        let Some(name) = entry.to_str() else {
            continue;
        };
        if !kind.is_symlink() || InstanceName::parse(name).is_err() {
            continue;
        }
        found.push(Name {
            name: name.to_owned(),
            target: fsx::canonical_path(&registry.join(name))?,
        });
    }
    Ok(found)
}

/// The entries of the directory store, under its canonical path, sorted.
/// A store that does not exist has none.
pub fn entries(registries: &Registries) -> Result<Vec<PathBuf>, Error> {
    let Some(root) = fsx::canonical_path(&registries.store)? else {
        return Ok(Vec::new());
    };
    Ok(fsx::list_dir(&root)?
        .into_iter()
        .map(|(entry, _)| root.join(entry))
        .filter(|path| store::is_entry(&root, path))
        .collect())
}

/// Which instance links point at a path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Holders {
    /// The user registry names.
    pub names: Vec<String>,
    /// The instances whose `current` link points at it.
    pub current: Vec<String>,
    /// The instances whose `booted` link points at it.
    pub booted: Vec<String>,
}

impl Holders {
    /// Whether nothing holds it.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.current.is_empty() && self.booted.is_empty()
    }
}

/// What holds each path: the user registry names, and the `current` and
/// `booted` links of every instance under `state_root`, by canonical
/// target. A link that dangles holds nothing.
pub fn holders(
    registries: &Registries,
    state_root: &StateRoot,
) -> Result<BTreeMap<PathBuf, Holders>, Error> {
    let mut held: BTreeMap<PathBuf, Holders> = BTreeMap::new();
    for name in names(registries)? {
        if let Some(target) = name.target {
            held.entry(target).or_default().names.push(name.name);
        }
    }
    for instance in crate::instance::discover(state_root)?.names {
        let (current, booted) = super::links(&state_root.instance(&instance))?;
        if let Some(current) = current {
            held.entry(current)
                .or_default()
                .current
                .push(instance.to_string());
        }
        if let Some(booted) = booted {
            held.entry(booted)
                .or_default()
                .booted
                .push(instance.to_string());
        }
    }
    Ok(held)
}

/// `dshd release prune`: removes every entry of the store nothing in
/// `held` points at, and returns the removed entries, sorted. The caller
/// holds the root lock, so no instance is created meanwhile.
pub fn prune(registries: &Registries, held: &BTreeSet<PathBuf>) -> Result<Vec<PathBuf>, Error> {
    let selected = store::prune_selection(&entries(registries)?, held);
    for entry in &selected {
        remove_entry(entry)?;
    }
    Ok(selected)
}

/// Removes a read-only entry: its directories are made writable first,
/// then the tree goes.
fn remove_entry(entry: &Path) -> Result<(), Error> {
    make_writable(entry)?;
    fsx::remove_tree(entry)
}

/// Gives every directory under `path` (links not followed) the owner's
/// write permission.
fn make_writable(path: &Path) -> Result<(), Error> {
    let Some(kind) = fsx::kind(path)? else {
        return Ok(());
    };
    if !kind.is_dir() {
        return Ok(());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| Error::io("set mode", path, error))?;
    for (entry, _) in fsx::list_dir(path)? {
        make_writable(&path.join(entry))?;
    }
    Ok(())
}

/// The user registry, or why there is none.
fn user_registry(registries: &Registries) -> Result<PathBuf, Error> {
    registries.user.clone().ok_or_else(|| {
        Error::SystemdUnavailable(
            "the service manager names no configuration root for the user release registry"
                .to_owned(),
        )
    })
}

#[cfg(test)]
#[path = "user_tests.rs"]
mod tests;
