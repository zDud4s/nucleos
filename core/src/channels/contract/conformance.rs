//! Shape conformance (Anexo C, C.11): every fixture of the shared tree is run through the
//! core's strict decoders. Valid ones must decode and re-encode to their canonical form; invalid
//! ones must fail with the code and pointer the index names. The Go side reads the same files;
//! the `go` overlay of an index entry is none of the core's business and is ignored here.
//! (The `scenario` entries belong to the capability scenarios of I2, not to this file.)

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::*;

#[derive(Deserialize)]
struct Index {
    contract_version: String,
    tags: Vec<String>,
    fixtures: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    file: String,
    shape: String,
    #[serde(default)]
    covers: Vec<String>,
    valid: bool,
    error: Option<Expected>,
    canonical: Option<String>,
    expect: Option<serde_json::Map<String, Value>>,
    compatible: Option<bool>,
}

#[derive(Deserialize)]
struct Expected {
    code: String,
    at: Option<String>,
}

fn read(rel: &str) -> Vec<u8> {
    let path = fixtures_dir().join(rel);
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()))
}

fn read_json(rel: &str) -> Value {
    serde_json::from_slice(&read(rel)).unwrap_or_else(|e| panic!("{rel} is not JSON: {e}"))
}

fn index() -> Index {
    serde_json::from_slice(&read("index.json")).expect("index.json must match its schema")
}

/// Every `*.json` under `dir`, as a `/`-separated path relative to `root`.
fn tree_files(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
    for entry in std::fs::read_dir(dir).expect("the fixture directory must be readable") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            tree_files(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "json") {
            let rel = path.strip_prefix(root).expect("inside the root");
            let rel = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if rel != "index.json" {
                out.insert(rel);
            }
        }
    }
}

fn run<T: Shape>(bytes: &[u8]) -> Result<Value, ContractError>
where
    T::Valid: Serialize,
{
    decode::<T>(bytes).map(|v| serde_json::to_value(v).expect("a valid shape must encode"))
}

/// Decodes `bytes` as the named shape and returns the re-encoded value. `None`: not a decoder
/// shape (`scenario`).
fn decode_as(shape: &str, bytes: &[u8]) -> Option<Result<Value, ContractError>> {
    Some(match shape {
        "hello" => run::<HelloShape>(bytes),
        "hello_response" => run::<HelloResponseShape>(bytes),
        "error_response" => run::<ErrorResponseShape>(bytes),
        "media_response" => run::<MediaResponseShape>(bytes),
        "event" => run::<EventShape>(bytes),
        "event_response" => run::<EventResponseShape>(bytes),
        "outbox_response" => run::<OutboxResponseShape>(bytes),
        "action" => run::<OutActionShape>(bytes),
        "ack" => run::<AckShape>(bytes),
        "ack_response" => run::<AckResponseShape>(bytes),
        "renew_response" => run::<RenewResponseShape>(bytes),
        "scenario" => return None,
        other => panic!("index.json names an unknown shape {other:?}"),
    })
}

/// Why `entry` does not conform, if it does not.
fn check_entry(entry: &Entry) -> Result<(), String> {
    let Some(outcome) = decode_as(&entry.shape, &read(&entry.file)) else {
        return Ok(());
    };
    if !entry.valid {
        let want = entry
            .error
            .as_ref()
            .ok_or("an invalid entry names no error")?;
        let err = match outcome {
            Ok(_) => return Err(format!("decoded, expected {}", want.code)),
            Err(e) => e,
        };
        if err.code.wire() != want.code || err.at != want.at {
            return Err(format!(
                "failed with {} at {:?}, expected {} at {:?}",
                err.code.wire(),
                err.at,
                want.code,
                want.at
            ));
        }
        return Ok(());
    }
    let encoded = outcome.map_err(|e| format!("refused: {} at {:?}", e.code.wire(), e.at))?;
    let canonical = read_json(entry.canonical.as_deref().unwrap_or(&entry.file));
    if encoded != canonical {
        return Err(format!(
            "re-encodes to {encoded}, canonical form is {canonical}"
        ));
    }
    for (pointer, want) in entry.expect.iter().flatten() {
        match encoded.pointer(pointer) {
            Some(got) if got == want => {}
            got => return Err(format!("{pointer} is {got:?}, expected {want}")),
        }
    }
    if let Some(want) = entry.compatible {
        let version = encoded["version"].as_str().ok_or("no hello version")?;
        if compatible(version) != want {
            return Err(format!("compatible({version}) should be {want}"));
        }
    }
    Ok(())
}

