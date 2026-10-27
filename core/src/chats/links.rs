//! Which conversation a browser agent answers to.
//!
//! A child conversation (the browser agent) has at most one ACTIVE link (`unlinked_at IS NULL`),
//! to a parent conversation (the mother) or to nobody (`parent_chat_id` NULL: only the panel
//! talks to it). Relinking closes the active row and opens another; nothing is ever deleted, so
//! the history of who a child answered to stays readable. "Linked pair" = a child and the parent
//! of its active link, in either direction (spec 4.1).
//!
//! **This module owns the `chat_links` SQL and nothing else.** `relay::admit` asks `are_linked`
//! to lift the `Cycle` brake for a linked pair; it decides nothing else here.

use sqlx::SqlitePool;

/// One row of `chat_links`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, sqlx::FromRow)]
pub struct Link {
    pub id: i64,
    pub child_chat_id: String,
    pub parent_chat_id: Option<String>,
    pub kind: String,
    pub created_by_run: Option<i64>,
    pub created_at: String,
    pub unlinked_at: Option<String>,
}

/// Why `open` refused to write a link.
#[derive(Debug)]
pub enum LinkError {
    /// The child would be its own parent.
    ToSelf,
    /// The parent is missing or archived.
    ParentNotFound,
    /// Following the active parents up from the new parent reaches the child: the link would
    /// close a loop. `at` is the child, the conversation where the loop closes.
    Cycle { at: String },
    /// The child is missing or archived.
    ChildNotFound,
    /// A read or write failed outright.
    Db(sqlx::Error),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::ToSelf => write!(f, "a conversation cannot be linked to itself"),
            LinkError::ParentNotFound => write!(f, "the parent conversation does not exist"),
            LinkError::Cycle { at } => write!(f, "the link would close a cycle at {at}"),
            LinkError::ChildNotFound => write!(f, "the child conversation does not exist"),
            LinkError::Db(error) => write!(f, "chat link storage failed: {error}"),
        }
    }
}

impl std::error::Error for LinkError {}

impl From<sqlx::Error> for LinkError {
    fn from(error: sqlx::Error) -> Self {
        LinkError::Db(error)
    }
}

/// Closes `child`'s active link (if any) and opens a new one to `parent` (`None` = no mother,
/// only the panel), in one transaction. Refuses a link to itself, to a missing or archived
/// parent, from a missing or archived child, and one that would close a cycle of links.
pub async fn open(
    pool: &SqlitePool,
    child: &str,
    parent: Option<&str>,
    kind: &str,
    created_by_run: Option<i64>,
) -> Result<Link, LinkError> {
    if parent == Some(child) {
        return Err(LinkError::ToSelf);
    }
    let mut tx = pool.begin().await?;
    if !is_live(&mut *tx, child).await? {
        return Err(LinkError::ChildNotFound);
    }
    if let Some(parent) = parent {
        if !is_live(&mut *tx, parent).await? {
            return Err(LinkError::ParentNotFound);
        }
        // Walk the active parents up from the new parent; reaching the child closes a loop. The
        // visited set bounds the walk even if a loop already exists among other conversations.
        let mut seen = std::collections::HashSet::new();
        let mut cursor = Some(parent.to_string());
        while let Some(current) = cursor {
            if current == child {
                return Err(LinkError::Cycle {
                    at: child.to_string(),
                });
            }
            if !seen.insert(current.clone()) {
                break;
            }
            cursor = sqlx::query_scalar::<_, Option<String>>(
                "SELECT parent_chat_id FROM chat_links
                  WHERE child_chat_id = ? AND unlinked_at IS NULL",
            )
            .bind(&current)
            .fetch_optional(&mut *tx)
            .await?
            .flatten();
        }
    }
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE chat_links SET unlinked_at = ? WHERE child_chat_id = ? AND unlinked_at IS NULL",
    )
    .bind(&now)
    .bind(child)
    .execute(&mut *tx)
    .await?;
    let link = sqlx::query_as::<_, Link>(
        "INSERT INTO chat_links (child_chat_id, parent_chat_id, kind, created_by_run, created_at)
         VALUES (?, ?, ?, ?, ?)
         RETURNING id, child_chat_id, parent_chat_id, kind, created_by_run, created_at, unlinked_at",
    )
    .bind(child)
    .bind(parent)
    .bind(kind)
    .bind(created_by_run)
    .bind(&now)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(link)
}

/// Whether `chat_id` names a conversation that exists and is not archived.
async fn is_live(conn: &mut sqlx::SqliteConnection, chat_id: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM chats WHERE chat_id = ? AND archived_at IS NULL",
    )
    .bind(chat_id)
    .fetch_one(conn)
    .await
    .map(|n| n > 0)
}

/// Closes `child`'s active link, if it has one. The row stays, with `unlinked_at` set.
pub async fn unlink(pool: &SqlitePool, child: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE chat_links SET unlinked_at = ? WHERE child_chat_id = ? AND unlinked_at IS NULL",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(child)
    .execute(pool)
    .await
    .map(|_| ())
}

