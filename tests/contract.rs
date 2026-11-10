// SPDX-License-Identifier: Apache-2.0
// Adapted from herdr-core crates/herdr-core/tests/compat.rs @ ce81486; changes: compares $defs of output and record shapes instead of OpenRPC methods, adds the closed-record rule and a value validator.

//! The machine contract only grows. `tests/contract/dshd.schema.json`
//! describes every JSON object dshd prints and every record it keeps;
//! `tests/contract/published.schema.json` is the contract as last
//! published. An output shape must keep giving every field the published
//! one gave, nothing it could not; a record shape must keep accepting every
//! record the published one accepted, and stays closed, since a reader
//! refuses keys it does not know. The JSON the golden cases print, and the
//! records they start from, must fit the current contract.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// The contract and its published copy.
const CONTRACT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/contract/dshd.schema.json"
);
const PUBLISHED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/contract/published.schema.json"
);
/// The golden cases and the record fixtures.
const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
/// The record a newer dshd wrote: its unknown key must not fit.
const REFUSED: &str = "base/root/state/dshd/broken/instance.json";
/// The top-level key that names the record shapes.
const RECORDS: &str = "x-records";
/// How deep `compare` follows nested shapes.
const DEPTH: u8 = 8;

static NULL: Value = Value::Null;

/// The member `key` of an object, or null.
fn at<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&NULL)
}

fn resolve<'a>(doc: &'a Value, schema: &'a Value) -> &'a Value {
    match at(schema, "$ref")
        .as_str()
        .and_then(|reference| reference.strip_prefix("#/$defs/"))
    {
        Some(name) => resolve(doc, at(at(doc, "$defs"), name)),
        None => schema,
    }
}

/// Its kinds of value (`string`, `null`, ...) and the object schemas among them.
fn shape<'a>(doc: &'a Value, schema: &'a Value) -> (BTreeSet<String>, Vec<&'a Value>) {
    let schema = resolve(doc, schema);
    let mut kinds = BTreeSet::new();
    let mut objects = Vec::new();
    match at(schema, "type") {
        Value::String(kind) => {
            kinds.insert(kind.clone());
        }
        Value::Array(listed) => kinds.extend(
            listed
                .iter()
                .filter_map(|kind| kind.as_str().map(String::from)),
        ),
        _ => {}
    }
    if schema.get("properties").is_some()
        || schema.get("items").is_some()
        || schema.get("enum").is_some()
    {
        objects.push(schema);
    }
    for member in at(schema, "anyOf")
        .as_array()
        .into_iter()
        .chain(at(schema, "oneOf").as_array())
        .flatten()
    {
        let (member_kinds, member_objects) = shape(doc, member);
        kinds.extend(member_kinds);
        objects.extend(member_objects);
    }
    (kinds, objects)
}

fn names(value: &Value) -> BTreeSet<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|name| name.as_str().map(String::from))
        .collect()
}

fn props(schema: &Value) -> Vec<(&String, &Value)> {
    at(schema, "properties")
        .as_object()
        .into_iter()
        .flatten()
        .collect()
}

fn defs(doc: &Value) -> BTreeMap<&String, &Value> {
    at(doc, "$defs").as_object().into_iter().flatten().collect()
}

