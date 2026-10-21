//! Notifications to the primary channel (flow H, Anexo D D.12). The state half (seed, reseed,
//! watermark, destination) is in `seed`, the feed announcements in `announce`; kill and
//! budget announcements in `alerts`; the loop is below.

use chrono::{DateTime, Utc};
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;

use crate::channels::bus::{Cadence, ChannelsBus};
use crate::channels::{access, registry};
use crate::state::AppState;

mod alerts;
mod announce;
mod policy;
mod seed;
pub use crate::channels::outbox::NOTIFICATION_TTL;
pub use alerts::SpendGate;
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

/// Resolves when `rx` changed. A closed topic never resolves, so the loop does not spin on it.
async fn changed(rx: &mut watch::Receiver<u64>) {
    if rx.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Folds into `wanted` every topic that changed while the loop was settling or looking again.
fn collect_changed(bus: &mut ChannelsBus, wanted: &mut Wanted) {
    let take = |rx: &mut watch::Receiver<u64>| {
        let hit = rx.has_changed().unwrap_or(false);
        if hit {
            rx.mark_unchanged();
        }
        hit
    };
    wanted.feed |= take(&mut bus.feed);
    wanted.proposals |= take(&mut bus.proposals);
    wanted.kill |= take(&mut bus.kill);
    wanted.budget |= take(&mut bus.budget);
    wanted.budget_from_spend |= take(&mut bus.runs);
}

/// The routing loop, spawned once from `main.rs` when a channel instance exists. H5.
///
/// Wakes on a topic signal or on the safety sweep (the first tick is the startup sweep, which
/// seeds a channel that has no routing row). A signal waits `settle` so a burst is one pass; a
/// pass that found nothing is repeated once after `second_look`, for a commit that landed just
/// after its signal. The sweep reads everything, with no second look.
pub async fn run_routing_loop(state: AppState, mut bus: ChannelsBus, cadence: Cadence) {
    let mut gate = SpendGate::new(cadence.budget_min_gap);
    let mut sweep = tokio::time::interval(cadence.safety_sweep);
    sweep.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        let mut wanted = Wanted::default();
        let swept = tokio::select! {
            _ = sweep.tick() => true,
            _ = changed(&mut bus.feed) => { wanted.feed = true; false }
            _ = changed(&mut bus.proposals) => { wanted.proposals = true; false }
            _ = changed(&mut bus.kill) => { wanted.kill = true; false }
            _ = changed(&mut bus.budget) => { wanted.budget = true; false }
            _ = changed(&mut bus.runs) => { wanted.budget_from_spend = true; false }
        };
        if swept {
            wanted = Wanted::all();
        } else {
            tokio::time::sleep(cadence.settle).await;
        }
        collect_changed(&mut bus, &mut wanted);
        let found = match route_pass(&state, Utc::now(), wanted, &mut gate).await {
            Ok(report) => report != RouteReport::default(),
            Err(error) => {
                tracing::warn!(%error, "a routing pass failed");
                continue;
            }
        };
        if swept || found {
            continue;
        }
        tokio::time::sleep(cadence.second_look).await;
        collect_changed(&mut bus, &mut wanted);
        if let Err(error) = route_pass(&state, Utc::now(), wanted, &mut gate).await {
            tracing::warn!(%error, "a routing second look failed");
        }
    }
}

/// One pass over what `wanted` names, with no spacing of spend-driven budget evaluations (the
/// loop uses `route_pass` with a gate that carries `Cadence::budget_min_gap` across passes).
pub async fn route_once(
    state: &AppState,
    now: DateTime<Utc>,
    wanted: Wanted,
) -> sqlx::Result<RouteReport> {
    route_pass(state, now, wanted, &mut SpendGate::open()).await
}

