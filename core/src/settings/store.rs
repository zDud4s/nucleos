//! Reads and writes the `settings` and `settings_history` tables. Every write is one transaction (the row, its
//! history line and a feed line) and swaps the in-memory snapshot only after the commit.

use std::sync::Arc;

use serde_json::Value;
use sqlx::SqlitePool;

use super::SettingsError;
use super::decode::Prepared;
use super::registry::{self, DocType, Document};
use super::scope::Scope;
use super::snapshot::{Entry, Settings, Snapshot, Unreadable};
use crate::feed;

/// One version of one document, as `settings_history` keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRow {
    pub version: i64,
    pub body_json: String,
    pub written_at: String,
    pub written_by: String,
}

impl HistoryRow {
    /// A deletion is recorded as the body `null`.
    pub fn deleted(&self) -> bool {
        self.body_json == "null"
    }
}

/// What a write does to a document.
enum Change {
    Write(Prepared),
    Delete,
}

/// A `settings` row: scope, name, body, version, updated_at, updated_by.
type Row = (String, String, String, i64, String, String);

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The document type for `name`, checked against the kind of `scope`.
fn lookup(scope: &Scope, name: &str) -> Result<&'static DocType, SettingsError> {
    let doc =
        registry::doc_type(name).ok_or_else(|| SettingsError::UnknownDocument(name.to_owned()))?;
    if doc.kind != scope.kind() {
        return Err(SettingsError::WrongScope {
            name: name.to_owned(),
            scope: scope.clone(),
        });
    }
    Ok(doc)
}

/// Every stored row, decoded. A row that names no known document or scope is skipped with a warning (an older
/// daemon reading a newer table); a row that names a known document but does not decode is recorded as
/// unreadable and left out of the entries, so readers fall back to the default.
pub async fn load_all(pool: &SqlitePool) -> Result<Snapshot, SettingsError> {
    let rows = sqlx::query_as::<_, Row>(
        "SELECT scope, name, body_json, version, updated_at, updated_by FROM settings ORDER BY scope, name",
    )
    .fetch_all(pool)
    .await?;
    let mut snapshot = Snapshot::default();
    for (scope_text, name, body_json, version, updated_at, updated_by) in rows {
        let scope = match scope_text.parse::<Scope>() {
            Ok(scope) => scope,
            Err(error) => {
                tracing::warn!(%error, %name, "settings row with an unknown scope skipped");
                continue;
            }
        };
        let Some(doc) = registry::doc_type(&name) else {
            tracing::warn!(%scope, %name, "settings row for an unknown document skipped");
            continue;
        };
        if doc.kind != scope.kind() {
            tracing::warn!(%scope, %name, "settings row at the wrong kind of scope skipped");
            continue;
        }
        match (doc.load)(&body_json) {
            Ok(value) => snapshot.insert(
                &scope,
                &name,
                Entry {
                    version,
                    updated_at,
                    updated_by,
                    canonical: Arc::from(body_json),
                    value,
                },
            ),
            Err(reason) => {
                tracing::warn!(%scope, %name, version, %reason, "stored settings document cannot be read; using the default");
                snapshot.mark_unreadable(Unreadable {
                    scope,
                    name,
                    version,
                    reason,
                });
            }
        }
    }
    Ok(snapshot)
}

/// Every version of one document, newest first. No registry lookup: the history of a document this daemon no
/// longer knows is still readable.
pub async fn history(
    pool: &SqlitePool,
    scope: &Scope,
    name: &str,
) -> Result<Vec<HistoryRow>, SettingsError> {
    let rows = sqlx::query_as::<_, (i64, String, String, String)>(
        "SELECT version, body_json, written_at, written_by FROM settings_history \
         WHERE scope = ? AND name = ? ORDER BY version DESC",
    )
    .bind(scope.to_string())
    .bind(name)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(version, body_json, written_at, written_by)| HistoryRow {
            version,
            body_json,
            written_at,
            written_by,
        })
        .collect())
}