/// `child`'s active link, if any.
pub async fn active(pool: &SqlitePool, child: &str) -> sqlx::Result<Option<Link>> {
    sqlx::query_as::<_, Link>(
        "SELECT id, child_chat_id, parent_chat_id, kind, created_by_run, created_at, unlinked_at
           FROM chat_links WHERE child_chat_id = ? AND unlinked_at IS NULL",
    )
    .bind(child)
    .fetch_optional(pool)
    .await
}

/// The parent of `child`'s active link; `None` when there is no active link or it has no parent.
pub async fn active_parent_of(pool: &SqlitePool, child: &str) -> sqlx::Result<Option<String>> {
    Ok(active(pool, child)
        .await?
        .and_then(|link| link.parent_chat_id))
}

/// Whether `a` and `b` are a linked pair, in either direction, through an ACTIVE link only.
pub async fn are_linked(pool: &SqlitePool, a: &str, b: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM chat_links
          WHERE unlinked_at IS NULL
            AND ((child_chat_id = ?1 AND parent_chat_id = ?2)
              OR (child_chat_id = ?2 AND parent_chat_id = ?1))",
    )
    .bind(a)
    .bind(b)
    .fetch_one(pool)
    .await
    .map(|n| n > 0)
}

/// Closes every active link `chat_id` is an end of, as child or as parent (archiving).
pub async fn close_all_for(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE chat_links SET unlinked_at = ?1
          WHERE unlinked_at IS NULL AND (child_chat_id = ?2 OR parent_chat_id = ?2)",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(chat_id)
    .execute(pool)
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chats::{Brain, create};

    async fn test_pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    async fn chat(pool: &SqlitePool) -> String {
        create(pool, Brain::Cloud, None).await.unwrap()
    }

    async fn rows_of(pool: &SqlitePool, child: &str) -> Vec<Link> {
        sqlx::query_as::<_, Link>(
            "SELECT id, child_chat_id, parent_chat_id, kind, created_by_run, created_at, unlinked_at
               FROM chat_links WHERE child_chat_id = ? ORDER BY id",
        )
        .bind(child)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// The partial unique index is the guarantee, not `open`'s care: a second ACTIVE row for the
    /// same child is refused by the database itself, while a closed row beside an active one is
    /// ordinary history.
    #[tokio::test]
    async fn a_chat_has_at_most_one_active_link() {
        let pool = test_pool().await;
        let insert =
            "INSERT INTO chat_links (child_chat_id, parent_chat_id, kind, created_at, unlinked_at)
                      VALUES (?, ?, 'browser', '2026-10-22T00:00:00Z', ?)";

        sqlx::query(insert)
            .bind("F")
            .bind("M1")
            .bind(Some("2026-10-22T00:00:01Z"))
            .execute(&pool)
            .await
            .expect("a closed link is history");
        sqlx::query(insert)
            .bind("F")
            .bind("M2")
            .bind(None::<&str>)
            .execute(&pool)
            .await
            .expect("one active link beside a closed one");

        let second_active = sqlx::query(insert)
            .bind("F")
            .bind("M3")
            .bind(None::<&str>)
            .execute(&pool)
            .await;
        assert!(
            second_active.is_err(),
            "a second active link for the same child must violate chat_links_one_active"
        );

        sqlx::query(insert)
            .bind("G")
            .bind("M3")
            .bind(None::<&str>)
            .execute(&pool)
            .await
            .expect("another child has its own active link");
    }

    #[tokio::test]
    async fn relinking_closes_the_previous_link_and_keeps_the_history() {
        let pool = test_pool().await;
        let child = chat(&pool).await;
        let first_mother = chat(&pool).await;
        let second_mother = chat(&pool).await;

        let first = open(&pool, &child, Some(&first_mother), "browser", Some(7))
            .await
            .expect("first link");
        assert_eq!(first.child_chat_id, child);
        assert_eq!(first.parent_chat_id.as_deref(), Some(first_mother.as_str()));
        assert_eq!(first.kind, "browser");
        assert_eq!(first.created_by_run, Some(7));
        assert_eq!(first.unlinked_at, None);

        let second = open(&pool, &child, Some(&second_mother), "browser", None)
            .await
            .expect("relink to another mother");
        assert_ne!(second.id, first.id);

        let active_now = active(&pool, &child)
            .await
            .unwrap()
            .expect("an active link");
        assert_eq!(active_now, second);
        assert_eq!(
            active_parent_of(&pool, &child).await.unwrap().as_deref(),
            Some(second_mother.as_str())
        );

        let history = rows_of(&pool, &child).await;
        assert_eq!(history.len(), 2, "the previous link is kept, not deleted");
        assert_eq!(history[0].id, first.id);
        assert!(
            history[0].unlinked_at.is_some(),
            "the previous link is closed"
        );
        assert_eq!(history[1].id, second.id);
        assert!(history[1].unlinked_at.is_none());

        let orphan = open(&pool, &child, None, "browser", None)
            .await
            .expect("a link with no mother: only the panel");
        assert_eq!(orphan.parent_chat_id, None);
        assert_eq!(active_parent_of(&pool, &child).await.unwrap(), None);
        assert_eq!(rows_of(&pool, &child).await.len(), 3);

        unlink(&pool, &child).await.unwrap();
        assert_eq!(active(&pool, &child).await.unwrap(), None);
        assert_eq!(
            rows_of(&pool, &child).await.len(),
            3,
            "unlink closes, never deletes"
        );
    }

    #[tokio::test]
    async fn a_link_to_itself_to_a_missing_parent_or_in_a_cycle_is_refused() {
        let pool = test_pool().await;
        let a = chat(&pool).await;
        let b = chat(&pool).await;
        let c = chat(&pool).await;
        let archived = chat(&pool).await;
        crate::chats::archive(&pool, &archived).await.unwrap();

        assert!(matches!(
            open(&pool, &a, Some(&a), "browser", None).await,
            Err(LinkError::ToSelf)
        ));
        assert!(matches!(
            open(&pool, &a, Some("ghost"), "browser", None).await,
            Err(LinkError::ParentNotFound)
        ));
        assert!(matches!(
            open(&pool, &a, Some(&archived), "browser", None).await,
            Err(LinkError::ParentNotFound)
        ));
        assert!(matches!(
            open(&pool, "ghost", Some(&a), "browser", None).await,
            Err(LinkError::ChildNotFound)
        ));
        assert_eq!(
            active(&pool, &a).await.unwrap(),
            None,
            "a refusal writes nothing"
        );

        // b answers to a, c answers to b; a answering to c would close a -> c -> b -> a.
        open(&pool, &b, Some(&a), "browser", None).await.unwrap();
        open(&pool, &c, Some(&b), "browser", None).await.unwrap();
        match open(&pool, &a, Some(&c), "browser", None).await {
            Err(LinkError::Cycle { at }) => assert_eq!(at, a, "the loop closes at the child"),
            other => panic!("expected a cycle, got {other:?}"),
        }
        assert!(matches!(
            open(&pool, &a, Some(&b), "browser", None).await,
            Err(LinkError::Cycle { .. })
        ));
        assert_eq!(
            active(&pool, &a).await.unwrap(),
            None,
            "a refused cycle writes nothing"
        );

        // A closed link is no longer a path: once c is unlinked, a may answer to c.
        unlink(&pool, &c).await.unwrap();
        open(&pool, &a, Some(&c), "browser", None)
            .await
            .expect("no active path from c back to a any more");
    }

    #[tokio::test]
    async fn archiving_either_end_closes_the_active_link() {
        let pool = test_pool().await;
        let mother = chat(&pool).await;
        let child = chat(&pool).await;
        open(&pool, &child, Some(&mother), "browser", None)
            .await
            .unwrap();
        crate::chats::archive(&pool, &mother).await.unwrap();
        assert_eq!(
            active(&pool, &child).await.unwrap(),
            None,
            "archiving the mother closes her child's link"
        );
        assert!(rows_of(&pool, &child).await[0].unlinked_at.is_some());

        let other_mother = chat(&pool).await;
        let other_child = chat(&pool).await;
        open(&pool, &other_child, Some(&other_mother), "browser", None)
            .await
            .unwrap();
        crate::chats::archive(&pool, &other_child).await.unwrap();
        assert_eq!(
            active(&pool, &other_child).await.unwrap(),
            None,
            "archiving the child closes its own link"
        );
        assert_eq!(
            rows_of(&pool, &other_child).await.len(),
            1,
            "closed, not deleted"
        );
    }

    #[tokio::test]
    async fn are_linked_is_symmetric_and_ignores_closed_links() {
        let pool = test_pool().await;
        let mother = chat(&pool).await;
        let child = chat(&pool).await;
        let stranger = chat(&pool).await;
        open(&pool, &child, Some(&mother), "browser", None)
            .await
            .unwrap();

        assert!(are_linked(&pool, &child, &mother).await.unwrap());
        assert!(are_linked(&pool, &mother, &child).await.unwrap());
        assert!(!are_linked(&pool, &mother, &stranger).await.unwrap());
        assert!(!are_linked(&pool, &stranger, &child).await.unwrap());

        unlink(&pool, &child).await.unwrap();
        assert!(!are_linked(&pool, &child, &mother).await.unwrap());
        assert!(!are_linked(&pool, &mother, &child).await.unwrap());

        open(&pool, &child, None, "browser", None).await.unwrap();
        assert!(
            !are_linked(&pool, &child, &mother).await.unwrap(),
            "a link with no mother pairs the child with nobody"
        );
    }
}
