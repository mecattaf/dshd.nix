//! Unit tests for profile.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use super::*;
use crate::fsx::testing::{Scratch, any_store_path, fake_nix_store, install_script};

/// A source profile with a manifest, a patch, the shared channels and
/// one link of every kind; `store` is an existing store path.
fn source_profile(root: &Path, store: &Path) -> PathBuf {
    let source = root.join("dsh/profiles/web");
    fs::create_dir_all(source.join(".git")).unwrap();
    fs::create_dir_all(source.join("plugins/local")).unwrap();
    fs::create_dir_all(source.join("node_modules/dep")).unwrap();
    fs::write(
        source.join(MANIFEST),
        r#"{"name":"web","dsh":{"profile":{"bundles":["@dsh/dsh-web-app"]}}}"#,
    )
    .unwrap();
    fs::write(source.join("cordis.yml"), "[]\n").unwrap();
    fs::write(source.join("cordis.patch.yml"), "- id: x\n").unwrap();
    fs::write(source.join(".git/HEAD"), "ref: main\n").unwrap();
    fs::write(source.join("plugins/local/cordis.yml"), "nested\n").unwrap();
    fs::write(source.join("node_modules/dep/index.js"), "1\n").unwrap();
    fs::write(root.join("outside.txt"), "outside\n").unwrap();
    install_script(&source.join("plugins/local/run"), "#!/bin/sh\nexit 0\n");
    fs::set_permissions(
        source.join("cordis.patch.yml"),
        fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    symlink("cordis.patch.yml", source.join("inside")).unwrap();
    symlink("../../../outside.txt", source.join("outside")).unwrap();
    symlink(store, source.join("store")).unwrap();
    symlink(store, source.join("plugins/store-again")).unwrap();
    source
}

// catches: a shared channel or .git copied into the instance, a store link copied without a GC root, or lost mode bits
#[test]
fn copy_skips_shared_channels_and_roots_store_links() {
    let scratch = Scratch::new("profile-copy");
    let (nix_store, log) = fake_nix_store(scratch.path());
    let store = any_store_path();
    let source = source_profile(scratch.path(), &store);
    let (dest, gcroots) = (scratch.path().join("copy"), scratch.path().join("gcroots"));
    copy_profile(&source, &dest, &gcroots, &nix_store).unwrap();

    assert!(!dest.join("cordis.yml").exists() && !dest.join(".git").exists());
    assert_eq!(
        fs::read_to_string(dest.join("plugins/local/cordis.yml")).unwrap(),
        "nested\n"
    );
    assert_eq!(
        fs::read_to_string(dest.join("node_modules/dep/index.js")).unwrap(),
        "1\n"
    );
    let patch_mode = fs::metadata(dest.join("cordis.patch.yml"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(
        patch_mode & 0o200,
        0o200,
        "a read-only source file becomes owner-writable"
    );
    let run_mode = fs::metadata(dest.join("plugins/local/run"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(run_mode & 0o100, 0o100, "execute bits are kept");
    assert_eq!(
        fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
        0o700
    );

    assert_eq!(
        fs::read_link(dest.join("inside")).unwrap(),
        PathBuf::from("cordis.patch.yml")
    );
    assert_eq!(
        fs::read_link(dest.join("outside")).unwrap(),
        scratch.path().join("outside.txt")
    );
    assert_eq!(fs::read_link(dest.join("store")).unwrap(), store);

    let store_root = release::store_path_of(&fs::canonicalize(&store).unwrap()).unwrap();
    assert_eq!(fs::read_link(gcroots.join("1")).unwrap(), store_root);
    assert!(!gcroots.join("2").exists(), "one root per store path");
    let logged = fs::read_to_string(log).unwrap();
    let expected = format!(
        "--option substitute false --realise {} --add-root {}",
        store_root.display(),
        gcroots.join("1").display()
    );
    assert_eq!(logged.lines().collect::<Vec<_>>(), vec![expected.as_str()]);
}

// catches: a bundle matched by suffix or substring, a scope that changes the transport, or an SDK or terminal profile held in a unit
#[test]
fn detect_transport_by_bundle_name() {
    let scratch = Scratch::new("profile-transport");
    let dir = scratch.path();
    let with = |bundles: &str| {
        fs::write(
            dir.join(MANIFEST),
            format!(r#"{{"dsh":{{"profile":{{"bundles":{bundles}}}}}}}"#),
        )
        .unwrap();
        detect_transport(dir, "p")
    };
    let refused = |result: Result<Transport, Error>| match result {
        Err(Error::UnsupportedProfile { profile, app }) if profile == "p" => app,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        refused(detect_transport(dir, "p")),
        UnheldApp::Other,
        "no manifest"
    );
    assert_eq!(
        with(r#"["@deepseek-ai/dsh-base","@deepseek-ai/dsh-headless"]"#).unwrap(),
        Transport::None
    );
    assert_eq!(
        with(r#"["@deepseek-ai/dsh-base","@deepseek-ai/dsh-web-app"]"#).unwrap(),
        Transport::Web
    );
    assert_eq!(
        with(r#"["@deepseek-ai/dsh-base","@deepseek-ai/dsh-acp-app"]"#).unwrap(),
        Transport::Acp
    );
    assert_eq!(
        with(r#"["dsh-web-app"]"#).unwrap(),
        Transport::Web,
        "unscoped"
    );
    assert_eq!(
        with(r#"["dsh-acp-app","dsh-web-app"]"#).unwrap(),
        Transport::Web,
        "web before acp"
    );
    assert_eq!(
        with(r#"["dsh-web-app","dsh-headless"]"#).unwrap(),
        Transport::None,
        "headless first"
    );
    assert_eq!(
        refused(with(r#"["@deepseek-ai/dsh-sdk-app"]"#)),
        UnheldApp::Sdk
    );
    assert_eq!(
        refused(with(r#"["@deepseek-ai/dsh-sdk-minimal"]"#)),
        UnheldApp::Sdk
    );
    assert_eq!(
        refused(with(r#"["@deepseek-ai/dsh-tui"]"#)),
        UnheldApp::Other
    );
    assert_eq!(
        refused(with(r#"["my-dsh-web-app","dsh-web-app-extra"]"#)),
        UnheldApp::Other,
        "exact names only"
    );
    assert_eq!(refused(with("[]")), UnheldApp::Other, "no bundles");
    fs::write(dir.join(MANIFEST), r#"{"name":"headless"}"#).unwrap();
    assert_eq!(
        refused(detect_transport(dir, "p")),
        UnheldApp::Other,
        "manifest without bundles"
    );
}

// catches: a profile link to another instance's copy replaced or removed, or a real profile directory deleted
#[test]
fn expose_verify_unlink_exact() {
    let scratch = Scratch::new("profile-expose");
    let home = scratch.path().join("dsh");
    fs::create_dir_all(profiles_dir(&home)).unwrap();
    let (copy, other) = (
        scratch.path().join("state/a/profile"),
        scratch.path().join("state/b/profile"),
    );
    assert!(!verify_link(&home, "web@a", &copy).unwrap());
    expose(&home, "web@a", &copy).unwrap();
    expose(&home, "web@a", &copy).unwrap();
    assert!(verify_link(&home, "web@a", &copy).unwrap());
    assert!(matches!(
        expose(&home, "web@a", &other),
        Err(Error::ProfileLinkExists(_))
    ));
    assert!(matches!(
        verify_link(&home, "web@a", &other),
        Err(Error::ProfileLinkExists(_))
    ));
    unlink_exact(&home, "web@a", &other).unwrap();
    assert!(
        verify_link(&home, "web@a", &copy).unwrap(),
        "a link to another copy is left alone"
    );
    unlink_exact(&home, "web@a", &copy).unwrap();
    assert!(!verify_link(&home, "web@a", &copy).unwrap());
    fs::create_dir(profiles_dir(&home).join("web@b")).unwrap();
    assert!(matches!(
        expose(&home, "web@b", &other),
        Err(Error::ProfileLinkExists(_))
    ));
    unlink_exact(&home, "web@b", &other).unwrap();
    assert!(
        profiles_dir(&home).join("web@b").is_dir(),
        "a real directory is never removed"
    );
}

// catches: an edited patch or copy reported as unedited, or dsh's own channel writes reported as an edit
#[test]
fn composition_stamp_detects_edit() {
    let scratch = Scratch::new("profile-stamp");
    let copy = scratch.path().join("profile");
    fs::create_dir_all(copy.join("node_modules/dep")).unwrap();
    fs::write(copy.join(MANIFEST), "{}\n").unwrap();
    fs::write(copy.join("cordis.patch.yml"), "- a\n").unwrap();
    let patch = scratch.path().join("extra.patch.yml");
    fs::write(&patch, "- p\n").unwrap();
    let recorded = composition_stamp(&copy, std::slice::from_ref(&patch)).unwrap();
    assert_eq!(
        recorded.files.keys().collect::<Vec<_>>(),
        vec!["cordis.patch.yml", MANIFEST]
    );
    assert_eq!(recorded.patches.len(), 1);
    assert!(!edited_since(&copy, &recorded).unwrap());

    fs::write(copy.join("cordis.yml"), "[rewritten by dsh]\n").unwrap();
    fs::write(copy.join("node_modules/dep/index.js"), "installed\n").unwrap();
    assert!(
        !edited_since(&copy, &recorded).unwrap(),
        "shared channels and dependencies are not composition"
    );

    fs::write(&patch, "- p\n- q\n").unwrap();
    assert!(
        edited_since(&copy, &recorded).unwrap(),
        "a patch edit is an edit"
    );
    fs::write(&patch, "- p\n").unwrap();
    let recorded = composition_stamp(&copy, std::slice::from_ref(&patch)).unwrap();
    fs::write(copy.join("cordis.patch.yml"), "- a\n- b\n").unwrap();
    assert!(
        edited_since(&copy, &recorded).unwrap(),
        "a copy edit is an edit"
    );
    assert!(composition_stamp(&copy, &[scratch.path().join("absent.yml")]).is_err());
}

// catches: a profile name escaping the DSH home, or the initialiser run through a dangling profile link
#[test]
fn source_resolution_and_initialiser() {
    let scratch = Scratch::new("profile-source");
    let home = scratch.path().join("dsh");
    let dsh = scratch.path().join("dsh-bin");
    let script =
        "#!/bin/sh\n[ \"$1\" = --version ] && exit 0\nmkdir -p \"$DSH_HOME/profiles/$2\"\n";
    install_script(&dsh, script);
    let made = initialise_source(&Tool::at("dsh", &dsh), &home, "acp").unwrap();
    assert_eq!(made, fs::canonicalize(home.join("profiles/acp")).unwrap());
    symlink(scratch.path().join("gone"), home.join("profiles/dangling")).unwrap();
    let refused = initialise_source(&Tool::at("dsh", &dsh), &home, "dangling");
    assert!(
        matches!(refused, Err(Error::InvalidArgument(message)) if message.contains("does not exist"))
    );
    assert!(
        !scratch.path().join("gone").exists(),
        "the initialiser never runs through a dangling link"
    );
    assert!(matches!(
        resolve_source(&home, "../dsh"),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        resolve_source(&home, "missing"),
        Err(Error::InvalidArgument(_))
    ));
}

// catches: a populated profile directory is published without a successful directory sync
#[test]
fn copying_reports_a_directory_sync_failure() {
    let scratch = Scratch::new("profile-sync");
    let source = scratch.path().join("source");
    let dest = scratch.path().join("dest");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("package.json"), "{}").unwrap();
    fs::create_dir(&dest).unwrap();
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o300)).unwrap();
    let (nix_store, _) = fake_nix_store(scratch.path());
    let copied = copy_profile(&source, &dest, &scratch.path().join("roots"), &nix_store);
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(copied, Err(Error::Io { action: "sync", path, .. }) if path == dest));
    assert_eq!(fs::read_to_string(dest.join("package.json")).unwrap(), "{}");
}