impl Settings {
    /// Replace the snapshot with the table's contents.
    pub async fn load(&self, pool: &SqlitePool) -> Result<(), SettingsError> {
        let _writes = self.lock_writes().await;
        let snapshot = load_all(pool).await?;
        self.swap(snapshot);
        Ok(())
    }

    /// Validate `body` strictly and store it as the next version. `expected` is the version the caller last saw
    /// (`Some(0)`: the document must not exist yet; `None`: no check). Returns the new version.
    pub async fn put(
        &self,
        pool: &SqlitePool,
        scope: &Scope,
        name: &str,
        body: &Value,
        by: &str,
        expected: Option<i64>,
    ) -> Result<i64, SettingsError> {
        let doc = lookup(scope, name)?;
        let prepared = (doc.prepare)(body).map_err(SettingsError::Invalid)?;
        let _writes = self.lock_writes().await;
        self.commit_locked(pool, scope, doc, Change::Write(prepared), by, expected)
            .await
    }

    /// Read-modify-write on a typed document: start from the stored value (or the default), apply `change`,
    /// validate and store the result. The read and the write happen under one hold of the write lock, so two
    /// concurrent updates compose instead of overwriting each other.
    pub async fn update<T, F>(
        &self,
        pool: &SqlitePool,
        scope: &Scope,
        by: &str,
        change: F,
    ) -> Result<i64, SettingsError>
    where
        T: Document + Default,
        F: FnOnce(&mut T) + Send,
    {
        let doc = lookup(scope, T::NAME)?;
        let _writes = self.lock_writes().await;
        let snapshot = self.snapshot();
        if let Some(record) = snapshot.unreadable_for(scope, T::NAME) {
            return Err(SettingsError::Unreadable {
                name: T::NAME.to_owned(),
                reason: record.reason.clone(),
            });
        }
        let mut typed: T = (*snapshot.get::<T>(scope)).clone();
        change(&mut typed);
        let body = serde_json::to_value(&typed)
            .map_err(|error| SettingsError::Invalid(error.to_string()))?;
        let prepared = (doc.prepare)(&body).map_err(SettingsError::Invalid)?;
        self.commit_locked(pool, scope, doc, Change::Write(prepared), by, None)
            .await
    }

    /// Write the body of an earlier version as a NEW version (history is append-only). The body is validated
    /// again under today's rules: a version whose fields this daemon no longer knows is refused, not repaired.
    pub async fn rollback(
        &self,
        pool: &SqlitePool,
        scope: &Scope,
        name: &str,
        to_version: i64,
        by: &str,
    ) -> Result<i64, SettingsError> {
        let doc = lookup(scope, name)?;
        // Read before taking the write lock: history rows are append-only, so the body of `to_version`
        // cannot change between this read and the commit below.
        let old: Option<String> = sqlx::query_scalar(
            "SELECT body_json FROM settings_history WHERE scope = ? AND name = ? AND version = ?",
        )
        .bind(scope.to_string())
        .bind(name)
        .bind(to_version)
        .fetch_optional(pool)
        .await?;
        let old = old.ok_or(SettingsError::NotFound)?;
        if old == "null" {
            return Err(SettingsError::Invalid(format!(
                "version {to_version} is a deletion"
            )));
        }
        let body: Value = serde_json::from_str(&old)
            .map_err(|error| SettingsError::Invalid(error.to_string()))?;
        let prepared = (doc.prepare)(&body).map_err(SettingsError::Invalid)?;
        let _writes = self.lock_writes().await;
        self.commit_locked(pool, scope, doc, Change::Write(prepared), by, None)
            .await
    }

    /// Remove a document, falling back to its default. Recorded as a history row with the body `null`, so the
    /// version sequence continues. Returns that row's version.
    pub async fn delete(
        &self,
        pool: &SqlitePool,
        scope: &Scope,
        name: &str,
        by: &str,
        expected: Option<i64>,
    ) -> Result<i64, SettingsError> {
        let doc = lookup(scope, name)?;
        let _writes = self.lock_writes().await;
        self.commit_locked(pool, scope, doc, Change::Delete, by, expected)
            .await
    }

