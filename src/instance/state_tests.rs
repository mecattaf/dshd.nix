//! Regression cases for name selection and recipe changes.

use super::*;

// catches: generation ignores a loaded unit whose state directory is gone, so its name is reused under it
#[test]
fn production_name_selection_unites_disk_and_loaded_names() {
    let scratch = Scratch::new("instance-name-union");
    let root = StateRoot::new(scratch.path().join("state"));
    fs::create_dir_all(root.instance(&InstanceName::parse("web-1").unwrap()).path()).unwrap();
    let loaded = vec![InstanceName::parse("web-2").unwrap()];
    assert_eq!(
        choose_name(&root, &loaded, "web").unwrap().as_str(),
        "web-3"
    );
}

// catches: nested newer fields are preserved for display but refused before the recipe is acted on
#[test]
fn strict_load_refuses_nested_unknown_keys() {
    let scratch = Scratch::new("instance-nested-record");
    let root = StateRoot::new(scratch.path().join("state"));
    let name = InstanceName::parse("web-1").unwrap();
    fs::create_dir_all(root.instance(&name).path()).unwrap();
    for (pointer, expected) in [
        ("/profile", "profile.source"),
        ("/environment/0", "environment[0].source"),
    ] {
        let mut value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        value
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("source".to_owned(), serde_json::json!("new"));
        fs::write(
            root.instance(&name).instance_json(),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert_eq!(
            load(&root, &name).unwrap().record.unrecognized_keys(),
            vec![expected]
        );
        assert!(
            matches!(load_strict(&root, &name), Err(Error::InconsistentState { reason, .. }) if reason.contains(expected))
        );
    }
}

// catches: update leaves newly excluded secret values in the durable recipe or reports their values
#[test]
fn update_strips_newly_unset_environment_atomically() {
    let scratch = Scratch::new("instance-strip");
    let root = StateRoot::new(scratch.path().join("state"));
    let (nix_store, _) = fake_nix_store(scratch.path());
    let mut requested = request(scratch.path(), "web-s");
    requested.environment.push(EnvVar {
        name: "API_TOKEN".into(),
        value: "sensitive-value".into(),
        unrecognized: serde_json::Map::new(),
    });
    let instance = create(&root, &nix_store, requested).unwrap();
    let removed = strip_environment(&instance, &["API_TOKEN".to_owned()]).unwrap();
    assert_eq!(removed, vec!["API_TOKEN"]);
    let updated = load_strict(&root, &instance.name).unwrap();
    assert!(
        updated
            .record
            .environment
            .iter()
            .all(|variable| variable.name.as_os_str() != "API_TOKEN")
    );
    assert!(
        !fs::read_to_string(updated.dir.instance_json())
            .unwrap()
            .contains("sensitive-value")
    );
    assert_eq!(
        fs::metadata(updated.dir.instance_json())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        strip_environment(&updated, &["API_TOKEN".to_owned()])
            .unwrap()
            .is_empty()
    );
}
