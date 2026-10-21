//! The channel hub (Anexo D, D.2): message channels as adapters over one contract. Phase 1 is
//! inert: `ChannelsRuntime::default()` has no configs and no channel is launched.

pub mod access;
pub mod bus;
pub mod commands;
#[cfg(test)]
pub(crate) mod conformance;
pub mod contract;
#[cfg(test)]
pub(crate) mod fake;
pub mod ingest;
pub mod media;
pub mod outbox;
pub mod registry;
pub mod replies;
pub mod routing;
#[cfg(test)]
pub(crate) mod schema;
pub mod strings;
#[cfg(test)]
mod switch_tests;
mod telegram_switch;
#[cfg(test)]
pub(crate) mod testkit;
pub mod tokens;

use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;
use std::time::Duration;

use axum::Router;
use chrono::Utc;

use crate::state::AppState;

/// Everything the channel hub keeps in memory. Durable state lives in the `channel_*` tables.
///
/// `Default` is the inert hub: no configs, no presence. There is no bus here: `main.rs` builds a
/// `ChannelsBus` and hands it to the loops as a parameter.
#[derive(Default)]
pub struct ChannelsRuntime {
    /// One long-poll waker per channel, plus the ephemeral `typing` set.
    pub outbox: outbox::Waiters,
    /// Who is connected, since when, and the last `hello`. Memory only.
    pub presence: registry::Presence,
    /// `~/.nucleos/channels/<id>.yaml`, read once at startup.
    pub(crate) configs: RwLock<HashMap<&'static str, registry::ChannelConfig>>,
}

impl ChannelsRuntime {
    /// Built by `main.rs` from the machine-config root; `None` gives the inert hub.
    pub fn load(machine_config_root: Option<&Path>) -> Self {
        Self::with_configs(registry::load_configs(machine_config_root))
    }

    /// Same, from configs already loaded (tests, and the reload after a settings edit).
    pub fn with_configs(configs: HashMap<&'static str, registry::ChannelConfig>) -> Self {
        Self {
            configs: RwLock::new(configs),
            ..Self::default()
        }
    }

    /// A clone of one channel's config; `None` for an unknown id or a missing file.
    pub fn config(&self, channel: &str) -> Option<registry::ChannelConfig> {
        self.configs.read().ok()?.get(channel).cloned()
    }
}

/// The routes of the hub, merged into `protected` by `http.rs` (behind `require_token`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(ingest::routes())
        .merge(media::routes())
        .merge(outbox::routes())
        .merge(access::routes())
}

/// Spawned once from `main.rs`: every retention TTL, hourly, in one loop.
pub async fn run_housekeeping(state: AppState) {
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    loop {
        tick.tick().await;
        let now = Utc::now();
        let pool = &state.pool;
        let results = [
            ("ingest", ingest::prune(pool, now).await),
            ("media", media::prune(pool, now).await),
            ("access", access::prune(pool, now).await),
            ("outbox", outbox::prune(pool, now).await),
            ("tokens", tokens::prune(pool, now).await),
            ("replies", replies::prune(pool, now).await),
        ];
        for (what, result) in results {
            if let Err(e) = result {
                tracing::warn!(error = %e, "channel housekeeping: {what} prune failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_hub_is_inert() {
        assert!(ChannelsRuntime::default().config("telegram").is_none());
    }

    #[test]
    fn routes_merge_without_a_path_conflict() {
        // axum panics on a conflicting route, so building the router is the assertion.
        let _ = Router::<AppState>::new().merge(routes());
    }
}
