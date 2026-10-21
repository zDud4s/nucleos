//! Kill switch and budget announcements to the primary channel (Anexo B B.6.2-B.6.3, Anexo D
//! D.12). A port of `KillChanged` and `BudgetChanged` from the Telegram notifier, with the last
//! observed state kept in `channel_routing_state` instead of in the process. These alerts never
//! consult the notification policy: the switch and the budget stay unsilenceable.

use chrono::{DateTime, Utc};

use super::seed::budget_paused;
use crate::channels::outbox::{self, Class, NewAction};
use crate::channels::strings::{self, Text};
use crate::channels::{access, contract};
use crate::governance::autopilot;
use crate::state::AppState;

/// Spaces out the spend-driven budget evaluations (`Cadence::budget_min_gap`). A configuration
/// change or the 60 s sweep is never held back; only the signal of a run ending is.
#[derive(Debug, Clone)]
pub struct SpendGate {
    min_gap: chrono::Duration,
    last: Option<DateTime<Utc>>,
}

impl SpendGate {
    pub fn new(min_gap: std::time::Duration) -> Self {
        Self {
            min_gap: chrono::Duration::from_std(min_gap).unwrap_or(chrono::Duration::zero()),
            last: None,
        }
    }

    /// A gate that holds nothing back, for a single pass with no loop around it.
    pub fn open() -> Self {
        Self::new(std::time::Duration::ZERO)
    }

    /// Whether a budget evaluation runs now. `forced` (configuration signal, sweep) always does;
    /// either way the evaluation counts as the latest one.
    pub(super) fn due(&mut self, now: DateTime<Utc>, forced: bool) -> bool {
        let spaced = self.last.is_none_or(|last| now - last >= self.min_gap);
        let run = forced || spaced;
        if run {
            self.last = Some(now);
        }
        run
    }
}

/// Enqueues one unsilenceable line to the home conversation, if there is a destination.
async fn enqueue_alert(
    conn: &mut sqlx::SqliteConnection,
    primary: &'static str,
    home: &Option<String>,
    text: String,
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
    let Some(home) = home else {
        return Ok(false);
    };
    outbox::enqueue(
        conn,
        &contract::ConversationKey::new(primary, home.clone()),
        NewAction::Send {
            reply_to: None,
            parts: vec![contract::OutPart::Text { text }],
            buttons: None,
            silent: false,
        },
        Class::Notification,
        None,
        now,
    )
    .await?;
    Ok(true)
}

async fn alert_home(
    state: &AppState,
    primary: &'static str,
    destination_ok: bool,
) -> sqlx::Result<Option<String>> {
    if destination_ok {
        access::home_conversation(&state.pool, primary).await
    } else {
        Ok(None)
    }
}

/// Announces a change of the kill switch against `last_kill` and records the new value. With no
/// destination the value is still recorded, so nothing is announced late. Returns whether a line
/// was enqueued.
pub(super) async fn announce_kill(
    state: &AppState,
    now: DateTime<Utc>,
    primary: &'static str,
    destination_ok: bool,
) -> sqlx::Result<bool> {
    let pool = &state.pool;
    let engaged = autopilot::kill_switch_engaged(pool).await?;
    let last: bool =
        sqlx::query_scalar("SELECT last_kill FROM channel_routing_state WHERE channel = ?")
            .bind(primary)
            .fetch_one(pool)
            .await?;
    if last == engaged {
        return Ok(false);
    }
    let home = alert_home(state, primary, destination_ok).await?;
    let text = strings::render(&if engaged {
        Text::KillEngaged
    } else {
        Text::KillDisengaged
    });
    let mut tx = pool.begin().await?;
    let announced = enqueue_alert(&mut tx, primary, &home, text, now).await?;
    sqlx::query("UPDATE channel_routing_state SET last_kill = ? WHERE channel = ?")
        .bind(engaged)
        .bind(primary)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if announced {
        outbox::wake(state, primary);
    }
    Ok(announced)
}

