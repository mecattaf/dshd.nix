//! The directory release store: releases that need no Nix, for a dshd
//! installed from a package. Each entry is
//! `<store root>/<32 hex>-dshd-release-<label>` and holds what a Nix
//! release holds: `bin/dsh` (a symlink to the canonical path of the DSH
//! the installer put in place), `bin/dshd-run` (a copy of the runner next
//! to the running `dshd`) and `share/dshd/release.json`. An entry is
//! written in a temporary directory beside it, made read-only, then
//! renamed into place, and never changes after that.
//!
//! The hash is the first 16 bytes of SHA-256, as lowercase hex, over the
//! canonical DSH path, a NUL, the runner's bytes, a NUL and the
//! `release.json` bytes. Identical inputs give the same entry; any
//! difference gives a new one. A user registry name is a symlink
//! `<registry dir>/<NAME>` to an entry; dshd reads it only for a name
//! `/etc/dshd/releases` does not hold.

use std::collections::BTreeSet;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::Error;
use crate::fsx;
use crate::name::{RELEASE_DSH, RELEASE_FILE, RELEASE_RUNNER, StateRoot};

/// The store's directory under the state root: a dot directory, so the
/// instance listing never reads it as an instance.
pub const STORE_DIR: &str = ".releases";
/// The infix every entry name carries after its hash.
pub const ENTRY_INFIX: &str = "-dshd-release-";
/// The mode of an entry's directories and of `bin/dshd-run`.
pub const READ_ONLY_EXEC: u32 = 0o555;
/// The mode of `release.json`.
pub const READ_ONLY_FILE: u32 = 0o444;

/// The inputs of one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryInputs<'a> {
    /// The release label: the release's registry name.
    pub label: &'a str,
    /// The DSH executable the installer put in place.
    pub dsh: &'a Path,
    /// The runner to copy.
    pub runner: &'a Path,
    /// The `release.json` text.
    pub release_json: &'a [u8],
}

/// `<state root>/dshd/.releases` (`%S/dshd/.releases`): beside the
/// instance directories, which the instance listing skips as a dot
/// directory.
#[must_use]
pub fn store_root(state_root: &StateRoot) -> PathBuf {
    state_root.instances_dir().join(STORE_DIR)
}

/// The entry hash of the inputs, by the rule in the module documentation.
/// The DSH path is made canonical first; a DSH that does not resolve is an
/// `Io` error.
pub fn entry_hash(inputs: &EntryInputs<'_>) -> Result<String, Error> {
    let (dsh, runner) = resolved(inputs)?;
    Ok(digest_hex(&dsh, &runner, inputs.release_json))
}

/// The canonical DSH path and the runner's bytes, each read once, so the
/// hash and the entry's contents come from the same bytes.
fn resolved(inputs: &EntryInputs<'_>) -> Result<(PathBuf, Vec<u8>), Error> {
    let dsh = inputs
        .dsh
        .canonicalize()
        .map_err(|e| Error::io("resolve", inputs.dsh, e))?;
    let runner = std::fs::read(inputs.runner).map_err(|e| Error::io("read", inputs.runner, e))?;
    Ok((dsh, runner))
}

