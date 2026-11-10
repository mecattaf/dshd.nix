//! Tests for release identity, roots and closure differences.

use std::fs;

use super::*;
use crate::fsx::testing::{Scratch, fake_nix_store, install_script};

/// A release directory with the given entries present.
fn release_dir(root: &Path, runner: bool, release_json: Option<&str>) -> PathBuf {
    let dir = root.join("release");
    fs::create_dir_all(dir.join("bin")).unwrap();
    fs::create_dir_all(dir.join("share/dshd")).unwrap();
    install_script(&dir.join(RELEASE_DSH), "#!/bin/sh\nexit 0\n");
    if runner {
        install_script(&dir.join(RELEASE_RUNNER), "#!/bin/sh\nexit 0\n");
    }
    if let Some(text) = release_json {
        fs::write(dir.join(RELEASE_FILE), text).unwrap();
    }
    dir
}

// catches: current, booted and registry differences are collapsed, hiding a missing, outdated or restart-pending release
#[test]
fn state_table() {
    let (r1, r2) = (Path::new("/nix/store/r1"), Path::new("/nix/store/r2"));
    let cases = [
        (Some(r1), None, Some(r1), ReleaseState::Current),
        (Some(r1), Some(r1), Some(r1), ReleaseState::Current),
        (Some(r1), Some(r2), Some(r1), ReleaseState::Stale),
        (Some(r1), Some(r1), Some(r2), ReleaseState::Outdated),
        (Some(r1), Some(r2), Some(r2), ReleaseState::Outdated),
        (Some(r1), Some(r1), None, ReleaseState::Missing),
        (None, Some(r1), Some(r1), ReleaseState::Missing),
        (None, None, None, ReleaseState::Missing),
    ];
    for (index, (current, booted, registry, expected)) in cases.into_iter().enumerate() {
        assert_eq!(
            release_state(current, booted, registry),
            expected,
            "case {index}"
        );
    }
    assert_eq!(ReleaseState::Stale.slug(), "stale");
}

// catches: release labels retain store hashes or discard a name prefix that is not a valid store hash
#[test]
fn version_label_strips_hash_and_prefix() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/fixtures/last-run.json")).unwrap();
    let release_path = Path::new(
        fixture
            .get("releasePath")
            .and_then(serde_json::Value::as_str)
            .unwrap(),
    );
    assert_eq!(version_label(release_path), "dsh-0.2.2-rc.1");
    assert_eq!(
        version_label(Path::new(
            "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-dsh-1.0"
        )),
        "dsh-1.0"
    );
    assert_eq!(version_label(Path::new("/srv/dshd-release-local")), "local");
    assert_eq!(
        version_label(Path::new("/srv/short-hash-name")),
        "short-hash-name"
    );
}

