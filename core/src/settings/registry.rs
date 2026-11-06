//! The document kinds the settings store knows: a trait that ties a Rust type to a name and a scope kind,
//! and the table of the documents: the 15 fixed ones plus one `channels/<id>` row per channel manifest.

use std::sync::LazyLock;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::decode::{self, Erased, Prepared};
use super::scope::Kind;
use crate::channels::registry::ChannelConfig;
use crate::config::{
    AutopilotRules, BrowserConfig, CalendarConfig, CouncilConfig, DevtimeConfig, EmailConfig,
    GithubConfig, ModelsConfig, VerifyConfig, VoiceConfig, WebConfig, validate_devtime,
    validate_rules,
};
use crate::engines::agy::AgyDoc;
use crate::machine_config as files;
use crate::models::route_doc::RouterDoc;
use crate::workflows::Pins;

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
    /// The JSON Schema (draft 2020-12) of the document type.
    schema_fn: fn() -> Value,
}

impl DocType {
    /// The JSON Schema of this document, with the `x-widget` hints the settings page renders from.
    pub fn schema(&self) -> Value {
        (self.schema_fn)()
    }

    /// A document whose name is only known at startup (`channels/<id>`, one per manifest). The caller leaks
    /// the strings once, at registry build, so the row stays `Copy` with `&'static str` fields.
    pub fn dynamic<T: Document + Default + schemars::JsonSchema>(
        name: &'static str,
        kind: Kind,
        origin_file: &'static str,
        takes_effect: &'static str,
    ) -> DocType {
        DocType {
            name,
            kind,
            origin_file,
            takes_effect,
            prepare: decode::prepare::<T>,
            load: decode::load::<T>,
            default_json: default_of::<T>,
            schema_fn: schema_of::<T>,
        }
    }
}

fn schema_of<T: schemars::JsonSchema>() -> Value {
    // The default settings of schemars 1.x emit draft 2020-12.
    let root = schemars::generate::SchemaSettings::default()
        .into_generator()
        .into_root_schema_for::<T>();
    serde_json::to_value(root).unwrap_or(Value::Null)
}

fn default_of<T: Document + Default>() -> Option<Value> {
    serde_json::to_value(T::default()).ok()
}

fn no_default() -> Option<Value> {
    None
}

fn of<T: Document + Default + schemars::JsonSchema>(
    origin_file: &'static str,
    takes_effect: &'static str,
) -> DocType {
    DocType {
        name: T::NAME,
        kind: T::KIND,
        origin_file,
        takes_effect,
        prepare: decode::prepare::<T>,
        load: decode::load::<T>,
        default_json: default_of::<T>,
        schema_fn: schema_of::<T>,
    }
}

/// A document with no `Default` (an empty roster is a mistake, not a configuration).
fn of_optional<T: Document + schemars::JsonSchema>(
    origin_file: &'static str,
    takes_effect: &'static str,
) -> DocType {
    DocType {
        name: T::NAME,
        kind: T::KIND,
        origin_file,
        takes_effect,
        prepare: decode::prepare::<T>,
        load: decode::load::<T>,
        default_json: no_default,
        schema_fn: schema_of::<T>,
    }
}

