//! Tests for the Telegram channel switch.
//!
//! The data migrations of the switch (0234 conversation prefix, 0235 origin, 0236 routing
//! watermark) are tested here against a fixture database built the way the owner's real one looked
//! before the switch: migrated through 230, filled with old-shaped rows, then migrated the rest of
//! the way. The helpers are shared so the later migrations append their tests to this file.

use sqlx::SqlitePool;

use crate::channels::contract::ConversationKey;
use crate::testdb::{apply_migrations_after, pool_migrated_through};

/// The last migration before the switch's own.
const BEFORE_SWITCH: i64 = 230;

/// 0234, verbatim, so a test can apply it a second time on an already migrated database.
const MIGRATION_0234: &str = include_str!("../../migrations/0234_telegram_conversation_prefix.sql");

/// 0235, verbatim, so a test can apply it a second time on an already migrated database.
const MIGRATION_0235: &str = include_str!("../../migrations/0235_telegram_origin_channel.sql");

/// A shell chat: a UUID that exists in `chats` and so is never a Telegram conversation.
const SHELL_UUID: &str = "0c9d2b1e-6f3a-4c57-9a41-5e7d8b2f1a30";
/// A numeric value that nevertheless exists in `chats`: the shape test alone would rewrite it, so
/// only the `NOT EXISTS` half of the predicate keeps it intact.
const NUMERIC_SHELL_CHAT: &str = "777000";

/// A fixture value of a conversation column: what the row holds before the switch, and what it
/// must hold after 0234. `after: None` means the migration must leave the value as it was.
struct Case {
    label: &'static str,
    /// `None` is SQL NULL.
    before: Option<&'static str>,
    /// `None` = unchanged.
    after: Option<&'static str>,
}

/// The six shapes of the plan plus the numeric-but-in-`chats` one. The order is the insertion
/// order, which is how `column_values` reads them back.
static CASES: [Case; 8] = [
    Case {
        label: "numeric supergroup",
        before: Some("-1001234"),
        after: Some("telegram:-1001234"),
    },
    Case {
        label: "private chat",
        before: Some("42"),
        after: Some("telegram:42"),
    },
    Case {
        label: "thread",
        before: Some("-100200:5"),
        after: Some("telegram:-100200:5"),
    },
    Case {
        label: "shell uuid in chats",
        before: Some(SHELL_UUID),
        after: None,
    },
    Case {
        label: "numeric id present in chats",
        before: Some(NUMERIC_SHELL_CHAT),
        after: None,
    },
    Case {
        label: "already prefixed",
        before: Some("telegram:-100999"),
        after: None,
    },
    Case {
        label: "null",
        before: None,
        after: None,
    },
    Case {
        label: "abc",
        before: Some("abc"),
        after: None,
    },
];

/// `(table, column, nullable)` for each of the 12 columns 0234 rewrites. `nullable` is false where
/// the schema forbids NULL, so the NULL case is skipped there.
const CONVERSATION_COLUMNS: [(&str, &str, bool); 12] = [
    ("runs", "chat_id", true),
    ("runs", "launched_by_chat_id", true),
    ("assistant_sessions", "chat_id", false),
    ("chat_tokens", "chat_id", false),
    ("chat_notices", "chat_id", false),
    ("team_runs", "report_to_chat_id", true),
    ("chat_relays", "from_chat_id", false),
    ("chat_relays", "to_chat_id", false),
    ("chat_queue", "chat_id", false),
    ("chat_said_now", "chat_id", false),
    ("chat_tasks", "chat_id", false),
    ("browser_sessions", "owner_chat_id", true),
];

/// The values the fixture puts in a column, in insertion order: every case the column can hold.
fn cases_for(nullable: bool) -> impl Iterator<Item = &'static Case> {
    CASES
        .iter()
        .filter(move |case| nullable || case.before.is_some())
}

/// A database at the pre-switch version, with the old-shaped rows in every conversation column.
/// Foreign keys are off for the fixture only: a `team_runs` row would otherwise drag in a team, an
/// agent and a director just to hold a chat id that none of the tests are about.
async fn pre_switch_pool() -> SqlitePool {
    let pool = pool_migrated_through(BEFORE_SWITCH).await;
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&pool)
        .await
        .unwrap();
    seed_conversation_fixture(&pool).await;
    pool
}

