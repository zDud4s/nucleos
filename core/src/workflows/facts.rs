//! §spec motor-de-workflows 4.1 / 4.2 - what a condition is evaluated against.
//!
//! `Facts` is a flat `key -> string` map. Engine facts are inserted as `job.<name>` /
//! `project.<name>`; a node's emission (`<worktree>/.nucleos/<node_id>.json`) is published as
//! `<node_id>.<key>`. Emissions are flat: scalar values only.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::execution::worktree::ARTIFACTS_DIR;

/// Ceiling for one emission file. A runaway or partial file must not be slurped.
pub const MAX_EMISSION_BYTES: usize = 64 * 1024;

/// Node ids reserved for engine facts.
const RESERVED_IDS: [&str; 2] = ["job", "project"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    map: BTreeMap<String, String>,
    emitted: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Emission {
    NotEmitted,
    Emitted(BTreeMap<String, String>),
}

impl Facts {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    /// Flat insert of an already-qualified key (`job.round`, `project.id`, `plan.done`).
    /// Last write wins.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.map.insert(key.into(), value.into());
    }

    /// Publish a node's emission under `<node_id>.<key>`. `NotEmitted` publishes nothing and
    /// records nothing. `Emitted(empty)` publishes nothing but records the node as having
    /// emitted (diagnostics only). Node ids `job` and `project` are reserved for engine facts.
    pub fn set_emission(&mut self, node_id: &str, emission: Emission) -> Result<(), String> {
        if RESERVED_IDS.contains(&node_id) {
            return Err(format!(
                "node id `{node_id}` is reserved for engine facts and cannot publish an emission"
            ));
        }
        if let Emission::Emitted(entries) = emission {
            self.emitted.insert(node_id.to_string());
            for (key, value) in entries {
                self.map.insert(format!("{node_id}.{key}"), value);
            }
        }
        Ok(())
    }

    /// True when `set_emission` saw `Emitted(_)` for this node.
    pub fn has_emitted(&self, node_id: &str) -> bool {
        self.emitted.contains(node_id)
    }

    /// Convenience for the engine: `job.<name>`.
    pub fn insert_job(&mut self, name: &str, value: impl Into<String>) {
        self.insert(format!("job.{name}"), value);
    }

    /// Convenience for the engine: `project.<name>`.
    pub fn insert_project(&mut self, name: &str, value: impl Into<String>) {
        self.insert(format!("project.{name}"), value);
    }
}

/// `<worktree>/.nucleos/<node_id>.json`. Err for an id that is not a plain file stem.
pub fn emission_path(worktree: &Path, node_id: &str) -> Result<PathBuf, String> {
    if node_id.is_empty()
        || node_id == "."
        || node_id == ".."
        || node_id.starts_with('.')
        || node_id.contains(['/', '\\', '\0'])
    {
        return Err(format!(
            "node id `{node_id}` is not a plain file stem and cannot name an emission file"
        ));
    }
    Ok(worktree.join(ARTIFACTS_DIR).join(format!("{node_id}.json")))
}

/// The contract function. Same as `read_emission_with(worktree, node_id, &[])`.
pub fn read_emission(worktree: &Path, node_id: &str) -> Result<Emission, String> {
    read_emission_with(worktree, node_id, &[])
}

/// Keys named in `opaque` are skipped instead of refused when their value is an array/object
/// (used for `plan.json`'s `items`).
pub fn read_emission_with(
    worktree: &Path,
    node_id: &str,
    opaque: &[&str],
) -> Result<Emission, String> {
    let path = emission_path(worktree, node_id)?;
    match std::fs::read(&path) {
        Ok(bytes) => parse_emission(node_id, Some(&bytes), opaque),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Emission::NotEmitted),
        Err(e) => Err(format!(
            "node `{node_id}`: cannot read {ARTIFACTS_DIR}/{node_id}.json: {e}"
        )),
    }
}

fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    !key.contains('.') && !key.chars().any(char::is_whitespace)
}

