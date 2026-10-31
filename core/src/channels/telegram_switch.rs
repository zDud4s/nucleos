//! Telegram channel switch: carries the v1 sidecar's own state over to the channel hub.
//!
//! The v1 sidecar kept its settings in `telegram-config.json` and the doctrine in `telegram.yaml`.
//! The hub keeps identities in `channel_identities` and doctrine plus the project map in the
//! `channels/telegram` settings document. Every step here is idempotent and never overwrites an
//! owner edit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use sqlx::SqlitePool;

use crate::channels::registry::ChannelConfig;
use crate::settings::{self, Scope};
use crate::state::AppState;

const CHANNEL: &str = "telegram";

/// The settings document the hub reads this channel from.
const DOCUMENT: &str = "channels/telegram";

/// Names the v1 config file's location, ahead of the `%LOCALAPPDATA%\nucleos` default.
pub const CONFIG_ENV: &str = "NUCLEOS_TELEGRAM_CONFIG";

/// Where the switch reads.
#[derive(Debug, Clone)]
pub struct SwitchPaths {
    /// The v1 `telegram-config.json`.
    pub v1_json: PathBuf,
    /// The old `telegram.yaml` (doctrine), kept in place.
    pub old_yaml: PathBuf,
}

/// The v1 JSON location: the env override, else `%LOCALAPPDATA%\nucleos\telegram-config.json`
/// (the platform local data directory elsewhere).
fn v1_json_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(CONFIG_ENV).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let base = match std::env::var_os("LOCALAPPDATA").filter(|p| !p.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => directories::BaseDirs::new()?.data_local_dir().to_path_buf(),
    };
    Some(base.join("nucleos").join("telegram-config.json"))
}

fn paths_of(state: &AppState) -> Option<SwitchPaths> {
    let root = state.machine_config_root.as_deref()?;
    Some(SwitchPaths {
        v1_json: v1_json_path()?,
        old_yaml: root.join("telegram.yaml"),
    })
}

/// Startup entry: carries the v1 state over. Without a v1 config (or a machine-config root) it
/// does nothing. Tests must use `run_at` with temp paths: `run` with a `machine_config_root` reads and renames the real v1 JSON.
pub async fn run(state: &AppState) -> Result<(), SwitchError> {
    match (state.machine_config_root.as_deref(), paths_of(state)) {
        (Some(root), Some(paths)) => carry_over(state, root, &paths).await,
        _ => Ok(()),
    }
}

/// `run_at`, then the hub told: it loaded its configs before this switch runs, so without the
/// reload the document just written would not reach it until the next start.
async fn carry_over(state: &AppState, root: &Path, paths: &SwitchPaths) -> Result<(), SwitchError> {
    run_at(&state.pool, paths).await?;
    state.channels.reload(root);
    Ok(())
}

/// `run` for the caller that must not stop on a failure: the SQL migration already ran, so the
/// switch is retried on the next start (every step is idempotent and the JSON is only renamed
/// once the document is written). Tests must use `run_at` with temp paths, not this.
pub async fn run_or_warn(state: &AppState) {
    if let Err(error) = run(state).await {
        tracing::warn!(%error, "telegram switch: the v1 import failed; startup continues");
    }
}

/// Merges the v1 state into the document, then retires the v1 JSON. A document that cannot be read
/// or that changed under the write is an `Err` with the JSON still in place, so nothing is lost and
/// the next start tries again. Nothing to carry over writes nothing: no empty document, no version.
pub async fn run_at(pool: &SqlitePool, paths: &SwitchPaths) -> Result<(), SwitchError> {
    let Some(v1) = read_v1_config(&paths.v1_json) else {
        return Ok(());
    };
    import_identities(pool, &v1)
        .await
        .map_err(|e| io_err("importing identities from", &paths.v1_json, e))?;
    if !v1.transcribe_cmd.trim().is_empty() {
        tracing::warn!(
            cmd = %v1.transcribe_cmd,
            "telegram v1 transcribe_cmd is not carried over; configure voice transcription again"
        );
    }
    let doctrine = non_blank(&read_mapping(&paths.old_yaml)?, "doctrine");
    let (current, version) = current_document()?;
    let merged = merge(&current, &v1, doctrine);
    if merged != current {
        let body = serde_json::to_value(&merged).map_err(|e| document_err("serializing", e))?;
        settings::put(
            pool,
            &Scope::Machine,
            DOCUMENT,
            &body,
            "telegram-switch",
            Some(version),
        )
        .await
        .map_err(|e| document_err("writing", e))?;
    }
    retire(&paths.v1_json)
}

