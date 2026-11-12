//! Unit tests for profile.

use std::fs;
use std::path::PathBuf;

use super::*;
use crate::fsx::testing::{Scratch, install_script, tree};

/// A fake dsh in `dir` that appends its working directory, `DSH_HOME` and
/// argv to `<dir>/fake-dsh.log`, and writes `manifest` as the profile's
/// `package.json` when it is not empty, as dsh's materialising of a
/// shipped profile does. `--version` only exits 0.
fn fake_dsh(dir: &Path, manifest: &str) -> (Tool, PathBuf) {
    let log = dir.join("fake-dsh.log");
    let script = dir.join("fake-dsh");
    let text = format!(
        r#"#!/bin/sh
[ "$1" = --version ] && exit 0
printf '%s|%s|%s\n' "$(pwd -P)" "$DSH_HOME" "$*" >> '{log}'
[ -n '{manifest}' ] || exit 0
mkdir -p "$DSH_HOME/profiles/$2"
printf '%s\n' '{manifest}' > "$DSH_HOME/profiles/$2/package.json"
"#,
        log = log.display()
    );
    install_script(&script, &text);
    (Tool::at("dsh", &script), log)
}

/// `dsh`'s command as a caller sets it up: `home` as `DSH_HOME`, `cwd` as
/// the working directory.
fn command(dsh: &Tool, home: &Path, cwd: &Path) -> Command {
    let mut command = dsh.command();
    command.env("DSH_HOME", home).current_dir(cwd);
    command
}

// catches: dsh run over an existing profile, or dshd writing in the DSH home while it reads a profile
#[test]
fn an_existing_profile_is_read_as_it_is() {
    let scratch = Scratch::new("profile-live");
    let home = scratch.path().join("dsh");
    let profile = home.join("profiles/web");
    fs::create_dir_all(profile.join("plugins")).unwrap();
    fs::write(
        profile.join(MANIFEST),
        r#"{"dsh":{"profile":{"bundles":["@deepseek-ai/dsh-web-app"]}}}"#,
    )
    .unwrap();
    fs::write(profile.join("cordis.patch.yml"), "- id: x\n").unwrap();
    std::os::unix::fs::symlink("cordis.patch.yml", profile.join("plugins/patch")).unwrap();
    let before = tree(&home);
    let (dsh, log) = fake_dsh(scratch.path(), "{}");
    let found = transport(&dsh, command(&dsh, &home, scratch.path()), &home, "web");
    assert_eq!(found.unwrap().transport, Transport::Web);
    assert!(!log.exists(), "dsh never runs for an existing profile");
    assert_eq!(tree(&home), before, "the DSH home is left as it was");
}

// catches: a missing profile read without dsh materialising it, or dsh run with other arguments, in another home or directory
#[test]
fn a_missing_profile_is_materialised_by_dsh() {
    let scratch = Scratch::new("profile-materialise");
    let home = scratch.path().join("dsh");
    let cwd = scratch.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    let manifest = r#"{"dsh":{"profile":{"bundles":["@deepseek-ai/dsh-acp-app"]}}}"#;
    let (dsh, log) = fake_dsh(scratch.path(), manifest);
    let found = transport(&dsh, command(&dsh, &home, &cwd), &home, "acp");
    assert_eq!(found.unwrap().transport, Transport::Acp);
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        format!(
            "{}|{}|--profile acp --dump-default-config\n",
            fs::canonicalize(&cwd).unwrap().display(),
            home.display()
        )
    );
    let written: Vec<PathBuf> = tree(&home).into_keys().collect();
    assert_eq!(
        written,
        [
            home.clone(),
            home.join("profiles"),
            home.join("profiles/acp"),
            home.join("profiles/acp").join(MANIFEST)
        ],
        "the home holds what dsh wrote and nothing else"
    );
    let again = transport(&dsh, command(&dsh, &home, &cwd), &home, "acp");
    assert_eq!(again.unwrap().transport, Transport::Acp);
    assert_eq!(
        fs::read_to_string(&log).unwrap().lines().count(),
        1,
        "dsh runs only while the manifest is missing"
    );
}