    /// One transaction: the `settings` row, the `settings_history` row and the feed line. The snapshot is
    /// swapped only after `commit()` succeeded, and the caller still holds the write lock, so no reader ever
    /// sees a change that did not persist and no writer sees a snapshot behind the table.
    async fn commit_locked(
        &self,
        pool: &SqlitePool,
        scope: &Scope,
        doc: &'static DocType,
        change: Change,
        by: &str,
        expected: Option<i64>,
    ) -> Result<i64, SettingsError> {
        let scope_text = scope.to_string();
        // IMMEDIATE takes the write lock up front: a deferred transaction that reads and then writes
        // fails with SQLITE_BUSY_SNAPSHOT under WAL when another connection committed in between
        // (same pattern as `distill.rs` and `verify/runs.rs`).
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;

        let current: Option<i64> =
            sqlx::query_scalar("SELECT version FROM settings WHERE scope = ? AND name = ?")
                .bind(&scope_text)
                .bind(doc.name)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(expected) = expected {
            let actual = current.unwrap_or(0);
            if actual != expected {
                return Err(SettingsError::Stale { current: actual });
            }
        }
        if matches!(change, Change::Delete) && current.is_none() {
            return Err(SettingsError::NotFound);
        }

        // The sequence never restarts: a deletion leaves its history row behind, and a row inserted by hand
        // without history still counts.
        let highest: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(version), 0) FROM settings_history WHERE scope = ? AND name = ?",
        )
        .bind(&scope_text)
        .bind(doc.name)
        .fetch_one(&mut *tx)
        .await?;
        let next = highest.max(current.unwrap_or(0)) + 1;
        let stamp = now();

        let (history_body, verb) = match &change {
            Change::Write(prepared) => {
                sqlx::query(
                    "INSERT INTO settings (scope, name, body_json, version, updated_at, updated_by) \
                     VALUES (?, ?, ?, ?, ?, ?) \
                     ON CONFLICT(scope, name) DO UPDATE SET body_json = excluded.body_json, \
                     version = excluded.version, updated_at = excluded.updated_at, \
                     updated_by = excluded.updated_by",
                )
                .bind(&scope_text)
                .bind(doc.name)
                .bind(&prepared.canonical)
                .bind(next)
                .bind(&stamp)
                .bind(by)
                .execute(&mut *tx)
                .await?;
                (prepared.canonical.as_str(), "wrote")
            }
            Change::Delete => {
                sqlx::query("DELETE FROM settings WHERE scope = ? AND name = ?")
                    .bind(&scope_text)
                    .bind(doc.name)
                    .execute(&mut *tx)
                    .await?;
                ("null", "deleted")
            }
        };
        sqlx::query(
            "INSERT INTO settings_history (scope, name, version, body_json, written_at, written_by) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&scope_text)
        .bind(doc.name)
        .bind(next)
        .bind(history_body)
        .bind(&stamp)
        .bind(by)
        .execute(&mut *tx)
        .await?;

        // The feed line names the document and who wrote it, never the body.
        let summary = format!("{verb} {scope} {} v{next} by {by}", doc.name);
        feed::append_on(
            &mut tx,
            scope.project_id(),
            "config_written",
            &summary,
            None,
            None,
        )
        .await?;
        tx.commit().await?;

        let mut snapshot = (*self.snapshot()).clone();
        match change {
            Change::Write(prepared) => snapshot.insert(
                scope,
                doc.name,
                Entry {
                    version: next,
                    updated_at: stamp,
                    updated_by: by.to_owned(),
                    canonical: Arc::from(prepared.canonical),
                    value: prepared.value,
                },
            ),
            Change::Delete => snapshot.remove(scope, doc.name),
        }
        self.swap(snapshot);
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::SqlitePool;

    use super::*;
    use crate::config::{AutopilotRules, VerifyConfig};

    async fn fixture() -> (SqlitePool, Settings) {
        (crate::testdb::fresh_pool().await, Settings::new())
    }

    async fn count(pool: &SqlitePool, table: &str) -> i64 {
        // audited: `table` is a literal chosen by the test.
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn put_stores_the_row_the_history_and_swaps_the_snapshot() {
        let (pool, settings) = fixture().await;
        let version = settings
            .put(
                &pool,
                &Scope::Machine,
                "verify",
                &json!({"capacity": 9}),
                "owner",
                None,
            )
            .await
            .unwrap();
        assert_eq!(version, 1);
        assert_eq!(
            settings
                .snapshot()
                .get::<VerifyConfig>(&Scope::Machine)
                .capacity,
            9
        );

        let row: (String, i64, String) = sqlx::query_as(
            "SELECT body_json, version, updated_by FROM settings WHERE scope = 'machine' AND name = 'verify'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((row.1, row.2.as_str()), (1, "owner"));
        let stored: serde_json::Value = serde_json::from_str(&row.0).unwrap();
        assert_eq!(stored["capacity"], 9);
        assert_eq!(
            stored["cache_days"], 7,
            "defaults are written out, so the row is self-describing"
        );

        let rows = history(&pool, &Scope::Machine, "verify").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].body_json, row.0);
        assert!(!rows[0].deleted());
    }

    #[tokio::test]
    async fn a_second_put_bumps_the_version_and_keeps_the_first_in_history() {
        let (pool, settings) = fixture().await;
        settings
            .put(
                &pool,
                &Scope::Machine,
                "verify",
                &json!({"capacity": 2}),
                "a",
                None,
            )
            .await
            .unwrap();
        let second = settings
            .put(
                &pool,
                &Scope::Machine,
                "verify",
                &json!({"capacity": 3}),
                "b",
                None,
            )
            .await
            .unwrap();
        assert_eq!(second, 2);
        assert_eq!(
            settings.snapshot().version(&Scope::Machine, "verify"),
            Some(2)
        );
        assert_eq!(count(&pool, "settings").await, 1);
        let rows = history(&pool, &Scope::Machine, "verify").await.unwrap();
        assert_eq!(
            rows.iter().map(|row| row.version).collect::<Vec<_>>(),
            vec![2, 1],
            "newest first"
        );
        assert_eq!(rows[1].written_by, "a");
    }

    #[tokio::test]
    async fn an_invalid_body_changes_nothing() {
        let (pool, settings) = fixture().await;
        let error = settings
            .put(
                &pool,
                &Scope::Machine,
                "verify",
                &json!({"capacity": 0}),
                "owner",
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&error, SettingsError::Invalid(why) if why.contains("capacity must be at least 1"))
        );
        let typo = settings
            .put(
                &pool,
                &Scope::Machine,
                "verify",
                &json!({"capasity": 3}),
                "owner",
                None,
            )
            .await;
        assert!(matches!(typo, Err(SettingsError::Invalid(_))));
        assert_eq!(count(&pool, "settings").await, 0);
        assert_eq!(count(&pool, "settings_history").await, 0);
        assert!(
            settings
                .snapshot()
                .get_opt::<VerifyConfig>(&Scope::Machine)
                .is_none()
        );
    }

    #[tokio::test]
    async fn an_unknown_document_and_a_wrong_scope_are_refused() {
        let (pool, settings) = fixture().await;
        let put = |scope: Scope, name: &'static str| {
            let (pool, settings) = (pool.clone(), settings.clone());
            async move {
                settings
                    .put(&pool, &scope, name, &json!({}), "t", None)
                    .await
            }
        };
        assert!(
            matches!(put(Scope::Machine, "nope").await, Err(SettingsError::UnknownDocument(name)) if name == "nope")
        );
        assert!(matches!(
            put(Scope::project("p1"), "verify").await,
            Err(SettingsError::WrongScope { .. })
        ));
        assert!(matches!(
            put(Scope::Machine, "autopilot").await,
            Err(SettingsError::WrongScope { .. })
        ));
        assert_eq!(count(&pool, "settings").await, 0);
    }

    #[tokio::test]
    async fn a_stale_expected_version_is_refused() {
        let (pool, settings) = fixture().await;
        let body = json!({"capacity": 2});
        settings
            .put(&pool, &Scope::Machine, "verify", &body, "t", Some(0))
            .await
            .expect("0 means must not exist");
        let stale = settings
            .put(&pool, &Scope::Machine, "verify", &body, "t", Some(0))
            .await;
        assert!(matches!(stale, Err(SettingsError::Stale { current: 1 })));
        let behind = settings
            .put(&pool, &Scope::Machine, "verify", &body, "t", Some(7))
            .await;
        assert!(matches!(behind, Err(SettingsError::Stale { current: 1 })));
        assert_eq!(
            settings
                .put(&pool, &Scope::Machine, "verify", &body, "t", Some(1))
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn a_failed_commit_leaves_the_snapshot_and_the_table_alone() {
        let (pool, settings) = fixture().await;
        sqlx::query("DROP TABLE settings_history")
            .execute(&pool)
            .await
            .unwrap();
        let result = settings
            .put(
                &pool,
                &Scope::Machine,
                "verify",
                &json!({"capacity": 9}),
                "t",
                None,
            )
            .await;
        assert!(
            matches!(result, Err(SettingsError::Database(_))),
            "{result:?}"
        );
        assert!(
            settings
                .snapshot()
                .get_opt::<VerifyConfig>(&Scope::Machine)
                .is_none()
        );
        assert_eq!(
            count(&pool, "settings").await,
            0,
            "the upsert was rolled back with the failed insert"
        );
    }

    #[tokio::test]
    async fn load_replaces_the_snapshot_from_the_table() {
        let (pool, writer) = fixture().await;
        writer
            .put(
                &pool,
                &Scope::Machine,
                "verify",
                &json!({"capacity": 6}),
                "t",
                None,
            )
            .await
            .unwrap();
        let reader = Settings::new();
        assert!(
            reader
                .snapshot()
                .get_opt::<VerifyConfig>(&Scope::Machine)
                .is_none()
        );
        reader.load(&pool).await.unwrap();
        assert_eq!(
            reader
                .snapshot()
                .get::<VerifyConfig>(&Scope::Machine)
                .capacity,
            6
        );
        assert_eq!(
            reader.snapshot().version(&Scope::Machine, "verify"),
            Some(1)
        );
    }

    #[tokio::test]
    async fn a_row_with_a_key_this_daemon_does_not_know_still_loads() {
        let (pool, settings) = fixture().await;
        insert_raw(
            &pool,
            "machine",
            "verify",
            r#"{"capacity":3,"from_the_future":1}"#,
            1,
        )
        .await;
        settings.load(&pool).await.unwrap();
        let snapshot = settings.snapshot();
        assert_eq!(snapshot.get::<VerifyConfig>(&Scope::Machine).capacity, 3);
        assert!(snapshot.unreadable().is_empty());
        assert!(
            snapshot
                .entry(&Scope::Machine, "verify")
                .unwrap()
                .canonical
                .contains("from_the_future")
        );
    }

    #[tokio::test]
    async fn rows_that_name_no_known_document_or_scope_are_skipped() {
        let (pool, settings) = fixture().await;
        insert_raw(&pool, "weird", "verify", "{}", 1).await;
        insert_raw(&pool, "machine", "nope", "{}", 1).await;
        insert_raw(&pool, "project:p1", "verify", "{}", 1).await; // verify is a machine document
        insert_raw(&pool, "machine", "web", "{}", 1).await;
        settings.load(&pool).await.unwrap();
        let snapshot = settings.snapshot();
        assert_eq!(snapshot.iter().count(), 1, "only the valid row is loaded");
        assert!(
            snapshot.unreadable().is_empty(),
            "skipped rows are not 'unreadable'"
        );
    }

    #[tokio::test]
    async fn an_unreadable_row_falls_back_to_the_default_and_is_recorded() {
        let (pool, settings) = fixture().await;
        insert_raw(&pool, "machine", "verify", r#"{"capacity":"many"}"#, 3).await;
        settings.load(&pool).await.unwrap();
        let snapshot = settings.snapshot();
        assert_eq!(snapshot.get::<VerifyConfig>(&Scope::Machine).capacity, 4);
        let record = snapshot
            .unreadable_for(&Scope::Machine, "verify")
            .expect("recorded");
        assert_eq!((record.version, record.name.as_str()), (3, "verify"));
        assert!(record.reason.contains("invalid type"), "{}", record.reason);

        // A valid write replaces it, and the version carries on from the stored row.
        let version = settings
            .put(
                &pool,
                &Scope::Machine,
                "verify",
                &json!({"capacity": 5}),
                "t",
                None,
            )
            .await
            .unwrap();
        assert_eq!(version, 4);
        assert!(!settings.snapshot().is_unreadable(&Scope::Machine, "verify"));
    }

    #[tokio::test]
    async fn a_write_appends_a_config_written_line_to_the_feed_without_the_body() {
        let (pool, settings) = fixture().await;
        settings
            .put(
                &pool,
                &Scope::Machine,
                "verify",
                &json!({"capacity": 9}),
                "owner",
                None,
            )
            .await
            .unwrap();
        let machine = crate::feed::list_feed(&pool, None, 10).await.unwrap();
        assert_eq!(machine.len(), 1);
        assert_eq!(machine[0].kind, "config_written");
        assert!(machine[0].summary.contains("verify") && machine[0].summary.contains("v1"));
        assert!(machine[0].summary.contains("owner"));
        assert!(
            !machine[0].summary.contains("capacity"),
            "the feed never carries a body"
        );

        let body = serde_json::to_value(AutopilotRules::default()).unwrap();
        settings
            .put(
                &pool,
                &Scope::project("p1"),
                "autopilot",
                &body,
                "owner",
                None,
            )
            .await
            .unwrap();
        let project = crate::feed::list_feed(&pool, Some("p1"), 10).await.unwrap();
        assert_eq!(project.len(), 1);
        assert_eq!(project[0].kind, "config_written");
    }

    #[tokio::test]
    async fn project_scopes_are_independent() {
        let (pool, settings) = fixture().await;
        let rules = serde_json::to_value(AutopilotRules::default()).unwrap();
        let with_gate = json!({"gate_command": "bash scripts/gates.sh all"});
        settings
            .put(&pool, &Scope::project("p1"), "autopilot", &rules, "t", None)
            .await
            .unwrap();
        settings
            .put(
                &pool,
                &Scope::project("p1"),
                "autopilot",
                &with_gate,
                "t",
                None,
            )
            .await
            .unwrap();
        settings
            .put(&pool, &Scope::project("p2"), "autopilot", &rules, "t", None)
            .await
            .unwrap();
        let snapshot = settings.snapshot();
        assert_eq!(
            snapshot.version(&Scope::project("p1"), "autopilot"),
            Some(2)
        );
        assert_eq!(
            snapshot.version(&Scope::project("p2"), "autopilot"),
            Some(1)
        );
        assert_eq!(
            snapshot
                .get::<AutopilotRules>(&Scope::project("p1"))
                .gate_command
                .as_deref(),
            Some("bash scripts/gates.sh all")
        );
        assert_eq!(
            snapshot
                .get::<AutopilotRules>(&Scope::project("p2"))
                .gate_command,
            None
        );
    }

    async fn insert_raw(pool: &SqlitePool, scope: &str, name: &str, body: &str, version: i64) {
        sqlx::query(
            "INSERT INTO settings (scope, name, body_json, version, updated_at, updated_by) \
             VALUES (?, ?, ?, ?, '2026-10-11T00:00:00.000Z', 'test')",
        )
        .bind(scope)
        .bind(name)
        .bind(body)
        .bind(version)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn update_starts_from_the_default_and_applies_the_closure() {
        let (pool, settings) = fixture().await;
        let version = settings
            .update::<VerifyConfig, _>(&pool, &Scope::Machine, "owner", |config| {
                config.capacity = 7
            })
            .await
            .unwrap();
        assert_eq!(version, 1);
        let config = settings.snapshot().get::<VerifyConfig>(&Scope::Machine);
        assert_eq!(
            (config.capacity, config.cache_days),
            (7, 7),
            "the rest keeps the default"
        );
    }

    #[tokio::test]
    async fn update_refuses_a_result_that_fails_validation() {
        let (pool, settings) = fixture().await;
        let result = settings
            .update::<VerifyConfig, _>(&pool, &Scope::Machine, "owner", |config| {
                config.capacity = 0
            })
            .await;
        assert!(
            matches!(result, Err(SettingsError::Invalid(why)) if why.contains("capacity must be at least 1"))
        );
        assert_eq!(count(&pool, "settings").await, 0);
    }

    #[tokio::test]
    async fn update_refuses_the_wrong_scope_kind() {
        let (pool, settings) = fixture().await;
        let result = settings
            .update::<VerifyConfig, _>(&pool, &Scope::project("p1"), "owner", |config| {
                config.capacity = 3
            })
            .await;
        assert!(matches!(result, Err(SettingsError::WrongScope { .. })));
    }

    #[tokio::test]
    async fn two_updates_at_once_both_land() {
        let (pool, settings) = fixture().await;
        let (a, b) = tokio::join!(
            settings.update::<VerifyConfig, _>(&pool, &Scope::Machine, "a", |config| config
                .capacity +=
                1),
            settings.update::<VerifyConfig, _>(&pool, &Scope::Machine, "b", |config| config
                .cache_days +=
                1),
        );
        a.unwrap();
        b.unwrap();
        let config = settings.snapshot().get::<VerifyConfig>(&Scope::Machine);
        assert_eq!(
            (config.capacity, config.cache_days),
            (5, 8),
            "neither update overwrote the other"
        );
        let versions: Vec<i64> = history(&pool, &Scope::Machine, "verify")
            .await
            .unwrap()
            .iter()
            .map(|row| row.version)
            .collect();
        assert_eq!(versions, vec![2, 1]);
    }

    #[tokio::test]
    async fn update_refuses_to_build_on_an_unreadable_document() {
        let (pool, settings) = fixture().await;
        insert_raw(&pool, "machine", "verify", r#"{"capacity":"many"}"#, 3).await;
        settings.load(&pool).await.unwrap();
        let result = settings
            .update::<VerifyConfig, _>(&pool, &Scope::Machine, "owner", |config| {
                config.capacity = 3
            })
            .await;
        assert!(matches!(result, Err(SettingsError::Unreadable { name, .. }) if name == "verify"));
    }

    #[tokio::test]
    async fn rollback_writes_an_old_body_as_a_new_version() {
        let (pool, settings) = fixture().await;
        settings
            .put(
                &pool,
                &Scope::Machine,
                "verify",
                &json!({"capacity": 2}),
                "a",
                None,
            )
            .await
            .unwrap();
        settings
            .put(
                &pool,
                &Scope::Machine,
                "verify",
                &json!({"capacity": 3}),
                "b",
                None,
            )
            .await
            .unwrap();
        let version = settings
            .rollback(&pool, &Scope::Machine, "verify", 1, "owner")
            .await
            .unwrap();
        assert_eq!(version, 3, "a rollback is a write, never a rewind");
        assert_eq!(
            settings
                .snapshot()
                .get::<VerifyConfig>(&Scope::Machine)
                .capacity,
            2
        );
        let rows = history(&pool, &Scope::Machine, "verify").await.unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0].body_json, rows[2].body_json,
            "the new version holds version 1's body"
        );
        assert_eq!(rows[0].written_by, "owner");
    }

    #[tokio::test]
    async fn rollback_refuses_a_missing_version_and_a_deletion() {
        let (pool, settings) = fixture().await;
        settings
            .put(
                &pool,
                &Scope::Machine,
                "verify",
                &json!({"capacity": 2}),
                "a",
                None,
            )
            .await
            .unwrap();
        settings
            .delete(&pool, &Scope::Machine, "verify", "a", None)
            .await
            .unwrap();
        let missing = settings
            .rollback(&pool, &Scope::Machine, "verify", 9, "a")
            .await;
        assert!(matches!(missing, Err(SettingsError::NotFound)));
        let deletion = settings
            .rollback(&pool, &Scope::Machine, "verify", 2, "a")
            .await;
        assert!(
            matches!(deletion, Err(SettingsError::Invalid(why)) if why.contains("is a deletion"))
        );
        let unknown = settings
            .rollback(&pool, &Scope::Machine, "nope", 1, "a")
            .await;
        assert!(matches!(unknown, Err(SettingsError::UnknownDocument(_))));
    }

    #[tokio::test]
    async fn rollback_brings_back_a_deleted_document() {
        let (pool, settings) = fixture().await;
        settings
            .put(
                &pool,
                &Scope::Machine,
                "verify",
                &json!({"capacity": 2}),
                "a",
                None,
            )
            .await
            .unwrap();
        settings
            .delete(&pool, &Scope::Machine, "verify", "a", None)
            .await
            .unwrap();
        assert!(
            settings
                .snapshot()
                .get_opt::<VerifyConfig>(&Scope::Machine)
                .is_none()
        );
        let version = settings
            .rollback(&pool, &Scope::Machine, "verify", 1, "a")
            .await
            .unwrap();
        assert_eq!(version, 3);
        assert_eq!(
            settings
                .snapshot()
                .get::<VerifyConfig>(&Scope::Machine)
                .capacity,
            2
        );
    }

    #[tokio::test]
    async fn delete_removes_the_row_and_the_sequence_carries_on() {
        let (pool, settings) = fixture().await;
        settings
            .put(
                &pool,
                &Scope::Machine,
                "verify",
                &json!({"capacity": 2}),
                "a",
                None,
            )
            .await
            .unwrap();
        let version = settings
            .delete(&pool, &Scope::Machine, "verify", "b", None)
            .await
            .unwrap();
        assert_eq!(version, 2);
        assert_eq!(count(&pool, "settings").await, 0);
        assert!(
            settings
                .snapshot()
                .get_opt::<VerifyConfig>(&Scope::Machine)
                .is_none()
        );
        assert_eq!(
            settings
                .snapshot()
                .get::<VerifyConfig>(&Scope::Machine)
                .capacity,
            4,
            "back to the default"
        );
        let rows = history(&pool, &Scope::Machine, "verify").await.unwrap();
        assert!(rows[0].deleted());
        assert_eq!(rows[0].written_by, "b");

        let recreated = settings
            .put(
                &pool,
                &Scope::Machine,
                "verify",
                &json!({"capacity": 5}),
                "a",
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            recreated, 3,
            "a re-created document does not restart at version 1"
        );
    }

    #[tokio::test]
    async fn delete_of_an_absent_document_is_not_found_and_honours_the_expected_version() {
        let (pool, settings) = fixture().await;
        let absent = settings
            .delete(&pool, &Scope::Machine, "verify", "a", None)
            .await;
        assert!(matches!(absent, Err(SettingsError::NotFound)));
        assert_eq!(
            count(&pool, "settings_history").await,
            0,
            "nothing was recorded"
        );

        settings
            .put(
                &pool,
                &Scope::Machine,
                "verify",
                &json!({"capacity": 2}),
                "a",
                None,
            )
            .await
            .unwrap();
        let stale = settings
            .delete(&pool, &Scope::Machine, "verify", "a", Some(5))
            .await;
        assert!(matches!(stale, Err(SettingsError::Stale { current: 1 })));
        assert_eq!(
            settings
                .delete(&pool, &Scope::Machine, "verify", "a", Some(1))
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn delete_clears_an_unreadable_document() {
        let (pool, settings) = fixture().await;
        insert_raw(&pool, "machine", "verify", r#"{"capacity":"many"}"#, 3).await;
        settings.load(&pool).await.unwrap();
        assert!(settings.snapshot().is_unreadable(&Scope::Machine, "verify"));
        assert_eq!(
            settings
                .delete(&pool, &Scope::Machine, "verify", "owner", None)
                .await
                .unwrap(),
            4
        );
        assert!(!settings.snapshot().is_unreadable(&Scope::Machine, "verify"));
    }

    #[tokio::test]
    async fn history_of_a_document_never_written_is_empty() {
        let (pool, _settings) = fixture().await;
        assert!(
            history(&pool, &Scope::Machine, "verify")
                .await
                .unwrap()
                .is_empty()
        );
    }
}
