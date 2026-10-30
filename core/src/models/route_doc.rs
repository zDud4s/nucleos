//! `router.yaml` as a settings document. `RouterConfig` holds enums and `&'static str` keys and cannot be
//! serialised; `RouterDoc` holds the same facts as plain strings, and will own the validation that
//! `route_advice::parse_config` does inline (RED stub: behaviour not written yet).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::route_advice::RouterConfig;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RouterDoc {
    /// `off`, `shadow` or `apply`.
    pub mode: String,
    pub url: String,
    pub timeout_ms: u64,
    pub runners: Vec<String>,
    pub models: BTreeMap<String, Vec<String>>,
    /// Surface name (`runs`, `team`, `council`, `recruit`) to mode.
    pub surfaces: BTreeMap<String, String>,
}

impl Default for RouterDoc {
    /// The router off, at the default address.
    fn default() -> Self {
        todo!("RED stub: RouterDoc::default")
    }
}

impl RouterDoc {
    /// The configuration this document names, or why it is refused.
    pub fn to_config(&self) -> Result<RouterConfig, String> {
        todo!("RED stub: RouterDoc::to_config")
    }

    /// The document that names `config`.
    pub fn from_config(_config: &RouterConfig) -> Self {
        todo!("RED stub: RouterDoc::from_config")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::models::route_advice::{DEFAULT_TIMEOUT_MS, DEFAULT_URL, parse_config};

    fn shadow_doc() -> RouterDoc {
        RouterDoc {
            mode: "shadow".to_owned(),
            url: DEFAULT_URL.to_owned(),
            timeout_ms: 5,
            runners: vec!["codex".to_owned()],
            models: BTreeMap::from([("claude".to_owned(), vec!["opus*".to_owned()])]),
            surfaces: BTreeMap::from([("runs".to_owned(), "apply".to_owned())]),
        }
    }

    #[test]
    fn the_default_doc_is_the_router_off() {
        let doc = RouterDoc::default();
        assert_eq!(doc.mode, "off");
        assert_eq!(doc.url, DEFAULT_URL);
        assert_eq!(doc.timeout_ms, DEFAULT_TIMEOUT_MS);
        assert_eq!(doc.to_config(), Ok(RouterConfig::off()));
    }

    #[test]
    fn a_doc_means_what_the_yaml_file_meant() {
        let text = "mode: shadow\ntimeout_ms: 5\nrunners: [codex]\nmodels:\n  claude: [\"opus*\"]\nsurfaces:\n  runs: apply\n";
        let from_file = parse_config(text).expect("the file is valid");
        assert_eq!(shadow_doc().to_config(), Ok(from_file));
    }

    #[test]
    fn the_timeout_is_clamped() {
        let config = shadow_doc().to_config().unwrap();
        assert_eq!(config.timeout_ms, 100, "5 ms is raised to the minimum");
        let slow = RouterDoc { timeout_ms: 999_999, ..shadow_doc() };
        assert_eq!(slow.to_config().unwrap().timeout_ms, 10_000);
    }

    #[test]
    fn from_config_inverts_to_config() {
        let config = shadow_doc().to_config().unwrap();
        let doc = RouterDoc::from_config(&config);
        assert_eq!(doc.mode, "shadow");
        assert_eq!(doc.timeout_ms, 100);
        assert_eq!(doc.surfaces.get("runs").map(String::as_str), Some("apply"));
        assert_eq!(doc.to_config(), Ok(config));
    }

    #[test]
    fn a_router_off_this_machine_is_refused() {
        let doc = RouterDoc { url: "http://example.com:18733".to_owned(), ..RouterDoc::default() };
        let why = doc.to_config().unwrap_err();
        assert!(why.contains("only loopback is allowed"), "{why}");
    }

    #[test]
    fn an_unknown_mode_or_surface_is_refused() {
        let bad_mode = RouterDoc { mode: "sometimes".to_owned(), ..RouterDoc::default() };
        assert!(bad_mode.to_config().unwrap_err().contains("unknown router mode"));
        let bad_surface = RouterDoc {
            surfaces: BTreeMap::from([("kitchen".to_owned(), "apply".to_owned())]),
            ..RouterDoc::default()
        };
        assert!(bad_surface.to_config().unwrap_err().contains("unknown router surface kitchen"));
        let bad_surface_mode = RouterDoc {
            surfaces: BTreeMap::from([("runs".to_owned(), "maybe".to_owned())]),
            ..RouterDoc::default()
        };
        assert!(bad_surface_mode.to_config().unwrap_err().contains("for surface runs"));
    }

    #[test]
    fn a_mode_with_spaces_around_it_is_trimmed() {
        let doc = RouterDoc { mode: " shadow ".to_owned(), ..RouterDoc::default() };
        assert_eq!(doc.to_config().unwrap().mode.as_str(), "shadow");
    }

    #[test]
    fn an_unknown_key_is_refused_when_a_body_is_read_strictly() {
        let body = serde_json::json!({"mode": "off", "colour": "blue"});
        let error = serde_json::from_value::<RouterDoc>(body).unwrap_err().to_string();
        assert!(error.contains("unknown field"), "{error}");
    }
}