/// PURE: bytes in, decision out. `None` = no file.
pub fn parse_emission(
    node_id: &str,
    contents: Option<&[u8]>,
    opaque: &[&str],
) -> Result<Emission, String> {
    let Some(bytes) = contents else {
        return Ok(Emission::NotEmitted);
    };
    let file = format!("{ARTIFACTS_DIR}/{node_id}.json");
    if bytes.len() > MAX_EMISSION_BYTES {
        return Err(format!(
            "node `{node_id}` wrote {file} of {} bytes, over the {MAX_EMISSION_BYTES} byte ceiling",
            bytes.len()
        ));
    }
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(format!(
            "node `{node_id}` wrote {file} but it is empty (a truncated write, not an empty emission)"
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| format!("node `{node_id}` wrote {file} that is not valid JSON: {e}"))?;
    let serde_json::Value::Object(object) = value else {
        return Err(format!(
            "node `{node_id}` wrote {file} whose top level is not a JSON object"
        ));
    };
    let mut out = BTreeMap::new();
    // A duplicate key in the JSON object: serde_json keeps the last silently; accepted.
    for (key, value) in object {
        let skip = matches!(
            value,
            serde_json::Value::Array(_) | serde_json::Value::Object(_)
        ) && opaque.contains(&key.as_str());
        if skip {
            continue;
        }
        if !valid_key(&key) {
            return Err(format!(
                "node `{node_id}` key `{key}`: a key must start with a letter or `_` and contain no `.` or whitespace"
            ));
        }
        let text = match value {
            serde_json::Value::String(s) => s,
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Null => {
                return Err(format!("node `{node_id}` key `{key}`: value is null"));
            }
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                return Err(format!(
                    "node `{node_id}` key `{key}`: value is a list/object; emissions are flat (one level, scalar values)"
                ));
            }
        };
        out.insert(key, text);
    }
    Ok(Emission::Emitted(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<Emission, String> {
        parse_emission("triage", Some(json.as_bytes()), &[])
    }

    fn emitted(pairs: &[(&str, &str)]) -> Emission {
        Emission::Emitted(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn no_file_is_not_emitted_and_an_empty_object_is_emitted_empty() {
        let none = parse_emission("triage", None, &[]).unwrap();
        let empty = parse("{}").unwrap();
        assert_eq!(none, Emission::NotEmitted);
        assert_eq!(empty, Emission::Emitted(BTreeMap::new()));
        assert_ne!(none, empty);
    }

    #[test]
    fn an_empty_or_truncated_file_is_an_error_not_an_empty_emission() {
        for src in [&b""[..], b"  \n", b"{\"size\": \"sma", b"{"] {
            assert!(parse_emission("triage", Some(src), &[]).is_err(), "{src:?}");
        }
    }

    #[test]
    fn string_bool_and_number_values_are_flattened_to_strings() {
        let got = parse(r#"{"size":"small","done":true,"round":2,"ratio":1.5}"#).unwrap();
        assert_eq!(
            got,
            emitted(&[
                ("size", "small"),
                ("done", "true"),
                ("round", "2"),
                ("ratio", "1.5")
            ])
        );
    }

    #[test]
    fn a_nested_object_is_refused_naming_the_node_and_the_key() {
        let err = parse(r#"{"a":{"b":1}}"#).unwrap_err();
        assert!(err.contains("triage") && err.contains("`a`"), "{err}");
    }

    #[test]
    fn a_list_value_is_refused_naming_the_node_and_the_key() {
        let err = parse(r#"{"tags":["x"]}"#).unwrap_err();
        assert!(err.contains("triage") && err.contains("`tags`"), "{err}");
    }

    #[test]
    fn null_is_refused() {
        let err = parse(r#"{"a":null}"#).unwrap_err();
        assert!(err.contains("triage") && err.contains("`a`"), "{err}");
    }

    #[test]
    fn a_root_that_is_not_an_object_is_refused() {
        for src in ["[1]", "\"x\"", "3", "null"] {
            let err = parse(src).unwrap_err();
            assert!(err.contains("triage"), "{err}");
        }
    }

    #[test]
    fn opaque_keys_are_skipped_not_refused() {
        let src = br#"{"items":[{"description":"x"}],"done":true,"why":"ok"}"#;
        let got = parse_emission("plan", Some(src), &["items"]).unwrap();
        assert_eq!(got, emitted(&[("done", "true"), ("why", "ok")]));
        let err = parse_emission("plan", Some(src), &[]).unwrap_err();
        assert!(err.contains("`items`"), "{err}");
    }

    #[test]
    fn keys_with_dots_or_spaces_are_refused() {
        for src in [r#"{"a.b":"x"}"#, r#"{"a b":"x"}"#, r#"{"":"x"}"#] {
            let err = parse(src).unwrap_err();
            assert!(err.contains("triage"), "{src}: {err}");
        }
    }

    #[test]
    fn a_file_over_the_ceiling_is_refused() {
        let mut big = String::from("{\"a\":\"");
        big.push_str(&"x".repeat(MAX_EMISSION_BYTES));
        big.push_str("\"}");
        let err = parse(&big).unwrap_err();
        assert!(err.contains("triage") && err.contains("ceiling"), "{err}");
    }

    #[test]
    fn emission_path_refuses_ids_that_are_not_a_plain_stem() {
        let wt = Path::new("wt");
        for id in ["..", ".", "a/b", "a\\b", "", ".hidden", "a\0b"] {
            assert!(emission_path(wt, id).is_err(), "{id:?}");
        }
        let ok = emission_path(wt, "triage").unwrap();
        assert_eq!(ok, wt.join(".nucleos").join("triage.json"));
    }

    #[test]
    fn read_emission_reads_dot_nucleos_slash_id_json() {
        let dir = tempfile::tempdir().unwrap();
        // Missing directory -> NotEmitted.
        assert_eq!(
            read_emission(dir.path(), "triage").unwrap(),
            Emission::NotEmitted
        );
        let art = dir.path().join(ARTIFACTS_DIR);
        std::fs::create_dir_all(&art).unwrap();
        std::fs::write(art.join("triage.json"), r#"{"size":"small"}"#).unwrap();
        assert_eq!(
            read_emission(dir.path(), "triage").unwrap(),
            emitted(&[("size", "small")])
        );
        // A directory at the path is an io error, not NotEmitted.
        std::fs::create_dir_all(art.join("odd.json")).unwrap();
        assert!(read_emission(dir.path(), "odd").is_err());
    }

    #[test]
    fn set_emission_publishes_namespaced_keys_and_remembers_the_node() {
        let mut facts = Facts::new();
        facts
            .set_emission("triage", emitted(&[("size", "small")]))
            .unwrap();
        assert_eq!(facts.get("triage.size"), Some("small"));
        assert!(facts.has_emitted("triage"));

        facts
            .set_emission("empty", Emission::Emitted(BTreeMap::new()))
            .unwrap();
        assert!(facts.has_emitted("empty"));
        assert_eq!(facts.get("empty.size"), None);

        facts.set_emission("none", Emission::NotEmitted).unwrap();
        assert!(!facts.has_emitted("none"));
        assert_eq!(facts.get("none.size"), None);
    }

    #[test]
    fn job_and_project_facts_live_beside_node_facts() {
        let mut facts = Facts::new();
        facts.insert_job("round", "2");
        facts.insert_project("id", "p1");
        assert_eq!(facts.get("job.round"), Some("2"));
        assert_eq!(facts.get("project.id"), Some("p1"));
        assert!(
            facts
                .set_emission("job", emitted(&[("round", "9")]))
                .is_err()
        );
        assert!(
            facts
                .set_emission("project", emitted(&[("id", "x")]))
                .is_err()
        );
        assert_eq!(facts.get("job.round"), Some("2"));
    }
}
