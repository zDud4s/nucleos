//! Channel manifests, per-channel configuration and presence (Anexo D, D.4). P0-2b holds the
//! static table and `ChannelConfig`; P0-8 adds `Presence`, `record_hello`, `capabilities`, the
//! `GET /channels` readout (`list`, `channel_state`) and `RoutingState`; A2 owns the `todo!()`
//! bodies (`launch_all`, `env_for`, `health_rows`).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::SqlitePool;

use super::contract;
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
    // `pillars` is a private module of `config`; its glob re-export carries the `pub(crate)` fn.
    #[serde(default, deserialize_with = "crate::config::deserialize_blank_as_none")]
    pub doctrine: Option<String>,
    /// Project id to `conversation_ref` (a thread in Telegram).
    #[serde(default)]
    pub projects: BTreeMap<String, String>,
}

/// Reads one channel's `<root>/<settings_file>.yaml`. Absent, unreadable or malformed = default,
/// never fatal: a typo must cost fidelity, not stop the daemon.
pub fn load_config(machine_config_root: &Path, manifest: &Manifest) -> ChannelConfig {
    let path = machine_config_root.join(format!("{}.yaml", manifest.settings_file));
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_yaml::from_str(&text).ok())
        .unwrap_or_default()
}

/// Reads every channel's yaml. `None` (no machine-config root) gives the empty map, the inert hub.
pub fn load_configs(machine_config_root: Option<&Path>) -> HashMap<&'static str, ChannelConfig> {
    let Some(root) = machine_config_root else {
        return HashMap::new();
    };
    MANIFESTS
        .iter()
        .map(|manifest| (manifest.id, load_config(root, manifest)))
        .collect()
}

/// Reverse lookup: the project mapped to a conversation, if any.
pub fn project_for_conversation(config: &ChannelConfig, conversation_ref: &str) -> Option<String> {
    config
        .projects
        .iter()
        .find(|(_, conv)| conv.as_str() == conversation_ref)
        .map(|(project, _)| project.clone())
}

/// Online = the channel's last request is younger than this (Anexo E, E.2). The kit always keeps a
/// 30 s long-poll open, so 45 s tolerates one slow round trip.
pub const ONLINE_WINDOW: chrono::Duration = chrono::Duration::seconds(45);

/// What the hub last learned from one adapter's `hello` and requests.
#[derive(Debug, Clone)]
pub struct Seen {
    pub hello_at: DateTime<Utc>,
    /// The last request made with the channel's key (any route). Online = within `ONLINE_WINDOW`.
    pub last_poll_at: Option<DateTime<Utc>>,
    pub capabilities: contract::Capabilities,
    pub contract: String,
    /// Set when the last `hello` was refused with 409; shown by health.
    pub incompatible: Option<String>,
    /// When `incompatible` was set (E.2 `since` for the `incompatible` state).
    pub incompatible_at: Option<DateTime<Utc>>,
}

/// In-memory counters since the daemon started (E.2 `counted_since`): never persisted.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Drops {
    pub unknown_sender: u64,
    pub pairing_failures: u64,
    pub counted_since: DateTime<Utc>,
}

#[derive(Default)]
struct PresenceInner {
    seen: HashMap<&'static str, Seen>,
    launched: HashMap<&'static str, DateTime<Utc>>,
    /// `(unknown_sender, pairing_failures)` per channel.
    drops: HashMap<&'static str, (u64, u64)>,
    /// Cache of the required secrets still missing, per channel (`set_secrets_present`).
    missing: HashMap<&'static str, Vec<&'static str>>,
}

/// What the hub knows about each adapter's connection. Memory only: a row per long-poll would be
/// the hub's biggest source of idle writes. Every method takes a short `Mutex` and does no I/O.
pub struct Presence {
    inner: Mutex<PresenceInner>,
    counted_since: DateTime<Utc>,
}

impl Default for Presence {
    fn default() -> Self {
        Self {
            inner: Mutex::default(),
            counted_since: Utc::now(),
        }
    }
}

impl Presence {
    fn lock(&self) -> MutexGuard<'_, PresenceInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Called by the channel-key middleware on EVERY authenticated request of a channel scope.
    /// A channel that never said `hello` has nothing to keep: the call is a no-op for it.
    pub fn touch_poll(&self, channel: &'static str, now: DateTime<Utc>) {
        if let Some(seen) = self.lock().seen.get_mut(channel) {
            seen.last_poll_at = Some(now);
        }
    }

    pub fn seen(&self, channel: &str) -> Option<Seen> {
        self.lock().seen.get(channel).cloned()
    }

    /// When the supervisor launched the channel's process in this daemon life.
    pub fn launched_at(&self, channel: &str) -> Option<DateTime<Utc>> {
        self.lock().launched.get(channel).copied()
    }

    pub fn set_launched(&self, channel: &'static str, at: DateTime<Utc>) {
        self.lock().launched.insert(channel, at);
    }

    pub fn drop_unknown_sender(&self, channel: &'static str) {
        self.lock().drops.entry(channel).or_default().0 += 1;
    }

    pub fn drop_pairing_failure(&self, channel: &'static str) {
        self.lock().drops.entry(channel).or_default().1 += 1;
    }

    pub fn drops(&self, channel: &str) -> Drops {
        let (unknown_sender, pairing_failures) =
            self.lock().drops.get(channel).copied().unwrap_or_default();
        Drops {
            unknown_sender,
            pairing_failures,
            counted_since: self.counted_since,
        }
    }

