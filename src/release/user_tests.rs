//! The user release registry and the release verbs' logic.

use std::fs;
use std::os::unix::fs::symlink;

use super::*;
use crate::fsx::testing::{Scratch, fake_nix_store, install_script};
use crate::name::{RELEASE_DSH, RELEASE_FILE};
use crate::release::{
    Registries, in_directory_store, registry_target_in, resolve_in, validate, version_label,
};

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

fn environment(mode: &str) -> ReleaseEnvironment {
    ReleaseEnvironment {
        permission_mode: mode.to_owned(),
        ..ReleaseEnvironment::default()
    }
}

/// A scratch user: a state root, a configuration root, a system registry
/// that holds nothing, a DSH and a runner.
struct User {
    scratch: Scratch,
    registries: Registries,
    state_root: StateRoot,
    dsh: PathBuf,
    runner: PathBuf,
}

fn user(label: &str) -> User {
    let scratch = Scratch::new(label);
    let state_root = StateRoot::new(scratch.path().join("state"));
    let registries = Registries::new(
        &scratch.path().join("etc/releases"),
        Some(&scratch.path().join("config")),
        &state_root,
    );
    let dsh = scratch.path().join("dsh");
    install_script(&dsh, "#!/bin/sh\nexit 0\n");
    let runner = scratch.path().join("dshd-run");
    install_script(&runner, "#!/bin/sh\nexit 0\n");
    User {
        scratch,
        registries,
        state_root,
        dsh,
        runner,
    }
}

impl User {
    /// An entry written straight through the store, named by a link made
    /// here, so a test needs no registry writer.
    fn entry(&self, label: &str, json: &[u8]) -> PathBuf {
        let inputs = EntryInputs {
            label,
            dsh: &self.dsh,
            runner: &self.runner,
            release_json: json,
        };
        let entry = store::add_entry(&self.registries.store, &inputs).unwrap();
        fs::canonicalize(entry).unwrap()
    }

    fn name(&self, name: &str, target: &Path) {
        let registry = self.registries.user.clone().unwrap();
        fs::create_dir_all(&registry).unwrap();
        let link = registry.join(name);
        drop(fs::remove_file(&link));
        symlink(target, link).unwrap();
    }
}

// catches: a release written without its permission mode, or with a mode inherited from a default, or a
// release.json in another shape than lib.mkRelease writes
#[test]
fn release_json_states_the_permission_mode() {
    let mut given = environment("workspace-write");
    given
        .environment
        .insert("DSH_TELEMETRY_DISABLED".to_owned(), "1".to_owned());
    given.unset.push("OPENAI_API_KEY".to_owned());
    assert_eq!(
        String::from_utf8(given.release_json().unwrap()).unwrap(),
        "{\"environment\":{\"DSH_PERMISSION_MODE\":\"workspace-write\",\"DSH_TELEMETRY_DISABLED\":\"1\"},\
         \"unsetEnvironment\":[\"OPENAI_API_KEY\"]}\n"
    );
    for mode in ["", "Workspace", "has space", "a=b", &"x".repeat(65)] {
        let error = environment(mode).release_json().unwrap_err();
        assert_eq!(error.kind(), "invalid-argument", "{mode:?}");
    }
    assert!(permission_mode_shaped("danger-full-access"));
}

// catches: the refusals of lib.mkRelease skipped, or the permission mode set a second way through --env
#[test]
fn environment_refuses_what_mk_release_refuses() {
    let refused = |change: &dyn Fn(&mut ReleaseEnvironment)| {
        let mut given = environment("read-only");
        change(&mut given);
        given.check().unwrap_err().to_string()
    };
    assert!(
        refused(&|given| {
            given
                .environment
                .insert(PERMISSION_MODE.to_owned(), "x".to_owned());
        })
        .contains("--permission-mode")
    );
    assert!(refused(&|given| given.unset.push(String::new())).contains("not empty"));
    assert!(refused(&|given| given.unset.push("A=B".to_owned())).contains("'='"));
    assert!(refused(&|given| given.unset.push(PERMISSION_MODE.to_owned())).contains("also sets"));
    assert!(
        refused(&|given| {
            given.environment.insert("X".to_owned(), "1".to_owned());
            given.unset.push("X".to_owned());
        })
        .contains("also sets")
    );
    for name in RUNNER_SET {
        assert!(refused(&|given| given.unset.push(name.to_owned())).contains("runner"));
    }
    assert!(
        refused(&|given| given.unset.extend(["Y".to_owned(), "Y".to_owned()])).contains("twice")
    );
}

