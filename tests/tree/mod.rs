//! The state of a directory tree, for the proofs that a run left a DSH home
//! as it was. Shared by `tests/cli.rs` and `tests/runner.rs`.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// `root` and every entry under it, links not followed, each with what a
/// write changes: its kind, mode, inode, modification and change times,
/// and its bytes or a link's target. So a file rewritten with the same
/// bytes, or replaced by a rename, differs, and the times of a directory
/// show an entry that was added and removed again. An absent `root` has no
/// entries.
pub(crate) fn tree(root: &Path) -> Result<BTreeMap<PathBuf, String>, Box<dyn Error>> {
    let mut entries = BTreeMap::new();
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        found => add(root, &found?, &mut entries)?,
    }
    Ok(entries)
}

fn add(
    path: &Path,
    metadata: &fs::Metadata,
    entries: &mut BTreeMap<PathBuf, String>,
) -> Result<(), Box<dyn Error>> {
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
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            add(&entry.path(), &fs::symlink_metadata(entry.path())?, entries)?;
        }
        format!("directory, {stamp}")
    } else if metadata.is_symlink() {
        format!("link to {}, {stamp}", fs::read_link(path)?.display())
    } else {
        let bytes = fs::read(path)?;
        format!("file, {stamp}: {}", String::from_utf8_lossy(&bytes))
    };
    entries.insert(path.to_owned(), described);
    Ok(())
}
