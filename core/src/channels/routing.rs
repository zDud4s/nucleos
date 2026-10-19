//! Notifications to the primary channel (flow H, Anexo D D.12). P0 skeleton: A2 owns the bodies.
#![allow(dead_code, unused_variables)] // P0 skeleton: A2 removes this with the real body

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::channels::bus::{Cadence, ChannelsBus};
use crate::channels::registry;
use crate::state::AppState;

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

#[derive(Debug)]
pub enum PolicyError {
    Invalid(nucleos_base::notify_policy::ValidationError),
    Db(sqlx::Error),
}

/// The routing loop, spawned once from `main.rs` when a channel instance exists. A2.
pub async fn run_routing_loop(state: AppState, bus: ChannelsBus, cadence: Cadence) {
    todo!()
}

/// One pass over what `wanted` names. A2.
pub async fn route_once(
    state: &AppState,
    now: DateTime<Utc>,
    wanted: Wanted,
) -> sqlx::Result<RouteReport> {
    todo!()
}

/// First start or new channel: cursors = current max. A2.
pub async fn seed(conn: &mut sqlx::SqliteConnection, channel: &str) -> sqlx::Result<()> {
    todo!()
}

/// `seed` for every channel instance, in one transaction. A2.
pub async fn seed_all(state: &AppState) {
    todo!()
}

/// A channel that becomes primary after a time out of it. A2.
pub async fn reseed(conn: &mut sqlx::SqliteConnection, channel: &str) -> sqlx::Result<()> {
    todo!()
}

pub async fn turn_watermark(pool: &SqlitePool, channel: &str) -> sqlx::Result<i64> {
    todo!()
}

pub async fn advance_turn_watermark(
    conn: &mut sqlx::SqliteConnection,
    channel: &str,
    to: i64,
) -> sqlx::Result<()> {
    todo!()
}

/// Whether notifications have a destination now. A2.
pub async fn routing_state(state: &AppState) -> sqlx::Result<registry::RoutingState> {
    todo!()
}

/// Default rows merged with this channel's overrides. A2.
pub async fn resolved_policy(
    pool: &SqlitePool,
    channel: Option<&str>,
) -> sqlx::Result<nucleos_base::notify_policy::Policy> {
    todo!()
}

/// The channel's own override rows only. A2.
pub async fn channel_overrides(
    pool: &SqlitePool,
    channel: &str,
) -> sqlx::Result<nucleos_base::notify_policy::Policy> {
    todo!()
}

/// Longest family prefix + exact kind rules, fail-open. A2.
pub fn passes(policy: &nucleos_base::notify_policy::Policy, kind: &str) -> bool {
    todo!()
}

/// Replaces every override row of `channel` in one transaction. A2.
pub async fn put_channel_policy(
    pool: &SqlitePool,
    channel: &str,
    policy: &nucleos_base::notify_policy::Policy,
    now: DateTime<Utc>,
) -> Result<(), PolicyError> {
    todo!()
}