// catches: an entry's current and booted roots handed to nix-store, or a Nix store path no longer rooted
// through it
#[test]
fn a_directory_entry_is_rooted_with_a_plain_link() {
    let fixture = user("user-roots");
    let entry = fixture.entry("stable", b"{}");
    assert!(in_directory_store(&entry));
    assert!(!in_directory_store(Path::new(
        "/nix/store/0123456789abcdef0123456789abcdef-dshd-release-x"
    )));
    assert!(!in_directory_store(&fixture.dsh));
    let (nix_store, log) = fake_nix_store(fixture.scratch.path());
    let link = fixture.scratch.path().join("current");
    crate::release::add_root(&nix_store, &entry, &link).unwrap();
    assert_eq!(fs::read_link(&link).unwrap(), entry);
    assert!(!log.exists(), "nix-store ran for a directory entry");
    assert_eq!(version_label(&entry), "stable");
}

// catches: the user registry read for a name the system registry holds, a user name not found, or an entry
// refused by path
#[test]
fn names_resolve_through_the_system_registry_first() {
    let fixture = user("user-resolve");
    let json = br#"{"environment":{"DSH_PERMISSION_MODE":"read-only"},"unsetEnvironment":[]}"#;
    let entry = fixture.entry("stable", json);
    fixture.name("stable", &entry);
    let release = resolve_in(&fixture.registries, Some("stable")).unwrap();
    assert_eq!(release.path, entry);
    assert_eq!(release.name, "stable");
    assert_eq!(
        release
            .file
            .environment
            .get(PERMISSION_MODE)
            .map(String::as_str),
        Some("read-only")
    );
    let by_path = resolve_in(&fixture.registries, entry.to_str()).unwrap();
    assert_eq!(by_path.path, entry);

    let system = fixture.registries.system.clone();
    fs::create_dir_all(&system).unwrap();
    symlink(fixture.scratch.path().join("gone"), system.join("stable")).unwrap();
    assert_eq!(
        registry_target_in(&fixture.registries, "stable").unwrap(),
        None,
        "a name the system registry holds, even dangling, never reaches the user registry"
    );
    assert_eq!(
        resolve_in(&fixture.registries, None).unwrap_err().kind(),
        "release-not-found"
    );
    let elsewhere = fixture.scratch.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let refused = resolve_in(&fixture.registries, elsewhere.to_str()).unwrap_err();
    assert_eq!(refused.kind(), "release-invalid");
    assert!(
        refused
            .to_string()
            .contains("nor an entry of the release store")
    );
}

// catches: rm removing an entry with the name, a missing name taken as removed, or a dangling name refused
#[test]
fn remove_name_leaves_the_entry() {
    let fixture = user("user-rm");
    let entry = fixture.entry("stable", b"{}");
    fixture.name("stable", &entry);
    let name = InstanceName::parse("stable").unwrap();
    assert_eq!(
        remove_name(&fixture.registries, &name).unwrap(),
        Some(entry.clone())
    );
    assert!(entry.is_dir());
    assert_eq!(
        remove_name(&fixture.registries, &name).unwrap_err().kind(),
        "release-not-found"
    );
    fixture.name("stable", &fixture.scratch.path().join("gone"));
    assert_eq!(remove_name(&fixture.registries, &name).unwrap(), None);
}

// catches: an entry held by an instance's current or booted link, or by a name, reported as held by
// nothing, or a dangling name taken for a holder
#[test]
fn holders_name_every_link_that_points_at_an_entry() {
    let fixture = user("user-holders");
    let first = fixture.entry("stable", b"{}");
    let second = fixture.entry("stable", b"[]");
    fixture.name("stable", &second);
    fixture.name("dangling", &fixture.scratch.path().join("gone"));
    let dir = fixture
        .state_root
        .instance(&InstanceName::parse("web-1").unwrap());
    fs::create_dir_all(dir.path()).unwrap();
    symlink(&second, dir.current()).unwrap();
    symlink(&first, dir.booted()).unwrap();
    let held = holders(&fixture.registries, &fixture.state_root).unwrap();
    let of = |path: &Path| held.get(path).cloned().unwrap_or_default();
    assert_eq!(
        of(&second),
        Holders {
            names: vec!["stable".to_owned()],
            current: vec!["web-1".to_owned()],
            booted: Vec::new(),
        }
    );
    assert_eq!(
        of(&first),
        Holders {
            names: Vec::new(),
            current: Vec::new(),
            booted: vec!["web-1".to_owned()],
        }
    );
    assert_eq!(held.len(), 2);
    let mut listed = entries(&fixture.registries).unwrap();
    listed.sort();
    let mut expected = vec![first, second];
    expected.sort();
    assert_eq!(listed, expected);
    let names = names(&fixture.registries).unwrap();
    assert_eq!(
        names
            .iter()
            .map(|name| name.name.as_str())
            .collect::<Vec<_>>(),
        ["dangling", "stable"]
    );
    assert_eq!(names.first().unwrap().target, None);
}

