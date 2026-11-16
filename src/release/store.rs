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
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::name::StateRoot;

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

/// `<state root>/.releases`.
#[must_use]
pub fn store_root(state_root: &StateRoot) -> PathBuf {
    state_root.path().join(STORE_DIR)
}

/// The entry hash of the inputs, by the rule in the module documentation.
/// The DSH path is made canonical first; a DSH that does not resolve is an
/// `Io` error.
pub fn entry_hash(inputs: &EntryInputs<'_>) -> Result<String, Error> {
    let _ = inputs;
    Err(not_built())
}

/// Writes the entry for `inputs` under `root` (created 0700 when missing)
/// and returns its path; an entry that already exists is reused as it is.
/// A label that is not a valid release name is `release-invalid`.
pub fn add_entry(root: &Path, inputs: &EntryInputs<'_>) -> Result<PathBuf, Error> {
    let _ = (root, inputs);
    Err(not_built())
}

/// Whether `path` is an entry of the store at `root`: directly under it,
/// named `<32 hex>-dshd-release-<label>`, a directory that holds
/// `bin/dshd-run`. Does not follow a symlinked `path`.
#[must_use]
pub fn is_entry(root: &Path, path: &Path) -> bool {
    let _ = (root, path);
    false
}

/// Points registry name `name` in `registry_dir` (created 0700 when
/// missing) at `entry`, replacing what it pointed at.
pub fn register(registry_dir: &Path, name: &str, entry: &Path) -> Result<(), Error> {
    let _ = (registry_dir, name, entry);
    Err(not_built())
}

/// The entry registry name `name` points at, when it does.
pub fn registered(registry_dir: &Path, name: &str) -> Result<Option<PathBuf>, Error> {
    let _ = (registry_dir, name);
    Err(not_built())
}

/// The entries `release prune` removes: every entry not in `held` (the
/// targets of registry names and of every instance's `current` and
/// `booted`), sorted by path.
#[must_use]
pub fn prune_selection(entries: &[PathBuf], held: &BTreeSet<PathBuf>) -> Vec<PathBuf> {
    let _ = (entries, held);
    Vec::new()
}

fn not_built() -> Error {
    crate::platform::launchd::not_built("the directory release store")
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