/// `input` (a record): the new side must accept all the old did. Else (an
/// output) the new side must give all the old gave, nothing the old could not.
fn compare(
    old: (&Value, &Value),
    new: (&Value, &Value),
    input: bool,
    path: &str,
    depth: u8,
    out: &mut Vec<String>,
) {
    if depth > DEPTH {
        return;
    }
    let (old_kinds, old_objects) = shape(old.0, old.1);
    let (new_kinds, new_objects) = shape(new.0, new.1);
    let (wider, narrower) = if input {
        (&new_kinds, &old_kinds)
    } else {
        (&old_kinds, &new_kinds)
    };
    if !wider.is_empty() && !narrower.is_subset(wider) {
        out.push(format!("{path}: type {old_kinds:?} became {new_kinds:?}"));
    }
    // An output that promised a shape keeps promising it (untyped is wider).
    if !input
        && ((!old_kinds.is_empty() && new_kinds.is_empty())
            || (!old_objects.is_empty() && new_objects.is_empty()))
    {
        out.push(format!("{path}: lost its type"));
    }
    let (Some(old_object), Some(new_object)) = (old_objects.first(), new_objects.first()) else {
        return;
    };
    let (old_values, new_values) = (names(at(old_object, "enum")), names(at(new_object, "enum")));
    let values_kept = if input {
        old_values.is_subset(&new_values)
    } else {
        new_values.is_subset(&old_values)
    };
    if !old_values.is_empty() && !values_kept {
        out.push(format!(
            "{path}: values {old_values:?} became {new_values:?}"
        ));
    }
    if old_object.get("items").is_some() {
        let items = (at(old_object, "items"), at(new_object, "items"));
        compare(
            (old.0, items.0),
            (new.0, items.1),
            input,
            &format!("{path}[]"),
            depth + 1,
            out,
        );
    }
    let (old_required, new_required) = (
        names(at(old_object, "required")),
        names(at(new_object, "required")),
    );
    for (key, old_schema) in props(old_object) {
        let field = format!("{path}.{key}");
        match at(new_object, "properties").get(key) {
            None => out.push(format!("{field}: removed")),
            Some(new_schema) => compare(
                (old.0, old_schema),
                (new.0, new_schema),
                input,
                &field,
                depth + 1,
                out,
            ),
        }
        if !input && old_required.contains(key) && !new_required.contains(key) {
            out.push(format!("{field}: may be missing now"));
        }
    }
    if input {
        for key in new_required.difference(&old_required) {
            out.push(format!("{path}.{key}: newly required"));
        }
    }
}

/// What the new contract breaks of the old: a shape removed, an output
/// that gives less or other, a record that accepts less, or one that is
/// not closed.
fn breaks(old: &Value, new: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let records = names(at(new, RECORDS));
    for name in names(at(old, RECORDS)).difference(&records) {
        out.push(format!("{name}: no longer a record"));
    }
    let new_defs = defs(new);
    for (name, old_def) in defs(old) {
        match new_defs.get(name) {
            None => out.push(format!("{name}: removed")),
            Some(new_def) => compare(
                (old, old_def),
                (new, *new_def),
                records.contains(name),
                name,
                0,
                &mut out,
            ),
        }
    }
    for name in &records {
        let closed = new_defs.get(name).is_some_and(|def| {
            at(resolve(new, def), "additionalProperties") == &Value::Bool(false)
        });
        if !closed {
            out.push(format!(
                "{name}: a record must be closed (additionalProperties false)"
            ));
        }
    }
    out
}

