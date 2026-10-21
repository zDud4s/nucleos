//! Schema checks of the channel tables (test-only): migrations 0220-0227, 0230 and 0237 create what
//! D.17 says,
//! with the indexes and CHECKs the rest of the hub leans on.

use sqlx::SqlitePool;

use crate::testdb::{apply_migrations_after, fresh_pool, pool_migrated_through};

async fn columns(pool: &SqlitePool, table: &str) -> Vec<String> {
    sqlx::query_scalar::<_, String>("SELECT name FROM pragma_table_info(?)")
        .bind(table)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn index_sql(pool: &SqlitePool, name: &str) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?",
    )
    .bind(name)
    .fetch_optional(pool)
    .await
    .unwrap()
    .flatten()
}

async fn ok(pool: &SqlitePool, sql: &str) -> bool {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .execute(pool)
        .await
        .is_ok()
}

fn outbox_insert(lane: &str, action: &str, state: &str, class: &str) -> String {
    format!(
        "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class, \
         state, due_at, created_at) \
         VALUES ('c', 'conv', '{lane}', '{action}', '{{}}', '{class}', '{state}', 't', 't')"
    )
}

fn identity_insert(channel: &str, sender: &str, conv: &str, home: i64) -> String {
    format!(
        "INSERT INTO channel_identities (channel, sender_ref, conversation_ref, \
         conversation_kind, home, created_at) \
         VALUES ('{channel}', '{sender}', '{conv}', 'private', {home}, 't')"
    )
}

fn code_insert(channel: &str, hash: &str) -> String {
    format!(
        "INSERT INTO channel_pairing_codes (channel, code_hash, created_at, expires_at) \
         VALUES ('{channel}', '{hash}', 't', 't')"
    )
}

#[tokio::test]
async fn all_eleven_channel_tables_exist() {
    let pool = fresh_pool().await;
    for table in [
        "channel_events",
        "channel_media",
        "channel_identities",
        "channel_pairing_codes",
        "channels_global",
        "channel_outbox",
        "channel_action_tokens",
        "channel_routing_state",
        "channel_notify_policy",
        "channel_turn_replies",
        "channel_presence",
    ] {
        assert!(!columns(&pool, table).await.is_empty(), "missing {table}");
    }
}

#[tokio::test]
async fn the_queue_pause_marks_table_exists() {
    let pool = fresh_pool().await;
    assert_eq!(
        columns(&pool, "channel_queue_pause_marks").await,
        ["channel", "conversation", "announced_at"]
    );
}

#[tokio::test]
async fn outbox_has_the_lease_and_settlement_columns() {
    let pool = fresh_pool().await;
    let cols = columns(&pool, "channel_outbox").await;
    for want in [
        "lease_until",
        "expires_at",
        "replaced_by",
        "fail_reason",
        "settled_at",
    ] {
        assert!(
            cols.iter().any(|c| c == want),
            "channel_outbox lacks {want}"
        );
    }
}

#[tokio::test]
async fn identities_and_pairing_codes_have_their_columns() {
    let pool = fresh_pool().await;
    let ids = columns(&pool, "channel_identities").await;
    for want in ["conversation_kind", "parent", "sender_name", "created_at"] {
        assert!(
            ids.iter().any(|c| c == want),
            "channel_identities lacks {want}"
        );
    }
    let codes = columns(&pool, "channel_pairing_codes").await;
    for want in [
        "code_hash",
        "expires_at",
        "attempts",
        "superseded_at",
        "cancelled_at",
        "paired_at",
        "paired_sender_ref",
        "paired_conversation_ref",
    ] {
        assert!(
            codes.iter().any(|c| c == want),
            "channel_pairing_codes lacks {want}"
        );
    }
}

#[tokio::test]
async fn every_named_index_exists() {
    let pool = fresh_pool().await;
    for name in [
        "channel_outbox_due",
        "channel_outbox_settled",
        "channel_events_received",
        "channel_media_created",
        "channel_action_tokens_proposal",
        "channel_turn_replies_open",
        "channel_identities_one_home",
        "channel_pairing_codes_one_active",
    ] {
        assert!(
            index_sql(&pool, name).await.is_some(),
            "missing index {name}"
        );
    }
}