// catches: the runner looked for anywhere but beside dshd or in the package's libexec
#[test]
fn the_runner_comes_from_beside_the_running_dshd() {
    match own_runner() {
        Ok(runner) => assert!(runner.ends_with("dshd-run"), "{}", runner.display()),
        Err(error) => assert!(error.to_string().contains("no executable dshd-run")),
    }
}

// catches: a release that does not validate as one, or one whose entry is writable
#[test]
#[ignore = "packet D17: release registry and prune"]
fn add_writes_an_entry_and_points_the_name_at_it() {
    let fixture = user("user-add");
    let name = InstanceName::parse("stable").unwrap();
    let first = add(
        &fixture.registries,
        &name,
        &fixture.dsh,
        &fixture.runner,
        &environment("workspace-write"),
    )
    .unwrap();
    assert!(first.created);
    assert_eq!(first.previous, None);
    assert_eq!(mode(&first.entry), store::READ_ONLY_EXEC);
    assert_eq!(
        fs::read_link(first.entry.join(RELEASE_DSH)).unwrap(),
        fs::canonicalize(&fixture.dsh).unwrap()
    );
    let file = validate(&first.entry).unwrap();
    assert_eq!(
        file.environment.get(PERMISSION_MODE).map(String::as_str),
        Some("workspace-write")
    );
    assert_eq!(mode(&first.entry.join(RELEASE_FILE)), store::READ_ONLY_FILE);
    let again = add(
        &fixture.registries,
        &name,
        &fixture.dsh,
        &fixture.runner,
        &environment("workspace-write"),
    )
    .unwrap();
    assert_eq!(again.entry, first.entry);
    assert!(!again.created);
    assert_eq!(again.previous.as_ref(), Some(&first.entry));
    let other = add(
        &fixture.registries,
        &name,
        &fixture.dsh,
        &fixture.runner,
        &environment("read-only"),
    )
    .unwrap();
    assert_ne!(other.entry, first.entry);
    assert_eq!(other.previous, Some(first.entry));
    assert_eq!(
        resolve_in(&fixture.registries, Some("stable"))
            .unwrap()
            .path,
        other.entry
    );
    let not_executable = fixture.scratch.path().join("plain");
    fs::write(&not_executable, "x").unwrap();
    let refused = add(
        &fixture.registries,
        &name,
        &not_executable,
        &fixture.runner,
        &environment("read-only"),
    )
    .unwrap_err();
    assert_eq!(refused.kind(), "release-invalid");
}

// catches: prune removing an entry a name or an instance link holds, or leaving one nothing holds
#[test]
#[ignore = "packet D17: release registry and prune"]
fn prune_removes_only_what_nothing_holds() {
    let fixture = user("user-prune");
    let held_by_name = fixture.entry("stable", b"{}");
    let held_by_instance = fixture.entry("stable", b"[]");
    let loose = fixture.entry("old", b"{}");
    fixture.name("stable", &held_by_name);
    let dir = fixture
        .state_root
        .instance(&InstanceName::parse("web-1").unwrap());
    fs::create_dir_all(dir.path()).unwrap();
    symlink(&held_by_instance, dir.booted()).unwrap();
    let held: BTreeSet<PathBuf> = holders(&fixture.registries, &fixture.state_root)
        .unwrap()
        .into_keys()
        .collect();
    assert_eq!(
        prune(&fixture.registries, &held).unwrap(),
        std::slice::from_ref(&loose)
    );
    assert!(!loose.exists());
    assert!(held_by_name.is_dir() && held_by_instance.is_dir());
    assert!(prune(&fixture.registries, &held).unwrap().is_empty());
}