/// The first 16 bytes of SHA-256 over the canonical DSH path, a NUL, the
/// runner's bytes, a NUL and the `release.json` bytes, as lowercase hex.
fn digest_hex(dsh: &Path, runner: &[u8], release_json: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(dsh.as_os_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(runner);
    hasher.update(b"\0");
    hasher.update(release_json);
    let digest = hasher.finalize();
    digest
        .iter()
        .take(16)
        .flat_map(|byte| [byte >> 4, byte & 0x0f])
        .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
        .collect()
}

/// Writes the entry for `inputs` under `root` (created 0700 when missing)
/// and returns its path; an entry that already exists is reused as it is.
/// A label that is not a valid release name is `release-invalid`.
pub fn add_entry(root: &Path, inputs: &EntryInputs<'_>) -> Result<PathBuf, Error> {
    if !valid_label(inputs.label) {
        return Err(invalid_label(inputs.label));
    }
    let (dsh, runner) = resolved(inputs)?;
    let hash = digest_hex(&dsh, &runner, inputs.release_json);
    let name = format!("{hash}{ENTRY_INFIX}{}", inputs.label);
    let entry = root.join(name);
    if entry.exists() {
        return Ok(entry);
    }
    fsx::create_dir_private(root)?;
    let temp = root.join(format!(".tmp-{hash}-{}", std::process::id()));
    discard(&temp);
    if let Err(error) = build_entry(&temp, &dsh, &runner, inputs.release_json) {
        discard(&temp);
        return Err(error);
    }
    match fsx::rename(&temp, &entry) {
        Ok(()) => Ok(entry),
        Err(error) => {
            discard(&temp);
            if entry.exists() {
                Ok(entry)
            } else {
                Err(error)
            }
        }
    }
}

/// Whether `path` is an entry of the store at `root`: directly under it,
/// named `<32 hex>-dshd-release-<label>`, a directory that holds
/// `bin/dshd-run`. Does not follow a symlinked `path`.
#[must_use]
pub fn is_entry(root: &Path, path: &Path) -> bool {
    if path.parent() != Some(root) {
        return false;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if entry_label(name).is_none() {
        return false;
    }
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.is_dir() {
        return false;
    }
    path.join(RELEASE_RUNNER).exists()
}

/// Points registry name `name` in `registry_dir` (created 0700 when
/// missing) at `entry`, replacing what it pointed at.
pub fn register(registry_dir: &Path, name: &str, entry: &Path) -> Result<(), Error> {
    if !valid_label(name) {
        return Err(invalid_label(name));
    }
    fsx::create_dir_private(registry_dir)?;
    fsx::symlink_replace(entry, &registry_dir.join(name))
}

/// The entry registry name `name` points at, when it does.
pub fn registered(registry_dir: &Path, name: &str) -> Result<Option<PathBuf>, Error> {
    fsx::read_link(&registry_dir.join(name))
}

/// The entries `release prune` removes: every entry not in `held` (the
/// targets of registry names and of every instance's `current` and
/// `booted`), sorted by path.
#[must_use]
pub fn prune_selection(entries: &[PathBuf], held: &BTreeSet<PathBuf>) -> Vec<PathBuf> {
    let mut selected: Vec<PathBuf> = entries
        .iter()
        .filter(|entry| !held.contains(*entry))
        .cloned()
        .collect();
    selected.sort();
    selected
}

/// Whether `label` is a name an entry may carry: 1 to 64 characters of
/// ASCII letters, digits, `.`, `_` and `-`, not starting with `.` or `-`.
fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 64
        && !label.starts_with('.')
        && !label.starts_with('-')
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// The `release-invalid` refusal of a label [`valid_label`] rejects.
fn invalid_label(label: &str) -> Error {
    Error::ReleaseInvalid {
        release: label.to_owned(),
        reason: "a release name is 1 to 64 characters of letters, digits, '.', '_' \
            and '-', and does not start with '.' or '-'"
            .to_owned(),
    }
}

/// The label of the entry `name` names, when `name` has the shape of an
/// entry name: a 32 hex hash, the infix and a label.
fn entry_label(name: &str) -> Option<&str> {
    let hash = name.get(..32)?;
    if !hash
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let label = name.get(32..)?.strip_prefix(ENTRY_INFIX)?;
    if label.is_empty() {
        return None;
    }
    Some(label)
}

/// Builds the entry's tree under `temp`: the DSH link, the runner's copy
/// and the release metadata, then the modes, files before directories.
fn build_entry(temp: &Path, dsh: &Path, runner: &[u8], release_json: &[u8]) -> Result<(), Error> {
    let bin = temp.join("bin");
    let share = temp.join("share").join("dshd");
    fsx::create_dir_private(temp)?;
    fsx::create_dir_private(&bin)?;
    fsx::create_dir_private(&share)?;
    let link = temp.join(RELEASE_DSH);
    std::os::unix::fs::symlink(dsh, &link).map_err(|e| Error::io("create link", &link, e))?;
    write_file(&temp.join(RELEASE_RUNNER), runner, READ_ONLY_EXEC)?;
    write_file(&temp.join(RELEASE_FILE), release_json, READ_ONLY_FILE)?;
    set_mode(&share, READ_ONLY_EXEC)?;
    set_mode(share.parent().unwrap_or(&share), READ_ONLY_EXEC)?;
    set_mode(&bin, READ_ONLY_EXEC)?;
    set_mode(temp, READ_ONLY_EXEC)?;
    Ok(())
}

/// Writes `bytes` at `path` and gives it `mode`.
fn write_file(path: &Path, bytes: &[u8], mode: u32) -> Result<(), Error> {
    std::fs::write(path, bytes).map_err(|e| Error::io("write", path, e))?;
    set_mode(path, mode)
}

/// Gives `path` the mode `mode`.
fn set_mode(path: &Path, mode: u32) -> Result<(), Error> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| Error::io("set mode", path, e))
}

/// Removes the temporary tree (one a crashed earlier attempt left, or this
/// attempt's), making its directories writable first, so a read-only tree
/// can go away. A missing tree is nothing to do.
fn discard(temp: &Path) {
    make_writable(temp);
    drop(fsx::remove_tree(temp));
}

/// Gives every directory under `path` the write permission, so its files
/// can be removed.
fn make_writable(path: &Path) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                make_writable(&entry.path());
            }
        }
        drop(std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(0o700),
        ));
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