    /// Caches which required secrets are missing; `configured` is `missing.is_empty()`.
    pub fn set_secrets_present(&self, channel: &'static str, missing: Vec<&'static str>) {
        self.lock().missing.insert(channel, missing);
    }

    /// `(configured, missing_secrets)` from the cache. An uncached channel counts as not
    /// configured, listing its required keys as missing.
    fn secrets_status(&self, manifest: &Manifest) -> (bool, Vec<&'static str>) {
        match self.lock().missing.get(manifest.id) {
            Some(missing) => (missing.is_empty(), missing.clone()),
            None => (
                false,
                manifest
                    .secrets
                    .iter()
                    .filter(|s| s.required)
                    .map(|s| s.key)
                    .collect(),
            ),
        }
    }

    /// Live = launched in this daemon life, or an accepted `hello`; an incompatible one does not
    /// count.
    pub fn is_live(&self, channel: &str) -> bool {
        self.launched_at(channel).is_some()
            || self.seen(channel).is_some_and(|s| s.incompatible.is_none())
    }
}

/// True when some channel has all its required secrets (per the cache). Always false in phase 1.
pub fn channel_instances_exist(state: &AppState) -> bool {
    MANIFESTS
        .iter()
        .any(|m| state.channels.presence.secrets_status(m).0)
}