#[test]
fn contract_version_matches_the_index() {
    assert_eq!(index().contract_version, CONTRACT_VERSION);
}

#[test]
fn index_matches_tree() {
    let root = fixtures_dir();
    let mut on_disk = BTreeSet::new();
    tree_files(&root, &root, &mut on_disk);
    let listed: BTreeSet<String> = index().fixtures.into_iter().map(|e| e.file).collect();
    let orphans: Vec<_> = on_disk.difference(&listed).collect();
    let ghosts: Vec<_> = listed.difference(&on_disk).collect();
    assert!(
        orphans.is_empty() && ghosts.is_empty(),
        "orphans (on disk, not indexed): {orphans:?}; ghosts (indexed, missing): {ghosts:?}"
    );
}

#[test]
fn coverage_is_complete() {
    let index = index();
    let mut problems = Vec::new();
    for tag in &index.tags {
        let covering = |valid: bool| {
            index
                .fixtures
                .iter()
                .any(|e| e.valid == valid && e.covers.contains(tag))
        };
        if !covering(true) {
            problems.push(format!("tag {tag} has no valid fixture"));
        }
        if !covering(false) {
            problems.push(format!("tag {tag} has no invalid fixture"));
        }
    }
    for entry in &index.fixtures {
        for cover in &entry.covers {
            if !index.tags.contains(cover) {
                problems.push(format!("{} covers the unknown tag {cover}", entry.file));
            }
        }
        if entry.shape == "scenario" && !entry.covers.is_empty() {
            problems.push(format!(
                "{} is a scenario and must not have covers",
                entry.file
            ));
        }
        if let Some(canonical) = &entry.canonical {
            let target = index.fixtures.iter().find(|e| &e.file == canonical);
            if !target.is_some_and(|t| t.valid && t.shape == entry.shape) {
                problems.push(format!(
                    "{} points at {canonical}, which is not a valid fixture of its shape",
                    entry.file
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_fixture_conforms() {
    let index = index();
    let failures: Vec<String> = index
        .fixtures
        .iter()
        .filter_map(|e| check_entry(e).err().map(|why| format!("{}: {why}", e.file)))
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} fixtures do not conform:\n{}",
        failures.len(),
        index.fixtures.len(),
        failures.join("\n")
    );
}

// ---- by hand: the tables of C.4-C.9 that name their file -----------------------------------

fn value_of(shape: &str, rel: &str) -> Value {
    decode_as(shape, &read(rel))
        .expect("a decoder shape")
        .unwrap_or_else(|e| panic!("{rel} must decode, got {} at {:?}", e.code.wire(), e.at))
}

#[test]
fn edit_is_a_valid_outbox_action() {
    // Whether the adapter can render two refs over one part is the adapter's call
    // (`edit_unsupported_shape`); to the core this is a valid action.
    let edit = value_of("action", "outbox/edit.json");
    assert_eq!(edit["type"], "edit");
    assert_eq!(
        edit["target_platform_refs"],
        serde_json::json!(["993", "994"])
    );
}

#[test]
fn explicit_defaults_encode_to_the_canonical_form() {
    assert_eq!(
        value_of("hello", "hello/minimal-explicit.json"),
        value_of("hello", "hello/minimal.json")
    );
    assert_eq!(
        value_of("ack", "acks/ok-empty-refs.json"),
        value_of("ack", "acks/ok-no-refs.json")
    );
}

#[test]
fn event_time_is_kept_verbatim() {
    let event = value_of("event", "events/message-group-image.json");
    assert_eq!(event["at"], "2026-10-19T10:31:00+01:00");
}
