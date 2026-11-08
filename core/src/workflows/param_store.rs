//! §spec workflow-auto-afinacao
//!
//! The append-only value history of a declared workflow parameter, and its auto/fixed mode.
//!
//! Values are opaque JSON: this module never reads what a key means. Conventions copy the
//! `settings` store: history is append-only, a version never restarts, and every write also leaves
//! a feed line that names who and what, never the value. `append` and `set_mode_on` run on a
//! caller-owned transaction so the service can make them atomic with the file write.

use serde_json::Value;
use sqlx::{SqliteConnection, SqlitePool};

use crate::feed;
use crate::settings::Scope;

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Why a history row exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A value the daemon found and never wrote: a starting point.
    Baseline,
    Set,
    Rollback,
    /// The owner edited the file by hand.
    HandEdit,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Set => "set",
            Self::Rollback => "rollback",
            Self::HandEdit => "hand_edit",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "baseline" => Some(Self::Baseline),
            "set" => Some(Self::Set),
            "rollback" => Some(Self::Rollback),
            "hand_edit" => Some(Self::HandEdit),
            _ => None,
        }
    }
}

/// One recorded value of a parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub version: i64,
    pub value: Value,
    pub kind: Kind,
    pub actor: String,
    pub reason: String,
    pub rolled_back_to: Option<i64>,
    pub written_at: String,
}

type RawRow = (i64, String, String, String, String, Option<i64>, String);

fn decode(raw: RawRow) -> sqlx::Result<Row> {
    let (version, value_json, kind, actor, reason, rolled_back_to, written_at) = raw;
    let value = serde_json::from_str(&value_json).map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
    let kind = Kind::from_str(&kind)
        .ok_or_else(|| sqlx::Error::Decode(format!("unknown param kind `{kind}`").into()))?;
    Ok(Row {
        version,
        value,
        kind,
        actor,
        reason,
        rolled_back_to,
        written_at,
    })
}

const COLUMNS: &str = "version, value_json, kind, actor, reason, rolled_back_to, written_at";

/// The newest row of a parameter, if it has any.
pub async fn latest(
    pool: &SqlitePool,
    project_id: &str,
    bundle: &str,
    param: &str,
) -> sqlx::Result<Option<Row>> {
    let raw: Option<RawRow> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM workflow_param_history
         WHERE project_id = ? AND bundle = ? AND param = ? ORDER BY version DESC LIMIT 1"
    ))
    .bind(project_id)
    .bind(bundle)
    .bind(param)
    .fetch_optional(pool)
    .await?;
    raw.map(decode).transpose()
}

/// Every row of a parameter, newest first.
pub async fn history(
    pool: &SqlitePool,
    project_id: &str,
    bundle: &str,
    param: &str,
) -> sqlx::Result<Vec<Row>> {
    let raws: Vec<RawRow> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM workflow_param_history
         WHERE project_id = ? AND bundle = ? AND param = ? ORDER BY version DESC"
    ))
    .bind(project_id)
    .bind(bundle)
    .bind(param)
    .fetch_all(pool)
    .await?;
    raws.into_iter().map(decode).collect()
}

/// Appends the next version. The caller owns the transaction (use `BEGIN IMMEDIATE`, as the
/// settings store does, so the `MAX(version)` read and the insert cannot interleave). The feed line
/// is `"<kind> <bundle>/<param> v<N> by <actor>"` and never carries the value.
#[allow(clippy::too_many_arguments)]
pub async fn append(
    tx: &mut SqliteConnection,
    project_id: &str,
    bundle: &str,
    param: &str,
    value: &Value,
    kind: Kind,
    actor: &str,
    reason: &str,
    rolled_back_to: Option<i64>,
) -> sqlx::Result<i64> {
    let highest: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version), 0) FROM workflow_param_history
         WHERE project_id = ? AND bundle = ? AND param = ?",
    )
    .bind(project_id)
    .bind(bundle)
    .bind(param)
    .fetch_one(&mut *tx)
    .await?;
    let next = highest + 1;
    sqlx::query(
        "INSERT INTO workflow_param_history
         (project_id, bundle, param, version, value_json, kind, actor, reason, rolled_back_to, written_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(project_id)
    .bind(bundle)
    .bind(param)
    .bind(next)
    .bind(value.to_string())
    .bind(kind.as_str())
    .bind(actor)
    .bind(reason)
    .bind(rolled_back_to)
    .bind(now())
    .execute(&mut *tx)
    .await?;
    let summary = format!("{} {bundle}/{param} v{next} by {actor}", kind.as_str());
    feed::append_on(
        tx,
        Some(project_id),
        "workflow_param_written",
        &summary,
        None,
        None,
    )
    .await?;
    Ok(next)
}

/// Distinct parameter names with history in one bundle of one project (for orphans).
pub async fn params_with_history(
    pool: &SqlitePool,
    project_id: &str,
    bundle: &str,
) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT DISTINCT param FROM workflow_param_history
         WHERE project_id = ? AND bundle = ? ORDER BY param",
    )
    .bind(project_id)
    .bind(bundle)
    .fetch_all(pool)
    .await
}

