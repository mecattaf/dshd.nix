//! The directory release store.

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::fsx::testing::Scratch;
use crate::release::version_label;

struct Inputs {
    scratch: Scratch,
    dsh: PathBuf,
    runner: PathBuf,
}

fn inputs(label: &str) -> Inputs {
    let scratch = Scratch::new(label);
    let dsh = scratch.path().join("dsh-real");
    std::fs::write(&dsh, "#!/bin/sh\n").unwrap();
    let runner = scratch.path().join("dshd-run");
    std::fs::write(&runner, b"runner bytes").unwrap();
    Inputs {
        scratch,
        dsh,
        runner,
    }
}

impl Inputs {
    fn entry<'a>(&'a self, json: &'a [u8]) -> EntryInputs<'a> {
        EntryInputs {
            label: "stable",
            dsh: &self.dsh,
            runner: &self.runner,
            release_json: json,
        }
    }

    fn root(&self) -> PathBuf {
        self.scratch.path().join("state/.releases")
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

// catches: an entry with the wrong layout, writable after creation, or a name version_label cannot read
#[test]
#[ignore = "packet D16: directory release store"]
fn add_writes_a_read_only_entry() {
    let fixture = inputs("store-add");
    let entry = add_entry(&fixture.root(), &fixture.entry(b"{}")).unwrap();
    assert_eq!(entry.parent(), Some(fixture.root().as_path()));
    assert_eq!(version_label(&entry), "stable");
    assert!(is_entry(&fixture.root(), &entry));
    assert_eq!(
        std::fs::read_link(entry.join("bin/dsh")).unwrap(),
        fixture.dsh.canonicalize().unwrap()
    );
    assert_eq!(
        std::fs::read(entry.join("bin/dshd-run")).unwrap(),
        b"runner bytes"
    );
    assert_eq!(
        std::fs::read(entry.join("share/dshd/release.json")).unwrap(),
        b"{}"
    );
    assert_eq!(mode(&entry), READ_ONLY_EXEC);
    assert_eq!(mode(&entry.join("bin")), READ_ONLY_EXEC);
    assert_eq!(mode(&entry.join("bin/dshd-run")), READ_ONLY_EXEC);
    assert_eq!(mode(&entry.join("share/dshd/release.json")), READ_ONLY_FILE);
    let leftovers: Vec<_> = std::fs::read_dir(fixture.root()).unwrap().collect();
    assert_eq!(leftovers.len(), 1, "a temporary directory was left behind");
}

// catches: identical inputs making a second entry, or different inputs reusing the first
#[test]
#[ignore = "packet D16: directory release store"]
fn identical_inputs_reuse_the_entry() {
    let fixture = inputs("store-reuse");
    let first = add_entry(&fixture.root(), &fixture.entry(b"{}")).unwrap();
    let again = add_entry(&fixture.root(), &fixture.entry(b"{}")).unwrap();
    assert_eq!(first, again);
    let other = add_entry(&fixture.root(), &fixture.entry(b"{\"environment\":{}}")).unwrap();
    assert_ne!(first, other);
    let hash = entry_hash(&fixture.entry(b"{}")).unwrap();
    assert_eq!(hash.len(), 32);
    assert!(
        hash.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    assert!(
        first
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with(&hash)
    );
}

// catches: a label that would escape the store or break the entry name accepted
#[test]
#[ignore = "packet D16: directory release store"]
fn bad_labels_are_refused() {
    let fixture = inputs("store-label");
    for label in ["", "../x", "a/b", "has space"] {
        let entry = EntryInputs {
            label,
            ..fixture.entry(b"{}")
        };
        let error = add_entry(&fixture.root(), &entry).unwrap_err();
        assert_eq!(error.kind(), "release-invalid", "{label:?}");
    }
}

// catches: paths outside the store, or lookalike names, accepted as entries
#[test]
#[ignore = "packet D16: directory release store"]
fn is_entry_needs_the_shape_and_the_place() {
    let fixture = inputs("store-is-entry");
    let entry = add_entry(&fixture.root(), &fixture.entry(b"{}")).unwrap();
    assert!(!is_entry(
        &fixture.root(),
        &fixture.scratch.path().join("elsewhere")
    ));
    assert!(!is_entry(
        &fixture.root(),
        &fixture.root().join("short-dshd-release-x")
    ));
    assert!(!is_entry(
        &fixture.scratch.path().join("other-root"),
        &entry
    ));
}

// catches: a registry name not re-pointed, or a missing name read as an error
#[test]
#[ignore = "packet D17: release registry and prune"]
fn the_registry_points_names_at_entries() {
    let fixture = inputs("store-registry");
    let registry = fixture.scratch.path().join("config/releases");
    assert_eq!(registered(&registry, "stable").unwrap(), None);
    let first = add_entry(&fixture.root(), &fixture.entry(b"{}")).unwrap();
    register(&registry, "stable", &first).unwrap();
    assert_eq!(registered(&registry, "stable").unwrap(), Some(first));
    let second = add_entry(&fixture.root(), &fixture.entry(b"[]")).unwrap();
    register(&registry, "stable", &second).unwrap();
    assert_eq!(registered(&registry, "stable").unwrap(), Some(second));
    assert_eq!(mode(&registry), 0o700);
}

// catches: prune removing an entry a registry name or an instance still holds
#[test]
#[ignore = "packet D17: release registry and prune"]
fn prune_keeps_every_held_entry() {
    let entries: Vec<PathBuf> = ["/s/c", "/s/a", "/s/b"].iter().map(PathBuf::from).collect();
    let held: BTreeSet<PathBuf> = [PathBuf::from("/s/b")].into_iter().collect();
    assert_eq!(
        prune_selection(&entries, &held),
        [PathBuf::from("/s/a"), PathBuf::from("/s/c")]
    );
    let all: BTreeSet<PathBuf> = entries.iter().cloned().collect();
    assert!(prune_selection(&entries, &all).is_empty());
}