/// [`pre_switch_pool`] migrated the rest of the way: the database as the first start of the switch
/// build leaves it.
async fn migrated_pool() -> SqlitePool {
    let pool = pre_switch_pool().await;
    apply_migrations_after(&pool, BEFORE_SWITCH).await;
    pool
}

/// Two shell chats (see [`SHELL_UUID`], [`NUMERIC_SHELL_CHAT`]) and one row per case in each of
/// the 12 conversation columns.
async fn seed_conversation_fixture(pool: &SqlitePool) {
    for chat_id in [SHELL_UUID, NUMERIC_SHELL_CHAT] {
        sqlx::query("INSERT INTO chats (chat_id, created_at) VALUES (?, '2026-01-01T00:00:00Z')")
            .bind(chat_id)
            .execute(pool)
            .await
            .unwrap();
    }
    let mut serial = 0_i64;
    for (table, column, nullable) in CONVERSATION_COLUMNS {
        for case in cases_for(nullable) {
            serial += 1;
            sqlx::query(sqlx::AssertSqlSafe(insert_sql(table, column, serial)))
                .bind(case.before)
                .execute(pool)
                .await
                .unwrap_or_else(|error| {
                    panic!("fixture row {table}.{column} ({}): {error}", case.label)
                });
        }
    }
}

/// An `INSERT` of one fixture row whose single `?` is the conversation column under test. Every
/// other column is a literal; `serial` makes the unique ones unique. Built from this file's own
/// constants, never from input.
fn insert_sql(table: &str, column: &str, serial: i64) -> String {
    match (table, column) {
        ("runs", _) => format!(
            "INSERT INTO runs ({column}, prompt, status, created_at)
             VALUES (?, 'p', 'completed', '2026-01-01T00:00:00Z')"
        ),
        ("assistant_sessions", _) => {
            "INSERT INTO assistant_sessions (chat_id, session_id, updated_at)
             VALUES (?, 's', '2026-01-01T00:00:00Z')"
                .to_string()
        }
        ("chat_tokens", _) => {
            "INSERT INTO chat_tokens (chat_id, token) VALUES (?, 't')".to_string()
        }
        ("chat_notices", _) => "INSERT INTO chat_notices
                 (chat_id, team_run_id, from_agent_id, from_run_id, body, created_at)
             VALUES (?, 'tr', 'a', 1, 'b', '2026-01-01T00:00:00Z')"
            .to_string(),
        ("team_runs", _) => format!(
            "INSERT INTO team_runs
                 (id, team_id, request, workspace, token, state, created_at, updated_at,
                  report_to_chat_id)
             VALUES ('tr-{serial}', 'team', 'r', 'w', 't', 'done', '2026-01-01T00:00:00Z',
                     '2026-01-01T00:00:00Z', ?)"
        ),
        ("chat_relays", "from_chat_id") => "INSERT INTO chat_relays
                 (from_chat_id, to_chat_id, sending_run_id, body, depth, created_at)
             VALUES (?, 'untouched', 1, 'b', 1, '2026-01-01T00:00:00Z')"
            .to_string(),
        ("chat_relays", _) => "INSERT INTO chat_relays
                 (from_chat_id, to_chat_id, sending_run_id, body, depth, created_at)
             VALUES ('untouched', ?, 1, 'b', 1, '2026-01-01T00:00:00Z')"
            .to_string(),
        ("chat_queue", _) => "INSERT INTO chat_queue (chat_id, text, created_at)
             VALUES (?, 'x', '2026-01-01T00:00:00Z')"
            .to_string(),
        ("chat_said_now", _) => {
            "INSERT INTO chat_said_now (chat_id, run_id, text, origin, created_at)
             VALUES (?, 1, 'x', 'shell', '2026-01-01T00:00:00Z')"
                .to_string()
        }
        ("chat_tasks", _) => format!(
            "INSERT INTO chat_tasks
                 (chat_id, launched_by_run_id, tool_use_id, kind, status, started_at)
             VALUES (?, 1, 'tool-{serial}', 'subagent', 'running', '2026-01-01T00:00:00Z')"
        ),
        ("browser_sessions", _) => "INSERT INTO browser_sessions
                 (sidecar_id, profile_kind, profile_id, requested_url, final_url, rule, mode,
                  opened_at, owner_chat_id)
             VALUES ('sc', 'ephemeral', 'p', 'https://a', 'https://a', 'off-list-ephemeral',
                     'agent', '2026-01-01T00:00:00Z', ?)"
            .to_string(),
        _ => panic!("no fixture insert for {table}.{column}"),
    }
}

