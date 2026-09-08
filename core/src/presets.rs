use serde::{Deserialize, Serialize};

use crate::runs::CreateRunRequest;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct Preset {
    pub id: i64,
    pub name: String,
    pub prompt: String,
    pub project_id: Option<String>,
    pub cwd: Option<String>,
    pub mode: String,
    pub created_at: String,
    pub updated_at: String,
}

/// A name plus the exact request shape accepted by `/runs`.
#[derive(Deserialize)]
pub struct PresetRequest {
    pub name: String,
    #[serde(flatten)]
    pub run: CreateRunRequest,
}

#[derive(Debug)]
pub enum PresetError {
    DuplicateName,
    Invalid(&'static str),
    NotFound,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for PresetError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl std::fmt::Display for PresetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateName => formatter.write_str("a preset with that name already exists"),
            Self::Invalid(message) => formatter.write_str(message),
            Self::NotFound => formatter.write_str("preset not found"),
            Self::Db(error) => write!(formatter, "database error: {error}"),
        }
    }
}

impl std::error::Error for PresetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Db(error) => Some(error),
            Self::DuplicateName | Self::Invalid(_) | Self::NotFound => None,
        }
    }
}

fn validate(run: &CreateRunRequest) -> Result<(), PresetError> {
    if run.mode == "worktree" && (run.project_id.is_none() || run.cwd.is_none()) {
        return Err(PresetError::Invalid(
            "worktree mode requires project_id and cwd (the project root)",
        ));
    }
    Ok(())
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|database_error| database_error.is_unique_violation())
}

pub async fn create(
    pool: &sqlx::SqlitePool,
    name: &str,
    run: CreateRunRequest,
) -> Result<Preset, PresetError> {
    validate(&run)?;
    let now = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "INSERT INTO run_presets (name, prompt, project_id, cwd, mode, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(name)
    .bind(&run.prompt)
    .bind(&run.project_id)
    .bind(&run.cwd)
    .bind(&run.mode)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await;
    match result {
        Ok(result) => get(pool, result.last_insert_rowid())
            .await?
            .ok_or(PresetError::NotFound),
        Err(error) if is_unique_violation(&error) => Err(PresetError::DuplicateName),
        Err(error) => Err(PresetError::Db(error)),
    }
}

pub async fn list(pool: &sqlx::SqlitePool) -> Result<Vec<Preset>, PresetError> {
    sqlx::query_as(
        "SELECT id, name, prompt, project_id, cwd, mode, created_at, updated_at
         FROM run_presets ORDER BY created_at DESC, id DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(PresetError::Db)
}

pub async fn get(pool: &sqlx::SqlitePool, id: i64) -> Result<Option<Preset>, PresetError> {
    sqlx::query_as(
        "SELECT id, name, prompt, project_id, cwd, mode, created_at, updated_at
         FROM run_presets WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(PresetError::Db)
}

pub async fn update(
    pool: &sqlx::SqlitePool,
    id: i64,
    name: &str,
    run: CreateRunRequest,
) -> Result<Preset, PresetError> {
    validate(&run)?;
    let result = sqlx::query(
        "UPDATE run_presets
         SET name = ?, prompt = ?, project_id = ?, cwd = ?, mode = ?, updated_at = ?
         WHERE id = ?",
    )
    .bind(name)
    .bind(&run.prompt)
    .bind(&run.project_id)
    .bind(&run.cwd)
    .bind(&run.mode)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(id)
    .execute(pool)
    .await;
    match result {
        Ok(result) if result.rows_affected() == 0 => Err(PresetError::NotFound),
        Ok(_) => get(pool, id).await?.ok_or(PresetError::NotFound),
        Err(error) if is_unique_violation(&error) => Err(PresetError::DuplicateName),
        Err(error) => Err(PresetError::Db(error)),
    }
}

pub async fn delete(pool: &sqlx::SqlitePool, id: i64) -> Result<(), PresetError> {
    let result = sqlx::query("DELETE FROM run_presets WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(PresetError::NotFound);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    fn run(prompt: &str) -> CreateRunRequest {
        CreateRunRequest {
            prompt: prompt.to_owned(),
            project_id: Some("project-a".to_owned()),
            cwd: Some("C:/repo/project-a".to_owned()),
            mode: "real".to_owned(),
            steerable: false,
            permission_mode: None,
        }
    }

    #[tokio::test]
    async fn create_list_get_update_and_delete_round_trip() {
        let pool = pool().await;
        let first = create(&pool, "first", run("one")).await.unwrap();
        let second = create(&pool, "second", run("two")).await.unwrap();

        let listed = list(&pool).await.unwrap();
        assert_eq!(
            listed.iter().map(|preset| &preset.name).collect::<Vec<_>>(),
            ["second", "first"]
        );
        assert_eq!(get(&pool, first.id).await.unwrap(), Some(first.clone()));

        let updated = update(&pool, first.id, "renamed", run("updated"))
            .await
            .unwrap();
        assert_eq!(updated.name, "renamed");
        assert_eq!(updated.prompt, "updated");
        assert_eq!(updated.id, first.id);
        assert_eq!(second.project_id.as_deref(), Some("project-a"));

        delete(&pool, first.id).await.unwrap();
        assert_eq!(get(&pool, first.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn duplicate_names_are_a_typed_conflict() {
        let pool = pool().await;
        create(&pool, "duplicate", run("one")).await.unwrap();
        assert!(matches!(
            create(&pool, "duplicate", run("two")).await,
            Err(PresetError::DuplicateName)
        ));
    }

    #[tokio::test]
    async fn deleting_an_unknown_id_is_reported() {
        assert!(matches!(
            delete(&pool().await, 999).await,
            Err(PresetError::NotFound)
        ));
    }
}
