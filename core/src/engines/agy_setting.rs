//! The `agy` opt-in as a settings document. Off until the owner enables it; enabling is refused unless the
//! stored acknowledgement names the current warning, whichever door the write came through.

use serde::{Deserialize, Serialize};

use super::agy::WARNING_VERSION;
use crate::settings::{Document, Kind};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AgyDoc {
    /// Whether the agy engine may be used at all.
    pub enabled: bool,
    /// The version of the warning the owner accepted when enabling.
    pub acknowledged_warning_version: Option<u32>,
}

impl Document for AgyDoc {
    const NAME: &'static str = "agy";
    const KIND: Kind = Kind::Machine;
    fn finish(self) -> Result<Self, String> {
        if self.enabled && self.acknowledged_warning_version != Some(WARNING_VERSION) {
            return Err(format!(
                "the agy engine cannot be enabled without acknowledging warning version {WARNING_VERSION}"
            ));
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::agy::{WARNING, WARNING_VERSION};
    use crate::settings::Document;

    #[test]
    fn agy_doc_default_is_off_and_unacknowledged() {
        let doc = AgyDoc::default();
        assert!(!doc.enabled, "the agy engine is off until the owner opts in");
        assert_eq!(doc.acknowledged_warning_version, None);
        assert_eq!(AgyDoc::NAME, "agy");

        // An empty stored body decodes to the same default, and a stray field is refused.
        let decoded: AgyDoc = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(decoded, doc);
        assert!(
            serde_json::from_value::<AgyDoc>(serde_json::json!({ "enabeld": true })).is_err(),
            "deny_unknown_fields: a misspelt field must not be dropped silently"
        );
    }

    #[test]
    fn agy_doc_finish_rejects_enabled_without_current_warning_version() {
        for acknowledged in [None, Some(0), Some(WARNING_VERSION + 1)] {
            let doc = AgyDoc {
                enabled: true,
                acknowledged_warning_version: acknowledged,
            };
            let refusal = doc.finish().expect_err(&format!(
                "enabled with acknowledgement {acknowledged:?} must be refused"
            ));
            assert!(!refusal.is_empty());
        }

        let acknowledged = AgyDoc {
            enabled: true,
            acknowledged_warning_version: Some(WARNING_VERSION),
        };
        assert_eq!(acknowledged.clone().finish().unwrap(), acknowledged);

        // Off needs no acknowledgement, current or not.
        for acknowledged in [None, Some(0), Some(WARNING_VERSION)] {
            let doc = AgyDoc {
                enabled: false,
                acknowledged_warning_version: acknowledged,
            };
            assert!(doc.finish().is_ok(), "disabled with {acknowledged:?}");
        }
    }

    /// The registry snapshot of the document's schema. The settings schema tests only list their
    /// documents by name, so the agy one is pinned here, with the same `UPDATE_SNAPSHOTS=1` rule.
    #[test]
    fn agy_doc_snapshot_matches() {
        let schema = crate::settings::doc_type("agy")
            .expect("agy is registered")
            .schema();
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("settings")
            .join("snapshots")
            .join("agy.json");
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
        let stored: serde_json::Value =
            serde_json::from_str(&stored).expect("the snapshot is JSON");
        assert_eq!(
            schema, stored,
            "the schema of agy changed; review it and rerun with UPDATE_SNAPSHOTS=1"
        );
    }

    #[test]
    fn agy_warning_mentions_google_faq_terms_and_suspension() {
        assert!(WARNING.contains("Google"));
        assert!(WARNING.contains("FAQ"));
        assert!(WARNING.contains("Terms"));
        assert!(WARNING.contains("suspended"));
        assert!(WARNING.contains("not supported"));
        const { assert!(WARNING_VERSION >= 1) };
    }
}
