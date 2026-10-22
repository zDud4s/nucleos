//! The channel hub (Anexo D, D.2): message channels as adapters over one contract. Phase 1 is
//! inert: `ChannelsRuntime::default()` has no configs and no channel is launched.

pub mod access;
pub mod bus;
pub mod contract;
pub mod outbox;
pub mod registry;
pub mod replies;
pub mod routing;
#[cfg(test)]
pub mod schema;
pub mod streaming;
pub mod strings;
#[cfg(test)]
mod switch_tests;
pub mod telegram_switch;
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
pub mod tokens;

use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;

use axum::Router;

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
    /// `~/.nucleos/channels/<id>.yaml`, read at startup and re-read by [`ChannelsRuntime::reload`]
    /// when the app writes one (a hand edit on disk still needs a restart).
    pub configs: RwLock<HashMap<&'static str, registry::ChannelConfig>>,
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

    /// Re-reads every channel's yaml under `root` and swaps the map in, so a settings edit made
    /// through the app applies to the next turn without a restart. Absent or malformed files read
    /// as defaults, exactly as at startup. Hand edits on disk are not watched.
    pub fn reload(&self, root: &Path) {
        let fresh = registry::load_configs(Some(root));
        if let Ok(mut configs) = self.configs.write() {
            *configs = fresh;
        }
    }

    /// A clone of one channel's config; `None` for an unknown id or a missing file.
    pub fn config(&self, channel: &str) -> Option<registry::ChannelConfig> {
        self.configs.read().ok()?.get(channel).cloned()
    }
}

/// The routes of the hub that live in `nucleos-core`: the outbox and the access management. The
/// ingest and media routes sit in `nucleos-app` (they drive assistant turns), whose
/// `channels::routes()` merges these and is what `http.rs` mounts.
pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(outbox::routes())
        .merge(access::routes())
}

/// The channel a request speaks for, taken from the authenticated scope and never from the body
/// (section 7.3). Only a channel's own key is admitted: the control key has no channel to speak for
/// and gets a bare 403, and a key on another channel's path (which `require_token` already refuses)
/// gets the `channel_mismatch` body.
// The refusal is a ready-made HTTP response the caller returns as is; boxing it would only add noise.
#[allow(clippy::result_large_err)]
pub fn own_channel(
    scope: &crate::auth::Scope,
    named: &str,
) -> Result<&'static str, axum::response::Response> {
    use crate::auth::{Scope, Service};
    use axum::Json;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    match scope {
        Scope::Service(Service::Channel(id)) if *id == named => Ok(*id),
        Scope::Service(Service::Channel(_)) => Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "channel_mismatch" })),
        )
            .into_response()),
        _ => Err(StatusCode::FORBIDDEN.into_response()),
    }
}

/// The 400 of T9: `{error, at}` from a failed decode.
pub fn contract_error(error: contract::ContractError) -> axum::response::Response {
    use axum::Json;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    (
        StatusCode::BAD_REQUEST,
        Json(contract::ErrorResponse::from(error)),
    )
        .into_response()
}

/// The name a media row is shown under: its stored name, else its mime. Lives here, not in the
/// app's `media` module, because the outbox degrades unsendable media parts to text with it.
pub async fn display_name<'e, E>(executor: E, media_id: &str) -> sqlx::Result<Option<String>>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let row: Option<(Option<String>, String)> =
        sqlx::query_as("SELECT name, mime FROM channel_media WHERE id = ?")
            .bind(media_id)
            .fetch_optional(executor)
            .await?;
    Ok(row.map(|(name, mime)| name.filter(|n| !n.is_empty()).unwrap_or(mime)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_hub_is_inert() {
        assert!(ChannelsRuntime::default().config("telegram").is_none());
    }

    #[test]
    fn reload_picks_up_a_changed_yaml() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("channels")).unwrap();
        let file = temp.path().join("channels/telegram.yaml");
        std::fs::write(&file, "doctrine: a\n").unwrap();
        let hub = ChannelsRuntime::load(Some(temp.path()));
        assert!(hub.config("telegram").unwrap().stream_edits());

        std::fs::write(&file, "stream_edits: false\n").unwrap();
        hub.reload(temp.path());
        assert!(!hub.config("telegram").unwrap().stream_edits());
    }

    #[test]
    fn routes_merge_without_a_path_conflict() {
        // axum panics on a conflicting route, so building the router is the assertion.
        let _ = Router::<AppState>::new().merge(routes());
    }
}
