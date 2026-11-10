//! File-system primitives with the crate's error mapping: atomic writes,
//! private directories, exact symlink replacement, advisory locks and the
//! clock. Every path operation outside this module goes through it, so an
//! `io` error always names the action and the path.

use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, FileType, Metadata, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::Error;
use crate::record::to_json;

/// Mode of every directory dshd creates: instance state is the user's alone.
const PRIVATE_DIR_MODE: u32 = 0o700;
/// Mode of a lock file. It holds no data, only the advisory lock.
const LOCK_MODE: u32 = 0o600;

/// Distinguishes the temporary names of concurrent writers in one process;
/// the process id distinguishes processes.
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// An exclusive advisory lock, released on drop.
#[derive(Debug)]
pub struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        // Unlocking before the close also releases a descriptor that a child
        // forked by another thread still shares until its exec. A failed
        // unlock has no one to report to, and the close releases it anyway.
        drop(self.0.unlock());
    }
}

/// Reads and parses a JSON file. A missing file is `Ok(None)`.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, Error> {
    let Some(text) = read_text(path)? else {
        return Ok(None);
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| Error::io("parse", path, io::Error::from(e)))
}

/// Serialises with [`crate::record::to_json`] and writes atomically
/// (temp file in the same directory, `fsync`, rename) with `mode`.
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T, mode: u32) -> Result<(), Error> {
    let bytes = to_json(value).map_err(|e| Error::io("serialise", path, io::Error::from(e)))?;
    write_atomic(path, &bytes, mode)
}

/// Writes `bytes` atomically with `mode`.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<(), Error> {
    let temp = temp_beside(path)?;
    let written = write_new(&temp, bytes, mode).and_then(|()| rename(&temp, path));
    if written.is_err() {
        discard_temp(&temp);
    }
    written
}

/// Reads a whole file as text. A missing file is `Ok(None)`.
pub fn read_text(path: &Path) -> Result<Option<String>, Error> {
    absent_as_none(fs::read_to_string(path), "read", path)
}

/// Reads a symlink target. A missing link is `Ok(None)`.
pub fn read_link(path: &Path) -> Result<Option<PathBuf>, Error> {
    absent_as_none(fs::read_link(path), "read link", path)
}

/// Points `link` at `target`, replacing any existing link atomically
/// (create a temp link beside it, rename over).
pub fn symlink_replace(target: &Path, link: &Path) -> Result<(), Error> {
    let temp = temp_beside(link)?;
    symlink(target, &temp)?;
    let replaced = rename(&temp, link);
    if replaced.is_err() {
        discard_temp(&temp);
    }
    replaced
}

/// Removes `link` only when it is a symlink whose target is exactly
/// `expected`. Returns whether it was removed. A missing link is `Ok(false)`;
/// a link with another target is `Ok(false)` too, untouched.
pub fn unlink_if_target(link: &Path, expected: &Path) -> Result<bool, Error> {
    if !kind(link)?.is_some_and(|kind| kind.is_symlink()) {
        return Ok(false);
    }
    if read_link(link)?.as_deref() != Some(expected) {
        return Ok(false);
    }
    Ok(absent_as_none(fs::remove_file(link), "remove", link)?.is_some())
}

/// Creates `path` with mode 0700, parents included. Existing is fine.
pub fn create_dir_private(path: &Path) -> Result<(), Error> {
    DirBuilder::new()
        .recursive(true)
        .mode(PRIVATE_DIR_MODE)
        .create(path)
        .map_err(|e| Error::io("create directory", path, e))
}

/// Removes a tree. A missing path is fine. Symlinks inside are removed, not
/// followed.
pub fn remove_tree(path: &Path) -> Result<(), Error> {
    let removed = match kind(path)? {
        None => return Ok(()),
        Some(kind) if kind.is_dir() => fs::remove_dir_all(path),
        Some(_) => fs::remove_file(path),
    };
    absent_as_none(removed, "remove", path).map(|_| ())
}

/// Takes an exclusive lock on `path`, creating it 0600 when absent. Blocks.
pub fn lock_exclusive(path: &Path) -> Result<Lock, Error> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(LOCK_MODE)
        .open(path)
        .map_err(|e| Error::io("open", path, e))?;
    lock_file(file, path)
}