/// Whether `value` is of the JSON Schema type `kind`.
fn is_kind(value: &Value, kind: &str) -> bool {
    match kind {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "string" => value.is_string(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

/// Lists in `out` how `value` at `path` does not fit `schema`: the type,
/// the values, the minimum and maximum, the required and the closed keys,
/// the items, and `anyOf`/`oneOf` as alternatives.
fn fits(doc: &Value, schema: &Value, value: &Value, path: &str, out: &mut Vec<String>) {
    let schema = resolve(doc, schema);
    let alternatives: Vec<&Value> = at(schema, "anyOf")
        .as_array()
        .into_iter()
        .chain(at(schema, "oneOf").as_array())
        .flatten()
        .collect();
    if !alternatives.is_empty() {
        let fitting = |member: &&Value| {
            let mut misfits = Vec::new();
            fits(doc, member, value, path, &mut misfits);
            misfits.is_empty()
        };
        if !alternatives.iter().any(fitting) {
            out.push(format!("{path}: {value} fits none of its alternatives"));
        }
        return;
    }
    let (kinds, _) = shape(doc, schema);
    if !kinds.is_empty() && !kinds.iter().any(|kind| is_kind(value, kind)) {
        out.push(format!("{path}: {value} is not {kinds:?}"));
        return;
    }
    if let Some(allowed) = at(schema, "enum").as_array()
        && !allowed.contains(value)
    {
        out.push(format!(
            "{path}: {value} is not one of {}",
            Value::Array(allowed.clone())
        ));
    }
    if let (Some(number), Some(minimum)) = (value.as_f64(), at(schema, "minimum").as_f64())
        && number < minimum
    {
        out.push(format!("{path}: {value} is below {minimum}"));
    }
    if let (Some(number), Some(maximum)) = (value.as_f64(), at(schema, "maximum").as_f64())
        && number > maximum
    {
        out.push(format!("{path}: {value} is above {maximum}"));
    }
    if let Some(items) = value.as_array() {
        for (index, item) in items.iter().enumerate() {
            fits(
                doc,
                at(schema, "items"),
                item,
                &format!("{path}[{index}]"),
                out,
            );
        }
    }
    if let Some(object) = value.as_object() {
        for key in names(at(schema, "required")) {
            if !object.contains_key(&key) {
                out.push(format!("{path}.{key}: missing"));
            }
        }
        let declared = at(schema, "properties");
        let extra = at(schema, "additionalProperties");
        for (key, member) in object {
            let field = format!("{path}.{key}");
            match declared.get(key) {
                Some(member_schema) => fits(doc, member_schema, member, &field, out),
                None if extra == &Value::Bool(false) => {
                    out.push(format!("{field}: not in the contract"));
                }
                None if extra.is_object() => fits(doc, extra, member, &field, out),
                None => {}
            }
        }
    }
}

/// The shape a printed JSON object is checked against, by its keys.
fn output_def(value: &Value) -> &'static str {
    let has = |key: &str| value.get(key).is_some();
    if has("error") {
        "error"
    } else if has("outcome") {
        "outcome"
    } else if has("instances") {
        "list"
    } else if has("origin") {
        "webConnection"
    } else if has("lines") {
        "read"
    } else {
        "status"
    }
}

fn read_json(path: &Path) -> TestResult<Value> {
    Ok(serde_json::from_str(&fs::read_to_string(path).map_err(
        |error| format!("{}: {error}", path.display()),
    )?)?)
}

#[path = "contract/cache.rs"]
mod cache;

/// The files under `dir` named `name`, recursively.
fn files_named(dir: &Path, name: &str, found: &mut Vec<PathBuf>) -> TestResult {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            files_named(&entry.path(), name, found)?;
        } else if kind.is_file() && entry.file_name() == name {
            found.push(entry.path());
        }
    }
    Ok(())
}

/// Every JSON line a golden case expects on stdout or stderr, with the
/// file it is in.
fn golden_lines() -> TestResult<Vec<(String, Value)>> {
    let mut lines = Vec::new();
    for case in fs::read_dir(GOLDEN)? {
        let case = case?.path();
        for stream in ["stdout", "stderr"] {
            let file = case.join(stream);
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            for line in text.lines().filter(|line| line.starts_with('{')) {
                let value = serde_json::from_str(line)
                    .map_err(|error| format!("{}: {error}", file.display()))?;
                lines.push((file.display().to_string(), value));
            }
        }
    }
    Ok(lines)
}

// catches: a field added to, removed from or retyped in dshd's JSON output, a record or an error kind
// without the contract saying so, and a record with a key no reader of this version knows
#[test]
fn golden_json_fits_the_contract() -> TestResult {
    let doc = read_json(Path::new(CONTRACT))?;
    let mut misfits = Vec::new();
    let mut seen = BTreeSet::new();
    for (file, value) in golden_lines()? {
        let def = output_def(&value);
        seen.insert(def);
        fits(
            &doc,
            &json!({ "$ref": format!("#/$defs/{def}") }),
            &value,
            &format!("{file}: {def}"),
            &mut misfits,
        );
    }
    for def in [
        "status",
        "list",
        "outcome",
        "error",
        "webConnection",
        "read",
    ] {
        assert!(seen.contains(def), "no golden case prints a {def} object");
    }
    let mut records = Vec::new();
    for (name, def) in [
        ("instance.json", "instanceRecord"),
        ("last-run.json", "lastRunRecord"),
    ] {
        let mut found = Vec::new();
        files_named(Path::new(GOLDEN), name, &mut found)?;
        files_named(Path::new(FIXTURES), name, &mut found)?;
        assert!(!found.is_empty(), "no {name} to check");
        records.extend(found.into_iter().map(|path| (path, def)));
    }
    for (path, def) in records {
        let mut found = Vec::new();
        fits(
            &doc,
            &json!({ "$ref": format!("#/$defs/{def}") }),
            &read_json(&path)?,
            &path.display().to_string(),
            &mut found,
        );
        if path.ends_with(REFUSED) {
            assert!(
                found
                    .iter()
                    .any(|misfit| misfit.contains("not in the contract")),
                "{REFUSED} fits, but it holds an unknown key"
            );
        } else {
            misfits.extend(found);
        }
    }
    assert!(
        misfits.is_empty(),
        "JSON outside the contract:\n{}",
        misfits.join("\n")
    );
    Ok(())
}

