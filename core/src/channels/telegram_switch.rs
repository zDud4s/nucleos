//! Telegram channel switch: carries the v1 sidecar's own state over to the channel hub.
//!
//! The v1 sidecar kept its settings in `telegram-config.json` and the doctrine in `telegram.yaml`.
//! The hub keeps identities in `channel_identities` and doctrine plus the project map in
//! `channels/telegram.yaml`. Every step here is idempotent and never overwrites an owner edit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use sqlx::SqlitePool;

use crate::state::AppState;

const CHANNEL: &str = "telegram";

/// Names the v1 config file's location, ahead of the `%LOCALAPPDATA%\nucleos` default.
pub const CONFIG_ENV: &str = "NUCLEOS_TELEGRAM_CONFIG";

/// Where the switch reads and writes.
#[derive(Debug, Clone)]
pub struct SwitchPaths {
    /// The v1 `telegram-config.json`.
    pub v1_json: PathBuf,
    /// The old `telegram.yaml` (doctrine), kept in place.
    pub old_yaml: PathBuf,
    /// The hub's `channels/telegram.yaml`.
    pub new_yaml: PathBuf,
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
        new_yaml: root.join("channels").join("telegram.yaml"),
    })
}

/// Startup entry: carries the v1 state over. Without a v1 config (or a machine-config root) it
/// does nothing. Tests must use `run_at` with temp paths: `run` with a `machine_config_root` reads and renames the real v1 JSON.
pub async fn run(state: &AppState) -> Result<(), SwitchError> {
    match paths_of(state) {
        Some(paths) => run_at(&state.pool, &paths).await,
        None => Ok(()),
    }
}

/// `run` for the caller that must not stop on a failure: the SQL migration already ran, so the
/// switch is retried on the next start (every step is idempotent and the JSON is only renamed
/// once the targets are written). Tests must use `run_at` with temp paths, not this.
pub async fn run_or_warn(state: &AppState) {
    if let Err(error) = run(state).await {
        tracing::warn!(%error, "telegram switch: the v1 import failed; startup continues");
    }
}

pub async fn run_at(pool: &SqlitePool, paths: &SwitchPaths) -> Result<(), SwitchError> {
    let Some(v1) = read_v1_config(&paths.v1_json) else {
        return Ok(());
    };
    import_identities(pool, &v1)
        .await
        .map_err(|e| io_err("importing identities from", &paths.v1_json, e))?;
    if let Some(dir) = paths.new_yaml.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io_err("creating", dir, e))?;
    }
    move_doctrine(&paths.old_yaml, &paths.new_yaml)?;
    import_projects(&v1, &paths.new_yaml, &paths.v1_json)
}

/// Error of the file steps: carries the path in its message.
pub type SwitchError = Box<dyn std::error::Error + Send + Sync>;

