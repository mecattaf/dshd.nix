//! No public status field promises provider cache residency.

use super::{CONTRACT, TestResult, Value, golden_lines, json, read_json};
use std::path::Path;

/// Product field names cannot promise facts about a provider's cache.
/// Labels remain arbitrary user data rather than product assertions.
fn cache_claims(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let lower = key.to_ascii_lowercase();
                if ["cache", "kv", "warm", "prefix", "resident"]
                    .iter()
                    .any(|word| lower.contains(word))
                {
                    found.push(key.clone());
                }
                if key != "labels" {
                    cache_claims(value, found);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                cache_claims(item, found);
            }
        }
        _ => {}
    }
}

// catches: a status, list or probe field claiming provider cache residency (P3.6)
#[test]
fn no_status_field_claims_cache_residency() -> TestResult {
    let doc = read_json(Path::new(CONTRACT))?;
    let mut found = Vec::new();
    cache_claims(&doc, &mut found);
    for (_, value) in golden_lines()? {
        cache_claims(&value, &mut found);
    }
    assert!(found.is_empty(), "provider cache claims: {found:?}");
    cache_claims(&json!({"resources": {"cacheResident": true}}), &mut found);
    assert_eq!(found, ["cacheResident"]);
    found.clear();
    cache_claims(&json!({"labels": {"warm": "user data"}}), &mut found);
    assert!(found.is_empty());
    Ok(())
}
