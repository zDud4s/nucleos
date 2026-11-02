//! The JSON Schema of every settings document, with the `x-widget` annotations the settings page renders from.
//!
//! The schemas come from `DocType::schema()` (the `JsonSchema` derives on the document types); this file holds
//! the tests that pin them to the snapshots under `settings/snapshots/`.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::Value;

    use crate::settings::{doc_type, documents};

    /// The closed set of widget hints (spec §8).
    const WIDGETS: [&str; 4] = ["model-picker", "cron", "command", "secret-ref"];

    fn snapshot_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("settings")
            .join("snapshots")
            .join(format!("{}.json", name.replace('/', "__")))
    }

    fn schema_of(name: &str) -> Value {
        doc_type(name)
            .unwrap_or_else(|| panic!("{name} is registered"))
            .schema()
    }

    /// Compares the schema of `name` with its stored snapshot; `UPDATE_SNAPSHOTS=1` rewrites the snapshot.
    fn assert_schema_snapshot(name: &str) {
        let schema = schema_of(name);
        let path = snapshot_path(name);
        let mut text = serde_json::to_string_pretty(&schema).expect("a schema is JSON");
        text.push('\n');
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &text).unwrap();
            return;
        }
        let stored = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "no snapshot at {}; run with UPDATE_SNAPSHOTS=1 to create it",
                path.display()
            )
        });
        let stored: Value = serde_json::from_str(&stored).expect("the snapshot is JSON");
        assert_eq!(
            schema, stored,
            "the schema of {name} changed; review it and rerun with UPDATE_SNAPSHOTS=1"
        );
    }

    fn collect_widgets(value: &Value, found: &mut Vec<Value>) {
        match value {
            Value::Object(map) => {
                for (key, inner) in map {
                    if key == "x-widget" {
                        found.push(inner.clone());
                    } else {
                        collect_widgets(inner, found);
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect_widgets(item, found)),
            _ => {}
        }
    }

    #[test]
    fn every_document_has_a_2020_12_schema() {
        assert!(!documents().is_empty());
        for doc in documents() {
            let schema = doc.schema();
            let name = doc.name;
            assert!(
                schema["$schema"]
                    .as_str()
                    .is_some_and(|uri| uri.contains("2020-12")),
                "{name}: `$schema` must name draft 2020-12, got {:?}",
                schema["$schema"]
            );
            let kind_ok = match &schema["type"] {
                Value::String(kind) => kind == "object",
                // An optional document may allow null beside the object.
                Value::Array(kinds) => kinds.iter().any(|kind| kind == "object"),
                _ => false,
            };
            assert!(
                kind_ok,
                "{name}: the root must be an object, got {:?}",
                schema["type"]
            );
        }
    }

    macro_rules! stable_schema {
        ($($test:ident => $name:literal),* $(,)?) => {
            $(
                #[test]
                fn $test() {
                    assert_schema_snapshot($name);
                }
            )*
        };
    }

    stable_schema! {
        the_schema_of_email_is_stable => "email",
        the_schema_of_voice_is_stable => "voice",
        the_schema_of_calendar_is_stable => "calendar",
        the_schema_of_web_is_stable => "web",
        the_schema_of_browser_is_stable => "browser",
        the_schema_of_github_is_stable => "github",
        the_schema_of_council_is_stable => "council",
        the_schema_of_router_is_stable => "router",
        the_schema_of_models_is_stable => "models",
        the_schema_of_verify_is_stable => "verify",
        the_schema_of_keep_awake_is_stable => "keep_awake",
        the_schema_of_background_is_stable => "background",
        the_schema_of_machine_is_stable => "machine",
        the_schema_of_devtime_is_stable => "devtime",
        the_schema_of_autopilot_is_stable => "autopilot",
        the_schema_of_workflow_pins_is_stable => "workflow_pins",
    }

    /// One snapshot per production channel (`channels__telegram.json`); the test-only `fake` channel shares the type.
    #[test]
    fn the_schema_of_each_channel_is_stable() {
        for manifest in crate::channels::registry::PROD_MANIFESTS {
            assert_schema_snapshot(&format!("channels/{}", manifest.id));
        }
    }

    #[test]
    fn doc_comments_become_descriptions() {
        let schema = schema_of("models");
        let description = schema["properties"]["local_triage_model"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(
            !description.trim().is_empty(),
            "a documented field of `models` must carry its doc comment as `description`"
        );
        assert!(description.contains("local triage") || description.contains("message bodies"));
    }

    #[test]
    fn x_widget_values_are_a_closed_set() {
        let mut seen = 0;
        for doc in documents() {
            let mut found = Vec::new();
            collect_widgets(&doc.schema(), &mut found);
            for widget in found {
                let widget = widget.as_str().unwrap_or_else(|| {
                    panic!("{}: `x-widget` must be a string, got {widget}", doc.name)
                });
                assert!(
                    WIDGETS.contains(&widget),
                    "{}: unknown `x-widget` value {widget:?}; the set is {WIDGETS:?}",
                    doc.name
                );
                seen += 1;
            }
        }
        assert!(seen > 0, "at least one field is annotated with a widget");
    }
}