fn io_err(action: &str, path: &Path, err: impl std::fmt::Display) -> SwitchError {
    format!("{action} {}: {err}", path.display()).into()
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

/// Writes through a sibling temp file so a crash never leaves a half-written config.
fn write_mapping(path: &Path, map: &Mapping) -> Result<(), SwitchError> {
    let text = serde_yaml::to_string(map).map_err(|e| io_err("serializing", path, e))?;
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, text).map_err(|e| io_err("writing", &tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| io_err("replacing", path, e))
}

fn non_blank(map: &Mapping, key: &str) -> Option<String> {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

/// Copies `doctrine` from the old `telegram.yaml` into `channels/telegram.yaml` when the target
/// has none. A missing source is a no-op. Returns whether the target was written.
pub fn move_doctrine(old_yaml: &Path, new_yaml: &Path) -> Result<bool, SwitchError> {
    let Some(doctrine) = non_blank(&read_mapping(old_yaml)?, "doctrine") else {
        return Ok(false);
    };
    let mut target = read_mapping(new_yaml)?;
    if non_blank(&target, "doctrine").is_some() {
        return Ok(false);
    }
    target.insert("doctrine".into(), Value::String(doctrine));
    write_mapping(new_yaml, &target)?;
    Ok(true)
}

/// Imports `project_topics` as `projects: { project: "<chat>:<thread>" }` for absent keys only,
/// then retires the v1 JSON by renaming it to `.migrated`. The rename happens only after the
/// target was written.
pub fn import_projects(
    v1: &V1Config,
    new_yaml: &Path,
    source_json: &Path,
) -> Result<(), SwitchError> {
    if !v1.transcribe_cmd.trim().is_empty() {
        tracing::warn!(
            cmd = %v1.transcribe_cmd,
            "telegram v1 transcribe_cmd is not carried over; configure voice transcription again"
        );
    }
    let mut target = read_mapping(new_yaml)?;
    let mut projects = match target.remove("projects") {
        Some(Value::Mapping(map)) => map,
        _ => Mapping::new(),
    };
    if v1.allowed_chat_id != 0 {
        for (project, thread) in &v1.project_topics {
            if *thread <= 0 || projects.contains_key(project.as_str()) {
                continue;
            }
            let value = format!("{}:{thread}", v1.allowed_chat_id);
            projects.insert(Value::String(project.clone()), Value::String(value));
        }
    }
    if !projects.is_empty() {
        target.insert("projects".into(), Value::Mapping(projects));
    }
    write_mapping(new_yaml, &target)?;
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

    fn yaml_of(path: &Path) -> Mapping {
        read_mapping(path).unwrap()
    }

    #[test]
    fn doctrine_is_copied_when_the_target_has_none() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (
            dir.path().join("telegram.yaml"),
            dir.path().join("new.yaml"),
        );
        std::fs::write(&old, "doctrine: be brief\n").unwrap();
        std::fs::write(&new, "projects:\n  a: \"1:2\"\n").unwrap();
        assert!(move_doctrine(&old, &new).unwrap());
        let map = yaml_of(&new);
        assert_eq!(non_blank(&map, "doctrine").as_deref(), Some("be brief"));
        assert!(map.contains_key("projects"));
        // A missing source is a no-op.
        assert!(!move_doctrine(&dir.path().join("absent.yaml"), &new).unwrap());
    }

    #[test]
    fn doctrine_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (
            dir.path().join("telegram.yaml"),
            dir.path().join("new.yaml"),
        );
        std::fs::write(&old, "doctrine: old\n").unwrap();
        std::fs::write(&new, "doctrine: mine\n").unwrap();
        assert!(!move_doctrine(&old, &new).unwrap());
        assert_eq!(
            non_blank(&yaml_of(&new), "doctrine").as_deref(),
            Some("mine")
        );
    }

    fn with_topics(chat: i64, topics: &[(&str, i64)]) -> V1Config {
        V1Config {
            allowed_chat_id: chat,
            project_topics: topics.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            ..Default::default()
        }
    }

    fn project(path: &Path, name: &str) -> Option<String> {
        match yaml_of(path).get("projects") {
            Some(Value::Mapping(map)) => map.get(name).and_then(Value::as_str).map(str::to_string),
            _ => None,
        }
    }

    #[test]
    fn projects_are_imported_as_chat_colon_thread() {
        let dir = tempfile::tempdir().unwrap();
        let (new, json) = (dir.path().join("new.yaml"), dir.path().join("c.json"));
        import_projects(&with_topics(-100, &[("nucleos", 5)]), &new, &json).unwrap();
        assert_eq!(project(&new, "nucleos").as_deref(), Some("-100:5"));
    }

    #[test]
    fn existing_project_mappings_win_over_the_import() {
        let dir = tempfile::tempdir().unwrap();
        let (new, json) = (dir.path().join("new.yaml"), dir.path().join("c.json"));
        std::fs::write(&new, "projects:\n  nucleos: \"-100:9\"\n").unwrap();
        import_projects(&with_topics(-100, &[("nucleos", 5), ("b", 6)]), &new, &json).unwrap();
        assert_eq!(project(&new, "nucleos").as_deref(), Some("-100:9"));
        assert_eq!(project(&new, "b").as_deref(), Some("-100:6"));
    }

    #[test]
    fn import_twice_keeps_one_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let (new, json) = (dir.path().join("new.yaml"), dir.path().join("c.json"));
        let cfg = with_topics(-100, &[("nucleos", 5)]);
        import_projects(&cfg, &new, &json).unwrap();
        let first = std::fs::read_to_string(&new).unwrap();
        import_projects(&cfg, &new, &json).unwrap();
        assert_eq!(std::fs::read_to_string(&new).unwrap(), first);
    }

    #[test]
    fn source_json_is_renamed_only_after_the_target_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join("telegram-config.json");
        std::fs::write(&json, "{}").unwrap();
        // The target's parent does not exist, so the write fails and the JSON must stay.
        let bad = dir.path().join("missing-dir").join("new.yaml");
        assert!(import_projects(&with_topics(-100, &[("a", 1)]), &bad, &json).is_err());
        assert!(json.exists());
        let good = dir.path().join("new.yaml");
        import_projects(&with_topics(-100, &[("a", 1)]), &good, &json).unwrap();
        assert!(!json.exists());
        assert!(dir.path().join("telegram-config.json.migrated").exists());
    }

    fn paths_in(dir: &Path) -> SwitchPaths {
        SwitchPaths {
            v1_json: dir.join("telegram-config.json"),
            old_yaml: dir.join("telegram.yaml"),
            new_yaml: dir.join("channels").join("telegram.yaml"),
        }
    }

    const V1_JSON: &str =
        r#"{"allowed_chat_id":-100,"allowed_user_ids":[1],"project_topics":{"nucleos":5}}"#;

    #[tokio::test]
    async fn run_switch_is_a_noop_without_a_v1_config() {
        let st = state().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        std::fs::write(&paths.old_yaml, "doctrine: be brief\n").unwrap();
        run_at(&st.pool, &paths).await.unwrap();
        assert!(rows(&st.pool).await.is_empty());
        assert!(!paths.new_yaml.exists());
        // No machine-config root at all is also a no-op.
        assert!(st.machine_config_root.is_none());
        run(&st).await.unwrap();
        assert!(rows(&st.pool).await.is_empty());
    }

    #[tokio::test]
    async fn run_switch_twice_changes_nothing() {
        let st = state().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        std::fs::write(&paths.v1_json, V1_JSON).unwrap();
        std::fs::write(&paths.old_yaml, "doctrine: be brief\n").unwrap();
        run_at(&st.pool, &paths).await.unwrap();
        let (identities, yaml) = (
            rows(&st.pool).await,
            std::fs::read_to_string(&paths.new_yaml).unwrap(),
        );
        assert_eq!(identities.len(), 1);
        assert_eq!(
            project(&paths.new_yaml, "nucleos").as_deref(),
            Some("-100:5")
        );
        assert!(!paths.v1_json.exists());
        // The second pass finds no JSON (it was retired) and leaves everything as it was.
        run_at(&st.pool, &paths).await.unwrap();
        assert_eq!(rows(&st.pool).await, identities);
        assert_eq!(std::fs::read_to_string(&paths.new_yaml).unwrap(), yaml);
        // Even with the JSON restored, the import neither duplicates nor overlaps.
        std::fs::write(&paths.v1_json, V1_JSON).unwrap();
        run_at(&st.pool, &paths).await.unwrap();
        assert_eq!(rows(&st.pool).await, identities);
        assert_eq!(std::fs::read_to_string(&paths.new_yaml).unwrap(), yaml);
    }

    #[tokio::test]
    async fn run_switch_failure_does_not_stop_the_daemon() {
        let st = state().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        // `channels` is a file, so the target directory cannot be created.
        std::fs::write(dir.path().join("channels"), "not a dir").unwrap();
        std::fs::write(&paths.v1_json, V1_JSON).unwrap();
        assert!(run_at(&st.pool, &paths).await.is_err());
        // The JSON stays for the next start.
        assert!(paths.v1_json.exists());
        // The failure is an `Err` value, never a panic: `run_or_warn` (what `main` calls) logs it
        // and returns `()`, so startup goes on. Once the obstacle is gone the same call succeeds.
        std::fs::remove_file(dir.path().join("channels")).unwrap();
        run_at(&st.pool, &paths).await.unwrap();
        assert!(!paths.v1_json.exists());
    }

    #[test]
    fn a_project_topic_not_above_zero_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let (new, json) = (dir.path().join("new.yaml"), dir.path().join("c.json"));
        let cfg = with_topics(-100, &[("zero", 0), ("neg", -3), ("ok", 2)]);
        import_projects(&cfg, &new, &json).unwrap();
        assert_eq!(project(&new, "zero"), None);
        assert_eq!(project(&new, "neg"), None);
        assert_eq!(project(&new, "ok").as_deref(), Some("-100:2"));
    }
}