// catches: a release without an executable runner or metadata is accepted, or unknown metadata keys are silently acted on
#[test]
fn validate_rejects_missing_runner() {
    let fixture = include_str!("../../tests/fixtures/release.json");
    let scratch = Scratch::new("release-validate");
    let dir = release_dir(scratch.path(), false, Some(fixture));
    assert!(
        matches!(validate(&dir), Err(Error::ReleaseInvalid { reason, .. }) if reason.contains(RELEASE_RUNNER))
    );
    fs::write(dir.join(RELEASE_RUNNER), "not executable").unwrap();
    assert!(matches!(validate(&dir), Err(Error::ReleaseInvalid { .. })));
    install_script(&dir.join(RELEASE_RUNNER), "#!/bin/sh\nexit 0\n");
    let file = validate(&dir).unwrap();
    assert_eq!(
        file.environment
            .get("DSH_PERMISSION_MODE")
            .map(String::as_str),
        Some("danger-full-access")
    );
    fs::write(dir.join(RELEASE_FILE), r#"{"environment":{},"schema":2}"#).unwrap();
    assert!(
        matches!(validate(&dir), Err(Error::ReleaseInvalid { reason, .. }) if reason.contains("schema"))
    );
    fs::remove_file(dir.join(RELEASE_FILE)).unwrap();
    assert!(matches!(validate(&dir), Err(Error::ReleaseInvalid { .. })));
}

// catches: registry traversal or an absolute path outside the store is accepted as a release, or a missing default is hidden
#[test]
fn resolve_follows_registry_names_only() {
    let scratch = Scratch::new("release-resolve");
    let dir = release_dir(
        scratch.path(),
        true,
        Some(include_str!("../../tests/fixtures/release.json")),
    );
    let registry = scratch.path().join("releases");
    fs::create_dir_all(&registry).unwrap();
    std::os::unix::fs::symlink(&dir, registry.join("stable")).unwrap();
    let release = resolve(&registry, Some("stable")).unwrap();
    assert_eq!(release.name, "stable");
    assert_eq!(release.path, fs::canonicalize(&dir).unwrap());
    assert!(
        matches!(resolve(&registry, None), Err(Error::ReleaseNotFound(name)) if name == DEFAULT_RELEASE)
    );
    assert!(matches!(
        resolve(&registry, Some("../release")),
        Err(Error::ReleaseNotFound(_))
    ));
    let outside = dir.to_str().unwrap();
    assert!(matches!(
        resolve(&registry, Some(outside)),
        Err(Error::ReleaseInvalid { .. })
    ));
    assert_eq!(
        registry_target(&registry, outside).unwrap(),
        Some(fs::canonicalize(&dir).unwrap())
    );
    assert_eq!(registry_target(&registry, "gone").unwrap(), None);
}

// catches: retention fails to create the requested root, or the nix-store argv enables substitution or roots the wrong path
#[test]
fn add_root_realises_without_substitutes() {
    let scratch = Scratch::new("release-root");
    let (nix_store, log) = fake_nix_store(scratch.path());
    let (store_path, link) = (
        scratch.path().join("store-path"),
        scratch.path().join("state/current"),
    );
    add_root(&nix_store, &store_path, &link).unwrap();
    assert_eq!(fs::read_link(&link).unwrap(), store_path);
    let logged = fs::read_to_string(&log).unwrap();
    let expected = format!(
        "--option substitute false --realise {} --add-root {}",
        store_path.display(),
        link.display()
    );
    assert_eq!(logged.trim_end(), expected);
}

// catches: a closure root is taken from a store subpath or from a path whose prefix merely resembles the store
#[test]
fn store_path_of_takes_the_top_entry() {
    assert_eq!(
        store_path_of(Path::new("/nix/store/abc-x/bin/y")),
        Some(PathBuf::from("/nix/store/abc-x"))
    );
    assert_eq!(store_path_of(Path::new("/nix/store")), None);
    assert_eq!(store_path_of(Path::new("/nix/storefront/x")), None);
}

// catches: a stopped instance keeps the old booted root, or collapse rewrites the recorded actual run
#[test]
fn collapse_booted_releases_only_the_old_closure() {
    let scratch = Scratch::new("release-collapse");
    let dir = InstanceDir::new(scratch.path().join("instance"));
    fs::create_dir_all(dir.path()).unwrap();
    let old = scratch.path().join("old");
    let new = scratch.path().join("new");
    std::os::unix::fs::symlink(&old, dir.booted()).unwrap();
    std::os::unix::fs::symlink(&new, dir.current()).unwrap();
    fs::write(dir.last_run_json(), "the run used the old release\n").unwrap();
    assert!(collapse_booted(&dir).unwrap());
    assert_eq!(fs::read_link(dir.booted()).unwrap(), new);
    assert!(!collapse_booted(&dir).unwrap());
    assert_eq!(
        fs::read_to_string(dir.last_run_json()).unwrap(),
        "the run used the old release\n"
    );
    fs::remove_file(dir.current()).unwrap();
    assert!(collapse_booted(&dir).is_err());
}

// catches: update dumping store paths instead of nix's own package diff, or a failing or absent nix
// reported as a release with nothing new
#[test]
fn closure_difference_is_what_diff_closures_prints() {
    let scratch = Scratch::new("release-difference");
    let program = scratch.path().join("nix");
    let log = scratch.path().join("native-args");
    let script = format!(
        "#!/bin/sh\n[ \"$1\" = --version ] && exit 0\nprintf '%s\\n' \"$*\" > '{}'\nprintf 'package: 1 -> 2\\n'\n",
        log.display()
    );
    install_script(&program, &script);
    let (old, new) = (scratch.path().join("old"), scratch.path().join("new"));
    assert_eq!(
        closure_difference(&Tool::at("nix", &program), &old, &new).unwrap(),
        ["package: 1 -> 2"]
    );
    assert_eq!(
        fs::read_to_string(log).unwrap().trim(),
        format!(
            "--extra-experimental-features nix-command store diff-closures {} {}",
            old.display(),
            new.display()
        )
    );
    install_script(
        &program,
        "#!/bin/sh\n[ \"$1\" = --version ] && exit 0\nexit 1\n",
    );
    assert!(closure_difference(&Tool::at("nix", &program), &old, &new).is_err());
    let absent = Tool::at("nix", scratch.path().join("absent"));
    assert!(closure_difference(&absent, &old, &new).is_err());
}
