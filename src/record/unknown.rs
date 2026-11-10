//! Recursive unknown-key discovery for the recipe an acting reader must
//! understand completely. Reporting readers retain these maps unchanged.

use serde_json::{Map, Value};

use super::InstanceRecord;

impl InstanceRecord {
    /// Unknown keys at every acted-on record layer, as dotted paths.
    /// Array entries include their index; map entries retain their name.
    pub fn unrecognized_keys(&self) -> Vec<String> {
        let mut keys = Vec::new();
        append(&mut keys, "", &self.unrecognized);
        append(&mut keys, "profile", &self.profile.unrecognized);
        for (index, variable) in self.environment.iter().enumerate() {
            append(
                &mut keys,
                &format!("environment[{index}]"),
                &variable.unrecognized,
            );
        }
        append(&mut keys, "composition", &self.composition.unrecognized);
        for (name, stamp) in &self.composition.files {
            append(
                &mut keys,
                &format!("composition.files.{name}"),
                &stamp.unrecognized,
            );
        }
        for (index, patch) in self.composition.patches.iter().enumerate() {
            let path = format!("composition.patches[{index}]");
            append(&mut keys, &path, &patch.unrecognized);
            append(
                &mut keys,
                &format!("{path}.stamp"),
                &patch.stamp.unrecognized,
            );
        }
        keys
    }
}

fn append(keys: &mut Vec<String>, parent: &str, unknown: &Map<String, Value>) {
    keys.extend(unknown.keys().map(|key| {
        if parent.is_empty() {
            key.clone()
        } else {
            format!("{parent}.{key}")
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // catches: nested fields silently dropped during tolerant decode, so an older acting reader cannot refuse them
    #[test]
    fn nested_unknown_keys_survive_and_have_paths() {
        let mut value: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/instance.json")).unwrap();
        value
            .get_mut("profile")
            .and_then(Value::as_object_mut)
            .unwrap()
            .insert("sourceLink".into(), json!("/new/source"));
        value
            .get_mut("environment")
            .and_then(Value::as_array_mut)
            .unwrap()
            .first_mut()
            .and_then(Value::as_object_mut)
            .unwrap()
            .insert("unset".into(), json!(true));
        *value.get_mut("composition").unwrap() = json!({
            "files": {"config": {"size": 1, "mtimeSecs": 1, "mtimeNanos": 0, "digest": "new"}},
            "patches": [{"path": "/patch", "kind": "new", "stamp": {
                "size": 1, "mtimeSecs": 1, "mtimeNanos": 0, "digest": "new"
            }}],
            "source": "new"
        });
        let record: InstanceRecord = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            record.unrecognized_keys(),
            [
                "profile.sourceLink",
                "environment[0].unset",
                "composition.source",
                "composition.files.config.digest",
                "composition.patches[0].kind",
                "composition.patches[0].stamp.digest"
            ]
        );
        assert_eq!(serde_json::to_value(record).unwrap(), value);
    }
}
