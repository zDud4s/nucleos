//! Configuration kept in the database: one row per document and scope, a history row per write, and an
//! in-memory snapshot the rest of the daemon reads synchronously. Design:
//! `.ai/specs/2026-10-11-configuracao-na-base-de-dados-design.md`. RED stub: the error type and the module
//! layout are real, every function is a stub.

use std::fmt;
use std::sync::Arc;

use serde_json::Value;
use sqlx::SqlitePool;

mod checks;
pub mod decode;
pub mod registry;
pub mod scope;
pub mod snapshot;
pub mod store;

pub use decode::{Erased, Prepared};
pub use registry::{DocType, Document, doc_type, documents};
pub use scope::{Kind, Scope, ScopeParseError};
pub use snapshot::{Entry, Settings, Snapshot, Unreadable};
pub use store::{HistoryRow, history, load_all};

/// Why a settings operation did not happen.
#[derive(Debug)]
pub enum SettingsError {
    /// No such document name.
    UnknownDocument(String),
    /// The document exists but not at this kind of scope (a project document at machine scope, or the reverse).
    WrongScope { name: String, scope: Scope },
    /// The body was refused; the text is the reason to show the operator.
    Invalid(String),
    /// The caller's expected version is not the stored one (0 means "no row").
    Stale { current: i64 },
    /// Nothing to delete, or no such history version.
    NotFound,
    /// The stored document cannot be decoded; replace or delete it first.
    Unreadable { name: String, reason: String },
    Database(sqlx::Error),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!("RED stub: SettingsError Display")
    }
}

impl std::error::Error for SettingsError {}

impl From<sqlx::Error> for SettingsError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

/// The daemon's handle. Empty until [`init`] loads it, so a reader before startup sees defaults.
pub fn global() -> Settings {
    todo!("RED stub: settings::global")
}

/// Replace the process handle, returning the previous one. Tests go through [`testing::install`].
pub fn set_global(_settings: Settings) -> Settings {
    todo!("RED stub: settings::set_global")
}

/// The document `T` at `scope`, or its default. Sync: a read of the current snapshot.
pub fn get<T: Document + Default>(_scope: &Scope) -> Arc<T> {
    todo!("RED stub: settings::get")
}

pub fn get_opt<T: Document>(_scope: &Scope) -> Option<Arc<T>> {
    todo!("RED stub: settings::get_opt")
}

/// Load the table into the global handle (startup) and hand the handle back.
pub async fn init(_pool: &SqlitePool) -> Result<Settings, SettingsError> {
    todo!("RED stub: settings::init")
}

pub async fn load(_pool: &SqlitePool) -> Result<(), SettingsError> {
    todo!("RED stub: settings::load")
}

pub async fn put(
    _pool: &SqlitePool,
    _scope: &Scope,
    _name: &str,
    _body: &Value,
    _by: &str,
    _expected: Option<i64>,
) -> Result<i64, SettingsError> {
    todo!("RED stub: settings::put")
}

pub async fn update<T: Document + Default, F: FnOnce(&mut T) + Send>(
    _pool: &SqlitePool,
    _scope: &Scope,
    _by: &str,
    _change: F,
) -> Result<i64, SettingsError> {
    todo!("RED stub: settings::update")
}

pub async fn rollback(
    _pool: &SqlitePool,
    _scope: &Scope,
    _name: &str,
    _to_version: i64,
    _by: &str,
) -> Result<i64, SettingsError> {
    todo!("RED stub: settings::rollback")
}

pub async fn delete(
    _pool: &SqlitePool,
    _scope: &Scope,
    _name: &str,
    _by: &str,
    _expected: Option<i64>,
) -> Result<i64, SettingsError> {
    todo!("RED stub: settings::delete")
}

/// Swapping the process handle in a test. RED stub: `install` is not built yet.
#[cfg(any(test, feature = "testkit"))]
pub mod testing {
    use super::Settings;

    pub struct Installed {
        _private: (),
    }

    /// Make `settings` the process handle until the returned guard drops.
    pub fn install(_settings: Settings) -> Installed {
        todo!("RED stub: testing::install")
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn the_settings_tables_key_on_scope_and_name() {
        let pool = crate::testdb::fresh_pool().await;
        let insert = "INSERT INTO settings (scope, name, body_json, version, updated_at, updated_by) \
                      VALUES ('machine', 'verify', '{}', 1, '2026-10-11T00:00:00Z', 'test')";
        sqlx::query(insert).execute(&pool).await.expect("first row");
        assert!(
            sqlx::query(insert).execute(&pool).await.is_err(),
            "a second row for the same (scope, name) must be refused"
        );
        // Another scope is another row.
        sqlx::query(
            "INSERT INTO settings (scope, name, body_json, version, updated_at, updated_by) \
             VALUES ('project:p1', 'verify', '{}', 1, '2026-10-11T00:00:00Z', 'test')",
        )
        .execute(&pool)
        .await
        .expect("same name, other scope");

        let history = "INSERT INTO settings_history (scope, name, version, body_json, written_at, written_by) \
                       VALUES ('machine', 'verify', 1, '{}', '2026-10-11T00:00:00Z', 'test')";
        sqlx::query(history).execute(&pool).await.expect("first history row");
        assert!(
            sqlx::query(history).execute(&pool).await.is_err(),
            "history is keyed on (scope, name, version)"
        );
    }

    use super::*;
    use crate::config::VerifyConfig;

    #[test]
    fn an_installed_snapshot_is_what_get_reads_until_the_guard_drops() {
        // Every read of the global happens under an `install`, so no other test can interleave.
        let _empty = testing::install(Settings::new());
        assert_eq!(get::<VerifyConfig>(&Scope::Machine).capacity, 4, "the shipped default");
        {
            let snapshot = Snapshot::default()
                .with_document(&Scope::Machine, VerifyConfig { capacity: 9, ..VerifyConfig::default() });
            // Replace the global while still inside the outer guard's lifetime: use set_global directly,
            // because a second `install` would wait forever on the static lock this thread already holds.
            let previous = set_global(Settings::from_snapshot(snapshot));
            assert_eq!(get::<VerifyConfig>(&Scope::Machine).capacity, 9);
            assert_eq!(get_opt::<VerifyConfig>(&Scope::Machine).map(|config| config.capacity), Some(9));
            assert!(get_opt::<VerifyConfig>(&Scope::project("p1")).is_none());
            set_global(previous);
        }
        assert_eq!(get::<VerifyConfig>(&Scope::Machine).capacity, 4, "restored");
    }

    #[test]
    fn install_restores_the_previous_handle_on_drop() {
        {
            let snapshot = Snapshot::default()
                .with_document(&Scope::Machine, VerifyConfig { capacity: 9, ..VerifyConfig::default() });
            let _installed = testing::install(Settings::from_snapshot(snapshot));
            assert_eq!(get::<VerifyConfig>(&Scope::Machine).capacity, 9);
        }
        let _again = testing::install(Settings::new());
        assert_eq!(get::<VerifyConfig>(&Scope::Machine).capacity, 4, "the guard of the block above restored it");
    }

    #[test]
    fn errors_say_what_is_wrong() {
        assert!(SettingsError::UnknownDocument("x".into()).to_string().contains("`x`"));
        assert!(SettingsError::Stale { current: 3 }.to_string().contains("version 3"));
        let wrong = SettingsError::WrongScope { name: "verify".into(), scope: Scope::project("p1") };
        assert!(wrong.to_string().contains("project:p1"));
    }
}