// catches: a dsh refusal swallowed, reported without dsh's own words, or a profile dsh did not create held anyway
#[test]
fn a_profile_dsh_cannot_materialise_is_refused() {
    let scratch = Scratch::new("profile-refused");
    let home = scratch.path().join("dsh");
    let refusing = scratch.path().join("refusing-dsh");
    install_script(
        &refusing,
        "#!/bin/sh\n[ \"$1\" = --version ] && exit 0\necho \"profile $2 does not exist\" >&2\nexit 1\n",
    );
    let dsh = Tool::at("dsh", &refusing);
    match transport(&dsh, command(&dsh, &home, scratch.path()), &home, "mine") {
        Err(Error::Tool(failure)) => {
            assert_eq!(failure.status, Some(1));
            assert_eq!(failure.stderr, "profile mine does not exist");
            assert_eq!(failure.args, ["--profile", "mine", "--dump-default-config"]);
        }
        other => panic!("{other:?}"),
    }
    let (silent, _) = fake_dsh(scratch.path(), "");
    let unmade = transport(
        &silent,
        command(&silent, &home, scratch.path()),
        &home,
        "mine",
    );
    assert!(
        matches!(unmade, Err(Error::UnsupportedProfile { ref profile, app: UnheldApp::Other }) if profile == "mine"),
        "{unmade:?}"
    );
}

// catches: a bundle matched by suffix or substring, a scope that changes the transport, or an SDK or terminal profile held in a unit
#[test]
fn detect_transport_by_bundle_name() {
    let scratch = Scratch::new("profile-transport");
    let manifest = scratch.path().join(MANIFEST);
    let with = |bundles: &str| {
        fs::write(
            &manifest,
            format!(r#"{{"dsh":{{"profile":{{"bundles":{bundles}}}}}}}"#),
        )
        .unwrap();
        detect_transport(&manifest, "p")
    };
    let refused = |result: Result<Detected, Error>| match result {
        Err(Error::UnsupportedProfile { profile, app }) if profile == "p" => app,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        refused(detect_transport(&manifest, "p")),
        UnheldApp::Other,
        "no manifest"
    );
    assert_eq!(
        with(r#"["@deepseek-ai/dsh-base","@deepseek-ai/dsh-headless"]"#)
            .unwrap()
            .transport,
        Transport::None
    );
    assert_eq!(
        with(r#"["@deepseek-ai/dsh-base","@deepseek-ai/dsh-web-app"]"#)
            .unwrap()
            .transport,
        Transport::Web
    );
    assert_eq!(
        with(r#"["@deepseek-ai/dsh-base","@deepseek-ai/dsh-acp-app"]"#)
            .unwrap()
            .transport,
        Transport::Acp
    );
    assert_eq!(
        with(r#"["dsh-web-app"]"#).unwrap().transport,
        Transport::Web,
        "unscoped"
    );
    assert_eq!(
        with(r#"["dsh-acp-app","dsh-web-app"]"#).unwrap().transport,
        Transport::Web,
        "web before acp"
    );
    assert_eq!(
        with(r#"["dsh-web-app","dsh-headless"]"#).unwrap().transport,
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
    fs::write(&manifest, r#"{"name":"headless"}"#).unwrap();
    assert_eq!(
        refused(detect_transport(&manifest, "p")),
        UnheldApp::Other,
        "manifest without bundles"
    );
}

// catches: a profile that lists several apps reported as one, so `run` cannot say that DSH mounts them together
#[test]
fn every_listed_app_is_reported() {
    let scratch = Scratch::new("profile-apps");
    let manifest = scratch.path().join(MANIFEST);
    let apps = |bundles: &str| {
        fs::write(
            &manifest,
            format!(r#"{{"dsh":{{"profile":{{"bundles":{bundles}}}}}}}"#),
        )
        .unwrap();
        detect_transport(&manifest, "p").unwrap()
    };
    assert_eq!(
        apps(r#"["@deepseek-ai/dsh-base","@deepseek-ai/dsh-web-app","my-config"]"#),
        Detected {
            transport: Transport::Web,
            apps: vec!["dsh-web-app"],
        },
        "one app among other bundles"
    );
    assert_eq!(
        apps(r#"["@deepseek-ai/dsh-base","@deepseek-ai/dsh-web-app","@deepseek-ai/dsh-headless"]"#),
        Detected {
            transport: Transport::None,
            apps: vec!["dsh-headless", "dsh-web-app"],
        },
        "in the order that decides the transport, not the manifest's"
    );
    assert_eq!(
        apps(r#"["dsh-acp-app","dsh-sdk-app","dsh-web-app"]"#).apps,
        vec!["dsh-web-app", "dsh-acp-app", "dsh-sdk-app"],
        "an SDK bundle beside a held app is listed too"
    );
}