/// Every value of one column, in insertion order (NULL as `None`). `rowid` order, since neither
/// every table has an `id` nor does a primary key of text say anything about the order.
///
/// Only the rows the fixture inserted for this very column: a table with two conversation columns
/// (`runs`, `chat_relays`) holds the other column's rows too, where this one is NULL or a literal.
/// The fixture inserts one contiguous block per column, in `CONVERSATION_COLUMNS` order.
async fn column_values(pool: &SqlitePool, table: &str, column: &str) -> Vec<Option<String>> {
    let mut skip = 0;
    let mut take = 0;
    for (t, c, nullable) in CONVERSATION_COLUMNS {
        if t != table {
            continue;
        }
        let rows = cases_for(nullable).count();
        if c == column {
            take = rows;
            break;
        }
        skip += rows;
    }
    sqlx::query_scalar::<_, Option<String>>(sqlx::AssertSqlSafe(format!(
        "SELECT {column} FROM {table} ORDER BY rowid"
    )))
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .skip(skip)
    .take(take)
    .collect()
}

/// All 12 columns, as `(table.column, values)`: one comparable picture of the conversation data.
async fn conversation_snapshot(pool: &SqlitePool) -> Vec<(String, Vec<Option<String>>)> {
    let mut snapshot = Vec::new();
    for (table, column, _) in CONVERSATION_COLUMNS {
        snapshot.push((
            format!("{table}.{column}"),
            column_values(pool, table, column).await,
        ));
    }
    snapshot
}

/// What a column must hold after 0234: the `after` of each case, or the `before` where unchanged.
fn expected_after_0234(nullable: bool) -> Vec<Option<String>> {
    cases_for(nullable)
        .map(|case| case.after.or(case.before).map(str::to_owned))
        .collect()
}

/// The SQL shape of a Telegram conversation (`-?digits` or `-?digits:digits`) over `expr`, for the
/// rollback script. Same grammar as the predicate in 0234, minus the `chats` test.
fn telegram_shape(expr: &str) -> String {
    format!(
        "(({expr} <> '' AND substr({expr}, 1, 1) = '-' AND length({expr}) > 1
            AND substr({expr}, 2) NOT GLOB '*[^0-9]*')
          OR ({expr} <> '' AND {expr} NOT GLOB '*[^0-9]*')
          OR ({expr} NOT GLOB '*:*:*' AND instr({expr}, ':') > 1
              AND substr({expr}, instr({expr}, ':') + 1) <> ''
              AND substr({expr}, instr({expr}, ':') + 1) NOT GLOB '*[^0-9]*'
              AND ((substr({expr}, 1, 1) = '-' AND instr({expr}, ':') > 2
                      AND substr({expr}, 2, instr({expr}, ':') - 2) NOT GLOB '*[^0-9]*')
                   OR substr({expr}, 1, instr({expr}, ':') - 1) NOT GLOB '*[^0-9]*')))"
    )
}

/// The inverse of the switch's data migrations. Not a migration: it lives here and in
/// `C:\nucleos-dev\scratch\rollback-telegram-prefix.sql`, and is only meaningful on a database that
/// held no `telegram:`-prefixed value before the switch (the real one held none).
fn rollback_sql() -> String {
    let mut sql = String::new();
    for (table, column, _) in CONVERSATION_COLUMNS {
        let stripped = format!("substr({column}, 10)");
        sql.push_str(&format!(
            "UPDATE {table} SET {column} = {stripped}
              WHERE {column} GLOB 'telegram:*' AND {};\n",
            telegram_shape(&stripped)
        ));
    }
    for table in ["runs", "chat_queue", "owner_notes"] {
        sql.push_str(&format!(
            "UPDATE {table} SET origin = 'telegram' WHERE origin = 'channel:telegram';\n"
        ));
    }
    sql.push_str("DELETE FROM channel_routing_state WHERE channel = 'telegram';\n");
    sql
}

#[tokio::test]
async fn migration_0234_prefixes_every_telegram_conversation_column() {
    let pool = migrated_pool().await;

    for (table, column, nullable) in CONVERSATION_COLUMNS {
        let values = column_values(&pool, table, column).await;
        assert_eq!(
            values,
            expected_after_0234(nullable),
            "{table}.{column} after 0234"
        );
        assert!(
            values
                .iter()
                .flatten()
                .any(|value| value == "telegram:-1001234"),
            "{table}.{column} must hold a prefixed supergroup id"
        );
    }
}