// catches: a published field removed, retyped, made nullable or optional, a published state or outcome
// value dropped or a new one added to a closed vocabulary, or a record that refuses what it accepted
#[test]
fn the_contract_only_grows_since_publication() -> TestResult {
    let published = read_json(Path::new(PUBLISHED))?;
    let now = read_json(Path::new(CONTRACT))?;
    let broken = breaks(&published, &now);
    assert!(
        broken.is_empty(),
        "these changes break readers of the published contract:\n{}",
        broken.join("\n")
    );
    Ok(())
}

// catches: a compare or fit rule that lets a break through, or one that flags growth as a break
#[test]
fn the_rules_catch_what_breaks_and_allow_what_grows() {
    let closed = |props: Value, required: Value| json!({"type": "object", "properties": props, "required": required, "additionalProperties": false});
    let doc = |output: Value, record: Value| json!({"x-records": ["r"], "$defs": {"o": output, "r": record}});
    let base = doc(
        closed(
            json!({"x": {"type": "string"}, "y": {"type": ["integer", "null"]}, "s": {"type": "string", "enum": ["a", "b"]}}),
            json!(["x", "s"]),
        ),
        closed(
            json!({"id": {"type": "string"}, "t": {"type": "string", "enum": ["p", "q"]}}),
            json!(["id"]),
        ),
    );
    assert_eq!(breaks(&base, &base), Vec::<String>::new());
    let grown = doc(
        closed(
            json!({"x": {"type": "string"}, "y": {"type": "integer"}, "s": {"type": "string", "enum": ["a"]}, "z": {"type": "string"}}),
            json!(["x", "s", "z"]),
        ),
        closed(
            json!({"id": {"type": "string"}, "t": {"type": "string", "enum": ["p", "q", "u"]}, "n": {"type": "integer"}}),
            json!(["id"]),
        ),
    );
    assert_eq!(
        breaks(&base, &grown),
        Vec::<String>::new(),
        "added fields, narrower outputs and wider records are fine"
    );
    let broken = doc(
        closed(
            json!({"y": {"type": ["integer", "string", "null"]}, "s": {"type": "string", "enum": ["a", "b", "c"]}}),
            json!([]),
        ),
        json!({"type": "object", "properties": {"id": {"type": "string"}, "t": {"type": "string", "enum": ["p"]}, "n": {"type": "integer"}}, "required": ["id", "n"]}),
    );
    let found = breaks(&base, &broken).join("\n");
    for expected in [
        "o.x: removed",
        "o.y: type",
        "o.s: values",
        "o.s: may be missing now",
        "r.t: values",
        "r.n: newly required",
        "r: a record must be closed",
    ] {
        assert!(found.contains(expected), "missed {expected:?} in:\n{found}");
    }
    let gone = json!({"x-records": [], "$defs": {}});
    assert_eq!(
        breaks(&base, &gone),
        ["r: no longer a record", "o: removed", "r: removed"]
    );

    let schema = json!({"$ref": "#/$defs/o"});
    let misfits = |value: Value| {
        let mut out = Vec::new();
        fits(&base, &schema, &value, "o", &mut out);
        out
    };
    assert_eq!(
        misfits(json!({"x": "v", "y": null, "s": "a"})),
        Vec::<String>::new()
    );
    for (value, expected) in [
        (
            json!({"x": "v", "s": "a", "w": 1}),
            "o.w: not in the contract",
        ),
        (json!({"x": 1, "s": "a"}), "o.x: 1 is not"),
        (json!({"x": "v", "s": "c"}), "o.s: \"c\" is not one of"),
        (json!({"s": "a"}), "o.x: missing"),
        (json!({"x": "v", "s": "a", "y": "1"}), "o.y: \"1\" is not"),
    ] {
        let found = misfits(value).join("\n");
        assert!(found.contains(expected), "missed {expected:?} in:\n{found}");
    }
}
