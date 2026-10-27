//! `nucleos-app`: the top of the núcleo's library, compiled as its own crate
//! (`core/crates/nucleos-app/Cargo.toml` points here).
//!
//! These modules are read by the daemon and by nothing in `nucleos-core`, so an edit to one of them
//! recompiles this crate and the binary only. The glob import below keeps every
//! `crate::state::...`-style path inside them naming the same item it did when they were part of
//! `nucleos-core`; a module of `nucleos-core` that starts reading one of these does not compile,
//! which is the point. Split out on 2026-10-22 (the lib was one dependency cycle of 95% of its
//! lines, so every edit recompiled all of it).

#![cfg_attr(all(feature = "testkit", not(test)), allow(dead_code, unused_imports))]
// Same reason as in `lib.rs`: `pub` here means "visible to the binary".
#![allow(clippy::new_without_default, clippy::should_implement_trait)]

#[allow(unused_imports)]
use nucleos_core::*;

pub mod analytics;
pub mod analytics_pipeline;
pub mod assistant;
pub mod consolidate;
pub mod devtime;
pub mod fleet;
pub mod hooks;
// Shadows the library's `map`, which holds only `intent`; `map.rs` re-exports that one.
pub mod map;
pub mod presets;
pub mod price_refresh;
pub mod verify_cli;

/// The channel hub as the rest of the program sees it: everything `nucleos-core` keeps under
/// `channels` (re-exported, so `crate::channels::registry` and friends keep resolving), plus the
/// three modules that drive an assistant turn (`commands`, `ingest`, `media`) and so cannot sit
/// below the assistant. Their files stay in `core/src/channels/`; only the crate they compile in
/// changed. The local `channels` shadows the glob import of `nucleos_core::channels` above, which
/// is the point. `routes` and `run_housekeeping` are here because they reach those three modules.
pub mod channels {
    pub use nucleos_core::channels::*;

    pub mod commands;
    #[cfg(test)]
    pub mod conformance;
    #[cfg(test)]
    pub mod fake;
    pub mod ingest;
    pub mod media;
    #[cfg(test)]
    mod port_tests_commands;
    #[cfg(test)]
    mod port_tests_flow;

    use std::time::Duration;

    use axum::Router;
    use chrono::Utc;

    use crate::state::AppState;

    /// The routes of the hub, merged into `protected` by `http.rs` (behind `require_token`): the
    /// ones `nucleos-core` keeps, plus ingest and media.
    pub fn routes() -> Router<AppState> {
        Router::new()
            .merge(ingest::routes())
            .merge(media::routes())
            .merge(nucleos_core::channels::routes())
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
                (
                    "media",
                    media::prune(pool, state.files_root.as_deref(), now).await,
                ),
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
        fn routes_merge_without_a_path_conflict() {
            // axum panics on a conflicting route, so building the router is the assertion.
            let _ = routes();
        }
    }
}