#[derive(Debug)]
pub enum HelloError {
    Incompatible { adapter: String },
    KindMismatch,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for HelloError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// `PUT /channels/{channel}/hello` minus the HTTP: checks the contract version, writes
/// `channel_presence`, updates `Presence` and returns every leased outbox row to `pending`.
pub async fn record_hello(
    state: &AppState,
    channel: &'static str,
    hello: &contract::Hello,
    now: DateTime<Utc>,
) -> Result<contract::HelloReply, HelloError> {
    let manifest = by_id(channel).ok_or(HelloError::KindMismatch)?;
    if hello.kind != manifest.kind {
        return Err(HelloError::KindMismatch);
    }
    let presence = &state.channels.presence;
    if !contract::compatible(&hello.version) {
        let mut inner = presence.lock();
        let seen = inner.seen.entry(channel).or_insert_with(|| Seen {
            hello_at: now,
            last_poll_at: None,
            capabilities: hello.capabilities.clone(),
            contract: hello.version.clone(),
            incompatible: None,
            incompatible_at: None,
        });
        seen.contract = hello.version.clone();
        seen.last_poll_at = Some(now);
        seen.incompatible = Some(hello.version.clone());
        seen.incompatible_at = Some(now);
        return Err(HelloError::Incompatible {
            adapter: hello.version.clone(),
        });
    }
    let caps = serde_json::to_string(&hello.capabilities)
        .map_err(|e| HelloError::Db(sqlx::Error::Encode(Box::new(e))))?;
    sqlx::query(
        "INSERT INTO channel_presence (channel, kind, contract, capabilities, hello_at) \
         VALUES (?, ?, ?, ?, ?) ON CONFLICT(channel) DO UPDATE SET kind = excluded.kind, \
         contract = excluded.contract, capabilities = excluded.capabilities, \
         hello_at = excluded.hello_at",
    )
    .bind(channel)
    .bind(manifest.kind)
    .bind(&hello.version)
    .bind(&caps)
    .bind(stamp(now))
    .execute(&state.pool)
    .await?;
    super::outbox::release_all(&state.pool, channel).await?;
    presence.lock().seen.insert(
        channel,
        Seen {
            hello_at: now,
            last_poll_at: Some(now),
            capabilities: hello.capabilities.clone(),
            contract: hello.version.clone(),
            incompatible: None,
            incompatible_at: None,
        },
    );
    Ok(contract::HelloReply {
        contract_version: contract::CONTRACT_VERSION,
    })
}

/// Last known capabilities: memory, then `channel_presence`, then everything off with 4096.
pub async fn capabilities(state: &AppState, channel: &str) -> contract::Capabilities {
    if let Some(seen) = state.channels.presence.seen(channel)
        && seen.incompatible.is_none()
    {
        return seen.capabilities;
    }
    let stored: Option<String> =
        sqlx::query_scalar("SELECT capabilities FROM channel_presence WHERE channel = ?")
            .bind(channel)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();
    stored
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// Where notification routing stands (Anexo E, E.2 `routing.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingState {
    Ok,
    NoPrimary,
    PrimaryNotConfigured,
    PrimaryWithoutHome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelState {
    NotConfigured,
    Starting,
    Online,
    Offline,
    Incompatible,
}

/// Pure, from memory (no query). A channel never launched and never seen is `NotConfigured`
/// (configured channels are launched); `list` also forces that state when a secret is missing.
pub fn channel_state(
    _manifest: &Manifest,
    seen: Option<&Seen>,
    launched_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> (ChannelState, Option<DateTime<Utc>>) {
    match seen {
        Some(s) if s.incompatible.is_some() => (ChannelState::Incompatible, s.incompatible_at),
        Some(s) => {
            let last = s.last_poll_at.unwrap_or(s.hello_at);
            if now - last <= ONLINE_WINDOW {
                (ChannelState::Online, Some(s.hello_at))
            } else {
                (ChannelState::Offline, Some(last))
            }
        }
        None => match launched_at {
            Some(at) => (ChannelState::Starting, Some(at)),
            None => (ChannelState::NotConfigured, None),
        },
    }
}

/// Folds the supervisor's view of the adapter process into the E.2 state: a process that died
/// (`Restarting`) or never started (`FailedToSpawn`) is `Offline` even while its last poll is
/// still inside the online window. A `Starting` channel is only demoted by `FailedToSpawn`: a
/// `Restarting` entry before the first hello is just the spawn in progress. `None` (nobody
/// supervises it, e.g. a non-primary daemon) leaves the state alone. `since` becomes `now` only
/// when the state actually flips.
fn apply_liveness(
    (state, since): (ChannelState, Option<DateTime<Utc>>),
    liveness: Option<crate::sidecar::Liveness>,
    now: DateTime<Utc>,
) -> (ChannelState, Option<DateTime<Utc>>) {
    use crate::sidecar::Liveness;
    let dead = matches!(
        (state, liveness),
        (
            ChannelState::Online,
            Some(Liveness::Restarting | Liveness::FailedToSpawn(_))
        ) | (ChannelState::Starting, Some(Liveness::FailedToSpawn(_)))
    );
    if dead {
        (ChannelState::Offline, Some(now))
    } else {
        (state, since)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AdapterInfo {
    pub contract_version: String,
}

#[derive(Debug, serde::Serialize)]
pub struct HomeReadout {
    pub conversation_ref: String,
    pub kind: contract::ConversationKind,
    pub parent: Option<String>,
    pub sender_ref: String,
}

#[derive(Debug, serde::Serialize)]
pub struct IdentityCounts {
    pub senders: u32,
    pub conversations: u32,
}

#[derive(Debug, serde::Serialize)]
pub struct PairingReadout {
    pub expires_at: DateTime<Utc>,
    pub attempts_left: u32,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct OutboxCounts {
    pub pending: u32,
    pub leased: u32,
    pub failed: u32,
    pub oldest_due_at: Option<DateTime<Utc>>,
}

#[derive(Debug, serde::Serialize)]
pub struct ChannelStatus {
    pub id: &'static str,
    pub kind: &'static str,
    pub label: &'static str,
    pub health_row: &'static str,
    pub configured: bool,
    pub missing_secrets: Vec<&'static str>,
    pub settings_area: &'static str,
    pub state: ChannelState,
    /// `None` only in `NotConfigured`.
    pub since: Option<DateTime<Utc>>,
    pub adapter: Option<AdapterInfo>,
    pub capabilities: Option<contract::Capabilities>,
    pub home: Option<HomeReadout>,
    pub identities: IdentityCounts,
    /// The active code's deadline and tries left; never the code itself.
    pub pairing: Option<PairingReadout>,
    pub outbox: OutboxCounts,
    pub drops: Drops,
}

#[derive(Debug, serde::Serialize)]
pub struct RoutingReadout {
    pub state: RoutingState,
}

/// `GET /channels` body (Anexo E, E.2).
#[derive(Debug, serde::Serialize)]
pub struct ChannelsReadout {
    pub contract_version: &'static str,
    pub primary: Option<&'static str>,
    pub routing: RoutingReadout,
    pub channels: Vec<ChannelStatus>,
}

async fn home_of(pool: &SqlitePool, channel: &str) -> sqlx::Result<Option<HomeReadout>> {
    Ok(super::access::home_identity(pool, channel)
        .await?
        .map(|home| HomeReadout {
            conversation_ref: home.conversation_ref,
            kind: home.conversation_kind,
            parent: home.parent,
            sender_ref: home.sender_ref,
        }))
}

async fn identity_counts(pool: &SqlitePool, channel: &str) -> sqlx::Result<IdentityCounts> {
    let (senders, conversations) = super::access::identity_counts(pool, channel).await?;
    Ok(IdentityCounts {
        senders,
        conversations,
    })
}

/// The active code's deadline, from the pairing status (the one place that reads the table).
async fn pairing_of(
    pool: &SqlitePool,
    channel: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<Option<PairingReadout>> {
    Ok(
        match super::access::pairing_status(pool, channel, now).await? {
            super::access::PairingStatus::Waiting {
                expires_at,
                attempts_left,
            } => Some(PairingReadout {
                expires_at,
                attempts_left,
            }),
            _ => None,
        },
    )
}

/// Whether every required secret of `id` is present, from the in-memory cache alone (no query).
/// An unknown or never-checked channel is not configured.
pub(crate) fn is_configured(state: &AppState, id: &str) -> bool {
    by_id(id).is_some_and(|manifest| state.channels.presence.secrets_status(manifest).0)
}

/// `GET /channels`: one status per compiled manifest. Presence, cache and drops come from memory;
/// SQLite answers the small per-channel aggregates.
pub async fn list(state: &AppState) -> sqlx::Result<ChannelsReadout> {
    let now = Utc::now();
    let pool = &state.pool;
    let presence = &state.channels.presence;
    let primary_raw: Option<String> =
        sqlx::query_scalar("SELECT primary_channel FROM channels_global WHERE id = 1")
            .fetch_optional(pool)
            .await?
            .flatten();
    let primary = primary_raw.as_deref().and_then(by_id).map(|m| m.id);
    let mut channels = Vec::with_capacity(MANIFESTS.len());
    for manifest in MANIFESTS {
        let id = manifest.id;
        let (configured, missing_secrets) = presence.secrets_status(manifest);
        let seen = presence.seen(id);
        let (state_now, since) = if configured {
            let (state_now, since) =
                channel_state(manifest, seen.as_ref(), presence.launched_at(id), now);
            apply_liveness(
                (state_now, since),
                crate::sidecar::liveness_of(manifest.binary),
                now,
            )
        } else {
            (ChannelState::NotConfigured, None)
        };
        channels.push(ChannelStatus {
            id,
            kind: manifest.kind,
            label: manifest.label,
            health_row: manifest.health_name,
            configured,
            missing_secrets,
            settings_area: manifest.settings_area,
            state: state_now,
            since,
            adapter: seen.as_ref().map(|s| AdapterInfo {
                contract_version: s.contract.clone(),
            }),
            capabilities: seen
                .as_ref()
                .filter(|s| s.incompatible.is_none())
                .map(|s| s.capabilities.clone()),
            home: home_of(pool, id).await?,
            identities: identity_counts(pool, id).await?,
            pairing: pairing_of(pool, id, now).await?,
            outbox: super::outbox::counts(pool, id).await?,
            drops: presence.drops(id),
        });
    }
    let routing = match primary.and_then(|p| channels.iter().find(|c| c.id == p)) {
        None => RoutingState::NoPrimary,
        Some(c) if !c.configured => RoutingState::PrimaryNotConfigured,
        Some(c) if c.home.is_none() => RoutingState::PrimaryWithoutHome,
        Some(_) => RoutingState::Ok,
    };
    Ok(ChannelsReadout {
        contract_version: contract::CONTRACT_VERSION,
        primary,
        routing: RoutingReadout { state: routing },
        channels,
    })
}

/// Reads one secret off the async runtime (the credential store blocks). A store error counts as
/// absent: the channel then reads as not configured instead of taking the launcher down.
async fn read_secret(key: &'static str) -> Option<String> {
    match tokio::task::spawn_blocking(move || crate::secrets::load_secret(key)).await {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            tracing::warn!(%error, key, "channel: could not read a secret from the credential store");
            None
        }
        Err(error) => {
            tracing::warn!(%error, key, "channel: the secret read did not finish");
            None
        }
    }
}

/// Launches every channel whose required secrets are present. Phase 1 never calls it.
///
/// Refreshes the `configured` cache for every manifest, even when `sidecars_wanted` is false (a
/// non-primary daemon still reports configuration, it just spawns nothing).
pub async fn launch_all(state: &AppState, sidecars_wanted: bool) {
    let presence = &state.channels.presence;
    for manifest in MANIFESTS {
        let mut secrets: Vec<(&'static str, String)> = Vec::new();
        let mut missing: Vec<&'static str> = Vec::new();
        for spec in manifest.secrets {
            match read_secret(spec.key).await {
                Some(value) => secrets.push((spec.env, value)),
                None if spec.required => missing.push(spec.key),
                None => {}
            }
        }
        let configured = missing.is_empty();
        presence.set_secrets_present(manifest.id, missing);
        if !configured || !sidecars_wanted {
            continue;
        }
        let token = match crate::auth::mint_service_token(
            &state.pool,
            crate::auth::Service::Channel(manifest.id),
        )
        .await
        {
            Ok(token) => token,
            Err(error) => {
                tracing::warn!(%error, channel = manifest.id, "channel: could not mint its key");
                continue;
            }
        };
        let env = env_for(
            manifest,
            &crate::daemon_client::daemon_url(),
            &token,
            &secrets,
        );
        tokio::spawn(crate::sidecar::supervise(
            manifest.binary.to_string(),
            crate::sidecar::binary(manifest.binary),
            env,
        ));
        presence.set_launched(manifest.id, Utc::now());
        tracing::info!(channel = manifest.id, "channel adapter supervised");
    }
}

/// The environment of one adapter process: the daemon URL, the channel id, the channel key (sent
/// by the kit as `Authorization: Bearer <token>`), then each secret as `(env name, value)`.
pub fn env_for(
    manifest: &Manifest,
    daemon_url: &str,
    channel_token: &str,
    secrets: &[(&'static str, String)],
) -> Vec<(String, String)> {
    let mut env = vec![
        ("NUCLEOS_DAEMON_URL".to_string(), daemon_url.to_string()),
        ("NUCLEOS_CHANNEL_ID".to_string(), manifest.id.to_string()),
        (
            "NUCLEOS_CHANNEL_TOKEN".to_string(),
            channel_token.to_string(),
        ),
    ];
    env.extend(
        secrets
            .iter()
            .map(|(name, value)| (name.to_string(), value.clone())),
    );
    env
}

/// The health row of one channel, from the same status `list` returns.
fn health_row(status: &ChannelStatus, binary: &str) -> crate::health::SubsystemReadout {
    use crate::health::{FailureCategory, HealthState, SubsystemReadout};
    let (state, reason) = match status.state {
        ChannelState::NotConfigured => {
            (HealthState::Disabled, Some(FailureCategory::NotConfigured))
        }
        ChannelState::Online if status.outbox.failed > 0 => {
            (HealthState::Degraded, Some(FailureCategory::Unknown))
        }
        ChannelState::Online => (HealthState::Ok, None),
        ChannelState::Starting => (HealthState::Degraded, Some(FailureCategory::NotRunning)),
        ChannelState::Offline => (HealthState::Down, Some(FailureCategory::NotRunning)),
        ChannelState::Incompatible => (HealthState::Down, Some(FailureCategory::Unknown)),
    };
    // A binary that never started is worse news than a quiet adapter: say why.
    let reason = match crate::sidecar::liveness_of(binary) {
        Some(crate::sidecar::Liveness::FailedToSpawn(kind)) if state == HealthState::Down => {
            Some(match kind {
                std::io::ErrorKind::NotFound => FailureCategory::Missing,
                std::io::ErrorKind::PermissionDenied => FailureCategory::PermissionDenied,
                _ => FailureCategory::Unknown,
            })
        }
        _ => reason,
    };
    SubsystemReadout {
        name: status.health_row,
        status: state,
        reason,
        counts: Some(BTreeMap::from([
            (
                "unknown_sender",
                i64::try_from(status.drops.unknown_sender).unwrap_or(i64::MAX),
            ),
            (
                "pairing_failures",
                i64::try_from(status.drops.pairing_failures).unwrap_or(i64::MAX),
            ),
            ("outbox_failed", i64::from(status.outbox.failed)),
        ])),
    }
}

/// One health row per manifest, named `health_name`. Built from `list`, so the page and the row
/// never disagree. A failed readout query degrades every row to `down` rather than hiding them.
pub async fn health_rows(state: &AppState) -> Vec<crate::health::SubsystemReadout> {
    use crate::health::{FailureCategory, HealthState, SubsystemReadout};
    match list(state).await {
        Ok(readout) => readout
            .channels
            .iter()
            .map(|status| {
                let binary = by_id(status.id).map_or(status.id, |m| m.binary);
                health_row(status, binary)
            })
            .collect(),
        Err(error) => {
            tracing::warn!(%error, "channels: the health rows could not be read");
            MANIFESTS
                .iter()
                .map(|m| SubsystemReadout {
                    name: m.health_name,
                    status: HealthState::Down,
                    reason: Some(FailureCategory::Unknown),
                    counts: None,
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::testkit::state;

    /// `[a-z][a-z0-9-]{0,31}`; local until `contract::valid_channel_id` lands in P0-3.
    fn valid_id(id: &str) -> bool {
        let mut chars = id.chars();
        matches!(chars.next(), Some('a'..='z'))
            && id.len() <= 32
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    }

    fn write_yaml(root: &Path, file: &str, text: &str) {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn load_config_missing_or_malformed_is_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_config(dir.path(), &TELEGRAM), ChannelConfig::default());

        write_yaml(
            dir.path(),
            "channels/telegram.yaml",
            "doctrine: [not a string\n",
        );
        assert_eq!(load_config(dir.path(), &TELEGRAM), ChannelConfig::default());

        write_yaml(
            dir.path(),
            "channels/telegram.yaml",
            "doctrine: be brief\nprojects:\n  nucleos: \"-100:5\"\n",
        );
        let config = load_config(dir.path(), &TELEGRAM);
        assert_eq!(config.doctrine.as_deref(), Some("be brief"));
        assert_eq!(
            config.projects.get("nucleos").map(String::as_str),
            Some("-100:5")
        );

        let all = load_configs(Some(dir.path()));
        assert_eq!(all.get("telegram"), Some(&config));
        assert_eq!(all.get("fake"), Some(&ChannelConfig::default()));
        assert!(load_configs(None).is_empty());
    }

    #[test]
    fn project_for_conversation_reverse_lookup() {
        let config = ChannelConfig {
            doctrine: None,
            projects: BTreeMap::from([
                ("alpha".to_string(), "-1:5".to_string()),
                ("beta".to_string(), "-1:6".to_string()),
            ]),
        };
        assert_eq!(
            project_for_conversation(&config, "-1:6").as_deref(),
            Some("beta")
        );
        assert_eq!(project_for_conversation(&config, "-1:9"), None);
        assert_eq!(
            project_for_conversation(&ChannelConfig::default(), "-1:5"),
            None
        );
    }

    #[test]
    fn blank_doctrine_reads_as_none() {
        for text in [
            "doctrine: \"  \"\n",
            "doctrine: \"\"\n",
            "doctrine:\n",
            "projects: {}\n",
        ] {
            let config: ChannelConfig = serde_yaml::from_str(text).unwrap();
            assert_eq!(config.doctrine, None, "{text:?}");
        }
        let config: ChannelConfig = serde_yaml::from_str("doctrine: hi\n").unwrap();
        assert_eq!(config.doctrine.as_deref(), Some("hi"));
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

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()
    }

    fn hello(kind: &str, version: &str) -> contract::Hello {
        contract::Hello {
            kind: kind.to_string(),
            version: version.to_string(),
            capabilities: contract::Capabilities {
                buttons: true,
                max_text: 1000,
                ..contract::Capabilities::default()
            },
        }
    }

    async fn insert_leased(pool: &SqlitePool, channel: &str, attempts: i64) {
        sqlx::query(
            "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class, \
             state, due_at, lease_until, attempts, created_at) \
             VALUES (?, 'conv', 'ordered', 'send', '{}', 'turn', 'leased', 't', 'later', ?, 't')",
        )
        .bind(channel)
        .bind(attempts)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn touch_poll_sets_last_poll_and_keeps_hello() {
        let st = state().await;
        record_hello(&st, "fake", &hello("fake", "1.0.0"), at(0))
            .await
            .unwrap();
        st.channels.presence.touch_poll("fake", at(30));
        let seen = st.channels.presence.seen("fake").unwrap();
        assert_eq!(seen.last_poll_at, Some(at(30)));
        assert_eq!(seen.hello_at, at(0));
        assert!(seen.capabilities.buttons);
    }

    #[tokio::test]
    async fn record_hello_stores_capabilities_and_survives_a_fresh_presence() {
        let mut st = state().await;
        let reply = record_hello(&st, "fake", &hello("fake", "1.0.0"), at(0))
            .await
            .unwrap();
        assert_eq!(reply.contract_version, contract::CONTRACT_VERSION);
        let stored: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM channel_presence WHERE channel = 'fake'")
                .fetch_one(&st.pool)
                .await
                .unwrap();
        assert_eq!(stored, 1);
        st.channels = Default::default();
        assert!(st.channels.presence.seen("fake").is_none());
        let caps = capabilities(&st, "fake").await;
        assert!(caps.buttons);
        assert_eq!(caps.max_text, 1000);
    }

    #[tokio::test]
    async fn an_incompatible_hello_is_refused_and_remembered() {
        let st = state().await;
        let err = record_hello(&st, "fake", &hello("fake", "2.0.0"), at(5))
            .await
            .unwrap_err();
        assert!(matches!(err, HelloError::Incompatible { adapter } if adapter == "2.0.0"));
        let seen = st.channels.presence.seen("fake").unwrap();
        assert!(seen.incompatible.is_some());
        assert_eq!(seen.incompatible_at, Some(at(5)));
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channel_presence")
            .fetch_one(&st.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn a_hello_for_another_kind_is_refused() {
        let st = state().await;
        let err = record_hello(&st, "fake", &hello("telegram", "1.0.0"), at(0))
            .await
            .unwrap_err();
        assert!(matches!(err, HelloError::KindMismatch));
        assert!(st.channels.presence.seen("fake").is_none());
    }

    #[tokio::test]
    async fn a_hello_releases_leased_rows_without_a_second_attempt() {
        let st = state().await;
        insert_leased(&st.pool, "fake", 1).await;
        insert_leased(&st.pool, "fake", 1).await;
        record_hello(&st, "fake", &hello("fake", "1.0.0"), at(0))
            .await
            .unwrap();
        let rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT state, attempts FROM channel_outbox WHERE channel = 'fake'")
                .fetch_all(&st.pool)
                .await
                .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|(s, a)| s == "pending" && *a == 1));
    }

    #[tokio::test]
    async fn capabilities_default_is_everything_off_with_4096() {
        let st = state().await;
        let caps = capabilities(&st, "fake").await;
        assert_eq!(caps, contract::Capabilities::default());
        assert_eq!(caps.max_text, 4096);
        assert!(!caps.buttons && !caps.edit && !caps.typing && !caps.threads && !caps.reply_to);
        assert!(caps.media_in.is_empty() && caps.media_out.is_empty());
    }

    #[test]
    fn drop_counters_count_per_channel() {
        let p = Presence::default();
        p.drop_unknown_sender("telegram");
        p.drop_unknown_sender("telegram");
        p.drop_pairing_failure("telegram");
        p.drop_unknown_sender("fake");
        let t = p.drops("telegram");
        assert_eq!((t.unknown_sender, t.pairing_failures), (2, 1));
        assert_eq!(p.drops("fake").unknown_sender, 1);
        let none = p.drops("other");
        assert_eq!((none.unknown_sender, none.pairing_failures), (0, 0));
        assert_eq!(none.counted_since, t.counted_since);
    }

    #[tokio::test]
    async fn is_live_needs_a_launch_or_an_accepted_hello() {
        let st = state().await;
        assert!(!st.channels.presence.is_live("fake"));
        st.channels.presence.set_launched("fake", at(0));
        assert!(st.channels.presence.is_live("fake"));

        let st = state().await;
        record_hello(&st, "fake", &hello("fake", "1.0.0"), at(0))
            .await
            .unwrap();
        assert!(st.channels.presence.is_live("fake"));

        let st = state().await;
        let _ = record_hello(&st, "fake", &hello("fake", "2.0.0"), at(0)).await;
        assert!(!st.channels.presence.is_live("fake"));
    }

    #[tokio::test]
    async fn secrets_cache_feeds_channel_instances_exist() {
        let st = state().await;
        assert!(!channel_instances_exist(&st));
        st.channels
            .presence
            .set_secrets_present("telegram", vec!["telegram-token"]);
        assert!(!channel_instances_exist(&st));
        st.channels
            .presence
            .set_secrets_present("telegram", Vec::new());
        assert!(channel_instances_exist(&st));
    }

    #[test]
    fn routing_state_roundtrips_its_fields() {
        let all = [
            (RoutingState::Ok, "ok"),
            (RoutingState::NoPrimary, "no_primary"),
            (RoutingState::PrimaryNotConfigured, "primary_not_configured"),
            (RoutingState::PrimaryWithoutHome, "primary_without_home"),
        ];
        for (state, wire) in all {
            let json = serde_json::to_value(RoutingReadout { state }).unwrap();
            assert_eq!(json["state"], wire);
        }
    }

    #[test]
    fn fake_is_a_registered_channel_in_tests() {
        assert_eq!(by_id("fake").map(|m| m.kind), Some("fake"));
    }

    #[test]
    fn channel_state_follows_the_e2_table() {
        let m = by_id("fake").unwrap();
        let seen = |poll: Option<i64>, bad: bool| Seen {
            hello_at: at(0),
            last_poll_at: poll.map(at),
            capabilities: contract::Capabilities::default(),
            contract: "1.0.0".into(),
            incompatible: bad.then(|| "2.0.0".to_string()),
            incompatible_at: bad.then(|| at(3)),
        };
        assert_eq!(
            channel_state(m, None, None, at(9)),
            (ChannelState::NotConfigured, None)
        );
        assert_eq!(
            channel_state(m, None, Some(at(1)), at(9)),
            (ChannelState::Starting, Some(at(1)))
        );
        assert_eq!(
            channel_state(m, Some(&seen(Some(10), false)), None, at(50)),
            (ChannelState::Online, Some(at(0)))
        );
        assert_eq!(
            channel_state(m, Some(&seen(Some(10), false)), None, at(56)),
            (ChannelState::Offline, Some(at(10)))
        );
        assert_eq!(
            channel_state(m, Some(&seen(Some(10), true)), None, at(56)),
            (ChannelState::Incompatible, Some(at(3)))
        );
    }

    #[test]
    fn a_dead_adapter_process_reads_as_offline() {
        use crate::sidecar::Liveness::{FailedToSpawn, Restarting, Running};
        let kind = std::io::ErrorKind::NotFound;
        let online = (ChannelState::Online, Some(at(0)));
        let starting = (ChannelState::Starting, Some(at(1)));
        let offline = (ChannelState::Offline, Some(at(9)));
        assert_eq!(apply_liveness(online, Some(Restarting), at(9)), offline);
        assert_eq!(
            apply_liveness(online, Some(FailedToSpawn(kind)), at(9)),
            offline
        );
        assert_eq!(
            apply_liveness(starting, Some(FailedToSpawn(kind)), at(9)),
            offline
        );
        // Unchanged: a live or unsupervised process, a spawn still in progress, other states.
        assert_eq!(apply_liveness(online, Some(Running), at(9)), online);
        assert_eq!(apply_liveness(online, None, at(9)), online);
        assert_eq!(apply_liveness(starting, Some(Restarting), at(9)), starting);
        let incompatible = (ChannelState::Incompatible, Some(at(3)));
        assert_eq!(
            apply_liveness(incompatible, Some(Restarting), at(9)),
            incompatible
        );
    }

    #[tokio::test]
    async fn list_reports_every_manifest_and_a_no_primary_routing() {
        let st = state().await;
        let readout = list(&st).await.unwrap();
        assert_eq!(readout.contract_version, contract::CONTRACT_VERSION);
        assert_eq!(readout.primary, None);
        assert_eq!(readout.routing.state, RoutingState::NoPrimary);
        let ids: Vec<_> = readout.channels.iter().map(|c| c.id).collect();
        assert_eq!(ids, ["telegram", "fake"]);
        let tg = &readout.channels[0];
        assert!(!tg.configured && tg.state == ChannelState::NotConfigured);
        assert_eq!(tg.missing_secrets, ["telegram-token"]);
        assert!(tg.since.is_none() && tg.home.is_none() && tg.pairing.is_none());
        assert_eq!(tg.outbox.pending, 0);
    }

    #[tokio::test]
    async fn list_resolves_routing_and_aggregates_per_channel() {
        let st = state().await;
        st.channels
            .presence
            .set_secrets_present("telegram", Vec::new());
        sqlx::query("UPDATE channels_global SET primary_channel = 'telegram'")
            .execute(&st.pool)
            .await
            .unwrap();
        assert_eq!(
            list(&st).await.unwrap().routing.state,
            RoutingState::PrimaryWithoutHome
        );

        sqlx::query(
            "INSERT INTO channel_identities (channel, sender_ref, conversation_ref, \
             conversation_kind, parent, home, created_at) \
             VALUES ('telegram', '42', '-1:5', 'thread', '-1', 1, 't'), \
             ('telegram', '42', '7', 'private', NULL, 0, 't')",
        )
        .execute(&st.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO channel_pairing_codes (channel, code_hash, created_at, expires_at, \
             attempts) VALUES ('telegram', 'h', 't', '2999-01-01T00:00:00Z', 2)",
        )
        .execute(&st.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class, \
             state, due_at, attempts, created_at, settled_at) VALUES \
             ('telegram', 'c', 'ordered', 'send', '{}', 'turn', 'pending', \
              '2026-10-19T09:00:00Z', 0, 't', NULL), \
             ('telegram', 'c', 'ordered', 'send', '{}', 'turn', 'failed', 't', 3, 't', \
              '2999-01-01T00:00:00Z')",
        )
        .execute(&st.pool)
        .await
        .unwrap();
        st.channels.presence.drop_unknown_sender("telegram");

        let readout = list(&st).await.unwrap();
        assert_eq!(readout.primary, Some("telegram"));
        assert_eq!(readout.routing.state, RoutingState::Ok);
        let tg = &readout.channels[0];
        assert!(tg.configured && tg.missing_secrets.is_empty());
        // Configured but neither launched nor seen in this test: nothing to report yet.
        assert_eq!(tg.state, ChannelState::NotConfigured);
        let home = tg.home.as_ref().unwrap();
        assert_eq!(
            (home.conversation_ref.as_str(), home.sender_ref.as_str()),
            ("-1:5", "42")
        );
        assert_eq!(home.kind, contract::ConversationKind::Thread);
        assert_eq!((tg.identities.senders, tg.identities.conversations), (1, 2));
        assert_eq!(tg.pairing.as_ref().unwrap().attempts_left, 3);
        assert_eq!(
            (tg.outbox.pending, tg.outbox.failed, tg.outbox.leased),
            (1, 1, 0)
        );
        assert!(tg.outbox.oldest_due_at.is_some());
        assert_eq!(tg.drops.unknown_sender, 1);
    }

    #[tokio::test]
    async fn list_reports_not_configured_when_a_secret_is_missing() {
        let st = state().await;
        st.channels
            .presence
            .set_secrets_present("telegram", vec!["telegram-token"]);
        // Even a launched process does not make a channel without its secret anything else.
        st.channels.presence.set_launched("telegram", at(0));
        let readout = list(&st).await.unwrap();
        let tg = readout
            .channels
            .iter()
            .find(|c| c.id == "telegram")
            .unwrap();
        assert!(!tg.configured);
        assert_eq!(tg.missing_secrets, ["telegram-token"]);
        assert_eq!(tg.state, ChannelState::NotConfigured);
        assert!(tg.since.is_none());
    }

    #[tokio::test]
    async fn list_counts_identities_outbox_and_pairing() {
        let st = state().await;
        sqlx::query(
            "INSERT INTO channel_identities (channel, sender_ref, conversation_ref, \
             conversation_kind, parent, home, created_at) \
             VALUES ('telegram', '42', '7', 'private', NULL, 1, 't'), \
             ('telegram', '43', '8', 'private', NULL, 0, 't')",
        )
        .execute(&st.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO channel_pairing_codes (channel, code_hash, created_at, expires_at, \
             attempts) VALUES ('telegram', 'h', 't', '2999-01-01T00:00:00Z', 1)",
        )
        .execute(&st.pool)
        .await
        .unwrap();
        insert_leased(&st.pool, "telegram", 1).await;
        sqlx::query(
            "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class, \
             state, due_at, attempts, created_at) VALUES \
             ('telegram', 'c', 'ordered', 'send', '{}', 'turn', 'pending', \
              '2026-10-19T09:00:00Z', 0, 't'), \
             ('telegram', 'c', 'ordered', 'send', '{}', 'turn', 'pending', \
              '2026-10-19T08:00:00Z', 0, 't')",
        )
        .execute(&st.pool)
        .await
        .unwrap();

        let readout = list(&st).await.unwrap();
        let tg = readout
            .channels
            .iter()
            .find(|c| c.id == "telegram")
            .unwrap();
        assert_eq!((tg.identities.senders, tg.identities.conversations), (2, 2));
        assert_eq!(tg.pairing.as_ref().unwrap().attempts_left, 4);
        assert_eq!((tg.outbox.pending, tg.outbox.leased), (2, 1));
        assert_eq!(
            tg.outbox.oldest_due_at,
            Some("2026-10-19T08:00:00Z".parse().unwrap())
        );
        let fake = readout.channels.iter().find(|c| c.id == "fake").unwrap();
        assert_eq!((fake.identities.senders, fake.outbox.pending), (0, 0));
        assert!(fake.pairing.is_none());
    }

    #[tokio::test]
    async fn list_routing_state_is_primary_not_configured_without_secrets() {
        let st = state().await;
        sqlx::query("UPDATE channels_global SET primary_channel = 'telegram'")
            .execute(&st.pool)
            .await
            .unwrap();
        let readout = list(&st).await.unwrap();
        assert_eq!(readout.primary, Some("telegram"));
        assert_eq!(readout.routing.state, RoutingState::PrimaryNotConfigured);
    }

    #[tokio::test]
    async fn list_serializes_oldest_due_at_and_a_null_since_when_not_configured() {
        let st = state().await;
        sqlx::query(
            "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class, \
             state, due_at, attempts, created_at) VALUES \
             ('telegram', 'c', 'ordered', 'send', '{}', 'turn', 'pending', \
              '2026-10-19T09:00:00Z', 0, 't')",
        )
        .execute(&st.pool)
        .await
        .unwrap();
        let readout = list(&st).await.unwrap();
        let json = serde_json::to_value(&readout).unwrap();
        let tg = &json["channels"][0];
        assert_eq!(tg["id"], "telegram");
        assert_eq!(tg["state"], "not_configured");
        assert!(tg["since"].is_null());
        assert!(tg["outbox"]["oldest_due_at"].is_string());
        assert!(tg["outbox"].get("oldest_due").is_none());
    }

    #[test]
    fn env_for_orders_daemon_url_channel_token_then_secrets() {
        let env = env_for(
            &TELEGRAM,
            "http://127.0.0.1:8791",
            "telegram.svc-secret",
            &[("TELEGRAM_BOT_TOKEN", "bot".to_string())],
        );
        assert_eq!(
            env,
            vec![
                (
                    "NUCLEOS_DAEMON_URL".to_string(),
                    "http://127.0.0.1:8791".to_string()
                ),
                ("NUCLEOS_CHANNEL_ID".to_string(), "telegram".to_string()),
                (
                    "NUCLEOS_CHANNEL_TOKEN".to_string(),
                    "telegram.svc-secret".to_string()
                ),
                ("TELEGRAM_BOT_TOKEN".to_string(), "bot".to_string()),
            ]
        );
        assert_eq!(env_for(&FAKE, "u", "t", &[]).len(), 3);
    }

    #[tokio::test]
    async fn health_rows_one_per_manifest_named_id_sidecar() {
        let st = state().await;
        let rows = health_rows(&st).await;
        let names: Vec<_> = rows.iter().map(|r| r.name).collect();
        assert_eq!(rows.len(), MANIFESTS.len());
        assert_eq!(names, ["telegram_sidecar", "fake_sidecar"]);
        for manifest in MANIFESTS {
            assert_eq!(manifest.health_name, format!("{}_sidecar", manifest.id));
        }
        // Nothing configured: every row is a state, not a fault.
        assert!(
            rows.iter()
                .all(|r| r.status == crate::health::HealthState::Disabled)
        );
    }
}
