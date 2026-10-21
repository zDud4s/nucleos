//! Outbox lease handling: claim, expiry and the long-poll wait (Anexo D D.10.2, section 6.1).
//!
//! `attempts` counts deliveries: it rises once per lease and nowhere else (Q6). The tenth delivery
//! without an `ok` is the last, the next poll fails the row with `attempts_exhausted`. A row with a
//! live lease is never handed out again (Q12), so a poll never raises `attempts` by itself.

use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

use super::{FailReason, degrade, parse_time, stamp};
use crate::channels::contract::{self, Button, Ephemeral, OutAction, OutPart, OutboxPoll};
use crate::channels::registry;
use crate::state::AppState;

/// How long an adapter holds a delivered action before it counts as lost.
const LEASE: Duration = Duration::seconds(60);
/// Deliveries allowed without an `ok`.
const MAX_DELIVERIES: i64 = 10;

#[derive(Deserialize)]
struct SendPayload {
    parts: Vec<OutPart>,
    #[serde(default)]
    buttons: Option<Vec<Vec<Button>>>,
    #[serde(default)]
    silent: bool,
    #[serde(default)]
    reply_to: Option<i64>,
}

#[derive(Deserialize)]
struct EditPayload {
    parts: Vec<OutPart>,
    #[serde(default)]
    buttons: Option<Vec<Vec<Button>>>,
    target: i64,
}

#[derive(Deserialize)]
struct AnswerPayload {
    action_ref: String,
    text: String,
}

/// The adapter-facing copy of a stored row, before degradation. `None` when the stored payload does
/// not decode.
fn build(id: i64, conversation: String, action: &str, payload: &str) -> Option<OutAction> {
    match action {
        "send" => {
            let p: SendPayload = serde_json::from_str(payload).ok()?;
            Some(OutAction::Send {
                id,
                conversation,
                reply_to: p.reply_to,
                reply_to_platform_ref: None,
                parts: p.parts,
                buttons: p.buttons,
                silent: p.silent,
            })
        }
        "edit" => {
            let p: EditPayload = serde_json::from_str(payload).ok()?;
            Some(OutAction::Edit {
                id,
                conversation,
                target: p.target,
                target_platform_refs: Vec::new(),
                parts: p.parts,
                buttons: p.buttons,
            })
        }
        "answer_action" => {
            let p: AnswerPayload = serde_json::from_str(payload).ok()?;
            Some(OutAction::AnswerAction {
                id,
                conversation,
                action_ref: p.action_ref,
                text: p.text,
            })
        }
        _ => None,
    }
}

/// Leases what is due: at most ONE ordered action per conversation (the oldest unsettled), every
/// due immediate action, the ephemeral set. Expiry is evaluated here, at delivery. Without anything
/// due, sleeps on the channel's waker or until the next `due_at` / `lease_until` / `expires_at`,
/// capped at `wait`, then looks once more.
pub async fn lease(
    state: &AppState,
    channel: &'static str,
    wait: StdDuration,
    now: DateTime<Utc>,
) -> sqlx::Result<OutboxPoll> {
    let started = Instant::now();
    let mut at = now;
    let mut second_look = false;
    loop {
        // Armed before the database is read: a wake between the read and the sleep is kept.
        let notify = state.channels.outbox.waiter(channel);
        let mut notified = std::pin::pin!(notify.notified());
        notified.as_mut().enable();

        let poll = lease_once(state, channel, at).await?;
        if !poll.actions.is_empty() || !poll.ephemeral.is_empty() || second_look || wait.is_zero() {
            return Ok(poll);
        }
        let remaining = wait.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Ok(poll);
        }
        let until_event = match next_event(state, channel, at).await? {
            Some(event) => (event - at).to_std().unwrap_or(StdDuration::ZERO),
            None => remaining,
        };
        let nap = until_event.min(remaining);
        if !nap.is_zero() {
            // Either outcome (woken or timed out) is followed by the one extra look.
            let _ = tokio::time::timeout(nap, notified.as_mut()).await;
        }
        second_look = true;
        at = now + Duration::from_std(started.elapsed()).unwrap_or_else(|_| Duration::zero());
    }
}