static DOCUMENTS: LazyLock<Vec<DocType>> = LazyLock::new(|| {
    let mut docs = vec![
        of::<EmailConfig>(
            files::EMAIL_FILE,
            "when the daemon restarts — the mailbox is polled by a sidecar started with this file's contents",
        ),
        of::<VoiceConfig>(
            files::VOICE_FILE,
            "when the daemon restarts — the chords are registered with the operating system at startup",
        ),
        of::<CalendarConfig>(files::CALENDAR_FILE, "when the daemon restarts"),
        of::<WebConfig>(
            files::WEB_FILE,
            "when the daemon restarts — the search sidecar is started with this file's contents",
        ),
        of::<BrowserConfig>(files::BROWSER_FILE, "when the daemon restarts"),
        of::<GithubConfig>(
            files::GITHUB_FILE,
            "when the daemon restarts — and note that this file EXISTING is itself the pillar's opt-in",
        ),
        of_optional::<CouncilConfig>(files::COUNCIL_FILE, "when the daemon restarts"),
        of::<RouterDoc>(
            files::ROUTER_FILE,
            "when the daemon restarts — the router is read once at startup; an absent file leaves routing off",
        ),
        of::<AgyDoc>(
            files::AGY_FILE,
            "a chat reads the switch at once; the router drops agy at once but only gains it when the daemon restarts; a primary_runner of agy takes effect when the daemon restarts",
        ),
        of::<crate::accounts::settings::AccountPoolDoc>(
            "account_pool.yaml",
            "at once for the next chat turn or run launch",
        ),
        of::<ModelsConfig>(
            crate::config::MODELS_CONFIG_FILE,
            "the model picker refreshes at once; the routes themselves are built at startup and move when the daemon restarts",
        ),
        of::<VerifyConfig>(
            files::VERIFY_FILE,
            "on the next command for switching admission off and for its weights; everything else, including switching it on from off, when the daemon restarts",
        ),
        of::<DevtimeConfig>(
            files::DEVTIME_FILE,
            "when the daemon restarts — except that on a save or a reset the actions.* settings apply from the next ingest cycle, and the pending actions decided under other actions.* thresholds are voided at once (those the owner marked stay)",
        ),
        of::<AutopilotRules>(
            crate::project::state::AUTOPILOT_FILE,
            "at the next tick of the autopilot for that project",
        ),
        of::<Pins>(
            crate::project::state::PINS_FILE,
            "at the next run that resolves a workflow for that project",
        ),
    ];
    // One machine document per channel manifest, listed after the fixed machine documents.
    let machine_end = docs
        .iter()
        .take_while(|doc| doc.kind == Kind::Machine)
        .count();
    let channels: Vec<DocType> = crate::channels::registry::MANIFESTS
        .iter()
        .map(|manifest| {
            let name: &'static str =
                Box::leak(format!("channels/{}", manifest.id).into_boxed_str());
            let origin: &'static str =
                Box::leak(format!("{}.yaml", manifest.settings_file).into_boxed_str());
            DocType::dynamic::<ChannelConfig>(
                name,
                Kind::Machine,
                origin,
                "at once — the channel hub re-reads its configuration when a document is published",
            )
        })
        .collect();
    docs.splice(machine_end..machine_end, channels);
    docs
});

/// Every document kind, machine documents first, in the order the settings page lists them.
pub fn documents() -> &'static [DocType] {
    &DOCUMENTS
}

pub fn doc_type(name: &str) -> Option<&'static DocType> {
    documents().iter().find(|doc| doc.name == name)
}

impl Document for EmailConfig {
    const NAME: &'static str = "email";
    const KIND: Kind = Kind::Machine;
    fn finish(self) -> Result<Self, String> {
        Ok(self.validated())
    }
}

impl Document for VoiceConfig {
    const NAME: &'static str = "voice";
    const KIND: Kind = Kind::Machine;
    fn finish(self) -> Result<Self, String> {
        Ok(self.validated())
    }
}

impl Document for CalendarConfig {
    const NAME: &'static str = "calendar";
    const KIND: Kind = Kind::Machine;
}

impl Document for WebConfig {
    const NAME: &'static str = "web";
    const KIND: Kind = Kind::Machine;
}

impl Document for BrowserConfig {
    const NAME: &'static str = "browser";
    const KIND: Kind = Kind::Machine;
}

impl Document for GithubConfig {
    const NAME: &'static str = "github";
    const KIND: Kind = Kind::Machine;
}

impl Document for CouncilConfig {
    const NAME: &'static str = "council";
    const KIND: Kind = Kind::Machine;
    fn finish(self) -> Result<Self, String> {
        // The local seat is assumed available: whether this machine can serve it is a runtime fact, checked
        // when a council actually runs, not when a roster is saved.
        let faults = self.faults(true);
        if !faults.is_empty() {
            return Err(faults.join("; "));
        }
        Ok(self.validated())
    }
}

impl Document for RouterDoc {
    const NAME: &'static str = "router";
    const KIND: Kind = Kind::Machine;
    fn finish(self) -> Result<Self, String> {
        self.to_config().map(|config| Self::from_config(&config))
    }
}

