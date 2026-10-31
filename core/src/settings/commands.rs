//! The fields of a settings document whose text the daemon later runs as a command.
//!
//! A paired remote device may edit settings, but never a command the host will execute: the HTTP layer compares
//! [`command_fields`] of the body it is asked to store against the stored one and refuses any difference. Only
//! the fields listed in `FIELDS` count; everything else in a document is free to change.

use std::collections::{BTreeMap, HashSet};

use serde_json::Value;

/// One step from the root of a document body to a command field.
#[derive(Clone, Copy)]
enum Seg {
    /// A key of an object.
    Key(&'static str),
    /// Every element of an array of objects, told apart by their `name`, not by position.
    Each,
    /// Every value of an object used as a map, told apart by its key.
    Map,
}

/// Document name and the path to each of its command fields. `schema::tests` fails when a field marked
/// `x-widget: command` in a registered schema is missing here; `broker_prefix` and `tts_command` carry no
/// such marker and are listed by hand.
const FIELDS: &[(&str, &[Seg])] = &[
    ("verify", &[Seg::Key("broker_prefix")]),
    ("voice", &[Seg::Key("stt_command")]),
    ("voice", &[Seg::Key("tts_command")]),
    ("autopilot", &[Seg::Key("gate_command")]),
    ("autopilot", &[Seg::Key("postgate_idle_command")]),
    (
        "autopilot",
        &[Seg::Key("schedules"), Seg::Each, Seg::Key("command")],
    ),
    (
        "autopilot",
        &[Seg::Key("schedules"), Seg::Each, Seg::Key("cwd")],
    ),
    (
        "workflow_pins",
        &[
            Seg::Key("workflows"),
            Seg::Each,
            Seg::Key("nodes"),
            Seg::Map,
            Seg::Key("command"),
        ],
    ),
    // Moving a pinned bundle's folder swaps the commands that run from it.
    (
        "workflow_pins",
        &[Seg::Key("workflows"), Seg::Each, Seg::Key("path")],
    ),
];

/// The command fields of `body`, a document called `doc`, as (path, value). A path names an array element by its
/// `name` and a map entry by its key, so reordering a list changes nothing. A field that is absent, `null`, an
/// empty string or an empty list runs nothing and is left out: absent and empty are the same answer.
pub fn command_fields(doc: &str, body: &Value) -> BTreeMap<String, Value> {
    let mut found = BTreeMap::new();
    for (name, path) in FIELDS {
        if *name == doc {
            collect(body, path, String::new(), &mut found);
        }
    }
    found
}

fn collect(value: &Value, rest: &[Seg], at: String, found: &mut BTreeMap<String, Value>) {
    let Some((head, rest)) = rest.split_first() else {
        let runs_nothing = match value {
            Value::Null => true,
            Value::String(text) => text.is_empty(),
            Value::Array(items) => items.is_empty(),
            _ => false,
        };
        if !runs_nothing {
            found.insert(at, value.clone());
        }
        return;
    };
    match head {
        Seg::Key(key) => {
            if let Some(inner) = value.get(*key) {
                collect(inner, rest, format!("{at}.{key}"), found);
            }
        }
        Seg::Each => {
            let mut seen = HashSet::new();
            for (index, item) in value.as_array().into_iter().flatten().enumerate() {
                // A repeated or missing name falls back to the position, so no element hides another.
                let label = match item.get("name").and_then(Value::as_str) {
                    Some(name) if seen.insert(name) => name.to_owned(),
                    _ => format!("#{index}"),
                };
                collect(item, rest, format!("{at}[{label}]"), found);
            }
        }
        Seg::Map => {
            for (key, inner) in value.as_object().into_iter().flatten() {
                collect(inner, rest, format!("{at}{{{key}}}"), found);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::*;
    use crate::settings::documents;

    /// `FIELDS` paths in the shape `widget_paths` produces: `.key`, `[]` for each element, `{}` for each entry.
    fn rendered(doc: &str) -> BTreeSet<String> {
        FIELDS
            .iter()
            .filter(|(name, _)| *name == doc)
            .map(|(_, path)| {
                path.iter()
                    .map(|seg| match seg {
                        Seg::Key(key) => format!(".{key}"),
                        Seg::Each => "[]".to_owned(),
                        Seg::Map => "{}".to_owned(),
                    })
                    .collect()
            })
            .collect()
    }

    /// Every place under `schema` that carries `x-widget: command`, following `$ref` and the `anyOf` family.
    fn widget_paths(
        schema: &Value,
        root: &Value,
        at: &str,
        depth: usize,
        out: &mut BTreeSet<String>,
    ) {
        if depth > 12 {
            return;
        }
        if let Some(target) = schema
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|reference| root.pointer(reference.trim_start_matches('#')))
        {
            widget_paths(target, root, at, depth + 1, out);
        }
        if schema.get("x-widget").and_then(Value::as_str) == Some("command") {
            out.insert(at.to_owned());
        }
        for (key, inner) in schema
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            widget_paths(inner, root, &format!("{at}.{key}"), depth + 1, out);
        }
        if let Some(items) = schema.get("items") {
            widget_paths(items, root, &format!("{at}[]"), depth + 1, out);
        }
        if let Some(extra) = schema
            .get("additionalProperties")
            .filter(|extra| extra.is_object())
        {
            widget_paths(extra, root, &format!("{at}{{}}"), depth + 1, out);
        }
        for key in ["anyOf", "oneOf", "allOf"] {
            for branch in schema
                .get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                widget_paths(branch, root, at, depth + 1, out);
            }
        }
    }

    #[test]
    fn every_command_widget_in_a_schema_is_covered() {
        let mut seen = 0;
        for doc in documents() {
            let schema = doc.schema();
            let mut marked = BTreeSet::new();
            widget_paths(&schema, &schema, "", 0, &mut marked);
            let covered = rendered(doc.name);
            for path in &marked {
                assert!(
                    covered.contains(path),
                    "{}: `{path}` is marked `x-widget: command` but is not in settings::commands::FIELDS",
                    doc.name
                );
            }
            seen += marked.len();
        }
        assert!(
            seen >= 4,
            "the walk found the known command widgets, saw {seen}"
        );
    }

    #[test]
    fn every_listed_field_belongs_to_a_registered_document() {
        for (name, _) in FIELDS {
            assert!(
                documents().iter().any(|doc| doc.name == *name),
                "`{name}` is listed in FIELDS but no document has that name"
            );
        }
    }

    #[test]
    fn the_fields_of_each_document_are_found() {
        let verify = command_fields(
            "verify",
            &json!({ "capacity": 2, "broker_prefix": ["py", "h.py"] }),
        );
        assert_eq!(verify[".broker_prefix"], json!(["py", "h.py"]));

        let voice = command_fields("voice", &json!({ "stt_command": "a", "tts_command": "b" }));
        assert_eq!(voice.len(), 2);

        let autopilot = command_fields(
            "autopilot",
            &json!({
                "gate_command": "g",
                "postgate_idle_command": "i",
                "schedules": [{ "name": "r1", "command": "c" }, { "name": "r2", "prompt": "p" }],
            }),
        );
        assert_eq!(autopilot.len(), 3);
        assert_eq!(autopilot[".schedules[r1].command"], json!("c"));

        let pins = command_fields(
            "workflow_pins",
            &json!({ "workflows": [{ "name": "w", "nodes": { "build": { "command": "make" } } }] }),
        );
        assert_eq!(pins[".workflows[w].nodes{build}.command"], json!("make"));

        let pinned = command_fields(
            "workflow_pins",
            &json!({ "workflows": [{ "name": "w", "path": "other/bundle" }, { "name": "x", "path": null }] }),
        );
        assert_eq!(pinned.len(), 1);
        assert_eq!(pinned[".workflows[w].path"], json!("other/bundle"));
    }

    #[test]
    fn a_schedule_cwd_is_a_command_field() {
        let found = command_fields(
            "autopilot",
            &json!({ "schedules": [{ "name": "r1", "command": "c", "cwd": "tools" }, { "name": "r2", "cwd": null }] }),
        );
        assert_eq!(found[".schedules[r1].cwd"], json!("tools"));
        assert!(!found.contains_key(".schedules[r2].cwd"));
    }

    #[test]
    fn a_document_without_command_fields_has_none() {
        assert!(command_fields("models", &json!({ "gate_command": "x" })).is_empty());
    }

    #[test]
    fn absent_null_and_empty_are_the_same_answer() {
        let none = command_fields("voice", &json!({}));
        assert!(none.is_empty());
        assert_eq!(
            command_fields("voice", &json!({ "stt_command": "", "tts_command": null })),
            none
        );
        assert_eq!(
            command_fields("verify", &json!({ "broker_prefix": [] })),
            none
        );
    }

    #[test]
    fn schedules_are_told_apart_by_name_not_by_position() {
        let one = json!({ "schedules": [{ "name": "a", "command": "x" }, { "name": "b", "command": "y" }] });
        let swapped = json!({ "schedules": [{ "name": "b", "command": "y" }, { "name": "a", "command": "x" }] });
        assert_eq!(
            command_fields("autopilot", &one),
            command_fields("autopilot", &swapped)
        );

        let renamed = json!({ "schedules": [{ "name": "a", "command": "y" }, { "name": "b", "command": "x" }] });
        assert_ne!(
            command_fields("autopilot", &one),
            command_fields("autopilot", &renamed)
        );
    }

    #[test]
    fn two_schedules_with_one_name_both_count() {
        let twin = json!({ "schedules": [{ "name": "a", "command": "x" }, { "name": "a", "command": "y" }] });
        assert_eq!(command_fields("autopilot", &twin).len(), 2);
    }
}