/// Error of the file steps: carries the path in its message.
pub type SwitchError = Box<dyn std::error::Error + Send + Sync>;

fn io_err(action: &str, path: &Path, err: impl std::fmt::Display) -> SwitchError {
    format!("{action} {}: {err}", path.display()).into()
}

fn document_err(action: &str, err: impl std::fmt::Display) -> SwitchError {
    format!("{action} the {DOCUMENT} document: {err}").into()
}

/// The fields of the v1 `telegram-config.json` the switch cares about; the rest is ignored.
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
pub struct V1Config {
    #[serde(default)]
    pub allowed_chat_id: i64,
    #[serde(default)]
    pub allowed_user_ids: Vec<i64>,
    /// Absent in real v1 files, hence optional.
    #[serde(default)]
    pub project_topics: BTreeMap<String, i64>,
    #[serde(default)]
    pub transcribe_cmd: String,
}

/// Reads the v1 JSON; `None` when the file is absent or unreadable as a config.
pub fn read_v1_config(path: &Path) -> Option<V1Config> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(config) => Some(config),
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "telegram v1 config is not valid JSON");
            None
        }
    }
}

/// One `owner` identity per allowed user in the allowed chat; the first one becomes home unless
/// the channel already has a home. A zero chat id imports nothing.
pub async fn import_identities(pool: &SqlitePool, v1: &V1Config) -> sqlx::Result<()> {
    let Some(first_user) = v1.allowed_user_ids.first() else {
        return Ok(());
    };
    if v1.allowed_chat_id == 0 {
        return Ok(());
    }
    let chat = v1.allowed_chat_id.to_string();
    let kind = if v1.allowed_chat_id > 0 {
        "private"
    } else {
        "group"
    };
    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = pool.begin().await?;
    for user in &v1.allowed_user_ids {
        sqlx::query(
            "INSERT OR IGNORE INTO channel_identities \
             (channel, sender_ref, conversation_ref, conversation_kind, role, home, created_at) \
             VALUES (?, ?, ?, ?, 'owner', 0, ?)",
        )
        .bind(CHANNEL)
        .bind(user.to_string())
        .bind(&chat)
        .bind(kind)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    let has_home: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM channel_identities WHERE channel = ? AND home = 1)",
    )
    .bind(CHANNEL)
    .fetch_one(&mut *tx)
    .await?;
    if !has_home {
        sqlx::query(
            "UPDATE channel_identities SET home = 1 \
             WHERE channel = ? AND sender_ref = ? AND conversation_ref = ?",
        )
        .bind(CHANNEL)
        .bind(first_user.to_string())
        .bind(&chat)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// A YAML file as a mapping; a missing file is an empty mapping.
fn read_mapping(path: &Path) -> Result<Mapping, SwitchError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Mapping::new()),
        Err(err) => return Err(io_err("reading", path, err)),
    };
    match serde_yaml::from_str::<Value>(&text).map_err(|e| io_err("parsing", path, e))? {
        Value::Mapping(map) => Ok(map),
        Value::Null => Ok(Mapping::new()),
        _ => Err(io_err("parsing", path, "not a YAML mapping")),
    }
}