/// One pass over what `wanted` names. A channel with no `channel_routing_state` row yet only
/// seeds (Anexo B, B.6.4). Budget (B.6.3): `wanted.budget` evaluates now, `budget_from_spend` only
/// when `gate` lets it through, and the 60 s sweep (`Wanted::all()`) always does.
pub async fn route_pass(
    state: &AppState,
    now: DateTime<Utc>,
    wanted: Wanted,
    gate: &mut SpendGate,
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
    if wanted.proposals {
        report.proposals =
            announce::announce_proposals(state, now, primary, destination_ok).await?;
    }
    if wanted.kill {
        report.kill = alerts::announce_kill(state, now, primary, destination_ok).await?;
    }
    if (wanted.budget || wanted.budget_from_spend) && gate.due(now, wanted.budget) {
        report.budget = alerts::announce_budget(state, now, primary, destination_ok).await?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::channels::testkit;

    /// Short real cadences; the sweep is long unless a test is about the sweep.
    fn cadence(sweep_ms: u64) -> Cadence {
        Cadence {
            safety_sweep: Duration::from_millis(sweep_ms),
            settle: Duration::from_millis(5),
            second_look: Duration::from_millis(150),
            budget_min_gap: Duration::ZERO,
            ping_after: Duration::from_secs(600),
        }
    }

    /// The senders stay with the test: a dropped sender closes its topic.
    fn private_bus() -> (Vec<watch::Sender<u64>>, ChannelsBus) {
        let (tx, rx): (Vec<_>, Vec<_>) = (0..5).map(|_| watch::channel(0u64)).unzip();
        let mut rx = rx.into_iter();
        let mut next = || rx.next().expect("five receivers");
        let bus = ChannelsBus::from_receivers(next(), next(), next(), next(), next());
        (tx, bus)
    }

    fn bump(tx: &watch::Sender<u64>) {
        tx.send_modify(|n| *n += 1);
    }

    /// A state whose primary is a configured `telegram` with a home; `seeded` adds the routing row.
    async fn primed(seeded: bool) -> AppState {
        let state = testkit::state().await;
        state
            .channels
            .presence
            .set_secrets_present("telegram", Vec::new());
        sqlx::query("UPDATE channels_global SET primary_channel = 'telegram'")
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO channel_identities (channel, sender_ref, conversation_ref, \
             conversation_kind, home, created_at) VALUES ('telegram', '42', '-100', 'group', 1, 't')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        if seeded {
            let mut conn = state.pool.acquire().await.unwrap();
            seed(&mut conn, "telegram").await.unwrap();
        }
        state
    }

    async fn line(state: &AppState, summary: &str) {
        nucleos_base::feed::append(&state.pool, None, "summary", summary, None, None)
            .await
            .unwrap();
    }

    async fn announced(state: &AppState) -> Vec<String> {
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT payload FROM channel_outbox ORDER BY id")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        rows.iter()
            .map(|payload| {
                let payload: serde_json::Value = serde_json::from_str(payload).unwrap();
                payload["parts"][0]["text"].as_str().unwrap().to_owned()
            })
            .collect()
    }

    /// Polls until the outbox holds `n` rows (or 3 s pass) and returns what it holds.
    async fn wait_for(state: &AppState, n: usize) -> Vec<String> {
        for _ in 0..300 {
            let rows = announced(state).await;
            if rows.len() >= n {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        announced(state).await
    }

    #[tokio::test]
    async fn run_routing_loop_announces_on_signal() {
        let state = primed(true).await;
        let (tx, bus) = private_bus();
        let task = tokio::spawn(run_routing_loop(state.clone(), bus, cadence(60_000)));
        tokio::time::sleep(Duration::from_millis(50)).await;
        line(&state, "a signalled line").await;
        bump(&tx[0]);
        let rows = wait_for(&state, 1).await;
        task.abort();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].contains("a signalled line"), "{rows:?}");
    }

    #[tokio::test]
    async fn run_routing_loop_announces_on_sweep_without_any_signal() {
        let state = primed(true).await;
        let (_tx, bus) = private_bus();
        let task = tokio::spawn(run_routing_loop(state.clone(), bus, cadence(40)));
        tokio::time::sleep(Duration::from_millis(10)).await;
        line(&state, "a line nobody signalled").await;
        let rows = wait_for(&state, 1).await;
        task.abort();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].contains("a line nobody signalled"), "{rows:?}");
    }

    #[tokio::test]
    async fn run_routing_loop_second_look_catches_a_late_commit() {
        let state = primed(true).await;
        let (tx, bus) = private_bus();
        let task = tokio::spawn(run_routing_loop(state.clone(), bus, cadence(60_000)));
        tokio::time::sleep(Duration::from_millis(50)).await;
        // The signal comes first and the first look (after `settle`) finds nothing; the line is
        // committed in between that look and the second one, with no signal of its own.
        bump(&tx[0]);
        tokio::time::sleep(Duration::from_millis(50)).await;
        line(&state, "a late commit").await;
        let rows = wait_for(&state, 1).await;
        task.abort();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].contains("a late commit"), "{rows:?}");
    }

    #[tokio::test]
    async fn run_routing_loop_seed_announces_nothing_old() {
        let state = primed(false).await;
        line(&state, "an old line").await;
        let (tx, bus) = private_bus();
        let task = tokio::spawn(run_routing_loop(state.clone(), bus, cadence(60_000)));
        // The startup sweep seeds the channel instead of announcing history.
        for _ in 0..300 {
            let seeded: Option<i64> = sqlx::query_scalar(
                "SELECT feed_cursor FROM channel_routing_state WHERE channel = 'telegram'",
            )
            .fetch_optional(&state.pool)
            .await
            .unwrap();
            if seeded.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(announced(&state).await.is_empty());
        line(&state, "a new line").await;
        bump(&tx[0]);
        let rows = wait_for(&state, 1).await;
        task.abort();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].contains("a new line"), "{rows:?}");
    }
}