impl Document for ModelsConfig {
    const NAME: &'static str = "models";
    const KIND: Kind = Kind::Machine;
    fn finish(self) -> Result<Self, String> {
        self.local_engine()
            .map(|_| ())
            .map_err(|refusal| refusal.message())?;
        Ok(self)
    }
}

impl Document for VerifyConfig {
    const NAME: &'static str = "verify";
    const KIND: Kind = Kind::Machine;
    fn finish(self) -> Result<Self, String> {
        self.check()?;
        Ok(self)
    }
}

impl Document for DevtimeConfig {
    const NAME: &'static str = "devtime";
    const KIND: Kind = Kind::Machine;
    fn finish(self) -> Result<Self, String> {
        validate_devtime(&self)?;
        Ok(self)
    }
}

impl Document for AutopilotRules {
    const NAME: &'static str = "autopilot";
    const KIND: Kind = Kind::Project;
    fn finish(self) -> Result<Self, String> {
        validate_rules(&self).map_err(|error| error.to_string())?;
        Ok(self)
    }
}

impl Document for Pins {
    const NAME: &'static str = "workflow_pins";
    const KIND: Kind = Kind::Project;
    fn finish(self) -> Result<Self, String> {
        self.check()?;
        Ok(self)
    }
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
        // 15 fixed documents plus one `channels/<id>` per manifest.
        let channels = crate::channels::registry::MANIFESTS.len();
        let docs = documents();
        assert_eq!(docs.len(), 15 + channels);
        let names: BTreeSet<_> = docs.iter().map(|doc| doc.name).collect();
        assert_eq!(names.len(), 15 + channels);
        assert_eq!(
            docs.iter().filter(|doc| doc.kind == Kind::Machine).count(),
            13 + channels
        );
        let project: Vec<_> = docs
            .iter()
            .filter(|doc| doc.kind == Kind::Project)
            .map(|doc| doc.name)
            .collect();
        assert_eq!(project, vec!["autopilot", "workflow_pins"]);
    }

    #[test]
    fn every_document_names_its_origin_file_and_when_it_takes_effect() {
        for doc in documents() {
            assert!(
                doc.origin_file.ends_with(".yaml"),
                "{}: {}",
                doc.name,
                doc.origin_file
            );
            assert!(!doc.takes_effect.is_empty(), "{}", doc.name);
        }
    }

    #[test]
    fn every_default_passes_its_own_validation() {
        let mut without_default = Vec::new();
        for doc in documents() {
            match (doc.default_json)() {
                Some(body) => {
                    (doc.prepare)(&body).unwrap_or_else(|why| {
                        panic!("the default of {} is refused: {why}", doc.name)
                    });
                }
                None => without_default.push(doc.name),
            }
        }
        assert_eq!(
            without_default,
            vec!["council"],
            "only the council has no default"
        );
    }

    #[test]
    fn preparing_a_canonical_body_again_changes_nothing() {
        for doc in documents() {
            let Some(body) = (doc.default_json)() else {
                continue;
            };
            let first = prepared_text(doc.name, &body).unwrap();
            let again: Value = serde_json::from_str(&first).unwrap();
            assert_eq!(
                prepared_text(doc.name, &again).unwrap(),
                first,
                "{}",
                doc.name
            );
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

        let loaded =
            (doc.load)(&body.to_string()).expect("a stored row with an extra key still loads");
        let config = loaded.downcast::<VerifyConfig>().expect("a VerifyConfig");
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
        assert!(
            prepared_text(
                "workflow_pins",
                &json!({"workflows": [
                    {"name": "../x", "version": "v1", "origin": "o", "hash": "h"}
                ]})
            )
            .unwrap_err()
            .contains("not a usable workflow name")
        );
    }

    /// Ported from `machine_config`'s models door when that table was deleted. `local_engine`'s refusals
    /// all deserialise perfectly, so a door that stopped at serde would accept the write and leave the
    /// daemon to refuse the same document on its next start.
    #[test]
    fn the_models_document_refuses_a_local_engine_the_daemon_would_refuse_at_startup() {
        let models = |engine: Option<&str>, base_url: Option<&str>| {
            let mut value = serde_json::to_value(ModelsConfig::default()).unwrap();
            if let Some(engine) = engine {
                value["local_engine"] = json!(engine);
            }
            if let Some(base_url) = base_url {
                value["local_base_url"] = json!(base_url);
            }
            value
        };

        let why = prepared_text("models", &models(Some("openai_compatible"), None)).unwrap_err();
        assert!(
            why.contains(&crate::config::LocalEngineRefusal::NoBaseUrl.message()),
            "the document says what startup would have said: {why}"
        );
        assert!(
            prepared_text("models", &models(Some("llamafile"), None)).is_err(),
            "an engine this daemon does not serve is refused"
        );
        assert!(
            prepared_text(
                "models",
                &models(
                    Some("openai_compatible"),
                    Some("http://127.0.0.1.example.com/v1")
                )
            )
            .is_err(),
            "an address that only begins like the loopback is refused"
        );
        assert!(
            prepared_text("models", &models(None, None)).is_ok(),
            "a document that names no engine still passes"
        );
        assert!(
            prepared_text("models", &models(Some("ollama"), None)).is_ok(),
            "naming the engine it already used changes nothing"
        );
    }

    /// Ported from `machine_config`'s channel door when that table was deleted: `stream_edits` is a bool
    /// or the write is refused, and absent means on.
    #[test]
    fn the_channel_document_takes_stream_edits_as_a_bool_and_absent_means_on() {
        let stored = |body: &Value| -> ChannelConfig {
            let text = prepared_text("channels/telegram", body).expect("a valid channel document");
            serde_json::from_str(&text).unwrap()
        };
        assert!(!stored(&json!({"stream_edits": false})).stream_edits());
        assert!(stored(&json!({"doctrine": "x"})).stream_edits());
        assert!(prepared_text("channels/telegram", &json!({"stream_edits": "maybe"})).is_err());
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
        assert_eq!(
            doc_type("autopilot").map(|doc| doc.kind),
            Some(Kind::Project)
        );
        assert!(doc_type("nope").is_none());
    }

    #[test]
    fn every_manifest_has_a_channels_document() {
        for manifest in crate::channels::registry::MANIFESTS {
            let name = format!("channels/{}", manifest.id);
            let doc = doc_type(&name).unwrap_or_else(|| panic!("{name} is registered"));
            assert_eq!(doc.name, name);
            assert_eq!(doc.kind, Kind::Machine);
            assert!(documents().iter().any(|doc| doc.name == name));
        }
    }

    #[tokio::test]
    async fn snapshot_entry_downcasts_a_dynamic_document() {
        use crate::channels::registry::ChannelConfig;
        use crate::settings::Scope;
        // `Home::new()` first and no `settings::testing::install`: the chunk-3 discipline for the global snapshot.
        let _home = crate::project::rules::testing::Home::new();
        let pool = crate::testdb::fresh_pool().await;
        crate::settings::put(
            &pool,
            &Scope::Machine,
            "channels/telegram",
            &json!({"doctrine": "be brief", "stream_edits": false}),
            "test",
            None,
        )
        .await
        .expect("channels/telegram is a registered document");

        let snapshot = crate::settings::global().snapshot();
        // Contract request: the plan types `entry` as returning `&dyn Any`, but `Snapshot::entry` already
        // returns `&Entry`. These tests use the new `entry_as::<T>` and `get_named::<T>` only.
        let config = snapshot
            .entry_as::<ChannelConfig>(&Scope::Machine, "channels/telegram")
            .expect("the published document is in the snapshot");
        assert_eq!(config.doctrine.as_deref(), Some("be brief"));
        assert_eq!(config.stream_edits, Some(false));
        let named: Option<ChannelConfig> =
            crate::settings::get_named(&Scope::Machine, "channels/telegram");
        assert_eq!(named.as_ref(), Some(config));

        assert!(
            snapshot
                .entry_as::<ChannelConfig>(&Scope::Machine, "channels/nope")
                .is_none()
        );
        assert!(
            snapshot
                .entry_as::<VerifyConfig>(&Scope::Machine, "channels/telegram")
                .is_none(),
            "a downcast to the wrong type finds nothing"
        );
    }
}