fn non_blank(map: &Mapping, key: &str) -> Option<String> {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

/// The document as it stands and its version: an absent one is empty at version 0, which is what
/// `settings::put` reads as "must not exist yet". An unreadable one is an error, not an empty one:
/// writing over it would throw away what the owner has there.
fn current_document() -> Result<(ChannelConfig, i64), SwitchError> {
    let snapshot = settings::global().snapshot();
    if let Some(unreadable) = snapshot.unreadable_for(&Scope::Machine, DOCUMENT) {
        return Err(document_err("reading", &unreadable.reason));
    }
    let current = snapshot
        .entry_as::<ChannelConfig>(&Scope::Machine, DOCUMENT)
        .cloned()
        .unwrap_or_default();
    Ok((
        current,
        snapshot.version(&Scope::Machine, DOCUMENT).unwrap_or(0),
    ))
}

/// `current` with the v1 state added under it: the document wins. The doctrine comes across only
/// when there is none, `project_topics` only for absent keys as `project: "<chat>:<thread>"`, and
/// everything else stays as it is.
fn merge(current: &ChannelConfig, v1: &V1Config, doctrine: Option<String>) -> ChannelConfig {
    let mut merged = current.clone();
    if merged.doctrine.is_none() {
        merged.doctrine = doctrine;
    }
    if v1.allowed_chat_id != 0 {
        for (project, thread) in &v1.project_topics {
            if *thread > 0 {
                merged
                    .projects
                    .entry(project.clone())
                    .or_insert_with(|| format!("{}:{thread}", v1.allowed_chat_id));
            }
        }
    }
    merged
}

/// Renames the v1 JSON to `.migrated`, once what it held is in the document.
fn retire(source_json: &Path) -> Result<(), SwitchError> {
    let mut retired = source_json.as_os_str().to_owned();
    retired.push(".migrated");
    match std::fs::rename(source_json, &retired) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            Err(io_err("retiring", source_json, err))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::testkit::state;
    use crate::project::rules::testing::Home;

    fn v1(chat: i64, users: &[i64]) -> V1Config {
        V1Config {
            allowed_chat_id: chat,
            allowed_user_ids: users.to_vec(),
            ..Default::default()
        }
    }

    async fn rows(pool: &SqlitePool) -> Vec<(String, String, String, i64)> {
        sqlx::query_as(
            "SELECT sender_ref, conversation_ref, conversation_kind, home \
             FROM channel_identities WHERE channel = 'telegram' ORDER BY sender_ref",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[test]
    fn v1_config_parses_allowed_ids_and_topics() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram-config.json");
        std::fs::write(
            &path,
            r#"{"allowed_chat_id":-100,"allowed_user_ids":[1,2],"project_topics":{"a":5},
               "transcribe_cmd":"whisper","poll_interval_seconds":3,"extra":true}"#,
        )
        .unwrap();
        let cfg = read_v1_config(&path).unwrap();
        assert_eq!(cfg.allowed_chat_id, -100);
        assert_eq!(cfg.allowed_user_ids, vec![1, 2]);
        assert_eq!(cfg.project_topics.get("a"), Some(&5));
        assert_eq!(cfg.transcribe_cmd, "whisper");
        // Real v1 files carry no project_topics.
        std::fs::write(&path, r#"{"allowed_chat_id":7,"allowed_user_ids":[7]}"#).unwrap();
        assert!(read_v1_config(&path).unwrap().project_topics.is_empty());
    }

    #[test]
    fn v1_config_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_v1_config(&dir.path().join("nope.json")).is_none());
    }

    #[tokio::test]
    async fn import_creates_owner_identities_and_one_home() {
        let st = state().await;
        import_identities(&st.pool, &v1(42, &[1, 2])).await.unwrap();
        let got = rows(&st.pool).await;
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|r| r.1 == "42" && r.2 == "private"));
        assert_eq!(got.iter().filter(|r| r.3 == 1).count(), 1);
    }

    #[tokio::test]
    async fn import_is_idempotent() {
        let st = state().await;
        import_identities(&st.pool, &v1(42, &[1, 2])).await.unwrap();
        let first = rows(&st.pool).await;
        import_identities(&st.pool, &v1(42, &[1, 2])).await.unwrap();
        assert_eq!(rows(&st.pool).await, first);
    }

    #[tokio::test]
    async fn import_does_not_override_an_existing_home() {
        let st = state().await;
        sqlx::query(
            "INSERT INTO channel_identities \
             (channel, sender_ref, conversation_ref, conversation_kind, home, created_at) \
             VALUES ('telegram', '9', '99', 'private', 1, '2026-10-20T10:00:00Z')",
        )
        .execute(&st.pool)
        .await
        .unwrap();
        import_identities(&st.pool, &v1(42, &[1])).await.unwrap();
        let got = rows(&st.pool).await;
        assert_eq!(got.len(), 2);
        let homes: Vec<_> = got.iter().filter(|r| r.3 == 1).collect();
        assert_eq!(homes.len(), 1);
        assert_eq!(homes[0].0, "9");
    }

    #[tokio::test]
    async fn import_marks_a_negative_chat_id_as_group() {
        let st = state().await;
        import_identities(&st.pool, &v1(-1001, &[1])).await.unwrap();
        let got = rows(&st.pool).await;
        assert_eq!(got, vec![("1".into(), "-1001".into(), "group".into(), 1)]);
    }

    #[tokio::test]
    async fn import_ignores_a_user_id_list_when_chat_id_is_zero() {
        let st = state().await;
        import_identities(&st.pool, &v1(0, &[1, 2])).await.unwrap();
        assert!(rows(&st.pool).await.is_empty());
    }

    fn with_doctrine(text: &str) -> ChannelConfig {
        ChannelConfig {
            doctrine: Some(text.to_string()),
            ..Default::default()
        }
    }

    fn with_topics(chat: i64, topics: &[(&str, i64)]) -> V1Config {
        V1Config {
            allowed_chat_id: chat,
            project_topics: topics.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            ..Default::default()
        }
    }

    fn project<'a>(config: &'a ChannelConfig, name: &str) -> Option<&'a str> {
        config.projects.get(name).map(String::as_str)
    }

    fn stored_version() -> Option<i64> {
        settings::global()
            .snapshot()
            .version(&Scope::Machine, DOCUMENT)
    }

    #[test]
    fn doctrine_is_copied_when_the_target_has_none() {
        let mut current = ChannelConfig::default();
        current.projects.insert("a".into(), "1:2".into());
        let merged = merge(&current, &V1Config::default(), Some("be brief".into()));
        assert_eq!(merged.doctrine.as_deref(), Some("be brief"));
        assert_eq!(project(&merged, "a"), Some("1:2"));
        // A missing source is a no-op.
        let dir = tempfile::tempdir().unwrap();
        let absent = read_mapping(&dir.path().join("absent.yaml")).unwrap();
        assert_eq!(non_blank(&absent, "doctrine"), None);
        assert_eq!(merge(&current, &V1Config::default(), None), current);
    }

    #[test]
    fn doctrine_is_never_overwritten() {
        let current = with_doctrine("mine");
        let merged = merge(&current, &V1Config::default(), Some("old".into()));
        assert_eq!(merged.doctrine.as_deref(), Some("mine"));
    }

    #[test]
    fn projects_are_imported_as_chat_colon_thread() {
        let merged = merge(
            &ChannelConfig::default(),
            &with_topics(-100, &[("nucleos", 5)]),
            None,
        );
        assert_eq!(project(&merged, "nucleos"), Some("-100:5"));
    }

    #[test]
    fn existing_project_mappings_win_over_the_import() {
        let mut current = ChannelConfig::default();
        current.projects.insert("nucleos".into(), "-100:9".into());
        let merged = merge(
            &current,
            &with_topics(-100, &[("nucleos", 5), ("b", 6)]),
            None,
        );
        assert_eq!(project(&merged, "nucleos"), Some("-100:9"));
        assert_eq!(project(&merged, "b"), Some("-100:6"));
    }

    #[test]
    fn import_twice_keeps_one_mapping() {
        let cfg = with_topics(-100, &[("nucleos", 5)]);
        let first = merge(&ChannelConfig::default(), &cfg, None);
        assert_eq!(first.projects.len(), 1);
        assert_eq!(merge(&first, &cfg, None), first);
    }

    #[test]
    fn stream_edits_stays_as_the_document_has_it() {
        let current = ChannelConfig {
            stream_edits: Some(false),
            ..Default::default()
        };
        let merged = merge(&current, &with_topics(-100, &[("a", 1)]), Some("x".into()));
        assert_eq!(merged.stream_edits, Some(false));
    }

    /// A row that does not decode as a `ChannelConfig`, loaded the way a start loads it.
    async fn unreadable_document(pool: &SqlitePool) {
        sqlx::query(
            r#"INSERT INTO settings (scope, name, body_json, version, updated_at, updated_by)
               VALUES (?, ?, '{"projects": 5}', 1, '2026-10-11T00:00:00.000Z', 'test')"#,
        )
        .bind(Scope::Machine.to_string())
        .bind(DOCUMENT)
        .execute(pool)
        .await
        .unwrap();
        settings::load(pool).await.unwrap();
    }

    #[tokio::test]
    async fn source_json_is_renamed_only_after_the_document_is_written() {
        let home = Home::new();
        let pool = crate::testdb::fresh_pool().await;
        unreadable_document(&pool).await;
        let paths = paths_in(home.path());
        std::fs::write(&paths.v1_json, V1_JSON).unwrap();

        assert!(run_at(&pool, &paths).await.is_err());

        assert!(paths.v1_json.exists(), "the JSON stays for the next start");
        let body: String = sqlx::query_scalar("SELECT body_json FROM settings WHERE name = ?")
            .bind(DOCUMENT)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            body, r#"{"projects": 5}"#,
            "the owner's row is not replaced"
        );
        // The owner repairs the row; the next start carries the state over.
        sqlx::query("UPDATE settings SET body_json = '{}' WHERE name = ?")
            .bind(DOCUMENT)
            .execute(&pool)
            .await
            .unwrap();
        settings::load(&pool).await.unwrap();
        run_at(&pool, &paths).await.unwrap();
        assert!(!paths.v1_json.exists());
        assert!(home.path().join("telegram-config.json.migrated").exists());
        let stored = settings::get_named::<ChannelConfig>(&Scope::Machine, DOCUMENT).unwrap();
        assert_eq!(project(&stored, "nucleos"), Some("-100:5"));
    }

    fn paths_in(dir: &Path) -> SwitchPaths {
        SwitchPaths {
            v1_json: dir.join("telegram-config.json"),
            old_yaml: dir.join("telegram.yaml"),
        }
    }

    const V1_JSON: &str =
        r#"{"allowed_chat_id":-100,"allowed_user_ids":[1],"project_topics":{"nucleos":5}}"#;

    #[tokio::test]
    async fn run_switch_is_a_noop_without_a_v1_config() {
        let home = Home::new();
        let st = state().await;
        let paths = paths_in(home.path());
        std::fs::write(&paths.old_yaml, "doctrine: be brief\n").unwrap();
        run_at(&st.pool, &paths).await.unwrap();
        assert!(rows(&st.pool).await.is_empty());
        assert_eq!(stored_version(), None);
        // No machine-config root at all is also a no-op.
        assert!(st.machine_config_root.is_none());
        run(&st).await.unwrap();
        assert!(rows(&st.pool).await.is_empty());
    }

    #[tokio::test]
    async fn a_v1_config_with_nothing_to_carry_creates_no_document() {
        let home = Home::new();
        let pool = crate::testdb::fresh_pool().await;
        let paths = paths_in(home.path());
        std::fs::write(&paths.v1_json, r#"{"allowed_chat_id":0}"#).unwrap();

        run_at(&pool, &paths).await.unwrap();

        assert_eq!(stored_version(), None);
        assert!(
            !paths.v1_json.exists(),
            "the v1 JSON is retired all the same"
        );
    }

    #[tokio::test]
    async fn run_switch_twice_changes_nothing() {
        let home = Home::new();
        let st = state().await;
        let paths = paths_in(home.path());
        std::fs::write(&paths.v1_json, V1_JSON).unwrap();
        std::fs::write(&paths.old_yaml, "doctrine: be brief\n").unwrap();
        run_at(&st.pool, &paths).await.unwrap();
        let identities = rows(&st.pool).await;
        assert_eq!(identities.len(), 1);
        let stored = settings::get_named::<ChannelConfig>(&Scope::Machine, DOCUMENT).unwrap();
        assert_eq!(project(&stored, "nucleos"), Some("-100:5"));
        assert_eq!(stored_version(), Some(1));
        assert!(!paths.v1_json.exists());
        // The second pass finds no JSON (it was retired) and leaves everything as it was.
        run_at(&st.pool, &paths).await.unwrap();
        assert_eq!(rows(&st.pool).await, identities);
        assert_eq!(stored_version(), Some(1));
        // Even with the JSON restored, the import neither duplicates nor overlaps.
        std::fs::write(&paths.v1_json, V1_JSON).unwrap();
        run_at(&st.pool, &paths).await.unwrap();
        assert_eq!(rows(&st.pool).await, identities);
        assert_eq!(stored_version(), Some(1));
    }

    #[tokio::test]
    async fn run_switch_failure_does_not_stop_the_daemon() {
        let home = Home::new();
        let st = state().await;
        let paths = paths_in(home.path());
        // `telegram.yaml` is a directory, so the doctrine cannot be read.
        std::fs::create_dir(&paths.old_yaml).unwrap();
        std::fs::write(&paths.v1_json, V1_JSON).unwrap();
        assert!(run_at(&st.pool, &paths).await.is_err());
        // The JSON stays for the next start.
        assert!(paths.v1_json.exists());
        assert_eq!(stored_version(), None);
        // The failure is an `Err` value, never a panic: `run_or_warn` (what `main` calls) logs it
        // and returns `()`, so startup goes on. Once the obstacle is gone the same call succeeds.
        std::fs::remove_dir(&paths.old_yaml).unwrap();
        run_at(&st.pool, &paths).await.unwrap();
        assert!(!paths.v1_json.exists());
    }

    /// The hub loads its configs before the switch runs at startup, so a document written here that
    /// it was not told about would change nothing until the next start.
    #[tokio::test]
    async fn the_hub_sees_the_carried_doctrine_without_a_restart() {
        let home = Home::new();
        let st = state().await;
        let paths = paths_in(home.path());
        std::fs::write(&paths.v1_json, V1_JSON).unwrap();
        std::fs::write(&paths.old_yaml, "doctrine: be brief\n").unwrap();

        carry_over(&st, home.path(), &paths).await.unwrap();

        let hub = st.channels.config(CHANNEL).expect("the hub knows telegram");
        assert_eq!(hub.doctrine.as_deref(), Some("be brief"));
        assert_eq!(project(&hub, "nucleos"), Some("-100:5"));
    }

    /// The settings import runs before this switch at startup and leaves a channel that has a document
    /// alone, so a `channels/telegram.yaml` written here is never read, while the v1 JSON it came from
    /// is retired all the same.
    #[tokio::test]
    async fn run_switch_merges_into_an_existing_document_and_creates_no_file() {
        use crate::channels::registry::ChannelConfig;
        use crate::settings::{self, Scope};

        let home = crate::project::rules::testing::Home::new();
        let pool = crate::testdb::fresh_pool().await;
        settings::put(
            &pool,
            &Scope::Machine,
            "channels/telegram",
            &serde_json::json!({ "stream_edits": false }),
            "test",
            None,
        )
        .await
        .expect("channels/telegram is a registered document");
        let paths = paths_in(home.path());
        let stray = home.path().join("channels").join("telegram.yaml");
        std::fs::write(&paths.v1_json, V1_JSON).unwrap();
        std::fs::write(&paths.old_yaml, "doctrine: be brief\n").unwrap();

        run_at(&pool, &paths).await.unwrap();

        let stored = settings::get_named::<ChannelConfig>(&Scope::Machine, "channels/telegram")
            .expect("the document stays");
        assert_eq!(stored.doctrine.as_deref(), Some("be brief"));
        assert_eq!(
            stored.projects.get("nucleos").map(String::as_str),
            Some("-100:5")
        );
        assert_eq!(stored.stream_edits, Some(false), "the owner's key stays");
        assert_eq!(
            settings::global()
                .snapshot()
                .version(&Scope::Machine, "channels/telegram"),
            Some(2)
        );
        assert!(!stray.exists(), "no file beside the document");
        assert!(!paths.v1_json.exists(), "the v1 JSON is retired");
    }

    /// No document yet: the importer has already run, so a file written here would not reach the hub
    /// before the next start. The document exists as soon as the switch returns.
    #[tokio::test]
    async fn run_switch_creates_the_document_when_there_is_none_and_no_file() {
        use crate::channels::registry::ChannelConfig;
        use crate::settings::{self, Scope};

        let home = crate::project::rules::testing::Home::new();
        let pool = crate::testdb::fresh_pool().await;
        let paths = paths_in(home.path());
        let stray = home.path().join("channels").join("telegram.yaml");
        std::fs::write(&paths.v1_json, V1_JSON).unwrap();
        std::fs::write(&paths.old_yaml, "doctrine: be brief\n").unwrap();

        run_at(&pool, &paths).await.unwrap();

        let stored = settings::get_named::<ChannelConfig>(&Scope::Machine, "channels/telegram")
            .expect("the switch creates the document");
        assert_eq!(stored.doctrine.as_deref(), Some("be brief"));
        assert_eq!(
            stored.projects.get("nucleos").map(String::as_str),
            Some("-100:5")
        );
        assert!(!stray.exists(), "no file beside the document");
        assert!(!paths.v1_json.exists(), "the v1 JSON is retired");
    }

    #[test]
    fn a_project_topic_not_above_zero_is_skipped() {
        let cfg = with_topics(-100, &[("zero", 0), ("neg", -3), ("ok", 2)]);
        let merged = merge(&ChannelConfig::default(), &cfg, None);
        assert_eq!(project(&merged, "zero"), None);
        assert_eq!(project(&merged, "neg"), None);
        assert_eq!(project(&merged, "ok"), Some("-100:2"));
    }
}