#[tokio::test]
async fn outbox_due_index_is_partial_over_open_rows() {
    let pool = fresh_pool().await;
    let sql = index_sql(&pool, "channel_outbox_due").await.unwrap();
    assert!(
        sql.contains("WHERE state IN ('pending', 'leased')"),
        "channel_outbox_due must be partial over open rows: {sql}"
    );
    assert!(index_sql(&pool, "channel_outbox_open").await.is_none());
}

#[tokio::test]
async fn turn_replies_open_index_is_partial() {
    let pool = fresh_pool().await;
    let sql = index_sql(&pool, "channel_turn_replies_open").await.unwrap();
    assert!(sql.contains("WHERE replied_at IS NULL"), "{sql}");
}

#[tokio::test]
async fn one_home_per_channel_but_one_per_channel_is_fine() {
    let pool = fresh_pool().await;
    assert!(ok(&pool, &identity_insert("a", "s1", "c1", 1)).await);
    // A second home on the same channel is refused, even for another sender.
    assert!(!ok(&pool, &identity_insert("a", "s2", "c2", 1)).await);
    // A non-home row is fine, and so is a home on another channel.
    assert!(ok(&pool, &identity_insert("a", "s2", "c2", 0)).await);
    assert!(ok(&pool, &identity_insert("b", "s1", "c1", 1)).await);
}

#[tokio::test]
async fn only_the_owner_role_is_accepted() {
    let pool = fresh_pool().await;
    let member = "INSERT INTO channel_identities (channel, sender_ref, conversation_ref, \
                  conversation_kind, role, created_at) \
                  VALUES ('a', 's', 'c', 'private', 'member', 't')";
    assert!(!ok(&pool, member).await);
    assert!(ok(&pool, &identity_insert("a", "s", "c", 0)).await);
}

