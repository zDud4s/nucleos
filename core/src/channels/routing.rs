//! Notifications to the primary channel (flow H, Anexo D D.12). The state half (seed, reseed,
//! watermark, destination) is in `seed`, the feed announcements in `announce`; the proposal,
//! kill and budget passes and the loop itself are still P0 skeletons.

use chrono::{DateTime, Utc};

use crate::channels::bus::{Cadence, ChannelsBus};
use crate::channels::{access, registry};
use crate::state::AppState;

mod announce;
mod policy;
mod seed;
pub use crate::channels::outbox::NOTIFICATION_TTL;
pub use policy::{PolicyError, channel_overrides, passes, put_channel_policy, resolved_policy};
pub use seed::{
    advance_turn_watermark, reseed, reseed_at, routing_state, seed, seed_all, seed_at,
    turn_watermark,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Wanted {
    pub feed: bool,
    pub proposals: bool,
    pub kill: bool,
    /// Budget configuration changed (`Topic::Budget`): evaluate now.
    pub budget: bool,
    /// A run ended (`Topic::Runs`): spend may have changed.
    pub budget_from_spend: bool,
}

impl Wanted {
    pub fn all() -> Self {
        Self {
            feed: true,
            proposals: true,
            kill: true,
            budget: true,
            budget_from_spend: true,
        }
    }

    pub fn any(self) -> bool {
        self.feed || self.proposals || self.kill || self.budget || self.budget_from_spend
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RouteReport {
    pub feed: u32,
    pub proposals: u32,
    pub kill: bool,
    pub budget: bool,
}

/// The routing loop, spawned once from `main.rs` when a channel instance exists. H5.
pub async fn run_routing_loop(_state: AppState, _bus: ChannelsBus, _cadence: Cadence) {
    todo!()
}

/// One pass over what `wanted` names. A channel with no `channel_routing_state` row yet only
/// seeds (Anexo B, B.6.4). Only the feed half exists so far (H3a); proposals, kill and budget
/// arrive with H3b and H4.
pub async fn route_once(
    state: &AppState,
    now: DateTime<Utc>,
    wanted: Wanted,
) -> sqlx::Result<RouteReport> {
    let mut report = RouteReport::default();
    let Some(primary) = access::primary(&state.pool).await? else {
        return Ok(report);
    };
    let seeded: Option<i64> =
        sqlx::query_scalar("SELECT feed_cursor FROM channel_routing_state WHERE channel = ?")
            .bind(primary)
            .fetch_optional(&state.pool)
            .await?;
    if seeded.is_none() {
        let paused = seed::budget_paused(&state.pool, now).await;
        let mut conn = state.pool.acquire().await?;
        seed::seed_at(&mut conn, primary, paused, now).await?;
        return Ok(report);
    }
    let destination_ok = routing_state(state).await? == registry::RoutingState::Ok;
    if wanted.feed {
        report.feed = announce::announce_feed(state, now, primary, destination_ok).await?;
    }
    Ok(report)
}
