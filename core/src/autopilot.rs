use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    Shadow,
    Active,
}

impl Mode {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::Active => "active",
        }
    }

    pub fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "shadow" => Some(Self::Shadow),
            "active" => Some(Self::Active),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum ActivationError {
    NotAGitRepo,
    ProjectRootRequired,
    NotOnboarded,
    HookNotRegistered,
    Database(sqlx::Error),
}

impl fmt::Display for ActivationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAGitRepo => write!(formatter, "project root is not a git repository"),
            Self::ProjectRootRequired => {
                write!(formatter, "project_root is required to enable shadow mode")
            }
            Self::NotOnboarded => write!(formatter, "project is not onboarded to .ai/workflow"),
            Self::HookNotRegistered => {
                write!(formatter, "a PreToolUse hook is not registered")
            }
            Self::Database(error) => write!(formatter, "database error: {error}"),
        }
    }
}

impl std::error::Error for ActivationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for ActivationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

pub async fn project_mode(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Mode> {
    let stored: Option<String> =
        sqlx::query_scalar("SELECT mode FROM autopilot_state WHERE project_id = ?")
            .bind(project_id)
            .fetch_optional(pool)
            .await?;

    stored.map_or(Ok(Mode::Off), |value| {
        Mode::from_db_str(&value).ok_or_else(|| {
            sqlx::Error::Protocol(format!("invalid autopilot mode in database: {value}"))
        })
    })
}

pub async fn set_project_mode(
    pool: &SqlitePool,
    project_id: &str,
    mode: Mode,
    project_root: Option<&Path>,
) -> Result<(), ActivationError> {
    match mode {
        Mode::Active => {
            let project_root = project_root.ok_or(ActivationError::ProjectRootRequired)?;
            activation_prerequisites(project_root)?;
            if !project_root.join(".git").exists() {
                return Err(ActivationError::NotAGitRepo);
            }
        }
        Mode::Shadow => {
            activation_prerequisites(project_root.ok_or(ActivationError::ProjectRootRequired)?)?
        }
        Mode::Off => {}
    }

    let persisted_root = match mode {
        Mode::Shadow | Mode::Active => project_root.map(|root| root.to_string_lossy().into_owned()),
        Mode::Off => None,
    };

    sqlx::query(
        "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES (?, ?, ?)
         ON CONFLICT(project_id) DO UPDATE SET
             mode = excluded.mode,
             project_root = excluded.project_root",
    )
    .bind(project_id)
    .bind(mode.as_db_str())
    .bind(persisted_root)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn shadow_projects(pool: &SqlitePool) -> sqlx::Result<Vec<(String, String)>> {
    sqlx::query_as(
        "SELECT project_id, project_root
         FROM autopilot_state
         WHERE mode = 'shadow' AND project_root IS NOT NULL
         ORDER BY project_id",
    )
    .fetch_all(pool)
    .await
}

pub async fn autopilot_projects(pool: &SqlitePool) -> sqlx::Result<Vec<(String, String, Mode)>> {
    let projects: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT project_id, project_root, mode
         FROM autopilot_state
         WHERE mode IN ('shadow', 'active') AND project_root IS NOT NULL
         ORDER BY project_id",
    )
    .fetch_all(pool)
    .await?;

    projects
        .into_iter()
        .map(|(project_id, project_root, mode)| {
            let mode = Mode::from_db_str(&mode).ok_or_else(|| {
                sqlx::Error::Protocol(format!("invalid autopilot mode in database: {mode}"))
            })?;
            Ok((project_id, project_root, mode))
        })
        .collect()
}

fn activation_prerequisites(project_root: &Path) -> Result<(), ActivationError> {
    if !project_root.join(".ai/workflow/workflow.md").is_file() {
        return Err(ActivationError::NotOnboarded);
    }

    let settings = std::fs::read_to_string(project_root.join(".claude/settings.json"))
        .map_err(|_| ActivationError::HookNotRegistered)?;
    let settings: serde_json::Value =
        serde_json::from_str(&settings).map_err(|_| ActivationError::HookNotRegistered)?;
    let registered = settings
        .get("hooks")
        .and_then(|hooks| hooks.get("PreToolUse"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|hooks| !hooks.is_empty());

    if !registered {
        return Err(ActivationError::HookNotRegistered);
    }

    Ok(())
}

pub async fn kill_switch_engaged(pool: &SqlitePool) -> sqlx::Result<bool> {
    let value: i64 = sqlx::query_scalar("SELECT kill_switch FROM autopilot_global LIMIT 1")
        .fetch_one(pool)
        .await?;
    Ok(value != 0)
}

pub async fn set_kill_switch(pool: &SqlitePool, engaged: bool) -> sqlx::Result<()> {
    sqlx::query("UPDATE autopilot_global SET kill_switch = ?")
        .bind(engaged)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    fn write_workflow(root: &TempDir) {
        let workflow_dir = root.path().join(".ai/workflow");
        fs::create_dir_all(&workflow_dir).unwrap();
        fs::write(workflow_dir.join("workflow.md"), "# Workflow").unwrap();
    }

    fn write_settings(root: &TempDir, contents: &str) {
        let claude_dir = root.path().join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        fs::write(claude_dir.join("settings.json"), contents).unwrap();
    }

    fn git_init(root: &TempDir) {
        fs::create_dir_all(root.path().join(".git")).unwrap();
    }

    #[tokio::test]
    async fn project_mode_defaults_to_off_without_a_row() {
        let pool = test_pool().await;

        assert_eq!(project_mode(&pool, "project-a").await.unwrap(), Mode::Off);
    }

    #[tokio::test]
    async fn shadow_mode_round_trips_when_both_prerequisites_exist() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_workflow(&root);
        write_settings(
            &root,
            r#"{"hooks":{"PreToolUse":[{"command":"nucleos hook"}]}}"#,
        );

        set_project_mode(&pool, "project-a", Mode::Shadow, Some(root.path()))
            .await
            .unwrap();

        assert_eq!(
            project_mode(&pool, "project-a").await.unwrap(),
            Mode::Shadow
        );
    }

    #[tokio::test]
    async fn missing_workflow_rejects_shadow_and_leaves_mode_off() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_settings(
            &root,
            r#"{"hooks":{"PreToolUse":[{"command":"nucleos hook"}]}}"#,
        );

        let error = set_project_mode(&pool, "project-a", Mode::Shadow, Some(root.path()))
            .await
            .unwrap_err();

        assert!(matches!(error, ActivationError::NotOnboarded));
        assert_eq!(project_mode(&pool, "project-a").await.unwrap(), Mode::Off);
    }

    #[tokio::test]
    async fn missing_settings_rejects_shadow_and_leaves_mode_off() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_workflow(&root);

        let error = set_project_mode(&pool, "project-a", Mode::Shadow, Some(root.path()))
            .await
            .unwrap_err();

        assert!(matches!(error, ActivationError::HookNotRegistered));
        assert_eq!(project_mode(&pool, "project-a").await.unwrap(), Mode::Off);
    }

    #[tokio::test]
    async fn malformed_settings_json_rejects_shadow_and_leaves_mode_off() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_workflow(&root);
        write_settings(&root, "{ this is not valid json ");

        let error = set_project_mode(&pool, "project-a", Mode::Shadow, Some(root.path()))
            .await
            .unwrap_err();
        assert!(matches!(error, ActivationError::HookNotRegistered));
        assert_eq!(project_mode(&pool, "project-a").await.unwrap(), Mode::Off);
    }

    #[tokio::test]
    async fn settings_without_pretooluse_key_rejects_shadow_and_leaves_mode_off() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_workflow(&root);
        write_settings(&root, r#"{"hooks":{"PostToolUse":[{"command":"x"}]}}"#);

        let error = set_project_mode(&pool, "project-a", Mode::Shadow, Some(root.path()))
            .await
            .unwrap_err();
        assert!(matches!(error, ActivationError::HookNotRegistered));
        assert_eq!(project_mode(&pool, "project-a").await.unwrap(), Mode::Off);
    }

    #[tokio::test]
    async fn empty_pretooluse_rejects_shadow_and_leaves_mode_off() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_workflow(&root);
        write_settings(&root, r#"{"hooks":{"PreToolUse":[]}}"#);

        let error = set_project_mode(&pool, "project-a", Mode::Shadow, Some(root.path()))
            .await
            .unwrap_err();

        assert!(matches!(error, ActivationError::HookNotRegistered));
        assert_eq!(project_mode(&pool, "project-a").await.unwrap(), Mode::Off);
    }

    #[tokio::test]
    async fn active_mode_round_trips_with_all_prerequisites() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_workflow(&root);
        write_settings(
            &root,
            r#"{"hooks":{"PreToolUse":[{"command":"nucleos hook"}]}}"#,
        );
        git_init(&root);

        set_project_mode(&pool, "project-a", Mode::Active, Some(root.path()))
            .await
            .unwrap();

        assert_eq!(
            project_mode(&pool, "project-a").await.unwrap(),
            Mode::Active
        );
        assert_eq!(
            autopilot_projects(&pool).await.unwrap(),
            vec![(
                "project-a".to_owned(),
                root.path().to_string_lossy().into_owned(),
                Mode::Active,
            )]
        );
    }

    #[tokio::test]
    async fn active_mode_rejected_when_not_a_git_repo() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_workflow(&root);
        write_settings(
            &root,
            r#"{"hooks":{"PreToolUse":[{"command":"nucleos hook"}]}}"#,
        );

        let error = set_project_mode(&pool, "project-a", Mode::Active, Some(root.path()))
            .await
            .unwrap_err();

        assert!(matches!(error, ActivationError::NotAGitRepo));
        assert_eq!(project_mode(&pool, "project-a").await.unwrap(), Mode::Off);
    }

    #[tokio::test]
    async fn active_mode_still_requires_onboarding_and_hook() {
        let pool = test_pool().await;
        let missing_workflow_root = tempfile::tempdir().unwrap();
        write_settings(
            &missing_workflow_root,
            r#"{"hooks":{"PreToolUse":[{"command":"nucleos hook"}]}}"#,
        );
        git_init(&missing_workflow_root);

        let error = set_project_mode(
            &pool,
            "missing-workflow",
            Mode::Active,
            Some(missing_workflow_root.path()),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ActivationError::NotOnboarded));

        let missing_hook_root = tempfile::tempdir().unwrap();
        write_workflow(&missing_hook_root);
        write_settings(&missing_hook_root, r#"{"hooks":{"PreToolUse":[]}}"#);
        git_init(&missing_hook_root);

        let error = set_project_mode(
            &pool,
            "missing-hook",
            Mode::Active,
            Some(missing_hook_root.path()),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ActivationError::HookNotRegistered));
    }

    #[tokio::test]
    async fn autopilot_projects_lists_shadow_and_active() {
        let pool = test_pool().await;
        let shadow_root = tempfile::tempdir().unwrap();
        write_workflow(&shadow_root);
        write_settings(
            &shadow_root,
            r#"{"hooks":{"PreToolUse":[{"command":"nucleos hook"}]}}"#,
        );
        let active_root = tempfile::tempdir().unwrap();
        write_workflow(&active_root);
        write_settings(
            &active_root,
            r#"{"hooks":{"PreToolUse":[{"command":"nucleos hook"}]}}"#,
        );
        git_init(&active_root);

        set_project_mode(
            &pool,
            "project-shadow",
            Mode::Shadow,
            Some(shadow_root.path()),
        )
        .await
        .unwrap();
        set_project_mode(
            &pool,
            "project-active",
            Mode::Active,
            Some(active_root.path()),
        )
        .await
        .unwrap();
        set_project_mode(&pool, "project-off", Mode::Off, None)
            .await
            .unwrap();

        assert_eq!(
            autopilot_projects(&pool).await.unwrap(),
            vec![
                (
                    "project-active".to_owned(),
                    active_root.path().to_string_lossy().into_owned(),
                    Mode::Active,
                ),
                (
                    "project-shadow".to_owned(),
                    shadow_root.path().to_string_lossy().into_owned(),
                    Mode::Shadow,
                ),
            ]
        );
    }

    #[tokio::test]
    async fn kill_switch_round_trips() {
        let pool = test_pool().await;

        assert!(!kill_switch_engaged(&pool).await.unwrap());
        set_kill_switch(&pool, true).await.unwrap();
        assert!(kill_switch_engaged(&pool).await.unwrap());
        set_kill_switch(&pool, false).await.unwrap();
        assert!(!kill_switch_engaged(&pool).await.unwrap());
    }
}
