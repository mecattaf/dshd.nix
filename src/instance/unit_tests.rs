//! Unit tests for instance.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::fsx::testing::{Scratch, fake_nix_store, install_script, tree};
use crate::record::ReleaseFile;

const FIXTURE: &str = include_str!("../../tests/fixtures/instance.json");

fn release_at(path: PathBuf) -> Release {
    let file = ReleaseFile {
        environment: BTreeMap::new(),
        unset_environment: Vec::new(),
        unrecognized: serde_json::Map::new(),
    };
    Release {
        name: "default".to_owned(),
        path,
        file,
    }
}

/// A request for a `web` instance named `name` whose DSH home, profile,
/// cwd and release live under `scratch`.
fn request(scratch: &Path, name: &str) -> CreateRequest {
    let home = scratch.join("dsh");
    let profile = home.join("profiles/web");
    fs::create_dir_all(&profile).unwrap();
    fs::write(
        profile.join("package.json"),
        r#"{"dsh":{"profile":{"bundles":["dsh-web-app"]}}}"#,
    )
    .unwrap();
    let cwd = scratch.join("work");
    fs::create_dir_all(&cwd).unwrap();
    CreateRequest {
        name: InstanceName::parse(name).unwrap(),
        release: release_at(scratch.join("release")),
        release_given: true,
        cwd,
        args: vec!["web".into(), "--port".into(), "4100".into()],
        environment: vec![EnvVar {
            name: "TERM".into(),
            value: "xterm".into(),
            unrecognized: serde_json::Map::new(),
        }],
        profile: ProfileRef {
            name: "web".to_owned(),
            dsh_home: OsText::new(home),
            unrecognized: serde_json::Map::new(),
        },
        transport: Transport::Web,
        labels: BTreeMap::new(),
    }
}