/// Whether the tuner may write a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Fixed,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Fixed => "fixed",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "fixed" => Some(Self::Fixed),
            _ => None,
        }
    }
}

/// Where an effective mode came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeSource {
    Project,
    Machine,
    Default,
}

/// Sets (or, with `None`, clears) a mode on the caller's transaction.
pub async fn set_mode_on(
    tx: &mut SqliteConnection,
    scope: &Scope,
    bundle: &str,
    param: &str,
    mode: Option<Mode>,
    by: &str,
) -> sqlx::Result<()> {
    let scope_text = scope.to_string();
    match mode {
        Some(m) => {
            sqlx::query(
                "INSERT INTO workflow_param_modes (scope, bundle, param, mode, updated_at, updated_by)
                 VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT (scope, bundle, param) DO UPDATE SET
                   mode = excluded.mode, updated_at = excluded.updated_at, updated_by = excluded.updated_by",
            )
            .bind(&scope_text)
            .bind(bundle)
            .bind(param)
            .bind(m.as_str())
            .bind(now())
            .bind(by)
            .execute(&mut *tx)
            .await?;
        }
        None => {
            sqlx::query(
                "DELETE FROM workflow_param_modes WHERE scope = ? AND bundle = ? AND param = ?",
            )
            .bind(&scope_text)
            .bind(bundle)
            .bind(param)
            .execute(&mut *tx)
            .await?;
        }
    }
    let what = mode.map_or("cleared", Mode::as_str);
    let summary = format!("{what} {scope_text} {bundle}/{param} by {by}");
    feed::append_on(
        tx,
        scope.project_id(),
        "workflow_param_mode",
        &summary,
        None,
        None,
    )
    .await?;
    Ok(())
}

/// `set_mode_on` in a transaction of its own.
pub async fn set_mode(
    pool: &SqlitePool,
    scope: &Scope,
    bundle: &str,
    param: &str,
    mode: Option<Mode>,
    by: &str,
) -> sqlx::Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    set_mode_on(&mut tx, scope, bundle, param, mode, by).await?;
    tx.commit().await
}

async fn mode_row(pool: &SqlitePool, scope: &Scope, bundle: &str, param: &str) -> sqlx::Result<Option<Mode>> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT mode FROM workflow_param_modes WHERE scope = ? AND bundle = ? AND param = ?",
    )
    .bind(scope.to_string())
    .bind(bundle)
    .bind(param)
    .fetch_optional(pool)
    .await?;
    text.map(|t| {
        Mode::from_str(&t)
            .ok_or_else(|| sqlx::Error::Decode(format!("unknown param mode `{t}`").into()))
    })
    .transpose()
}