/// One pass: settle what is past due, then lease. Everything in one transaction.
async fn lease_once(
    state: &AppState,
    channel: &'static str,
    now: DateTime<Utc>,
) -> sqlx::Result<OutboxPoll> {
    let caps: contract::Capabilities = registry::capabilities(state, channel).await;
    let stamped = stamp(now);
    let lease_until = stamp(now + LEASE);
    let mut tx = state.pool.begin().await?;

    // Expiry is checked at delivery, before anything is chosen.
    sqlx::query(
        "UPDATE channel_outbox SET state = 'failed', fail_reason = ?, lease_until = NULL, \
         settled_at = ? WHERE channel = ? AND state IN ('pending', 'leased') \
         AND expires_at IS NOT NULL AND expires_at <= ?",
    )
    .bind(FailReason::Expired.to_text())
    .bind(&stamped)
    .bind(channel)
    .bind(&stamped)
    .execute(&mut *tx)
    .await?;

    // A lease that ran out is a pending row again; `attempts` stays (it counted the delivery).
    sqlx::query(
        "UPDATE channel_outbox SET state = 'pending', lease_until = NULL \
         WHERE channel = ? AND state = 'leased' AND lease_until <= ?",
    )
    .bind(channel)
    .bind(&stamped)
    .execute(&mut *tx)
    .await?;

    // Ten deliveries have already gone out without an `ok`.
    sqlx::query(
        "UPDATE channel_outbox SET state = 'failed', fail_reason = ?, settled_at = ? \
         WHERE channel = ? AND state = 'pending' AND attempts >= ?",
    )
    .bind(FailReason::AttemptsExhausted.to_text())
    .bind(&stamped)
    .bind(channel)
    .bind(MAX_DELIVERIES)
    .execute(&mut *tx)
    .await?;

    // The oldest unsettled ordered row of each conversation, only if it is pending (a live lease
    // blocks its conversation), plus every due immediate row.
    let due: Vec<(i64, String, String, String)> = sqlx::query_as(
        "SELECT o.id, o.conversation, o.action, o.payload FROM channel_outbox o \
         WHERE o.channel = ?1 AND o.state = 'pending' AND o.due_at <= ?2 \
         AND (o.lane = 'immediate' OR (o.lane = 'ordered' AND o.id = \
              (SELECT MIN(x.id) FROM channel_outbox x \
               WHERE x.channel = o.channel AND x.conversation = o.conversation \
               AND x.lane = 'ordered' AND x.state IN ('pending', 'leased')))) \
         ORDER BY o.id",
    )
    .bind(channel)
    .bind(&stamped)
    .fetch_all(&mut *tx)
    .await?;

    let mut actions: Vec<OutAction> = Vec::with_capacity(due.len());
    for (id, conversation, action, payload) in due {
        let Some(built) = build(id, conversation, &action, &payload) else {
            sqlx::query(
                "UPDATE channel_outbox SET state = 'failed', fail_reason = 'invalid_payload', \
                 settled_at = ? WHERE id = ?",
            )
            .bind(&stamped)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            continue;
        };
        sqlx::query(
            "UPDATE channel_outbox SET state = 'leased', lease_until = ?, \
             attempts = attempts + 1 WHERE id = ?",
        )
        .bind(&lease_until)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        // The stored row stays as enqueued; only the delivered copy is degraded.
        match degrade::degrade(&mut tx, channel, &caps, built).await? {
            Some(action) => actions.push(action),
            None => {
                // A best-effort progress edit that cannot be applied in place: settled without a
                // delivery, so it neither retries nor holds back the conversation.
                sqlx::query(
                    "UPDATE channel_outbox SET state = 'done', fail_reason = 'dropped',                      lease_until = NULL, settled_at = ? WHERE id = ?",
                )
                .bind(&stamped)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    tx.commit().await?;

    let ephemeral = state
        .channels
        .outbox
        .take_typing(channel)
        .into_iter()
        .map(|conversation| Ephemeral::Typing { conversation })
        .collect();
    Ok(OutboxPoll { actions, ephemeral })
}

/// The earliest future moment something about this channel's queue changes: a `due_at`, a
/// `lease_until` or an `expires_at`.
async fn next_event(
    state: &AppState,
    channel: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<Option<DateTime<Utc>>> {
    let stamped = stamp(now);
    let next: Option<String> = sqlx::query_scalar(
        "SELECT MIN(t) FROM ( \
           SELECT MIN(due_at) AS t FROM channel_outbox \
             WHERE channel = ?1 AND state = 'pending' AND due_at > ?2 \
           UNION ALL \
           SELECT MIN(lease_until) FROM channel_outbox \
             WHERE channel = ?1 AND state = 'leased' AND lease_until > ?2 \
           UNION ALL \
           SELECT MIN(expires_at) FROM channel_outbox \
             WHERE channel = ?1 AND state IN ('pending', 'leased') AND expires_at > ?2)",
    )
    .bind(channel)
    .bind(&stamped)
    .fetch_one(&state.pool)
    .await?;
    Ok(next.as_deref().and_then(parse_time))
}

#[cfg(test)]
mod tests {
    use super::super::{Class, NewAction, Subject, enqueue, typing, wake};
    use super::*;
    use crate::channels::contract::ConversationKey;
    use sqlx::SqlitePool;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()
    }

    fn key(conversation: &str) -> ConversationKey {
        ConversationKey::new("fake", conversation)
    }

    async fn push_as(
        state: &AppState,
        conversation: &str,
        class: Class,
        now: DateTime<Utc>,
    ) -> i64 {
        let mut conn = state.pool.acquire().await.unwrap();
        enqueue(
            &mut conn,
            &key(conversation),
            NewAction::Send {
                reply_to: None,
                parts: vec![OutPart::Text {
                    text: "hi".to_string(),
                }],
                buttons: None,
                silent: false,
            },
            class,
            None,
            now,
        )
        .await
        .unwrap()
    }

    async fn push(state: &AppState, conversation: &str) -> i64 {
        push_as(state, conversation, Class::Turn, at(0)).await
    }

    fn action_id(action: &OutAction) -> i64 {
        match action {
            OutAction::Send { id, .. }
            | OutAction::Edit { id, .. }
            | OutAction::AnswerAction { id, .. } => *id,
        }
    }

    fn ids(poll: &OutboxPoll) -> Vec<i64> {
        poll.actions.iter().map(action_id).collect()
    }

    async fn poll(state: &AppState, now: DateTime<Utc>) -> OutboxPoll {
        lease(state, "fake", StdDuration::ZERO, now).await.unwrap()
    }

    /// `(state, attempts, fail_reason, lease_until, settled_at)`.
    async fn row(
        pool: &SqlitePool,
        id: i64,
    ) -> (String, i64, Option<String>, Option<String>, Option<String>) {
        sqlx::query_as(
            "SELECT state, attempts, fail_reason, lease_until, settled_at \
             FROM channel_outbox WHERE id = ?",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn one_ordered_action_per_conversation_oldest_first() {
        let state = crate::channels::testkit::state().await;
        let first = push(&state, "c1").await;
        let second = push(&state, "c1").await;

        assert_eq!(ids(&poll(&state, at(0)).await), vec![first]);
        // The first is still out: its conversation stays blocked.
        assert!(poll(&state, at(1)).await.actions.is_empty());

        sqlx::query("UPDATE channel_outbox SET state = 'done', settled_at = 's' WHERE id = ?")
            .bind(first)
            .execute(&state.pool)
            .await
            .unwrap();
        assert_eq!(ids(&poll(&state, at(2)).await), vec![second]);
    }

    #[tokio::test]
    async fn different_conversations_lease_in_parallel() {
        let state = crate::channels::testkit::state().await;
        let a = push(&state, "c1").await;
        let b = push(&state, "c2").await;
        push(&state, "c1").await;

        assert_eq!(ids(&poll(&state, at(0)).await), vec![a, b]);
    }

    #[tokio::test]
    async fn an_immediate_action_is_not_blocked_by_a_stuck_send() {
        let state = crate::channels::testkit::state().await;
        let send = push(&state, "c1").await;
        assert_eq!(ids(&poll(&state, at(0)).await), vec![send]);

        let mut conn = state.pool.acquire().await.unwrap();
        let answer = enqueue(
            &mut conn,
            &key("c1"),
            NewAction::AnswerAction {
                action_ref: "cb-1".to_string(),
                text: "ok".to_string(),
            },
            Class::Command,
            Some(Subject::Run(1)),
            at(1),
        )
        .await
        .unwrap();
        drop(conn);

        let got = poll(&state, at(1)).await;
        assert_eq!(ids(&got), vec![answer]);
        assert!(matches!(got.actions[0], OutAction::AnswerAction { .. }));
    }

    #[tokio::test]
    async fn a_leased_row_with_a_live_lease_is_not_returned_by_a_later_poll_and_attempts_do_not_rise()
     {
        let state = crate::channels::testkit::state().await;
        let id = push(&state, "c1").await;
        assert_eq!(ids(&poll(&state, at(0)).await), vec![id]);
        for secs in [1, 30, 59] {
            assert!(poll(&state, at(secs)).await.actions.is_empty());
        }
        let (st, attempts, _, lease_until, _) = row(&state.pool, id).await;
        assert_eq!((st.as_str(), attempts), ("leased", 1));
        assert_eq!(lease_until, Some(stamp(at(60))));
    }

    #[tokio::test]
    async fn each_delivery_counts_one_attempt() {
        let state = crate::channels::testkit::state().await;
        let id = push(&state, "c1").await;
        assert_eq!(row(&state.pool, id).await.1, 0);
        poll(&state, at(0)).await;
        assert_eq!(row(&state.pool, id).await.1, 1);
    }

    #[tokio::test]
    async fn an_expired_lease_is_redelivered_and_counts_a_second_delivery() {
        let state = crate::channels::testkit::state().await;
        let id = push(&state, "c1").await;
        assert_eq!(ids(&poll(&state, at(0)).await), vec![id]);

        assert_eq!(ids(&poll(&state, at(61)).await), vec![id]);
        let (st, attempts, _, lease_until, _) = row(&state.pool, id).await;
        assert_eq!((st.as_str(), attempts), ("leased", 2));
        assert_eq!(lease_until, Some(stamp(at(121))));
    }

    #[tokio::test]
    async fn the_tenth_delivery_without_ok_is_the_last_then_the_row_fails() {
        let state = crate::channels::testkit::state().await;
        let id = push(&state, "c1").await;
        for delivery in 1..=10_i64 {
            let got = poll(&state, at(delivery * 100)).await;
            assert_eq!(ids(&got), vec![id], "delivery {delivery}");
            assert_eq!(row(&state.pool, id).await.1, delivery);
        }
        let later = at(1100);
        assert!(poll(&state, later).await.actions.is_empty());
        let (st, attempts, reason, lease_until, settled) = row(&state.pool, id).await;
        assert_eq!((st.as_str(), attempts), ("failed", 10));
        assert_eq!(reason.as_deref(), Some("attempts_exhausted"));
        assert_eq!(lease_until, None);
        assert_eq!(settled, Some(stamp(later)));
    }

    #[tokio::test]
    async fn a_row_past_expires_at_is_failed_not_leased() {
        let state = crate::channels::testkit::state().await;
        let id = push_as(&state, "c1", Class::Notification, at(0)).await;
        let late = at(25 * 60 * 60);

        assert!(poll(&state, late).await.actions.is_empty());
        let (st, attempts, reason, _, settled) = row(&state.pool, id).await;
        assert_eq!((st.as_str(), attempts), ("failed", 0));
        assert_eq!(reason.as_deref(), Some("expired"));
        assert_eq!(settled, Some(stamp(late)));
    }

    #[tokio::test]
    async fn waiting_returns_when_an_action_is_enqueued() {
        let state = crate::channels::testkit::state().await;
        let writer = state.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(StdDuration::from_millis(50)).await;
            let id = push(&writer, "c1").await;
            wake(&writer, "fake");
            id
        });

        let started = Instant::now();
        let got = lease(&state, "fake", StdDuration::from_secs(2), at(0))
            .await
            .unwrap();
        let id = task.await.unwrap();
        assert_eq!(ids(&got), vec![id]);
        assert!(started.elapsed() < StdDuration::from_secs(1));
    }

    #[tokio::test]
    async fn waiting_returns_empty_after_wait() {
        let state = crate::channels::testkit::state().await;
        let started = Instant::now();
        let got = lease(&state, "fake", StdDuration::from_millis(100), at(0))
            .await
            .unwrap();
        assert!(got.actions.is_empty() && got.ephemeral.is_empty());
        let waited = started.elapsed();
        assert!(waited >= StdDuration::from_millis(90), "{waited:?}");
        assert!(waited < StdDuration::from_secs(2), "{waited:?}");
    }

    #[tokio::test]
    async fn typing_is_returned_once() {
        let state = crate::channels::testkit::state().await;
        let hello = contract::Hello {
            kind: "fake".to_string(),
            version: "1.0.0".to_string(),
            capabilities: contract::Capabilities {
                typing: true,
                ..contract::Capabilities::default()
            },
        };
        registry::record_hello(&state, "fake", &hello, at(0))
            .await
            .unwrap();
        typing(&state, &key("c1"));

        let first = poll(&state, at(0)).await;
        assert!(first.actions.is_empty());
        assert_eq!(
            first.ephemeral,
            vec![Ephemeral::Typing {
                conversation: "c1".to_string()
            }]
        );
        assert!(poll(&state, at(1)).await.ephemeral.is_empty());
    }
}
