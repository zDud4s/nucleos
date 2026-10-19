//! Channel manifests, per-channel configuration and presence (Anexo D, D.4). P0-2b holds the
//! static table, `ChannelConfig`, the `RoutingState` type and a placeholder `Presence`; P0-8 fills
//! `Presence`, A2 owns the `todo!()` bodies.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::state::AppState;

/// What a channel kind compiles in (D.4.1). Everything that must be `&'static str` elsewhere is
/// written here once.
pub struct Manifest {
    /// `[a-z][a-z0-9-]{0,31}`; today `id == kind`.
    pub id: &'static str,
    pub kind: &'static str,
    /// The name `sidecar::binary` resolves.
    pub binary: &'static str,
    /// Display name for the shell.
    pub label: &'static str,
    pub secrets: &'static [SecretSpec],
    /// Relative to the machine-config root, without the extension.
    pub settings_file: &'static str,
    /// The machine-config area the shell links to.
    pub settings_area: &'static str,
    /// The limit the media handler enforces on `POST /channels/{channel}/media`.
    pub max_media_bytes: usize,
    /// `"<id>_sidecar"`, the health row name.
    pub health_name: &'static str,
    /// `"channel:<id>"`, the wire spelling of `Origin::Channel(id)`.
    pub origin_wire: &'static str,
    /// Extra secret shapes for `governance::redact`.
    pub redact_patterns: &'static [&'static str],
}

pub struct SecretSpec {
    /// Credential-store key.
    pub key: &'static str,
    /// Environment variable the adapter reads.
    pub env: &'static str,
    pub required: bool,
}

pub const TELEGRAM: Manifest = Manifest {
    id: "telegram",
    kind: "telegram",
    binary: crate::sidecar::TELEGRAM,
    label: "Telegram",
    secrets: &[SecretSpec {
        key: "telegram-token",
        env: "TELEGRAM_BOT_TOKEN",
        required: true,
    }],
    settings_file: "channels/telegram",
    settings_area: "channels/telegram",
    max_media_bytes: 20_971_520,
    health_name: "telegram_sidecar",
    origin_wire: "channel:telegram",
    redact_patterns: &[],
};

/// The manifest the contract scenarios and the isolation tests use. Never part of production.
#[cfg(test)]
pub const FAKE: Manifest = Manifest {
    id: "fake",
    kind: "fake",
    binary: "fake",
    label: "Fake",
    secrets: &[],
    settings_file: "channels/fake",
    settings_area: "channels/fake",
    max_media_bytes: 1_048_576,
    health_name: "fake_sidecar",
    origin_wire: "channel:fake",
    redact_patterns: &[],
};

/// Production manifests. Deliberately not behind `cfg`, so a test can assert `fake` is absent.
pub const PROD_MANIFESTS: &[&Manifest] = &[&TELEGRAM];

#[cfg(not(test))]
pub const MANIFESTS: &[&Manifest] = PROD_MANIFESTS;
/// Production manifests plus the fake one under test.
#[cfg(test)]
pub const MANIFESTS: &[&Manifest] = &[&TELEGRAM, &FAKE];

pub fn all() -> &'static [&'static Manifest] {
    MANIFESTS
}

pub fn by_id(id: &str) -> Option<&'static Manifest> {
    MANIFESTS.iter().copied().find(|m| m.id == id)
}

/// `~/.nucleos/channels/<id>.yaml`. Absent or malformed file = default, never fatal.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct ChannelConfig {
    #[serde(default)]
    pub doctrine: Option<String>,
    /// Project id to `conversation_ref` (a thread in Telegram).
    #[serde(default)]
    pub projects: BTreeMap<String, String>,
}

/// Reads every channel's yaml. P0 returns the empty map (the inert hub); A2 reads the files.
pub fn load_configs(_machine_config_root: Option<&Path>) -> HashMap<&'static str, ChannelConfig> {
    HashMap::new()
}

/// Reverse lookup: the project mapped to a conversation, if any.
pub fn project_for_conversation(config: &ChannelConfig, conversation_ref: &str) -> Option<String> {
    config
        .projects
        .iter()
        .find(|(_, conv)| conv.as_str() == conversation_ref)
        .map(|(project, _)| project.clone())
}

/// What the hub knows about each adapter's connection. Memory only. P0-8 fills it.
#[derive(Default)]
pub struct Presence;

/// Where notification routing stands (Anexo E, E.2 `routing.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingState {
    Ok,
    NoPrimary,
    PrimaryNotConfigured,
    PrimaryWithoutHome,
}

/// Launches every channel whose required secrets are present. A2; phase 1 never calls it.
pub async fn launch_all(_state: &AppState, _sidecars_wanted: bool) {
    todo!()
}

/// The environment of one adapter process. A2.
pub fn env_for(
    _manifest: &Manifest,
    _daemon_url: &str,
    _channel_token: &str,
    _secrets: &[(&'static str, String)],
) -> Vec<(String, String)> {
    todo!()
}

/// One health row per manifest. A2.
pub async fn health_rows(_state: &AppState) -> Vec<crate::health::SubsystemReadout> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `[a-z][a-z0-9-]{0,31}`; local until `contract::valid_channel_id` lands in P0-3.
    fn valid_id(id: &str) -> bool {
        let mut chars = id.chars();
        matches!(chars.next(), Some('a'..='z'))
            && id.len() <= 32
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    }

    #[test]
    fn manifest_ids_are_valid_and_not_reserved() {
        for m in MANIFESTS {
            assert!(valid_id(m.id), "bad channel id {:?}", m.id);
            for reserved in ["email", "council", "primary", "api", "team", "chat"] {
                assert_ne!(m.id, reserved);
            }
        }
    }

    #[test]
    fn by_id_finds_telegram() {
        assert!(by_id("telegram").is_some());
        assert!(by_id("nope").is_none());
    }

    #[test]
    fn by_id_finds_fake_in_tests() {
        assert!(by_id("fake").is_some());
    }

    #[test]
    fn prod_manifests_contain_telegram_and_never_fake() {
        assert!(PROD_MANIFESTS.iter().any(|m| m.id == "telegram"));
        assert!(PROD_MANIFESTS.iter().all(|m| m.id != "fake"));
    }
}
