//! Database fixtures the test suite shares.
//!
//! It exists because of one gap: `sqlx::migrate!().run()` applies the whole chain against empty
//! tables, so every `UPDATE` in every migration is unreachable by a suite that only ever builds a
//! fresh database — delete one and nothing goes red. Stopping the chain part-way and putting rows
//! in the gap is all it takes to reach them, and that is what these two functions are for.
//!
//! They were written in `vcs.rs` for migration `0049`, with a note asking that the second module
//! to need them move them somewhere neutral rather than copy them. `0129`'s backfill is the second
//! module; this is that somewhere.

use sqlx::sqlite::{SqliteConnectOptions, SqliteOwnedBuf, SqlitePoolOptions};
use tokio::sync::OnceCell;

static MIGRATED_SCHEMA: OnceCell<Vec<u8>> = OnceCell::const_new();

/// A fresh, isolated in-memory database with the current schema.
///
/// The migrator runs once per test process. Each caller receives a separate writable
/// SQLite allocation, including its own copy of `_sqlx_migrations`.
pub async fn fresh_pool() -> sqlx::SqlitePool {
    let schema = MIGRATED_SCHEMA
        .get_or_init(|| async {
            let pool = empty_memory_pool().await;
            sqlx::migrate!("./migrations").run(&pool).await.unwrap();
            let mut connection = pool.acquire().await.unwrap();
            let bytes = connection.serialize(None).await.unwrap().to_vec();
            drop(connection);
            pool.close().await;
            bytes
        })
        .await;

    let pool = empty_memory_pool().await;
    let mut connection = pool.acquire().await.unwrap();
    connection
        .deserialize(
            None,
            SqliteOwnedBuf::try_from(schema.as_slice()).unwrap(),
            false,
        )
        .await
        .unwrap();
    drop(connection);
    pool
}

async fn empty_memory_pool() -> sqlx::SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(":memory:")
                .create_if_missing(true),
        )
        .await
        .unwrap()
}

#[cfg(test)]
mod fresh_pool_tests {
    #[tokio::test]
    async fn fresh_pools_have_the_full_schema_and_do_not_share_rows() {
        let first = super::fresh_pool().await;
        let migrations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&first)
            .await
            .unwrap();
        assert_eq!(
            migrations as usize,
            sqlx::migrate!("./migrations").iter().count()
        );
        sqlx::query(
            "INSERT INTO runs (prompt, status, created_at) VALUES ('only-first', 'running', 'now')",
        )
        .execute(&first)
        .await
        .unwrap();

        let second = super::fresh_pool().await;
        let copied: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE prompt = 'only-first'")
                .fetch_one(&second)
                .await
                .unwrap();
        assert_eq!(copied, 0);
        first.close().await;
        second.close().await;
    }
}

/// A database with every migration up to and including `version` applied, and none after.
///
/// The migrator's own list is walked rather than the files read directly, so this cannot drift
/// from what ships: the SQL is the SQL that will run on the real database, in the order it will
/// run there. Nothing is written to `_sqlx_migrations` — the bookkeeping is not what is under
/// test, and a caller finishes the chain with `apply_migrations_after`.
pub async fn pool_migrated_through(version: i64) -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(":memory:")
                .create_if_missing(true),
        )
        .await
        .unwrap();
    apply_migrations(&pool, |candidate| candidate <= version).await;
    pool
}

/// Finishes the chain a `pool_migrated_through` stopped, running everything above `version`.
pub async fn apply_migrations_after(pool: &sqlx::SqlitePool, version: i64) {
    apply_migrations(pool, |candidate| candidate > version).await;
}

/// Runs exactly one migration, for a test whose claim is about that file alone and must not see
/// what later migrations add on top of it.
pub async fn apply_migration(pool: &sqlx::SqlitePool, version: i64) {
    apply_migrations(pool, |candidate| candidate == version).await;
}

async fn apply_migrations(pool: &sqlx::SqlitePool, wanted: impl Fn(i64) -> bool) {
    for migration in sqlx::migrate!("./migrations").iter() {
        if !wanted(migration.version) {
            continue;
        }
        // `raw_sql` rather than `query`: a migration is many statements, and `query` runs the
        // first and silently drops the rest — which would have made this harness quietly test
        // a fraction of each file.
        sqlx::raw_sql(migration.sql.clone())
            .execute(pool)
            .await
            .unwrap_or_else(|error| panic!("migration {} failed: {error}", migration.version));
    }
}

/// Two migrations with one version number break every database, fresh or not. sqlx keys a
/// migration by its number alone: on an empty database the second insert into `_sqlx_migrations`
/// hits the primary key and the run fails; on one that already applied either file, the other's
/// checksum disagrees and startup stops with `VersionMismatch`. Git sees nothing wrong — the file
/// names differ — so two branches cut from the same tip can each take "the next number" and both
/// land. That is how two `0159_*` files reached master on 2026-10-04.
#[test]
fn no_two_migrations_share_a_version() {
    let mut seen = std::collections::BTreeMap::new();
    for migration in sqlx::migrate!("./migrations").iter() {
        if let Some(first) = seen.insert(migration.version, migration.description.clone()) {
            panic!(
                "migrations '{first}' and '{}' both carry version {} — renumber the one the \
                 live database has not applied yet",
                migration.description, migration.version
            );
        }
    }
}
