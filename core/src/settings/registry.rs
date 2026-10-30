//! The document kinds the settings store knows: a trait that ties a Rust type to a name and a scope kind,
//! and the table of the 13 documents. RED stub: the table is not built yet.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::decode::{Erased, Prepared};
use super::scope::Kind;
use crate::config::{AutopilotRules, VerifyConfig, WebConfig};

/// A configuration document: a typed value stored as JSON under `(scope, NAME)`.
pub trait Document: Serialize + DeserializeOwned + Clone + Send + Sync + 'static {
    /// The key in `settings.name`.
    const NAME: &'static str;
    /// Whether the document lives at machine or project scope.
    const KIND: Kind;
    /// Validation and normalisation after a successful decode. The `Err` text is shown to the operator.
    fn finish(self) -> Result<Self, String> {
        Ok(self)
    }
}

/// One row of the registry: everything the store and the API need to know about a document without its type.
#[derive(Debug, Clone, Copy)]
pub struct DocType {
    pub name: &'static str,
    pub kind: Kind,
    /// The YAML file this document used to live in; the importer reads it.
    pub origin_file: &'static str,
    /// What the operator is told about when a change applies.
    pub takes_effect: &'static str,
    /// Strict decode of a body, `finish`, canonical text.
    pub prepare: fn(&Value) -> Result<Prepared, String>,
    /// Lenient decode of stored text.
    pub load: fn(&str) -> Result<Erased, String>,
    /// The default value as JSON, when the type has one.
    pub default_json: fn() -> Option<Value>,
}

/// Every document kind, machine documents first, in the order the settings page lists them.
pub fn documents() -> &'static [DocType] {
    todo!("RED stub: registry::documents")
}

pub fn doc_type(_name: &str) -> Option<&'static DocType> {
    todo!("RED stub: registry::doc_type")
}

// RED stub: only the three impls the snapshot and store tests need to compile. The `finish` bodies
// (validators) and the other ten impls belong to the GREEN step.
impl Document for WebConfig {
    const NAME: &'static str = "web";
    const KIND: Kind = Kind::Machine;
}

impl Document for VerifyConfig {
    const NAME: &'static str = "verify";
    const KIND: Kind = Kind::Machine;
}

impl Document for AutopilotRules {
    const NAME: &'static str = "autopilot";
    const KIND: Kind = Kind::Project;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::{Value, json};

    use super::*;
    use crate::config::{ModelsConfig, VerifyConfig};

    fn prepared_text(name: &str, body: &Value) -> Result<String, String> {
        let doc = doc_type(name).unwrap_or_else(|| panic!("{name} is registered"));
        (doc.prepare)(body).map(|prepared| prepared.canonical)
    }

    #[test]
    fn there_are_thirteen_documents_with_unique_names() {
        let docs = documents();
        assert_eq!(docs.len(), 13);
        let names: BTreeSet<_> = docs.iter().map(|doc| doc.name).collect();
        assert_eq!(names.len(), 13);
        assert_eq!(docs.iter().filter(|doc| doc.kind == Kind::Machine).count(), 11);
        let project: Vec<_> = docs.iter().filter(|doc| doc.kind == Kind::Project).map(|doc| doc.name).collect();
        assert_eq!(project, vec!["autopilot", "workflow_pins"]);
    }

    #[test]
    fn every_document_names_its_origin_file_and_when_it_takes_effect() {
        for doc in documents() {
            assert!(doc.origin_file.ends_with(".yaml"), "{}: {}", doc.name, doc.origin_file);
            assert!(!doc.takes_effect.is_empty(), "{}", doc.name);
        }
    }

    #[test]
    fn every_default_passes_its_own_validation() {
        let mut without_default = Vec::new();
        for doc in documents() {
            match (doc.default_json)() {
                Some(body) => {
                    (doc.prepare)(&body)
                        .unwrap_or_else(|why| panic!("the default of {} is refused: {why}", doc.name));
                }
                None => without_default.push(doc.name),
            }
        }
        assert_eq!(without_default, vec!["council"], "only the council has no default");
    }

    #[test]
    fn preparing_a_canonical_body_again_changes_nothing() {
        for doc in documents() {
            let Some(body) = (doc.default_json)() else { continue };
            let first = prepared_text(doc.name, &body).unwrap();
            let again: Value = serde_json::from_str(&first).unwrap();
            assert_eq!(prepared_text(doc.name, &again).unwrap(), first, "{}", doc.name);
        }
    }

    #[test]
    fn a_council_roster_prepares_and_an_empty_one_is_refused() {
        let roster = json!({
            "chairman": {"kind": "cloud", "ref": "claude-opus-4-8"},
            "members": [
                {"kind": "cloud", "ref": "claude-opus-4-8"},
                {"kind": "cloud", "ref": "gpt-5.6-terra"}
            ]
        });
        let first = prepared_text("council", &roster).expect("a roster prepares");
        let again: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(prepared_text("council", &again).unwrap(), first);

        let empty = json!({"chairman": {"kind": "cloud", "ref": "claude-opus-4-8"}, "members": []});
        let why = prepared_text("council", &empty).unwrap_err();
        assert!(why.contains("the roster has no members"), "{why}");
    }

    #[test]
    fn a_write_refuses_an_unknown_field_but_a_read_tolerates_it() {
        let doc = doc_type("verify").unwrap();
        let body = json!({"capacity": 3, "gone": 1});
        let why = (doc.prepare)(&body).unwrap_err();
        assert!(why.contains("unknown field"), "{why}");

        let loaded = (doc.load)(&body.to_string()).expect("a stored row with an extra key still loads");
        let config = loaded.downcast::<VerifyConfig>().ok().expect("a VerifyConfig");
        assert_eq!(config.capacity, 3);
    }

    #[test]
    fn the_existing_validators_run_over_the_typed_value() {
        let why = prepared_text("verify", &json!({"capacity": 0})).unwrap_err();
        assert!(why.contains("capacity must be at least 1"), "{why}");

        let mut models = serde_json::to_value(ModelsConfig::default()).unwrap();
        models["local_engine"] = json!("nope");
        let why = prepared_text("models", &models).unwrap_err();
        assert!(why.contains("names no local engine"), "{why}");

        assert!(prepared_text("autopilot", &json!({"schedules": "daily"})).is_err());
        assert!(prepared_text("workflow_pins", &json!({"workflows": [
            {"name": "../x", "version": "v1", "origin": "o", "hash": "h"}
        ]}))
        .unwrap_err()
        .contains("not a usable workflow name"));
    }

    #[test]
    fn the_router_is_stored_in_its_canonical_form() {
        let text = prepared_text("router", &json!({"mode": " shadow ", "timeout_ms": 5})).unwrap();
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["mode"], "shadow");
        assert_eq!(body["timeout_ms"], 100);
    }

    #[test]
    fn doc_type_finds_by_name_and_refuses_the_rest() {
        assert_eq!(doc_type("verify").map(|doc| doc.kind), Some(Kind::Machine));
        assert_eq!(doc_type("autopilot").map(|doc| doc.kind), Some(Kind::Project));
        assert!(doc_type("nope").is_none());
    }
}
