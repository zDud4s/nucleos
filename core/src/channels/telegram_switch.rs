//! Telegram channel switch: carries the v1 sidecar's own state over to the channel hub.
//!
//! The v1 sidecar kept its settings in `telegram-config.json` and the doctrine in `telegram.yaml`.
//! The hub keeps identities in `channel_identities` and doctrine plus the project map in
//! `channels/telegram.yaml`. Every step here is idempotent and never overwrites an owner edit.

// Wired by run() in M6c.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use sqlx::SqlitePool;

const CHANNEL: &str = "telegram";

/// Error of the file steps: carries the path in its message.
pub(crate) type SwitchError = Box<dyn std::error::Error + Send + Sync>;

fn io_err(action: &str, path: &Path, err: impl std::fmt::Display) -> SwitchError {
    format!("{action} {}: {err}", path.display()).into()
}

/// The fields of the v1 `telegram-config.json` the switch cares about; the rest is ignored.
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
pub(crate) struct V1Config {
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
pub(crate) fn read_v1_config(path: &Path) -> Option<V1Config> {
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
pub(crate) async fn import_identities(pool: &SqlitePool, v1: &V1Config) -> sqlx::Result<()> {
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
pub(crate) fn move_doctrine(old_yaml: &Path, new_yaml: &Path) -> Result<bool, SwitchError> {
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
pub(crate) fn import_projects(
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
