//! Recursive unknown-key discovery for the recipe an acting reader must
//! understand completely. Reporting readers retain these maps unchanged.

use serde_json::{Map, Value};

use super::InstanceRecord;

impl InstanceRecord {
    /// Unknown keys at every acted-on record layer, as dotted paths.
    /// Array entries include their index.
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
        for (index, patch) in self.patches.iter().enumerate() {
            append(&mut keys, &format!("patches[{index}]"), &patch.unrecognized);
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

    // catches: top-level or nested fields silently dropped during tolerant decode, so an older acting reader cannot refuse them
    #[test]
    fn nested_unknown_keys_survive_and_have_paths() {
        let mut value: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/instance.json")).unwrap();
        value
            .get_mut("profile")
            .and_then(Value::as_object_mut)
            .unwrap()
            .insert("revision".into(), json!(2));
        value
            .get_mut("environment")
            .and_then(Value::as_array_mut)
            .unwrap()
            .first_mut()
            .and_then(Value::as_object_mut)
            .unwrap()
            .insert("unset".into(), json!(true));
        value
            .as_object_mut()
            .unwrap()
            .insert("sandbox".into(), json!({"network": false}));
        let record: InstanceRecord = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            record.unrecognized_keys(),
            ["sandbox", "profile.revision", "environment[0].unset"]
        );
        assert_eq!(serde_json::to_value(record).unwrap(), value);
    }
}