fn lock_file(file: File, path: &Path) -> Result<Lock, Error> {
    file.lock().map_err(|e| Error::io("lock", path, e))?;
    let opened = file.metadata().map_err(|e| Error::io("stat", path, e))?;
    let current = fs::symlink_metadata(path).map_err(|e| Error::io("lock", path, e))?;
    if !current.is_file() || (opened.dev(), opened.ino()) != (current.dev(), current.ino()) {
        return Err(Error::io(
            "lock",
            path,
            io::Error::from(io::ErrorKind::NotFound),
        ));
    }
    Ok(Lock(file))
}

/// Whole seconds since the Unix epoch.
pub fn unix_now() -> u64 {
    // A clock set before the epoch saturates at zero.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Canonicalises `path` and checks it is a directory.
pub fn canonical_dir(path: &Path) -> Result<PathBuf, Error> {
    let canonical = fs::canonicalize(path).map_err(|e| Error::io("resolve", path, e))?;
    let metadata = fs::metadata(&canonical).map_err(|e| Error::io("resolve", path, e))?;
    if metadata.is_dir() {
        Ok(canonical)
    } else {
        Err(Error::io(
            "resolve",
            path,
            io::Error::from(io::ErrorKind::NotADirectory),
        ))
    }
}

/// Canonicalises `path`, following every link. A missing path or a
/// dangling link is `Ok(None)`.
pub(crate) fn canonical_path(path: &Path) -> Result<Option<PathBuf>, Error> {
    absent_as_none(fs::canonicalize(path), "resolve", path)
}

/// The type of `path` itself (a link is a link). `Ok(None)` when absent.
pub(crate) fn kind(path: &Path) -> Result<Option<FileType>, Error> {
    Ok(absent_as_none(fs::symlink_metadata(path), "stat", path)?
        .map(|metadata| metadata.file_type()))
}

/// The metadata of what `path` resolves to. `Ok(None)` when absent or dangling.
pub(crate) fn metadata(path: &Path) -> Result<Option<Metadata>, Error> {
    absent_as_none(fs::metadata(path), "stat", path)
}

/// The entries of a directory with their own types (links not followed),
/// sorted by name. A missing directory has no entries.
pub(crate) fn list_dir(path: &Path) -> Result<Vec<(OsString, FileType)>, Error> {
    let Some(entries) = absent_as_none(fs::read_dir(path), "list", path)? else {
        return Ok(Vec::new());
    };
    let mut listed = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| Error::io("list", path, e))?;
        let file_type = entry
            .file_type()
            .map_err(|e| Error::io("stat", entry.path(), e))?;
        listed.push((entry.file_name(), file_type));
    }
    listed.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    Ok(listed)
}

/// Creates the symlink `link` → `target`; an existing `link` is an
/// `AlreadyExists` I/O error, never replaced.
fn symlink(target: &Path, link: &Path) -> Result<(), Error> {
    std::os::unix::fs::symlink(target, link).map_err(|e| Error::io("create link", link, e))
}

/// Syncs a populated directory before its parent makes it visible.
fn sync_directory(path: &Path) -> Result<(), Error> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| Error::io("sync", path, e))
}

/// Renames `from` over `to` and syncs the directory, so the new name
/// survives a crash.
pub(crate) fn rename(from: &Path, to: &Path) -> Result<(), Error> {
    fs::rename(from, to).map_err(|e| Error::io("rename", from, e))?;
    let parent = match to.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    sync_directory(parent)
}

/// Maps `NotFound` to `Ok(None)` and every other failure to `Error::Io`.
fn absent_as_none<T>(
    result: io::Result<T>,
    action: &'static str,
    path: &Path,
) -> Result<Option<T>, Error> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(action, path, e)),
    }
}

