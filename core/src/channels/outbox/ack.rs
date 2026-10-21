//! Outbox acknowledgement handling: ack, renew and the thread fallback (Anexo D D.10.2, section 6.2).
//!
//! `attempts` is not touched here: it was counted when the action was leased (Q6). A retryable
//! failure puts the same row back to `pending` with a later `due_at`; the tenth delivery without an
//! `ok` is permanent.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::{Class, FailReason, NewAction, Subject, enqueue, stamp, wake};
use crate::channels::contract::{self, Button, ConversationKey, MAX_RETRY_AFTER_MS, OutPart};
use crate::channels::registry;
use crate::channels::strings::{self, Text};
use crate::state::AppState;

/// Deliveries allowed without an `ok`.
const MAX_DELIVERIES: i64 = 10;
/// How far `renew` pushes the lease.
const RENEW: Duration = Duration::seconds(60);
/// Backoff of a retryable failure: 2 s, doubling per delivery, never above 5 minutes.
const BACKOFF_BASE_MS: i64 = 2_000;
const BACKOFF_MAX_MS: i64 = 5 * 60 * 1_000;
/// The adapter reason that triggers the home-conversation fallback.
const THREAD_NOT_FOUND: &str = "thread_not_found";

/// What `POST .../outbox/{id}/ack` answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AckOutcome {
    pub state: ActionState,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub ignored: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionState {
    Pending,
    Done,
    Failed,
}

/// What the ack decided for the row.
enum Decision {
    Done(Vec<String>),
    Retry { due_at: DateTime<Utc> },
    Fail(FailReason),
}

/// The part of a stored payload a fallback copy needs (send and edit share these fields).
#[derive(Deserialize)]
struct Body {
    parts: Vec<OutPart>,
    #[serde(default)]
    buttons: Option<Vec<Vec<Button>>>,
    #[serde(default)]
    silent: bool,
}

fn backoff_ms(attempts: i64) -> i64 {
    let exponent = (attempts - 1).clamp(0, 20) as u32;
    BACKOFF_BASE_MS
        .saturating_mul(1_i64 << exponent)
        .min(BACKOFF_MAX_MS)
}

fn decide(ack: contract::Ack, attempts: i64, now: DateTime<Utc>) -> Decision {
    match ack {
        contract::Ack::Ok {
            ok: true,
            platform_refs,
        } => Decision::Done(platform_refs),
        contract::Ack::Ok { ok: false, .. } => {
            Decision::Fail(FailReason::Adapter("ack_not_ok".to_string()))
        }
        contract::Ack::Failed { failed } => {
            if !failed.retryable {
                return Decision::Fail(FailReason::Adapter(failed.reason));
            }
            if attempts >= MAX_DELIVERIES {
                return Decision::Fail(FailReason::AttemptsExhausted);
            }
            let asked = failed.retry_after_ms.unwrap_or(0).min(MAX_RETRY_AFTER_MS) as i64;
            let delay = asked.max(backoff_ms(attempts));
            Decision::Retry {
                due_at: now + Duration::milliseconds(delay),
            }
        }
    }
}