/// Announces a budget pause or resume against `last_budget_paused` and records the new value. The
/// evaluation is `seed::budget_paused` (no-limit shortcut, otherwise only
/// `budget_permits_new_run`); the line carries no figures, so `budget::snapshot` is never read.
pub(super) async fn announce_budget(
    state: &AppState,
    now: DateTime<Utc>,
    primary: &'static str,
    destination_ok: bool,
) -> sqlx::Result<bool> {
    let pool = &state.pool;
    let paused = budget_paused(pool, now).await;
    let last: bool = sqlx::query_scalar(
        "SELECT last_budget_paused FROM channel_routing_state WHERE channel = ?",
    )
    .bind(primary)
    .fetch_one(pool)
    .await?;
    if last == paused {
        return Ok(false);
    }
    let home = alert_home(state, primary, destination_ok).await?;
    let text = strings::render(&if paused {
        Text::BudgetPaused
    } else {
        Text::BudgetResumed
    });
    let mut tx = pool.begin().await?;
    let announced = enqueue_alert(&mut tx, primary, &home, text, now).await?;
    sqlx::query("UPDATE channel_routing_state SET last_budget_paused = ? WHERE channel = ?")
        .bind(paused)
        .bind(primary)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if announced {
        outbox::wake(state, primary);
    }
    Ok(announced)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use chrono::TimeZone;

    use super::*;
    use crate::channels::registry::ChannelConfig;
    use crate::channels::routing::{self, Wanted};
    use crate::channels::{ChannelsRuntime, testkit};
    use crate::governance::budget::{BudgetConfig, BudgetPeriod, set_budget_config};

    const HOME: &str = "-100";

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0).unwrap()
    }

    /// A state whose primary is `telegram`, configured, with a home and a seeded routing row.
    async fn ready() -> AppState {
        let mut state = testkit::state().await;
        let mut configs = HashMap::new();
        configs.insert(
            "telegram",
            ChannelConfig {
                doctrine: None,
                projects: Default::default(),
                stream_edits: None,
            },
        );
        state.channels = Arc::new(ChannelsRuntime::with_configs(configs));
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
             conversation_kind, home, created_at) VALUES ('telegram', '42', ?, 'group', 1, 't')",
        )
        .bind(HOME)
        .execute(&state.pool)
        .await
        .unwrap();
        let mut conn = state.pool.acquire().await.unwrap();
        routing::seed(&mut conn, "telegram").await.unwrap();
        drop(conn);
        state
    }

    fn kill_only() -> Wanted {
        Wanted {
            kill: true,
            ..Wanted::default()
        }
    }

    fn budget_only() -> Wanted {
        Wanted {
            budget: true,
            ..Wanted::default()
        }
    }

    fn spend_only() -> Wanted {
        Wanted {
            budget_from_spend: true,
            ..Wanted::default()
        }
    }

    /// The text of every outbox row, oldest first.
    async fn texts(state: &AppState) -> Vec<String> {
        let payloads: Vec<String> =
            sqlx::query_scalar("SELECT payload FROM channel_outbox ORDER BY id")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        payloads
            .iter()
            .map(|p| {
                let v: serde_json::Value = serde_json::from_str(p).unwrap();
                v["parts"][0]["text"].as_str().unwrap().to_owned()
            })
            .collect()
    }

    /// A limit the reserve alone overshoots pauses new runs with no run rows at all; no limit
    /// resumes them.
    async fn set_paused(state: &AppState, paused: bool) {
        set_budget_config(
            &state.pool,
            &BudgetConfig {
                limit_usd: paused.then_some(1.0),
                period: BudgetPeriod::Monthly,
                hourly_limit_usd: None,
                per_run_reserve_usd: 5.0,
                time_cost_per_hour_usd: 3.0,
            },
        )
        .await
        .unwrap();
    }

    async fn last_budget_paused(state: &AppState) -> bool {
        sqlx::query_scalar(
            "SELECT last_budget_paused FROM channel_routing_state WHERE channel = 'telegram'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn route_kill_announces_changes_only() {
        let state = ready().await;
        let report = routing::route_once(&state, now(), kill_only())
            .await
            .unwrap();
        assert!(!report.kill, "an unchanged switch announces nothing");
        assert!(texts(&state).await.is_empty());

        autopilot::set_kill_switch(&state.pool, true).await.unwrap();
        let report = routing::route_once(&state, now(), kill_only())
            .await
            .unwrap();
        assert!(report.kill);
        let again = routing::route_once(&state, now(), kill_only())
            .await
            .unwrap();
        assert!(!again.kill, "the same state is not announced twice");

        autopilot::set_kill_switch(&state.pool, false)
            .await
            .unwrap();
        let report = routing::route_once(&state, now(), kill_only())
            .await
            .unwrap();
        assert!(report.kill);
        assert_eq!(
            texts(&state).await,
            vec![
                strings::render(&Text::KillEngaged),
                strings::render(&Text::KillDisengaged)
            ]
        );
        let (conversation, class): (String, String) =
            sqlx::query_as("SELECT conversation, class FROM channel_outbox ORDER BY id LIMIT 1")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            (conversation.as_str(), class.as_str()),
            (HOME, "notification")
        );
    }

    #[tokio::test]
    async fn route_budget_shortcut_reads_no_runs() {
        let state = ready().await;
        // With no limit the answer must not need `runs`: take the table away.
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("DROP TABLE runs")
            .execute(&state.pool)
            .await
            .unwrap();
        let report = routing::route_once(&state, now(), budget_only())
            .await
            .unwrap();
        assert!(!report.budget);
        assert!(texts(&state).await.is_empty());
        assert!(!last_budget_paused(&state).await);
    }

    #[tokio::test]
    async fn route_budget_from_spend_respects_min_gap() {
        let state = ready().await;
        let mut gate = SpendGate::new(Duration::from_secs(10));
        let at = |secs: i64| now() + chrono::Duration::seconds(secs);

        set_paused(&state, true).await;
        let report = routing::route_pass(&state, at(0), spend_only(), &mut gate)
            .await
            .unwrap();
        assert!(report.budget, "the first spend signal is evaluated");

        set_paused(&state, false).await;
        let report = routing::route_pass(&state, at(1), spend_only(), &mut gate)
            .await
            .unwrap();
        assert!(
            !report.budget,
            "a second spend signal inside the gap is held"
        );
        assert!(last_budget_paused(&state).await);

        let report = routing::route_pass(&state, at(11), spend_only(), &mut gate)
            .await
            .unwrap();
        assert!(report.budget, "past the gap the change is seen");
        assert!(!last_budget_paused(&state).await);

        // A configuration signal is never held back.
        set_paused(&state, true).await;
        let report = routing::route_pass(&state, at(12), budget_only(), &mut gate)
            .await
            .unwrap();
        assert!(report.budget);
        assert_eq!(texts(&state).await.len(), 3);
    }

    #[tokio::test]
    async fn route_budget_announces_pause_and_resume() {
        let state = ready().await;
        set_paused(&state, true).await;
        let report = routing::route_once(&state, now(), budget_only())
            .await
            .unwrap();
        assert!(report.budget);
        assert!(last_budget_paused(&state).await);
        let again = routing::route_once(&state, now(), budget_only())
            .await
            .unwrap();
        assert!(!again.budget, "a pause already announced is not repeated");

        set_paused(&state, false).await;
        let report = routing::route_once(&state, now(), budget_only())
            .await
            .unwrap();
        assert!(report.budget);
        assert!(!last_budget_paused(&state).await);

        let texts = texts(&state).await;
        assert_eq!(texts.len(), 2);
        assert!(texts[0].contains("New runs are paused"), "{}", texts[0]);
        assert!(texts[1].contains("New runs are allowed"), "{}", texts[1]);
    }
}