#[tokio::test]
async fn migration_0234_keeps_shell_uuid_prefixed_and_null_values() {
    let pool = migrated_pool().await;

    for (table, column, nullable) in CONVERSATION_COLUMNS {
        let values = column_values(&pool, table, column).await;
        for kept in [
            Some(SHELL_UUID),
            // The qualified `NOT EXISTS`: this value has the Telegram shape and survives only
            // because it is a `chats` row. An unqualified `chat_id` would make it a tautology.
            Some(NUMERIC_SHELL_CHAT),
            Some("telegram:-100999"),
            Some("abc"),
        ] {
            assert!(
                values.iter().any(|value| value.as_deref() == kept),
                "{table}.{column} lost the untouched value {kept:?}: {values:?}"
            );
        }
        assert_eq!(
            values.iter().any(Option::is_none),
            nullable,
            "{table}.{column} NULL must stay NULL where the column allows it"
        );
        // Nothing may have been prefixed twice or had a prefix invented for it.
        assert!(
            values
                .iter()
                .flatten()
                .all(|value| !value.starts_with("telegram:telegram:")),
            "{table}.{column} was prefixed twice: {values:?}"
        );
    }
    // The shell chats themselves are not a conversation column of 0234 and must be untouched.
    let chats: Vec<String> = sqlx::query_scalar("SELECT chat_id FROM chats ORDER BY rowid")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(chats.contains(&SHELL_UUID.to_owned()));
    assert!(chats.contains(&NUMERIC_SHELL_CHAT.to_owned()));
}

#[tokio::test]
async fn migration_0234_prefixes_thread_ids() {
    let pool = migrated_pool().await;

    for (table, column, nullable) in CONVERSATION_COLUMNS {
        let values = column_values(&pool, table, column).await;
        assert!(
            values
                .iter()
                .any(|value| value.as_deref() == Some("telegram:-100200:5")),
            "{table}.{column} must hold the prefixed thread: {values:?} (expected {:?})",
            expected_after_0234(nullable)
        );
        assert!(
            !values
                .iter()
                .any(|value| value.as_deref() == Some("-100200:5")),
            "{table}.{column} still holds the unprefixed thread"
        );
    }
}

#[tokio::test]
async fn migration_0234_is_idempotent() {
    let pool = migrated_pool().await;
    let once = conversation_snapshot(&pool).await;

    sqlx::raw_sql(MIGRATION_0234).execute(&pool).await.unwrap();
    let twice = conversation_snapshot(&pool).await;

    assert_eq!(once, twice, "a second application must change nothing");
}

#[tokio::test]
async fn migration_0234_rollback_script_restores_fixture() {
    let pool = pre_switch_pool().await;
    let before = conversation_snapshot(&pool).await;
    apply_migrations_after(&pool, BEFORE_SWITCH).await;
    assert_ne!(
        before,
        conversation_snapshot(&pool).await,
        "the fixture must be changed by the migration, or the rollback proves nothing"
    );

    sqlx::raw_sql(sqlx::AssertSqlSafe(rollback_sql()))
        .execute(&pool)
        .await
        .unwrap();

    // The rollback cannot tell a migrated value from one that was already prefixed, so the one
    // `telegram:-100999` row legitimately comes back without its prefix; everything else is as it was.
    let restored = conversation_snapshot(&pool).await;
    for ((name, before_values), (_, restored_values)) in before.iter().zip(&restored) {
        let expected: Vec<Option<String>> = before_values
            .iter()
            .map(|value| match value.as_deref() {
                Some("telegram:-100999") => Some("-100999".to_owned()),
                _ => value.clone(),
            })
            .collect();
        assert_eq!(restored_values, &expected, "{name} after the rollback");
    }
}