/// Settles one acknowledgement. `None` = no such row on this channel (404). A late ack of a row
/// that is already `done`/`failed` changes nothing and answers `ignored`. Wakes the channel at the
/// end: the next action of the conversation may now be due.
pub async fn ack(
    state: &AppState,
    channel: &'static str,
    id: i64,
    ack: contract::Ack,
    now: DateTime<Utc>,
) -> sqlx::Result<Option<AckOutcome>> {
    let stamped = stamp(now);
    let mut tx = state.pool.begin().await?;
    type OutboxRow = (String, i64, String, String, String, String, Option<String>);
    let row: Option<OutboxRow> = sqlx::query_as(
        "SELECT state, attempts, conversation, action, payload, class, subject \
             FROM channel_outbox WHERE id = ? AND channel = ?",
    )
    .bind(id)
    .bind(channel)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((row_state, attempts, conversation, action, payload, class, subject)) = row else {
        return Ok(None);
    };
    match row_state.as_str() {
        "leased" | "pending" => {}
        "done" => {
            return Ok(Some(AckOutcome {
                state: ActionState::Done,
                ignored: true,
            }));
        }
        _ => {
            return Ok(Some(AckOutcome {
                state: ActionState::Failed,
                ignored: true,
            }));
        }
    }

    let outcome_state = match decide(ack, attempts, now) {
        Decision::Done(refs) => {
            let refs =
                serde_json::to_string(&refs).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
            sqlx::query(
                "UPDATE channel_outbox SET state = 'done', platform_refs = ?, lease_until = NULL, \
                 fail_reason = NULL, settled_at = ? WHERE id = ?",
            )
            .bind(refs)
            .bind(&stamped)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            ActionState::Done
        }
        Decision::Retry { due_at } => {
            sqlx::query(
                "UPDATE channel_outbox SET state = 'pending', due_at = ?, lease_until = NULL \
                 WHERE id = ?",
            )
            .bind(stamp(due_at))
            .bind(id)
            .execute(&mut *tx)
            .await?;
            ActionState::Pending
        }
        Decision::Fail(reason) => {
            sqlx::query(
                "UPDATE channel_outbox SET state = 'failed', fail_reason = ?, lease_until = NULL, \
                 settled_at = ? WHERE id = ?",
            )
            .bind(reason.to_text())
            .bind(&stamped)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            if reason == FailReason::Adapter(THREAD_NOT_FOUND.to_string()) {
                fall_back(
                    &mut tx,
                    state,
                    channel,
                    id,
                    &conversation,
                    &action,
                    &payload,
                    &class,
                    subject.as_deref(),
                    now,
                )
                .await?;
            }
            ActionState::Failed
        }
    };
    tx.commit().await?;
    wake(state, channel);
    Ok(Some(AckOutcome {
        state: outcome_state,
        ignored: false,
    }))
}