// catches: a second run of the same recipe making a new instance, a different recipe overwriting one, argv
// not recorded as typed, or anything written in the DSH home or kept in the instance besides the record and roots
#[test]
fn create_twice_same_recipe_keeps_id() {
    let scratch = Scratch::new("instance-create");
    let root = StateRoot::new(scratch.path().join("state"));
    let (nix_store, log) = fake_nix_store(scratch.path());
    let request = request(scratch.path(), "web-a");
    let home = scratch.path().join("dsh");
    let before = tree(&home);

    let first = create(&root, &nix_store, request.clone()).unwrap();
    assert_eq!(first.dir.path(), root.instances_dir().join("web-a"));
    assert_eq!(
        fs::metadata(first.dir.instance_json())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::read_link(first.dir.current()).unwrap(),
        request.release.path
    );
    assert_eq!(
        fs::read_link(first.dir.booted()).unwrap(),
        request.release.path
    );
    let entries: Vec<_> = fsx::list_dir(first.dir.path())
        .unwrap()
        .into_iter()
        .map(|(entry, _)| entry)
        .collect();
    assert_eq!(entries, ["booted", "current", "instance.json"]);
    let args: Vec<&OsStr> = first.record.args.iter().map(OsText::as_os_str).collect();
    assert_eq!(args, ["web", "--port", "4100"]);
    assert!(!root.instances_dir().join(".tmp-web-a").exists());
    let logged = fs::read_to_string(&log).unwrap();
    for link in [first.dir.current(), first.dir.booted()] {
        assert!(
            logged.contains(&format!("--add-root {}\n", link.display())),
            "{} is rooted at its final path",
            link.display()
        );
    }
    assert_eq!(load_strict(&root, &first.name).unwrap(), first);

    let again = create(&root, &nix_store, request.clone()).unwrap();
    assert_eq!(again.record.id, first.record.id);

    let mut moved = request.clone();
    moved.cwd = scratch.path().to_path_buf();
    match create(&root, &nix_store, moved) {
        Err(Error::ExistsDifferentRecipe(difference)) => {
            let expected = (
                "cwd",
                request.cwd.display().to_string(),
                scratch.path().display().to_string(),
            );
            assert_eq!(difference.fields, vec![expected]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        load(&root, &first.name).unwrap(),
        first,
        "a refused request changes nothing"
    );

    remove(&first.dir).unwrap();
    assert!(!first.dir.path().exists());
    let recreated = create(&root, &nix_store, request).unwrap();
    assert_ne!(
        recreated.record.id, first.record.id,
        "a re-created name gets a new id"
    );
    assert_eq!(tree(&home), before, "the DSH home is never written");
}

// catches: a generated name that is taken on disk, or a failed creation leaving half an instance
#[test]
fn create_generates_past_taken_names_and_rolls_back() {
    let scratch = Scratch::new("instance-generate");
    let root = StateRoot::new(scratch.path().join("state"));
    let (nix_store, _) = fake_nix_store(scratch.path());
    let mut request = request(scratch.path(), "web-1");
    create(&root, &nix_store, request.clone()).unwrap();
    request.name = choose_name(&root, &[], "web").unwrap();
    let created = create(&root, &nix_store, request.clone()).unwrap();
    assert_eq!(created.name.as_str(), "web-2");

    let failing = scratch.path().join("failing-nix-store");
    install_script(
        &failing,
        "#!/bin/sh\n[ \"$1\" = --version ] && exit 0\nexit 1\n",
    );
    request.name = InstanceName::parse("web-3").unwrap();
    let refused = create(&root, &Tool::at("nix-store", &failing), request);
    assert!(matches!(refused, Err(Error::Tool(_))), "{refused:?}");
    assert_eq!(
        discover(&root).unwrap().names,
        vec![InstanceName::parse("web-1").unwrap(), created.name],
        "no half instance is left"
    );
    assert!(!root.instances_dir().join(".tmp-web-3").exists());
}

// catches: an update that moves the booted release of a running instance
#[test]
fn update_current_repoints_current_only() {
    let scratch = Scratch::new("instance-update");
    let root = StateRoot::new(scratch.path().join("state"));
    let (nix_store, _) = fake_nix_store(scratch.path());
    let request = request(scratch.path(), "web-b");
    let instance = create(&root, &nix_store, request.clone()).unwrap();
    let newer = release_at(scratch.path().join("release-2"));
    assert!(update_current(&instance, &nix_store, &newer).unwrap());
    assert!(!update_current(&instance, &nix_store, &newer).unwrap());
    assert_eq!(fs::read_link(instance.dir.current()).unwrap(), newer.path);
    assert_eq!(
        fs::read_link(instance.dir.booted()).unwrap(),
        request.release.path,
        "booted moves at the next start"
    );
}

// catches: a second run refused for a changed terminal or label, or a changed directory or transport accepted
#[test]
fn compare_recipe_reports_fields() {
    let stored: InstanceRecord = serde_json::from_str(FIXTURE).unwrap();
    let mut request = CreateRequest {
        name: InstanceName::parse(&stored.name).unwrap(),
        release: release_at(PathBuf::from("/nix/store/r")),
        release_given: false,
        cwd: PathBuf::from(stored.cwd.as_os_str()),
        args: stored
            .args
            .iter()
            .map(|arg| arg.as_os_str().to_owned())
            .collect(),
        environment: stored.environment.clone(),
        profile: stored.profile.clone(),
        transport: stored.transport,
        labels: stored.labels.clone(),
    };
    assert_eq!(compare_recipe(&stored, &request), None);
    request.environment.push(EnvVar {
        name: "KITTY_WINDOW_ID".into(),
        value: "7".into(),
        unrecognized: serde_json::Map::new(),
    });
    request.environment.push(EnvVar {
        name: "API_TOKEN".into(),
        value: "secret-value".into(),
        unrecognized: serde_json::Map::new(),
    });
    request.labels.clear();
    request.release.name = "candidate".to_owned();
    assert_eq!(
        compare_recipe(&stored, &request),
        None,
        "environment, labels and an implied release are no recipe"
    );

    request.release_given = true;
    request.args.push("--verbose".into());
    request.transport = Transport::Acp;
    let difference = compare_recipe(&stored, &request).unwrap();
    assert_eq!(difference.name, "web-1");
    let fields: Vec<&str> = difference
        .fields
        .iter()
        .map(|(field, _, _)| *field)
        .collect();
    assert_eq!(fields, vec!["args", "transport", "release"]);
    assert!(
        !format!("{difference:?}").contains("secret-value"),
        "environment values are never shown"
    );
}

// catches: labels written when nothing changed, an empty value stored instead of removing the key, or a loose mode
#[test]
fn set_labels_merges_and_removes() {
    let scratch = Scratch::new("instance-labels");
    let root = StateRoot::new(scratch.path().join("state"));
    let (nix_store, _) = fake_nix_store(scratch.path());
    let mut request = request(scratch.path(), "web-l");
    request.labels = BTreeMap::from([("agency".to_owned(), "a".to_owned())]);
    let instance = create(&root, &nix_store, request).unwrap();
    let set = |labels: &[(&str, &str)]| {
        let labels: BTreeMap<String, String> = labels
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let current = load_strict(&root, &instance.name).unwrap();
        set_labels(&current, &labels).unwrap()
    };
    assert!(!set(&[("agency", "a")]), "the same value changes nothing");
    assert!(set(&[("owner", "ci"), ("agency", "")]));
    let labels = load_strict(&root, &instance.name).unwrap().record.labels;
    assert_eq!(
        labels,
        BTreeMap::from([("owner".to_owned(), "ci".to_owned())])
    );
    assert!(
        !set(&[("gone", "")]),
        "removing an absent key changes nothing"
    );
    assert_eq!(
        fs::metadata(instance.dir.instance_json())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

// catches: a generated name that is taken, outside the grammar, or longer than 63 bytes
#[test]
fn generate_name_skips_taken() {
    let names: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/fixtures/instance-names.json")).unwrap();
    for (profile, expected) in names
        .get("generated")
        .and_then(serde_json::Value::as_object)
        .unwrap()
    {
        assert_eq!(
            Some(generate_name(profile, &[]).unwrap().as_str()),
            expected.as_str()
        );
    }
    let taken = |names: &[&str]| {
        names
            .iter()
            .map(|name| InstanceName::parse(name).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        generate_name("web", &taken(&["web-1", "web-3"]))
            .unwrap()
            .as_str(),
        "web-2"
    );
    assert_eq!(
        generate_name("web", &taken(&["web-1", "web-2", "acp-3"]))
            .unwrap()
            .as_str(),
        "web-3"
    );
    assert_eq!(
        generate_name("My.Profile", &[]).unwrap().as_str(),
        "my-profile-1"
    );
    assert_eq!(generate_name("--@@", &[]).unwrap().as_str(), "dsh-1");
    let longest = format!("{}-1", "x".repeat(61));
    let long = generate_name(&"x".repeat(80), &taken(&[longest.as_str()])).unwrap();
    assert_eq!(long.as_str(), format!("{}-2", "x".repeat(61)));
}

// catches: a record from a newer dshd acted on, or an invalid directory name aborting the listing
#[test]
fn load_strict_refuses_unknown_keys() {
    let scratch = Scratch::new("instance-load");
    let root = StateRoot::new(scratch.path().to_path_buf());
    let (web1, web2) = (
        InstanceName::parse("web-1").unwrap(),
        InstanceName::parse("web-2").unwrap(),
    );
    for name in [&web1, &web2] {
        fs::create_dir_all(root.instance(name).path()).unwrap();
    }
    fs::write(
        root.instance(&web1).instance_json(),
        FIXTURE.replacen('{', "{\"colour\": 1,", 1),
    )
    .unwrap();
    fs::write(root.instance(&web2).instance_json(), FIXTURE).unwrap();
    fs::create_dir_all(root.instances_dir().join(".tmp-web-3")).unwrap();
    fs::create_dir_all(root.instances_dir().join("Not_A_Name")).unwrap();
    drop(lock_root(&root).unwrap());
    drop(lock(&root.instance(&web1)).unwrap());

    let found = discover(&root).unwrap();
    assert_eq!(found.names, vec![web1.clone(), web2.clone()]);
    assert_eq!(found.invalid.len(), 1, "{:?}", found.invalid);
    assert!(
        found.invalid.iter().all(|line| line.contains("Not_A_Name")),
        "{:?}",
        found.invalid
    );
    assert!(matches!(
        lock_loaded(&root, &InstanceName::parse("web-9").unwrap()),
        Err(Error::NotFound(_))
    ));
    assert_eq!(
        lock_loaded(&root, &web2).map(|_| ()).unwrap_err().kind(),
        "inconsistent-state"
    );
    assert_eq!(load(&root, &web1).unwrap().record.unrecognized.len(), 1);
    assert!(
        matches!(load_strict(&root, &web1), Err(Error::InconsistentState { reason, .. }) if reason.contains("colour"))
    );
    assert!(
        matches!(
            load_strict(&root, &web2),
            Err(Error::InconsistentState { .. })
        ),
        "the record names web-1"
    );
    assert!(matches!(
        load(&root, &InstanceName::parse("web-9").unwrap()),
        Err(Error::NotFound(_))
    ));
    fs::write(root.instance(&web2).instance_json(), "{").unwrap();
    assert!(matches!(
        load(&root, &web2),
        Err(Error::InconsistentState { .. })
    ));
}

// catches: a web port or last run misread, or an instance id that is not a fresh UUID
#[test]
fn web_port_last_run_and_id() {
    let scratch = Scratch::new("instance-port");
    let dir = InstanceDir::new(scratch.path().to_path_buf());
    assert_eq!(read_web_port(&dir).unwrap(), None);
    write_web_port(&dir, 41234).unwrap();
    assert_eq!(read_web_port(&dir).unwrap(), Some(41234));
    assert_eq!(
        fs::metadata(dir.web_port()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::write(dir.web_port(), "port\n").unwrap();
    assert!(read_web_port(&dir).is_err());
    assert_eq!(read_last_run(&dir).unwrap(), None);
    let recorded = include_str!("../../tests/fixtures/last-run.json");
    fs::write(dir.last_run_json(), recorded).unwrap();
    assert_eq!(
        read_last_run(&dir).unwrap(),
        Some(serde_json::from_str::<LastRun>(recorded).unwrap())
    );
    let (one, two) = (new_id().unwrap(), new_id().unwrap());
    assert!(name::id_shaped(&one), "{one}");
    assert_ne!(one, two);
}

#[path = "state_tests.rs"]
mod state;