/// The project's mode if set, else the machine's, else `Auto`.
pub async fn effective_mode(
    pool: &SqlitePool,
    project_id: &str,
    bundle: &str,
    param: &str,
) -> sqlx::Result<(Mode, ModeSource)> {
    if let Some(m) = mode_row(pool, &Scope::project(project_id), bundle, param).await? {
        return Ok((m, ModeSource::Project));
    }
    if let Some(m) = mode_row(pool, &Scope::Machine, bundle, param).await? {
        return Ok((m, ModeSource::Machine));
    }
    Ok((Mode::Auto, ModeSource::Default))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn add(
        pool: &SqlitePool,
        project: &str,
        bundle: &str,
        param: &str,
        value: Value,
    ) -> i64 {
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let v = append(&mut tx, project, bundle, param, &value, Kind::Set, "owner:x", "", None)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        v
    }

    #[tokio::test]
    async fn versions_count_up_per_parameter_and_never_restart() {
        let pool = crate::testdb::fresh_pool().await;
        assert_eq!(add(&pool, "p", "b", "x", json!(1)).await, 1);
        assert_eq!(add(&pool, "p", "b", "x", json!(2)).await, 2);
        assert_eq!(add(&pool, "p", "b", "y", json!(1)).await, 1);
        // Nothing deletes a row, but a gap must still be skipped: the sequence follows MAX.
        sqlx::query("DELETE FROM workflow_param_history WHERE param = 'x' AND version = 1")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(add(&pool, "p", "b", "x", json!(3)).await, 3);
    }

    #[tokio::test]
    async fn latest_is_the_highest_version_and_history_is_newest_first() {
        let pool = crate::testdb::fresh_pool().await;
        assert!(latest(&pool, "p", "b", "x").await.unwrap().is_none());
        add(&pool, "p", "b", "x", json!(1)).await;
        add(&pool, "p", "b", "x", json!("two")).await;
        let last = latest(&pool, "p", "b", "x").await.unwrap().unwrap();
        assert_eq!((last.version, last.value), (2, json!("two")));
        assert_eq!(last.kind, Kind::Set);
        let all = history(&pool, "p", "b", "x").await.unwrap();
        assert_eq!(all.iter().map(|r| r.version).collect::<Vec<_>>(), [2, 1]);
    }

    #[tokio::test]
    async fn two_projects_and_two_bundles_keep_separate_histories() {
        let pool = crate::testdb::fresh_pool().await;
        add(&pool, "p1", "b", "x", json!(1)).await;
        add(&pool, "p1", "b", "x", json!(2)).await;
        assert_eq!(add(&pool, "p2", "b", "x", json!(9)).await, 1);
        assert_eq!(add(&pool, "p1", "other", "x", json!(9)).await, 1);
        assert_eq!(history(&pool, "p1", "b", "x").await.unwrap().len(), 2);
        assert_eq!(history(&pool, "p2", "b", "x").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_append_leaves_a_feed_line_that_does_not_carry_the_value() {
        let pool = crate::testdb::fresh_pool().await;
        add(&pool, "p", "b", "x", json!(424242)).await;
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT kind, summary FROM feed WHERE kind = 'workflow_param_written'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, "set b/x v1 by owner:x");
        assert!(!rows[0].1.contains("424242"));
    }

    #[tokio::test]
    async fn set_mode_on_joins_the_callers_transaction_and_rolls_back_with_it() {
        let pool = crate::testdb::fresh_pool().await;
        let scope = Scope::project("p");
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        set_mode_on(&mut tx, &scope, "b", "x", Some(Mode::Fixed), "daemon")
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(
            effective_mode(&pool, "p", "b", "x").await.unwrap(),
            (Mode::Auto, ModeSource::Default)
        );
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        set_mode_on(&mut tx, &scope, "b", "x", Some(Mode::Fixed), "daemon")
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            effective_mode(&pool, "p", "b", "x").await.unwrap(),
            (Mode::Fixed, ModeSource::Project)
        );
    }

    #[tokio::test]
    async fn a_project_mode_beats_the_machine_default_which_beats_auto() {
        let pool = crate::testdb::fresh_pool().await;
        let get = || effective_mode(&pool, "p", "b", "x");
        assert_eq!(get().await.unwrap(), (Mode::Auto, ModeSource::Default));
        set_mode(&pool, &Scope::Machine, "b", "x", Some(Mode::Fixed), "owner:x")
            .await
            .unwrap();
        assert_eq!(get().await.unwrap(), (Mode::Fixed, ModeSource::Machine));
        set_mode(&pool, &Scope::project("p"), "b", "x", Some(Mode::Auto), "owner:x")
            .await
            .unwrap();
        assert_eq!(get().await.unwrap(), (Mode::Auto, ModeSource::Project));
        set_mode(&pool, &Scope::project("p"), "b", "x", None, "owner:x")
            .await
            .unwrap();
        assert_eq!(get().await.unwrap(), (Mode::Fixed, ModeSource::Machine));
        // Another project is not touched by the first one's override.
        assert_eq!(
            effective_mode(&pool, "q", "b", "x").await.unwrap(),
            (Mode::Fixed, ModeSource::Machine)
        );
    }

    #[tokio::test]
    async fn params_with_history_lists_each_name_once() {
        let pool = crate::testdb::fresh_pool().await;
        add(&pool, "p", "b", "x", json!(1)).await;
        add(&pool, "p", "b", "x", json!(2)).await;
        add(&pool, "p", "b", "y", json!(1)).await;
        add(&pool, "p", "other", "z", json!(1)).await;
        assert_eq!(params_with_history(&pool, "p", "b").await.unwrap(), ["x", "y"]);
    }
}