/// A project thread that no longer exists: the same content goes to the home conversation, marked
/// with the project, and the failed original points at its replacement. Silent when the
/// conversation is not a mapped project thread, there is no home, or the action has no text to
/// carry (`answer_action`). An `edit` becomes a `send`: its target lives in the lost thread.
#[allow(clippy::too_many_arguments)]
async fn fall_back(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    state: &AppState,
    channel: &'static str,
    id: i64,
    conversation: &str,
    action: &str,
    payload: &str,
    class: &str,
    subject: Option<&str>,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    if action == "answer_action" {
        return Ok(());
    }
    let Some(config) = state.channels.config(channel) else {
        return Ok(());
    };
    let Some(project) = registry::project_for_conversation(&config, conversation) else {
        return Ok(());
    };
    // Same query as `access::home_conversation`, on the open transaction.
    let home: Option<String> = sqlx::query_scalar(
        "SELECT conversation_ref FROM channel_identities WHERE channel = ? AND home = 1",
    )
    .bind(channel)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(home) = home.filter(|home| home != conversation) else {
        return Ok(());
    };
    let (Ok(body), Some(class)) = (serde_json::from_str::<Body>(payload), Class::parse(class))
    else {
        return Ok(());
    };

    let prefix = strings::render(&Text::ProjectPrefix { project });
    let mut parts = body.parts;
    match parts.first_mut() {
        Some(OutPart::Text { text }) => text.insert_str(0, &prefix),
        _ => parts.insert(0, OutPart::Text { text: prefix }),
    }
    let replacement = enqueue(
        tx,
        &ConversationKey::new(channel, &home),
        NewAction::Send {
            reply_to: None,
            parts,
            buttons: body.buttons,
            silent: body.silent,
        },
        class,
        subject.and_then(Subject::from_text),
        now,
    )
    .await?;
    sqlx::query("UPDATE channel_outbox SET replaced_by = ? WHERE id = ?")
        .bind(replacement)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// What `POST .../outbox/{id}/renew` answers.
#[derive(Debug, Serialize)]
pub struct Renewed {
    pub lease_until: DateTime<Utc>,
}

#[derive(Debug)]
pub enum RenewError {
    NotFound,
    NotLeased,
    Db(sqlx::Error),
}

/// Extends the lease of a row still `leased` on this channel by 60 s from `now`.
pub async fn renew(
    pool: &sqlx::SqlitePool,
    channel: &str,
    id: i64,
    now: DateTime<Utc>,
) -> Result<Renewed, RenewError> {
    let until = now + RENEW;
    let updated = sqlx::query(
        "UPDATE channel_outbox SET lease_until = ? \
         WHERE id = ? AND channel = ? AND state = 'leased'",
    )
    .bind(stamp(until))
    .bind(id)
    .bind(channel)
    .execute(pool)
    .await
    .map_err(RenewError::Db)?;
    if updated.rows_affected() > 0 {
        return Ok(Renewed { lease_until: until });
    }
    let exists: Option<i64> =
        sqlx::query_scalar("SELECT id FROM channel_outbox WHERE id = ? AND channel = ?")
            .bind(id)
            .bind(channel)
            .fetch_optional(pool)
            .await
            .map_err(RenewError::Db)?;
    Err(if exists.is_some() {
        RenewError::NotLeased
    } else {
        RenewError::NotFound
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration as StdDuration;

    use super::super::{enqueue, lease};
    use super::*;
    use crate::channels::ChannelsRuntime;
    use crate::channels::contract::Failure;
    use sqlx::SqlitePool;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()
    }

    async fn push_to(state: &AppState, conversation: &str) -> i64 {
        let mut conn = state.pool.acquire().await.unwrap();
        enqueue(
            &mut conn,
            &ConversationKey::new("fake", conversation),
            NewAction::Send {
                reply_to: None,
                parts: vec![OutPart::Text {
                    text: "hi".to_string(),
                }],
                buttons: None,
                silent: false,
            },
            Class::Turn,
            None,
            at(0),
        )
        .await
        .unwrap()
    }

    async fn push(state: &AppState) -> i64 {
        push_to(state, "c1").await
    }

    async fn deliver(state: &AppState, now: DateTime<Utc>) {
        lease(state, "fake", StdDuration::ZERO, now).await.unwrap();
    }

    fn ok(refs: &[&str]) -> contract::Ack {
        contract::Ack::Ok {
            ok: true,
            platform_refs: refs.iter().map(|r| r.to_string()).collect(),
        }
    }

    fn failed(retryable: bool, retry_after_ms: Option<u64>, reason: &str) -> contract::Ack {
        contract::Ack::Failed {
            failed: Failure {
                retryable,
                retry_after_ms,
                reason: reason.to_string(),
            },
        }
    }

    async fn settle(
        state: &AppState,
        id: i64,
        ack_body: contract::Ack,
        now: DateTime<Utc>,
    ) -> Option<AckOutcome> {
        ack(state, "fake", id, ack_body, now).await.unwrap()
    }

    /// `(state, attempts, fail_reason, lease_until, settled_at, due_at)`.
    #[allow(clippy::type_complexity)]
    async fn row(
        pool: &SqlitePool,
        id: i64,
    ) -> (
        String,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    ) {
        sqlx::query_as(
            "SELECT state, attempts, fail_reason, lease_until, settled_at, due_at \
             FROM channel_outbox WHERE id = ?",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn refs_of(pool: &SqlitePool, id: i64) -> Option<String> {
        sqlx::query_scalar("SELECT platform_refs FROM channel_outbox WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn outcome(state: ActionState, ignored: bool) -> Option<AckOutcome> {
        Some(AckOutcome { state, ignored })
    }

    #[tokio::test]
    async fn ok_settles_and_stores_platform_refs() {
        let state = crate::channels::testkit::state().await;
        let id = push(&state).await;
        deliver(&state, at(0)).await;

        let got = settle(&state, id, ok(&["993", "994"]), at(5)).await;
        assert_eq!(got, outcome(ActionState::Done, false));
        let (st, attempts, reason, lease_until, settled, _) = row(&state.pool, id).await;
        assert_eq!((st.as_str(), attempts), ("done", 1));
        assert_eq!(reason, None);
        assert_eq!(lease_until, None);
        assert_eq!(settled, Some(stamp(at(5))));
        assert_eq!(
            refs_of(&state.pool, id).await.as_deref(),
            Some(r#"["993","994"]"#)
        );
    }

    #[tokio::test]
    async fn ok_without_platform_refs_settles_done_with_an_empty_list() {
        let state = crate::channels::testkit::state().await;
        let id = push(&state).await;
        deliver(&state, at(0)).await;

        let got = settle(&state, id, ok(&[]), at(5)).await;
        assert_eq!(got, outcome(ActionState::Done, false));
        assert_eq!(row(&state.pool, id).await.0, "done");
        assert_eq!(refs_of(&state.pool, id).await.as_deref(), Some("[]"));
    }

    #[tokio::test]
    async fn a_late_ok_on_a_failed_row_is_ignored() {
        let state = crate::channels::testkit::state().await;
        let id = push(&state).await;
        deliver(&state, at(0)).await;
        settle(&state, id, failed(false, None, "chat_gone"), at(1)).await;

        let got = settle(&state, id, ok(&["1"]), at(2)).await;
        assert_eq!(got, outcome(ActionState::Failed, true));
        let (st, _, reason, _, settled, _) = row(&state.pool, id).await;
        assert_eq!(st, "failed");
        assert_eq!(reason.as_deref(), Some("chat_gone"));
        assert_eq!(settled, Some(stamp(at(1))));
        assert_eq!(refs_of(&state.pool, id).await, None);
    }

    #[tokio::test]
    async fn a_late_ack_on_done_is_ignored_with_state_done() {
        let state = crate::channels::testkit::state().await;
        let id = push(&state).await;
        deliver(&state, at(0)).await;
        settle(&state, id, ok(&["1"]), at(1)).await;

        let got = settle(&state, id, failed(true, None, "rate_limited"), at(2)).await;
        assert_eq!(got, outcome(ActionState::Done, true));
        assert_eq!(row(&state.pool, id).await.0, "done");
        assert_eq!(refs_of(&state.pool, id).await.as_deref(), Some(r#"["1"]"#));
    }

    #[tokio::test]
    async fn retryable_backs_off_exponentially_and_keeps_the_id() {
        let state = crate::channels::testkit::state().await;
        let id = push(&state).await;

        // Delivery 1 fails at t=10: due 2 s later.
        deliver(&state, at(0)).await;
        let got = settle(&state, id, failed(true, None, "rate_limited"), at(10)).await;
        assert_eq!(got, outcome(ActionState::Pending, false));
        let (st, attempts, _, lease_until, settled, due) = row(&state.pool, id).await;
        assert_eq!((st.as_str(), attempts), ("pending", 1));
        assert_eq!((lease_until, settled), (None, None));
        assert_eq!(due, stamp(at(12)));

        // Delivery 2: 4 s. Delivery 3: 8 s. `attempts` only moves with the lease.
        deliver(&state, at(12)).await;
        settle(&state, id, failed(true, None, "rate_limited"), at(20)).await;
        let (_, attempts, _, _, _, due) = row(&state.pool, id).await;
        assert_eq!((attempts, due), (2, stamp(at(24))));

        deliver(&state, at(24)).await;
        settle(&state, id, failed(true, None, "rate_limited"), at(30)).await;
        let (st, attempts, _, _, _, due) = row(&state.pool, id).await;
        assert_eq!((st.as_str(), attempts, due), ("pending", 3, stamp(at(38))));
    }

    #[tokio::test]
    async fn retry_after_wins_when_larger_than_the_backoff() {
        let state = crate::channels::testkit::state().await;
        let id = push(&state).await;
        deliver(&state, at(0)).await;

        settle(
            &state,
            id,
            failed(true, Some(45_000), "rate_limited"),
            at(10),
        )
        .await;
        assert_eq!(row(&state.pool, id).await.5, stamp(at(55)));
    }

    #[tokio::test]
    async fn retry_after_is_capped_at_24_hours() {
        let state = crate::channels::testkit::state().await;
        let id = push(&state).await;
        deliver(&state, at(0)).await;

        settle(
            &state,
            id,
            failed(true, Some(u64::MAX / 2), "rate_limited"),
            at(10),
        )
        .await;
        assert_eq!(row(&state.pool, id).await.5, stamp(at(10 + 24 * 60 * 60)));
    }

    #[tokio::test]
    async fn a_retryable_failure_blocks_the_next_action_of_that_conversation_but_not_others() {
        let state = crate::channels::testkit::state().await;
        let first = push_to(&state, "c1").await;
        let second = push_to(&state, "c1").await;
        let other = push_to(&state, "c2").await;
        let other_next = push_to(&state, "c2").await;

        deliver(&state, at(0)).await;
        settle(
            &state,
            first,
            failed(true, Some(60_000), "rate_limited"),
            at(1),
        )
        .await;
        settle(&state, other, ok(&["9"]), at(1)).await;

        let got = lease(&state, "fake", StdDuration::ZERO, at(2))
            .await
            .unwrap();
        let ids: Vec<i64> = got
            .actions
            .iter()
            .map(|a| match a {
                contract::OutAction::Send { id, .. }
                | contract::OutAction::Edit { id, .. }
                | contract::OutAction::AnswerAction { id, .. } => *id,
            })
            .collect();
        assert_eq!(ids, vec![other_next]);
        assert_ne!(ids, vec![second]);
    }

    #[tokio::test]
    async fn ten_deliveries_without_ok_make_it_permanent() {
        let state = crate::channels::testkit::state().await;
        let id = push(&state).await;
        deliver(&state, at(0)).await;

        // The ninth delivery may still be retried.
        sqlx::query("UPDATE channel_outbox SET attempts = 9 WHERE id = ?")
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
        let got = settle(&state, id, failed(true, None, "rate_limited"), at(1)).await;
        assert_eq!(got, outcome(ActionState::Pending, false));

        // The tenth delivery failed: no more retries.
        sqlx::query("UPDATE channel_outbox SET state = 'leased', attempts = 10 WHERE id = ?")
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
        let got = settle(&state, id, failed(true, None, "rate_limited"), at(2)).await;
        assert_eq!(got, outcome(ActionState::Failed, false));
        let (st, attempts, reason, lease_until, settled, _) = row(&state.pool, id).await;
        assert_eq!((st.as_str(), attempts), ("failed", 10));
        assert_eq!(reason.as_deref(), Some("attempts_exhausted"));
        assert_eq!(lease_until, None);
        assert_eq!(settled, Some(stamp(at(2))));
    }

    #[tokio::test]
    async fn thread_not_found_falls_back_to_home_with_a_project_prefix_and_marks_replaced_by() {
        let mut state = crate::channels::testkit::state().await;
        let config = registry::ChannelConfig {
            projects: [("alpha".to_string(), "thread-7".to_string())].into(),
            ..registry::ChannelConfig::default()
        };
        state.channels = Arc::new(ChannelsRuntime::with_configs(HashMap::from([(
            "fake", config,
        )])));
        sqlx::query(
            "INSERT INTO channel_identities (channel, sender_ref, conversation_ref, \
             conversation_kind, home, created_at) VALUES ('fake', 'u1', 'home', 'private', 1, 't')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        let id = push_to(&state, "thread-7").await;
        deliver(&state, at(0)).await;

        let got = settle(&state, id, failed(false, None, THREAD_NOT_FOUND), at(3)).await;
        assert_eq!(got, outcome(ActionState::Failed, false));

        let (st, _, reason, _, settled, _) = row(&state.pool, id).await;
        assert_eq!(st, "failed");
        assert_eq!(reason.as_deref(), Some(THREAD_NOT_FOUND));
        assert_eq!(settled, Some(stamp(at(3))));
        let replaced_by: Option<i64> =
            sqlx::query_scalar("SELECT replaced_by FROM channel_outbox WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        let new_id = replaced_by.expect("the original points at its replacement");

        let (conversation, lane, action, class, new_state, payload): (
            String,
            String,
            String,
            String,
            String,
            String,
        ) = sqlx::query_as(
            "SELECT conversation, lane, action, class, state, payload \
             FROM channel_outbox WHERE id = ?",
        )
        .bind(new_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            (conversation.as_str(), lane.as_str(), action.as_str()),
            ("home", "ordered", "send")
        );
        assert_eq!((class.as_str(), new_state.as_str()), ("turn", "pending"));
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["parts"][0]["text"], "[alpha] hi");
    }

    #[tokio::test]
    async fn a_permanent_failure_without_fallback_just_fails() {
        let state = crate::channels::testkit::state().await;
        let id = push(&state).await;
        deliver(&state, at(0)).await;

        // `thread_not_found` on a conversation that is no mapped project: nothing to fall back to.
        let got = settle(&state, id, failed(false, None, THREAD_NOT_FOUND), at(1)).await;
        assert_eq!(got, outcome(ActionState::Failed, false));
        let (st, _, reason, _, settled, _) = row(&state.pool, id).await;
        assert_eq!(st, "failed");
        assert_eq!(reason.as_deref(), Some(THREAD_NOT_FOUND));
        assert_eq!(settled, Some(stamp(at(1))));
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channel_outbox")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[tokio::test]
    async fn ack_of_an_unknown_id_is_none() {
        let state = crate::channels::testkit::state().await;
        assert_eq!(settle(&state, 999, ok(&[]), at(0)).await, None);

        // A row of another channel is not this channel's row either.
        let id = push(&state).await;
        let other = ack(&state, "telegram", id, ok(&[]), at(0)).await.unwrap();
        assert_eq!(other, None);
        assert_eq!(row(&state.pool, id).await.0, "pending");
    }

    #[tokio::test]
    async fn ack_out_of_order_between_conversations_is_fine() {
        let state = crate::channels::testkit::state().await;
        let a = push_to(&state, "c1").await;
        let b = push_to(&state, "c2").await;
        deliver(&state, at(0)).await;

        assert_eq!(
            settle(&state, b, ok(&["2"]), at(1)).await,
            outcome(ActionState::Done, false)
        );
        assert_eq!(
            settle(&state, a, ok(&["1"]), at(2)).await,
            outcome(ActionState::Done, false)
        );
        assert_eq!(row(&state.pool, a).await.0, "done");
        assert_eq!(row(&state.pool, b).await.0, "done");
    }

    #[tokio::test]
    async fn renew_extends_a_held_lease() {
        let state = crate::channels::testkit::state().await;
        let id = push(&state).await;
        deliver(&state, at(0)).await;
        assert_eq!(row(&state.pool, id).await.3, Some(stamp(at(60))));

        let renewed = renew(&state.pool, "fake", id, at(30)).await.unwrap();
        assert_eq!(renewed.lease_until, at(90));
        assert_eq!(row(&state.pool, id).await.3, Some(stamp(at(90))));
    }

    #[tokio::test]
    async fn renew_after_done_is_not_leased() {
        let state = crate::channels::testkit::state().await;
        let id = push(&state).await;
        deliver(&state, at(0)).await;
        settle(&state, id, ok(&["1"]), at(1)).await;

        assert!(matches!(
            renew(&state.pool, "fake", id, at(2)).await,
            Err(RenewError::NotLeased)
        ));
        assert!(matches!(
            renew(&state.pool, "fake", id + 100, at(2)).await,
            Err(RenewError::NotFound)
        ));
    }

    #[tokio::test]
    async fn ack_wakes_the_channel_waiter() {
        let state = crate::channels::testkit::state().await;
        let id = push(&state).await;
        deliver(&state, at(0)).await;

        // Armed before the ack, like a long-poll that is already waiting.
        let notify = state.channels.outbox.waiter("fake");
        let mut notified = std::pin::pin!(notify.notified());
        notified.as_mut().enable();

        settle(&state, id, ok(&["1"]), at(1)).await;
        let woke = tokio::time::timeout(StdDuration::from_secs(2), notified).await;
        assert!(woke.is_ok());
    }
}