/// `.<name>.tmp-<pid>-<n>` beside `path`: same directory, so the rename is
/// atomic, and a dot name, so listings skip it.
fn temp_beside(path: &Path) -> Result<PathBuf, Error> {
    let Some(name) = path.file_name() else {
        return Err(Error::io(
            "write",
            path,
            io::Error::from(io::ErrorKind::InvalidInput),
        ));
    };
    let mut temp = OsString::from(".");
    temp.push(name);
    temp.push(format!(
        ".tmp-{}-{}",
        process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    Ok(path.with_file_name(temp))
}

/// Creates `temp` (never following or reusing an existing name), writes and
/// syncs it.
fn write_new(temp: &Path, bytes: &[u8], mode: u32) -> Result<(), Error> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(temp)
        .map_err(|e| Error::io("create", temp, e))?;
    file.write_all(bytes)
        .map_err(|e| Error::io("write", temp, e))?;
    file.sync_all().map_err(|e| Error::io("sync", temp, e))
}

/// Removes a temporary name after a failed write or replace. The caller
/// reports the failure that matters; a leftover dot name is inert.
fn discard_temp(temp: &Path) {
    drop(fs::remove_file(temp));
}

#[cfg(test)]
pub(crate) mod testing {
    //! Scratch directories, a fake `nix-store` and a tree listing for the
    //! unit tests of the state modules.

    use std::collections::BTreeMap;
    use std::fs;
    use std::io;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::process::{self, Command};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::tool::Tool;

    /// Bounded retries while a freshly written script is still open for
    /// writing in a child that another test thread forked.
    const BUSY_RETRIES: u32 = 1000;

    static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

    /// A directory under the system temp directory, removed on drop.
    pub(crate) struct Scratch(PathBuf);

    impl Scratch {
        pub(crate) fn new(label: &str) -> Self {
            let sequence = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("dshd-{label}-{}-{sequence}", process::id()));
            if path.exists() {
                fs::remove_dir_all(&path).unwrap();
            }
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            drop(fs::remove_dir_all(&self.0));
        }
    }

    /// A fake `nix-store` in `dir`: it appends its argv as one line to
    /// `<dir>/nix-store.log` and makes the `--add-root` link point at the
    /// `--realise` path.
    pub(crate) fn fake_nix_store(dir: &Path) -> (Tool, PathBuf) {
        let log = dir.join("nix-store.log");
        let script = dir.join("nix-store");
        let text = format!(
            r#"#!/bin/sh
[ "$1" = --version ] && exit 0
printf '%s\n' "$*" >> '{log}'
realise= root=
while [ $# -gt 0 ]; do
  case $1 in
    --realise) realise=$2; shift ;;
    --add-root) root=$2; shift ;;
  esac
  shift
done
mkdir -p "$(dirname "$root")"
ln -sfn "$realise" "$root"
printf '%s\n' "$root"
"#,
            log = log.display()
        );
        install_script(&script, &text);
        (Tool::at("nix-store", &script), log)
    }

    /// Writes an executable script and runs it once with `--version` until
    /// the kernel stops reporting it busy, so later executions cannot race
    /// a descriptor a concurrently forked child inherited during the write.
    pub(crate) fn install_script(path: &Path, text: &str) {
        fs::write(path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        for _ in 0..BUSY_RETRIES {
            match Command::new(path).arg("--version").status() {
                Ok(status) => {
                    assert!(status.success(), "{} --version failed", path.display());
                    return;
                }
                Err(e) if e.kind() == io::ErrorKind::ExecutableFileBusy => std::thread::yield_now(),
                Err(e) => panic!("{}: {e}", path.display()),
            }
        }
        panic!("{} stayed busy", path.display());
    }

    /// `root` and every entry under it, links not followed, each with what
    /// a write changes: its kind, mode, inode, modification and change
    /// times, and its bytes or a link's target. So a file rewritten with the
    /// same bytes, or replaced by a rename, differs, and the times of a
    /// directory show an entry that was added and removed again. An absent
    /// `root` has no entries.
    pub(crate) fn tree(root: &Path) -> BTreeMap<PathBuf, String> {
        let mut entries = BTreeMap::new();
        match fs::symlink_metadata(root) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            found => add(root, &found.unwrap(), &mut entries),
        }
        entries
    }

    fn add(path: &Path, metadata: &fs::Metadata, entries: &mut BTreeMap<PathBuf, String>) {
        let stamp = format!(
            "mode {:o}, inode {}, modified {}.{:09}, changed {}.{:09}",
            metadata.mode(),
            metadata.ino(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec()
        );
        let described = if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                let entry = entry.unwrap().path();
                add(&entry, &fs::symlink_metadata(&entry).unwrap(), entries);
            }
            format!("directory, {stamp}")
        } else if metadata.is_symlink() {
            format!(
                "link to {}, {stamp}",
                fs::read_link(path).unwrap().display()
            )
        } else {
            let bytes = fs::read(path).unwrap();
            format!("file, {stamp}: {}", String::from_utf8_lossy(&bytes))
        };
        entries.insert(path.to_owned(), described);
    }
}

#[cfg(test)]
#[path = "fsx/unit_tests.rs"]
mod tests;