#[tokio::test]
async fn migration_0234_adapter_conversation_ref_matches_prefixed_form() {
    let pool = migrated_pool().await;

    // What `sidecars/telegram/adapter/inbound.go` emits as `conversation.ref`: the chat id in
    // decimal, or `<chat>:<thread>` for a topic (`chat + ":" + strconv.FormatInt(thread, 10)`).
    let adapter_refs = [
        ("-1001234".to_owned(), "-1001234".to_owned()),
        ("42".to_owned(), "42".to_owned()),
        (format!("{}:{}", -100200, 5), "-100200:5".to_owned()),
    ];
    let migrated = column_values(&pool, "runs", "chat_id").await;

    for (adapter_ref, old_value) in adapter_refs {
        assert_eq!(adapter_ref, old_value, "the fixture mirrors the adapter");
        let key = ConversationKey::new("telegram", adapter_ref.clone());
        let stored = key.chat_id();
        assert!(
            migrated.contains(&Some(stored.clone())),
            "migrated runs.chat_id must hold {stored}: {migrated:?}"
        );
        // The core reads the migrated value back into the very key the adapter's ref makes.
        assert_eq!(ConversationKey::parse(&stored), Some(key));
        // The old, unprefixed spelling is what the core can no longer read.
        assert_eq!(ConversationKey::parse(&old_value), None);
    }
    // A shell chat is not a channel conversation.
    assert_eq!(ConversationKey::parse(SHELL_UUID), None);
    assert_eq!(ConversationKey::parse("abc"), None);
}

/// The origins the 0235 fixture puts in each table, in insertion order, and what 0235 makes of them.
/// `None` is SQL NULL, which only `runs` and `chat_queue` allow.
const ORIGIN_CASES: [(Option<&str>, Option<&str>); 7] = [
    (Some("telegram"), Some("channel:telegram")),
    (Some("shell"), Some("shell")),
    (Some("voice"), Some("voice")),
    (Some("browser-panel"), Some("browser-panel")),
    (Some("task"), Some("task")),
    (Some("channel:telegram"), Some("channel:telegram")),
    (None, None),
];

