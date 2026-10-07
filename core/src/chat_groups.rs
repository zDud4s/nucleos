//! User-defined groups for the Chats column (migration 0157).
//!
//! A group is a name and a position. Assigning a chat only writes `chats.group_id`; no turn is
//! ever read or written here.

use sqlx::SqlitePool;

const NAME_LIMIT: usize = 80;

#[derive(Debug, serde::Serialize, sqlx::FromRow)]
pub struct ChatGroup {
    pub id: i64,
    pub name: String,
    pub position: i64,
    pub created_at: String,
}

#[derive(Debug)]
pub enum GroupError {
    Blank,
    /// More than 80 characters.
    TooLong,
    NoGroup,
    NoChat,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for GroupError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

fn clean(name: &str) -> Result<&str, GroupError> {
    let name = name.trim();
    if name.is_empty() {
        Err(GroupError::Blank)
    } else if name.chars().count() > NAME_LIMIT {
        Err(GroupError::TooLong)
    } else {
        Ok(name)
    }
}

pub async fn list(pool: &SqlitePool) -> sqlx::Result<Vec<ChatGroup>> {
    sqlx::query_as("SELECT id, name, position, created_at FROM chat_groups ORDER BY position, id")
        .fetch_all(pool)
        .await
}

pub async fn create(pool: &SqlitePool, name: &str) -> Result<ChatGroup, GroupError> {
    let name = clean(name)?;
    Ok(sqlx::query_as(
        "INSERT INTO chat_groups (name, position, created_at)
         VALUES (?, (SELECT COALESCE(MAX(position), 0) + 1 FROM chat_groups), ?)
         RETURNING id, name, position, created_at",
    )
    .bind(name)
    .bind(chrono::Utc::now().to_rfc3339())
    .fetch_one(pool)
    .await?)
}

pub async fn rename(pool: &SqlitePool, id: i64, name: &str) -> Result<ChatGroup, GroupError> {
    let name = clean(name)?;
    sqlx::query_as(
        "UPDATE chat_groups SET name = ? WHERE id = ?
         RETURNING id, name, position, created_at",
    )
    .bind(name)
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(GroupError::NoGroup)
}

/// Deletes a group and ungroups its chats. `false` when there was no such group.
pub async fn delete(pool: &SqlitePool, id: i64) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    // Explicit: this does not rely on the foreign-key pragma being on.
    sqlx::query("UPDATE chats SET group_id = NULL WHERE group_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let done = sqlx::query("DELETE FROM chat_groups WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(done.rows_affected() > 0)
}

/// Puts a chat in a group, or takes it out with `None`.
pub async fn assign(
    pool: &SqlitePool,
    chat_id: &str,
    group_id: Option<i64>,
) -> Result<(), GroupError> {
    let live: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM chats WHERE chat_id = ? AND archived_at IS NULL")
            .bind(chat_id)
            .fetch_optional(pool)
            .await?;
    if live.is_none() {
        return Err(GroupError::NoChat);
    }
    if let Some(group) = group_id {
        let known: Option<i64> = sqlx::query_scalar("SELECT 1 FROM chat_groups WHERE id = ?")
            .bind(group)
            .fetch_optional(pool)
            .await?;
        if known.is_none() {
            return Err(GroupError::NoGroup);
        }
    }
    sqlx::query("UPDATE chats SET group_id = ? WHERE chat_id = ?")
        .bind(group_id)
        .bind(chat_id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chats::{self, Brain};

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        crate::storage::MIGRATOR.run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn create_rename_delete_and_deleting_ungroups_its_chats() {
        let pool = test_pool().await;
        let a = create(&pool, "  Work ").await.unwrap();
        let b = create(&pool, "Home").await.unwrap();
        assert_eq!(a.name, "Work");
        assert!(b.position > a.position);

        let renamed = rename(&pool, a.id, "Job").await.unwrap();
        assert_eq!(renamed.name, "Job");

        let chat = chats::create(&pool, Brain::Cloud, None).await.unwrap();
        assign(&pool, &chat, Some(a.id)).await.unwrap();
        assert_eq!(
            chats::get(&pool, &chat).await.unwrap().unwrap().group_id,
            Some(a.id)
        );

        assert!(delete(&pool, a.id).await.unwrap());
        assert!(!delete(&pool, a.id).await.unwrap());
        assert_eq!(
            chats::get(&pool, &chat).await.unwrap().unwrap().group_id,
            None
        );
        assert_eq!(list(&pool).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn blank_names_and_unknown_groups_are_refused() {
        let pool = test_pool().await;
        assert!(matches!(create(&pool, "   ").await, Err(GroupError::Blank)));
        assert!(matches!(
            create(&pool, &"x".repeat(81)).await,
            Err(GroupError::TooLong)
        ));
        assert!(matches!(
            rename(&pool, 99, "x").await,
            Err(GroupError::NoGroup)
        ));
        let chat = chats::create(&pool, Brain::Cloud, None).await.unwrap();
        assert!(matches!(
            assign(&pool, &chat, Some(99)).await,
            Err(GroupError::NoGroup)
        ));
        assert!(matches!(
            assign(&pool, "missing", None).await,
            Err(GroupError::NoChat)
        ));
        chats::archive(&pool, &chat).await.unwrap();
        assert!(matches!(
            assign(&pool, &chat, None).await,
            Err(GroupError::NoChat)
        ));
    }
}
