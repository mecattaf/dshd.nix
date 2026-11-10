//! Filesystem and lock identity regressions.

use super::testing::Scratch;
use super::*;

// catches: replacing a record keeps stale bytes, loosens its mode, leaves a temporary file, or hides malformed JSON
#[test]
fn atomic_write_mode_and_replace() {
    let scratch = Scratch::new("fsx-write");
    let path = scratch.path().join("record.json");
    write_atomic(&path, b"first\n", 0o600).unwrap();
    write_atomic(&path, b"second\n", 0o600).unwrap();
    assert_eq!(read_text(&path).unwrap().as_deref(), Some("second\n"));
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let names: Vec<OsString> = list_dir(scratch.path())
        .unwrap()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        names,
        vec![OsString::from("record.json")],
        "no temporary file is left behind"
    );

    let value = vec![1_u32, 2, 3];
    write_json_atomic(&path, &value, 0o600).unwrap();
    assert_eq!(read_json::<Vec<u32>>(&path).unwrap(), Some(value));
    assert_eq!(
        read_json::<Vec<u32>>(&scratch.path().join("absent")).unwrap(),
        None
    );
    assert_eq!(read_text(&scratch.path().join("absent")).unwrap(), None);
    fs::write(&path, "{not json").unwrap();
    assert!(matches!(
        read_json::<Vec<u32>>(&path),
        Err(Error::Io {
            action: "parse",
            ..
        })
    ));
}

// catches: a replacement keeps the old link, or unlink removes a different target or a regular file
#[test]
fn symlink_replace_and_exact_unlink() {
    let scratch = Scratch::new("fsx-link");
    let link = scratch.path().join("current");
    let (one, two) = (scratch.path().join("one"), scratch.path().join("two"));
    assert_eq!(read_link(&link).unwrap(), None);
    symlink_replace(&one, &link).unwrap();
    symlink_replace(&two, &link).unwrap();
    assert_eq!(read_link(&link).unwrap(), Some(two.clone()));
    assert!(
        !unlink_if_target(&link, &one).unwrap(),
        "another target stays"
    );
    assert_eq!(read_link(&link).unwrap(), Some(two.clone()));
    assert!(unlink_if_target(&link, &two).unwrap());
    assert_eq!(kind(&link).unwrap(), None);
    assert!(
        !unlink_if_target(&link, &two).unwrap(),
        "a missing link is not an error"
    );
    fs::write(&link, "file").unwrap();
    assert!(
        !unlink_if_target(&link, &two).unwrap(),
        "a regular file is never removed"
    );
}

// catches: tree removal follows a symlink outside the owned tree, or creates state directories with loose permissions
#[test]
fn remove_tree_keeps_link_targets() {
    let scratch = Scratch::new("fsx-remove");
    let outside = scratch.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), "kept").unwrap();
    let tree = scratch.path().join("tree");
    create_dir_private(&tree.join("nested")).unwrap();
    assert_eq!(
        fs::metadata(&tree).unwrap().permissions().mode() & 0o777,
        0o700
    );
    symlink(&outside, &tree.join("nested/link")).unwrap();
    remove_tree(&tree).unwrap();
    remove_tree(&tree).unwrap();
    assert_eq!(kind(&tree).unwrap(), None);
    assert_eq!(
        read_text(&outside.join("keep")).unwrap().as_deref(),
        Some("kept")
    );
}

// catches: a released lock cannot be reacquired, its file is readable by others, or a regular file is accepted as a directory
#[test]
fn lock_file_is_private_and_reusable() {
    let scratch = Scratch::new("fsx-lock");
    let path = scratch.path().join("lock");
    drop(lock_exclusive(&path).unwrap());
    let held = lock_exclusive(&path).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(held);
    assert!(canonical_dir(scratch.path()).is_ok());
    assert!(canonical_dir(&path).is_err(), "a file is not a directory");
}

// catches: a waiter acts under an unlinked old lock after the path has been recreated for a new instance
#[test]
fn a_waiter_refuses_a_replaced_lock_inode() {
    let scratch = Scratch::new("lock-replaced");
    let path = scratch.path().join("lock");
    let first = lock_exclusive(&path).unwrap();
    let waiting_file = OpenOptions::new().write(true).open(&path).unwrap();
    let waiting_path = path.clone();
    let waiter = std::thread::spawn(move || lock_file(waiting_file, &waiting_path));
    fs::remove_file(&path).unwrap();
    fs::write(&path, "replacement").unwrap();
    drop(first);
    let result = waiter.join().unwrap();
    assert!(
        matches!(result, Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound)
    );
    drop(lock_exclusive(&path).unwrap());
    assert_eq!(fs::read_to_string(&path).unwrap(), "replacement");
}