/// A pre-switch database that also holds one row per [`ORIGIN_CASES`] entry in `runs`,
/// `chat_queue` and `owner_notes` (NULL skipped where the column is NOT NULL), plus a
/// `chat_said_now` row spelled `telegram`.
async fn pre_switch_pool_with_origins() -> SqlitePool {
    let pool = pre_switch_pool().await;
    for (before, _) in ORIGIN_CASES {
        sqlx::query(
            "INSERT INTO runs (origin, prompt, status, created_at)
             VALUES (?, 'origin-fixture', 'completed', '2026-01-01T00:00:00Z')",
        )
        .bind(before)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO chat_queue (chat_id, text, origin, created_at)
             VALUES ('origin-fixture', 'x', ?, '2026-01-01T00:00:00Z')",
        )
        .bind(before)
        .execute(&pool)
        .await
        .unwrap();
        if before.is_some() {
            sqlx::query(
                "INSERT INTO owner_notes (note_text, origin, created_at, updated_at)
                 VALUES ('n', ?, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            )
            .bind(before)
            .execute(&pool)
            .await
            .unwrap();
        }
    }
    sqlx::query(
        "INSERT INTO chat_said_now (chat_id, run_id, text, origin, created_at)
         VALUES ('said-now-fixture', 1, 'x', 'telegram', '2026-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

/// The `origin` of every row of `table` that the 0235 fixture inserted, in insertion order. The
/// conversation fixture's own rows (other prompts and chats, NULL origin) are left out.
async fn origins_of(pool: &SqlitePool, table: &str) -> Vec<Option<String>> {
    let filter = match table {
        "runs" => "WHERE prompt = 'origin-fixture'",
        "chat_queue" => "WHERE chat_id = 'origin-fixture'",
        _ => "",
    };
    sqlx::query_scalar::<_, Option<String>>(sqlx::AssertSqlSafe(format!(
        "SELECT origin FROM {table} {filter} ORDER BY rowid"
    )))
    .fetch_all(pool)
    .await
    .unwrap()
}

/// What the fixture holds in `table` before 0235 (`before == true`) or after it.
fn expected_origins(table: &str, before: bool) -> Vec<Option<String>> {
    ORIGIN_CASES
        .iter()
        .filter(|(value, _)| table != "owner_notes" || value.is_some())
        .map(|(old, new)| if before { old } else { new }.map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn migration_0235_renames_telegram_origin_in_runs_queue_and_notes() {
    let pool = pre_switch_pool_with_origins().await;
    for table in ["runs", "chat_queue", "owner_notes"] {
        assert_eq!(
            origins_of(&pool, table).await,
            expected_origins(table, true),
            "{table} before 0235"
        );
    }

    apply_migrations_after(&pool, BEFORE_SWITCH).await;

    for table in ["runs", "chat_queue", "owner_notes"] {
        let origins = origins_of(&pool, table).await;
        assert_eq!(
            origins,
            expected_origins(table, false),
            "{table} after 0235"
        );
        assert!(
            !origins.contains(&Some("telegram".to_owned())),
            "{table} still holds the bare telegram origin"
        );
    }
}

#[tokio::test]
async fn migration_0235_leaves_other_origins_alone() {
    let pool = pre_switch_pool_with_origins().await;
    apply_migrations_after(&pool, BEFORE_SWITCH).await;

    for table in ["runs", "chat_queue", "owner_notes"] {
        let origins = origins_of(&pool, table).await;
        for kept in ["shell", "voice", "browser-panel", "task"] {
            assert!(
                origins.contains(&Some(kept.to_owned())),
                "{table} lost the {kept} origin: {origins:?}"
            );
        }
        // Exactly one row was renamed and one was already spelled that way.
        assert_eq!(
            origins
                .iter()
                .filter(|origin| origin.as_deref() == Some("channel:telegram"))
                .count(),
            2,
            "{table}: {origins:?}"
        );
        assert_eq!(
            origins.iter().any(Option::is_none),
            table != "owner_notes",
            "{table}: NULL must stay NULL where the column allows it"
        );
    }
}

#[tokio::test]
async fn migration_0235_is_idempotent() {
    let pool = pre_switch_pool_with_origins().await;
    apply_migrations_after(&pool, BEFORE_SWITCH).await;
    let mut once = Vec::new();
    for table in ["runs", "chat_queue", "owner_notes"] {
        once.push(origins_of(&pool, table).await);
    }

    sqlx::raw_sql(MIGRATION_0235).execute(&pool).await.unwrap();

    let mut twice = Vec::new();
    for table in ["runs", "chat_queue", "owner_notes"] {
        twice.push(origins_of(&pool, table).await);
    }
    assert_eq!(once, twice, "a second application must change nothing");
}

#[tokio::test]
async fn a_queued_message_with_the_migrated_origin_replays_as_a_channel_turn() {
    use crate::assistant::Origin;

    let pool = pre_switch_pool_with_origins().await;
    apply_migrations_after(&pool, BEFORE_SWITCH).await;

    let stored: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT origin FROM chat_queue WHERE chat_id = 'origin-fixture' ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    // The first queued message was a Telegram one before the switch; the drain reads its stored
    // origin back through `from_wire` and must get the channel turn, not a silent `Shell`.
    assert_eq!(stored[0].as_deref(), Some("channel:telegram"));
    assert_eq!(
        Origin::from_wire(stored[0].as_deref()),
        Origin::Channel("telegram")
    );
    assert!(Origin::from_wire(stored[0].as_deref()).is_remote());
    // The pre-existing spelling of a message that was already queued by the new code reads the same.
    assert_eq!(
        Origin::from_wire(stored[5].as_deref()),
        Origin::Channel("telegram")
    );
}

#[tokio::test]
async fn chat_said_now_origin_telegram_is_read_as_before() {
    use crate::assistant::Origin;

    let pool = pre_switch_pool_with_origins().await;
    apply_migrations_after(&pool, BEFORE_SWITCH).await;

    // 0235 does not touch `chat_said_now.origin`: the column keeps the spelling the old code wrote.
    let origin: String =
        sqlx::query_scalar("SELECT origin FROM chat_said_now WHERE chat_id = 'said-now-fixture'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(origin, "telegram");
    // ...and that spelling stays valid on the wire.
    assert_eq!(Origin::from_wire(Some(&origin)), Origin::Telegram);
}

/// 0236, verbatim, so a test can apply it a second time on an already migrated database.
const MIGRATION_0236: &str = include_str!("../../migrations/0236_telegram_routing_watermark.sql");

/// The `telegram` row of `channel_routing_state`, as
/// `(feed_cursor, proposal_cursor, last_kill, last_budget_paused, turn_watermark, seeded_at)`.
type RoutingRow = (i64, i64, i64, i64, i64, String);

async fn telegram_routing_row(pool: &SqlitePool) -> Option<RoutingRow> {
    sqlx::query_as(
        "SELECT feed_cursor, proposal_cursor, last_kill, last_budget_paused, turn_watermark, \
         seeded_at FROM channel_routing_state WHERE channel = 'telegram'",
    )
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// Inserts one run into a (pre-switch) pool and returns its id. `chat_id` is stored as given, so a
/// bare numeric id is what 0234 prefixes afterwards.
async fn insert_switch_run(
    pool: &SqlitePool,
    chat_id: &str,
    status: &str,
    mode: &str,
    origin: Option<&str>,
) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO runs (prompt, status, mode, chat_id, origin, stdout, created_at)
         VALUES ('p', ?, ?, ?, ?, 'Done.', '2026-10-20T00:00:00Z') RETURNING id",
    )
    .bind(status)
    .bind(mode)
    .bind(chat_id)
    .bind(origin)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn max_run_id(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM runs")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A state over `pool` whose `telegram` channel was launched, so the replies discovery looks at it.
async fn live_state_over(pool: SqlitePool) -> crate::state::AppState {
    let mut state = crate::channels::testkit::state().await;
    state.pool = pool;
    state
        .channels
        .presence
        .set_launched("telegram", chrono::Utc::now());
    state
}

#[tokio::test]
async fn migration_0236_watermark_is_max_run_id_without_open_telegram_runs() {
    let pool = pre_switch_pool().await;
    insert_switch_run(&pool, "42", "completed", "assistant", None).await;
    // Open, but not a Telegram conversation: it must not pull the watermark down.
    insert_switch_run(&pool, SHELL_UUID, "running", "assistant", None).await;
    apply_migrations_after(&pool, BEFORE_SWITCH).await;

    let row = telegram_routing_row(&pool).await.expect("a telegram row");
    assert_eq!(row.4, max_run_id(&pool).await);
}

#[tokio::test]
async fn migration_0236_watermark_is_oldest_open_telegram_run_minus_one() {
    let pool = pre_switch_pool().await;
    insert_switch_run(&pool, "42", "completed", "assistant", None).await;
    // An older open turn in a shell chat is not a Telegram turn.
    insert_switch_run(&pool, SHELL_UUID, "running", "assistant", None).await;
    // Bare ids: 0234 prefixes them before 0236 looks, as on the owner's database.
    let oldest_open =
        insert_switch_run(&pool, "42", "running", "assistant", Some("telegram")).await;
    insert_switch_run(&pool, "-100200:5", "completed", "assistant", None).await;
    insert_switch_run(&pool, "42", "pending", "assistant", Some("telegram")).await;
    apply_migrations_after(&pool, BEFORE_SWITCH).await;

    let row = telegram_routing_row(&pool).await.expect("a telegram row");
    assert_eq!(row.4, oldest_open - 1);
    assert!(row.4 < max_run_id(&pool).await);
}

#[tokio::test]
async fn migration_0236_sets_feed_and_proposal_cursors_to_current_max() {
    let pool = pre_switch_pool().await;
    for _ in 0..3 {
        sqlx::query(
            "INSERT INTO feed (kind, summary, created_at) VALUES ('k', 's', '2026-10-20T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
    }
    for _ in 0..2 {
        sqlx::query(
            "INSERT INTO proposals (kind, status, reasoning, created_at)
             VALUES ('tool', 'pending', 'why', '2026-10-20T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
    }
    let max_feed: i64 = sqlx::query_scalar("SELECT MAX(id) FROM feed")
        .fetch_one(&pool)
        .await
        .unwrap();
    let max_proposal: i64 = sqlx::query_scalar("SELECT MAX(id) FROM proposals")
        .fetch_one(&pool)
        .await
        .unwrap();
    apply_migrations_after(&pool, BEFORE_SWITCH).await;

    let row = telegram_routing_row(&pool).await.expect("a telegram row");
    assert_eq!((row.0, row.1), (max_feed, max_proposal));
    assert!(max_feed >= 3 && max_proposal >= 2);
    assert_eq!(
        row.3, 0,
        "the budget state starts as not paused, like `seed`"
    );
    // The `now.to_rfc3339()` shape, so the same readers parse both.
    assert!(
        chrono::DateTime::parse_from_rfc3339(&row.5).is_ok(),
        "seeded_at {:?} is not RFC 3339",
        row.5
    );
}

#[tokio::test]
async fn migration_0236_is_idempotent_and_wins_over_seed_at() {
    let pool = migrated_pool().await;
    let once = telegram_routing_row(&pool).await.expect("a telegram row");

    sqlx::raw_sql(MIGRATION_0236).execute(&pool).await.unwrap();
    assert_eq!(telegram_routing_row(&pool).await, Some(once.clone()));

    // The start-up seed finds the row and leaves it alone, even after the data moved on.
    insert_switch_run(&pool, "telegram:42", "completed", "assistant", None).await;
    {
        let mut conn = pool.acquire().await.unwrap();
        crate::channels::routing::seed_at(&mut conn, "telegram", true, chrono::Utc::now())
            .await
            .unwrap();
    }
    assert_eq!(telegram_routing_row(&pool).await, Some(once));
}

#[tokio::test]
async fn migration_0236_copies_kill_switch() {
    for kill in [1_i64, 0] {
        let pool = pre_switch_pool().await;
        sqlx::query("UPDATE autopilot_global SET kill_switch = ?")
            .bind(kill)
            .execute(&pool)
            .await
            .unwrap();
        apply_migrations_after(&pool, BEFORE_SWITCH).await;

        let row = telegram_routing_row(&pool).await.expect("a telegram row");
        assert_eq!(row.2, kill, "kill_switch {kill}");
    }
}

#[tokio::test]
async fn migration_0236_without_open_runs_resends_nothing() {
    let pool = pre_switch_pool().await;
    insert_switch_run(&pool, "42", "completed", "assistant", Some("telegram")).await;
    apply_migrations_after(&pool, BEFORE_SWITCH).await;
    let state = live_state_over(pool).await;

    let row = telegram_routing_row(&state.pool)
        .await
        .expect("a telegram row");
    assert_eq!(row.4, max_run_id(&state.pool).await);
    let report = crate::channels::replies::discover(&state, chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(report.discovered, 0);
    let replies: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channel_turn_replies")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(replies, 0);
}

#[tokio::test]
async fn migration_0236_ignores_open_task_runs() {
    let pool = pre_switch_pool().await;
    insert_switch_run(&pool, "42", "completed", "assistant", None).await;
    // Open and Telegram-prefixed, yet not assistant turns the sweep would answer.
    insert_switch_run(&pool, "42", "running", "assistant", Some("task")).await;
    insert_switch_run(&pool, "42", "running", "real", None).await;
    apply_migrations_after(&pool, BEFORE_SWITCH).await;

    let row = telegram_routing_row(&pool).await.expect("a telegram row");
    assert_eq!(row.4, max_run_id(&pool).await);
}

#[tokio::test]
async fn migration_0236_adds_no_row_when_telegram_was_never_used() {
    // No fixture: nothing in the database has the Telegram shape.
    let pool = pool_migrated_through(BEFORE_SWITCH).await;
    insert_switch_run(&pool, SHELL_UUID, "running", "assistant", None).await;
    apply_migrations_after(&pool, BEFORE_SWITCH).await;
    assert_eq!(telegram_routing_row(&pool).await, None);

    // A chat token alone is enough to show the instance used Telegram.
    let pool = pool_migrated_through(BEFORE_SWITCH).await;
    sqlx::query("INSERT INTO chat_tokens (chat_id, token) VALUES ('55', 't')")
        .execute(&pool)
        .await
        .unwrap();
    apply_migrations_after(&pool, BEFORE_SWITCH).await;
    let row = telegram_routing_row(&pool).await.expect("a telegram row");
    assert_eq!(row.4, max_run_id(&pool).await);
}

#[tokio::test]
async fn the_replies_sweep_finds_a_run_that_was_open_at_the_switch() {
    let pool = pre_switch_pool().await;
    insert_switch_run(&pool, "42", "completed", "assistant", Some("telegram")).await;
    let open = insert_switch_run(&pool, "42", "running", "assistant", Some("telegram")).await;
    let finished = insert_switch_run(&pool, "42", "completed", "assistant", Some("telegram")).await;
    apply_migrations_after(&pool, BEFORE_SWITCH).await;
    let state = live_state_over(pool).await;

    let report = crate::channels::replies::discover(&state, chrono::Utc::now())
        .await
        .unwrap();
    // The open turn is acknowledged, the one after it is answered; everything before is history.
    assert_eq!((report.discovered, report.acked, report.replied), (2, 1, 1));
    let covered: Vec<i64> =
        sqlx::query_scalar("SELECT run_id FROM channel_turn_replies ORDER BY run_id")
            .fetch_all(&state.pool)
            .await
            .unwrap();
    assert_eq!(covered, vec![open, finished]);
}