#[tokio::test]
async fn one_active_pairing_code_per_channel() {
    let pool = fresh_pool().await;
    assert!(ok(&pool, &code_insert("a", "h1")).await);
    assert!(!ok(&pool, &code_insert("a", "h2")).await);
    // Another channel is independent.
    assert!(ok(&pool, &code_insert("b", "h3")).await);
    // Once the first one is superseded, a new one may be minted.
    sqlx::query("UPDATE channel_pairing_codes SET superseded_at = 't' WHERE code_hash = 'h1'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(ok(&pool, &code_insert("a", "h2")).await);
}

#[tokio::test]
async fn duplicate_event_ids_are_refused() {
    let pool = fresh_pool().await;
    let insert = "INSERT INTO channel_events (channel, event_id, received_at, outcome) \
                  VALUES ('a', 'e1', 't', 'processing')";
    assert!(ok(&pool, insert).await);
    assert!(!ok(&pool, insert).await);
    let other_channel = "INSERT INTO channel_events (channel, event_id, received_at, outcome) \
                         VALUES ('b', 'e1', 't', 'accepted')";
    assert!(ok(&pool, other_channel).await);
    let bad = "INSERT INTO channel_events (channel, event_id, received_at, outcome) \
               VALUES ('a', 'e2', 't', 'maybe')";
    assert!(!ok(&pool, bad).await);
}

#[tokio::test]
async fn channels_global_holds_exactly_one_row_without_a_primary() {
    let pool = fresh_pool().await;
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels_global")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
    let primary: Option<String> =
        sqlx::query_scalar("SELECT primary_channel FROM channels_global WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(primary.is_none());
    // CHECK (id = 1): a second row cannot exist.
    assert!(
        !ok(
            &pool,
            "INSERT INTO channels_global (id, primary_channel) VALUES (2, NULL)"
        )
        .await
    );
}

#[tokio::test]
async fn outbox_checks_reject_unknown_values() {
    let pool = fresh_pool().await;
    assert!(ok(&pool, &outbox_insert("ordered", "send", "pending", "turn")).await);
    assert!(!ok(&pool, &outbox_insert("fast", "send", "pending", "turn")).await);
    assert!(
        !ok(
            &pool,
            &outbox_insert("ordered", "delete", "pending", "turn")
        )
        .await
    );
    assert!(!ok(&pool, &outbox_insert("ordered", "send", "queued", "turn")).await);
    assert!(!ok(&pool, &outbox_insert("ordered", "send", "pending", "spam")).await);
    // Every allowed value of each CHECK is accepted.
    assert!(
        ok(
            &pool,
            &outbox_insert("immediate", "answer_action", "done", "command")
        )
        .await
    );
    assert!(
        ok(
            &pool,
            &outbox_insert("ordered", "edit", "leased", "approval")
        )
        .await
    );
    assert!(
        ok(
            &pool,
            &outbox_insert("ordered", "send", "failed", "notification")
        )
        .await
    );
}

/// The last migration before the `progress` class.
const BEFORE_PROGRESS: i64 = 236;

/// `(id, conversation, action, class, state, subject, due_at, attempts, replaced_by, settled_at)`.
type OutboxSnapshot = (
    i64,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    i64,
    Option<i64>,
    Option<String>,
);

#[tokio::test]
async fn migration_0237_accepts_progress_class() {
    let pool = fresh_pool().await;
    assert!(
        ok(
            &pool,
            &outbox_insert("ordered", "edit", "pending", "progress")
        )
        .await
    );
    assert!(
        ok(
            &pool,
            &outbox_insert("ordered", "send", "pending", "progress")
        )
        .await
    );
}

#[tokio::test]
async fn migration_0237_still_rejects_unknown_class() {
    let pool = fresh_pool().await;
    assert!(!ok(&pool, &outbox_insert("ordered", "send", "pending", "spam")).await);
    assert!(!ok(&pool, &outbox_insert("ordered", "send", "pending", "")).await);
    // The other CHECKs survived the rebuild.
    assert!(!ok(&pool, &outbox_insert("fast", "send", "pending", "progress")).await);
    assert!(!ok(&pool, &outbox_insert("ordered", "delete", "pending", "progress")).await);
    assert!(!ok(&pool, &outbox_insert("ordered", "send", "queued", "progress")).await);
}

#[tokio::test]
async fn migration_0237_keeps_existing_rows_and_indexes() {
    let pool = pool_migrated_through(BEFORE_PROGRESS).await;
    for sql in [
        // Row 1 is the target of row 2's `replaced_by`; row 3 is settled.
        "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class,          subject, state, due_at, attempts, platform_refs, fail_reason, created_at, settled_at)          VALUES ('c', 'conv', 'ordered', 'send', '{\"a\":1}', 'notification', 'run:7', 'failed',          't1', 3, '[\"r1\"]', 'thread_not_found', 'k1', 's1')",
        "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class,          state, due_at, replaced_by, created_at)          VALUES ('c', 'home', 'ordered', 'send', '{}', 'turn', 'pending', 't2', 1, 'k2')",
        "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class,          state, due_at, created_at, settled_at)          VALUES ('c', 'conv', 'immediate', 'answer_action', '{}', 'command', 'done', 't3', 'k3', 's3')",
    ] {
        assert!(ok(&pool, sql).await, "{sql}");
    }
    let snapshot = || async {
        sqlx::query_as::<_, OutboxSnapshot>(
            "SELECT id, conversation, action, class, state, subject, due_at, attempts,              replaced_by, settled_at FROM channel_outbox ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    let before = snapshot().await;
    assert_eq!(before.len(), 3);

    apply_migrations_after(&pool, BEFORE_PROGRESS).await;

    assert_eq!(snapshot().await, before);
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'channel_outbox'          AND name LIKE 'channel_outbox_%' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(names, ["channel_outbox_due", "channel_outbox_settled"]);
    let due = index_sql(&pool, "channel_outbox_due").await.unwrap();
    assert!(due.contains("WHERE state IN ('pending', 'leased')"), "{due}");
    let settled = index_sql(&pool, "channel_outbox_settled").await.unwrap();
    assert!(settled.contains("settled_at IS NOT NULL"), "{settled}");

    // The self-reference is intact: no violation, and a dangling `replaced_by` is still refused.
    let violations = sqlx::query("PRAGMA foreign_key_check(channel_outbox)")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(violations.is_empty());
    assert!(
        !ok(
            &pool,
            "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class,              state, due_at, replaced_by, created_at)              VALUES ('c', 'x', 'ordered', 'send', '{}', 'turn', 'pending', 't', 999, 'k')"
        )
        .await
    );
    // Ids keep counting from the old high-water mark.
    let next: i64 = sqlx::query_scalar(
        "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class,          state, due_at, created_at)          VALUES ('c', 'x', 'ordered', 'send', '{}', 'progress', 'pending', 't', 'k') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(next, 4);
}
