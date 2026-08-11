use sqlx::SqlitePool;

/// Which model answers a conversation.
///
/// Stored on the chat rather than derived from the sender, because with more than one conversation
/// in the app "this one stays on the machine, that one goes out" becomes a choice worth making per
/// conversation. `Origin` still decides for anything WITHOUT a row here — see `assistant.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brain {
    Cloud,
    Local,
}

impl Brain {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cloud => "cloud",
            Self::Local => "local",
        }
    }

    /// An unreadable value reads as `Cloud`, matching the column default. A brain nobody can parse
    /// is a brain nobody chose, and the old path is the safe one to fall to.
    ///
    /// Not `std::str::FromStr`: that trait is for parsing that can fail, and this deliberately
    /// cannot. Naming it after the trait would promise an error case there is none of.
    pub fn from_wire(value: &str) -> Self {
        if value == "local" {
            Self::Local
        } else {
            Self::Cloud
        }
    }
}

/// A conversation as the list shows it: the row, plus the two facts the list needs and the row
/// cannot hold — what was first said, and when something last happened.
#[derive(Debug, serde::Serialize, sqlx::FromRow)]
pub struct ChatSummary {
    pub chat_id: String,
    pub title: Option<String>,
    pub brain: String,
    pub created_at: String,
    /// The fallback title. Read from the turns rather than copied into `title` at creation, so it
    /// cannot go stale.
    pub first_message: Option<String>,
    pub last_activity: Option<String>,
}

/// Opens a conversation. The id is minted HERE, not accepted from the caller.
///
/// `chat_id` becomes part of a filename in the temporary MCP config, and `assistant.rs` encodes it
/// precisely because it arrives from a sidecar and cannot be trusted. That encoding stays; this
/// simply declines to open a second door for arbitrary strings.
pub async fn create(pool: &SqlitePool, brain: Brain) -> sqlx::Result<String> {
    let chat_id = crate::auth::generate_uuid_v4();
    sqlx::query("INSERT INTO chats (chat_id, title, brain, created_at) VALUES (?, NULL, ?, ?)")
        .bind(&chat_id)
        .bind(brain.as_str())
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await?;
    Ok(chat_id)
}

/// The app's conversations, most recently active first.
///
/// The two correlated subqueries, and not a `LEFT JOIN` with aggregates: a chat with no turns yet is
/// exactly the case this table was added for, and an inner join would hide it — reintroducing the
/// old rule that a conversation is only real once it has answered.
///
/// Ordered by `id` inside each subquery rather than by `created_at`, for the reason
/// `get_assistant_chat` already gives: two turns of one conversation can share a timestamp to the
/// second, and "the first message" must not depend on which of them SQLite happens to return.
pub async fn list(pool: &SqlitePool) -> sqlx::Result<Vec<ChatSummary>> {
    sqlx::query_as::<_, ChatSummary>(
        "SELECT c.chat_id, c.title, c.brain, c.created_at,
                (SELECT r.prompt FROM runs r
                  WHERE r.chat_id = c.chat_id AND r.mode = 'assistant'
                  ORDER BY r.id ASC LIMIT 1) AS first_message,
                (SELECT r.created_at FROM runs r
                  WHERE r.chat_id = c.chat_id AND r.mode = 'assistant'
                  ORDER BY r.id DESC LIMIT 1) AS last_activity
           FROM chats c
          WHERE c.archived_at IS NULL
          ORDER BY COALESCE(last_activity, c.created_at) DESC",
    )
    .fetch_all(pool)
    .await
}

pub async fn get(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Option<ChatSummary>> {
    Ok(list(pool)
        .await?
        .into_iter()
        .find(|chat| chat.chat_id == chat_id))
}

/// The chat's brain, or `None` when this conversation has no row — which is every Telegram chat.
///
/// The `None` is load-bearing and is why this returns an Option rather than defaulting to `Cloud`:
/// `send_message` needs to tell "this chat chose cloud" apart from "nobody chose", because only the
/// second falls through to the origin rule that has always been there.
pub async fn brain_of(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Option<Brain>> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT brain FROM chats WHERE chat_id = ? AND archived_at IS NULL")
            .bind(chat_id)
            .fetch_optional(pool)
            .await?;
    Ok(value.as_deref().map(Brain::from_wire))
}

/// Sets which model answers this conversation from here on.
///
/// Says nothing about the session the previous model left behind — that is `http.rs`'s to forget,
/// because it is a decision about the conversation and not about this row.
pub async fn set_brain(pool: &SqlitePool, chat_id: &str, brain: Brain) -> sqlx::Result<()> {
    sqlx::query("UPDATE chats SET brain = ? WHERE chat_id = ?")
        .bind(brain.as_str())
        .bind(chat_id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// Names a conversation. A blank name clears it rather than storing whitespace, putting the
/// first-message fallback back.
pub async fn rename(pool: &SqlitePool, chat_id: &str, title: Option<&str>) -> sqlx::Result<()> {
    let title = title.map(str::trim).filter(|t| !t.is_empty());
    sqlx::query("UPDATE chats SET title = ? WHERE chat_id = ?")
        .bind(title)
        .bind(chat_id)
        .execute(pool)
        .await
        .map(|_| ())
}

pub async fn archive(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE chats SET archived_at = ? WHERE chat_id = ? AND archived_at IS NULL")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(chat_id)
        .execute(pool)
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn a_new_chat_is_listed_before_it_has_any_turns() {
        let pool = test_pool().await;

        let id = create(&pool, Brain::Cloud).await.unwrap();
        let listed = list(&pool).await.unwrap();

        // The whole reason for the table: a conversation you can open and not yet have used.
        assert!(listed.iter().any(|chat| chat.chat_id == id));
        assert_eq!(listed.iter().find(|c| c.chat_id == id).unwrap().title, None);
    }

    #[tokio::test]
    async fn a_telegram_conversation_has_no_row_and_is_not_listed() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, chat_id, created_at)
             VALUES ('olá', 'completed', 'assistant', 's', '-100200300', '2026-08-11T10:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let listed = list(&pool).await.unwrap();

        // No filter names Telegram anywhere. It is absent because nothing created a row for it.
        assert!(listed.iter().all(|chat| chat.chat_id != "-100200300"));
    }

    #[tokio::test]
    async fn archiving_removes_it_from_the_list_and_leaves_the_turns_alone() {
        let pool = test_pool().await;
        let id = create(&pool, Brain::Cloud).await.unwrap();
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, chat_id, created_at)
             VALUES ('olá', 'completed', 'assistant', 's', ?, '2026-08-11T10:00:00+00:00')",
        )
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();

        archive(&pool, &id).await.unwrap();

        assert!(list(&pool).await.unwrap().iter().all(|c| c.chat_id != id));
        // The turn is a billed run. Archiving hides the conversation, never the money.
        let turns: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE chat_id = ?")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(turns, 1);
    }

    #[tokio::test]
    async fn the_list_carries_the_first_message_so_an_unnamed_chat_has_something_to_show() {
        let pool = test_pool().await;
        let id = create(&pool, Brain::Cloud).await.unwrap();
        for (prompt, at) in [
            ("a primeira", "2026-08-11T10:00:00+00:00"),
            ("a segunda", "2026-08-11T11:00:00+00:00"),
        ] {
            sqlx::query(
                "INSERT INTO runs (prompt, status, mode, session_id, chat_id, created_at)
                 VALUES (?, 'completed', 'assistant', 's', ?, ?)",
            )
            .bind(prompt)
            .bind(&id)
            .bind(at)
            .execute(&pool)
            .await
            .unwrap();
        }

        let chat = list(&pool)
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.chat_id == id)
            .unwrap();

        assert_eq!(chat.first_message.as_deref(), Some("a primeira"));
        assert_eq!(
            chat.last_activity.as_deref(),
            Some("2026-08-11T11:00:00+00:00")
        );
    }

    #[tokio::test]
    async fn renaming_persists_and_an_empty_name_falls_back_to_no_name() {
        let pool = test_pool().await;
        let id = create(&pool, Brain::Cloud).await.unwrap();

        rename(&pool, &id, Some("sobre o orçamento")).await.unwrap();
        assert_eq!(
            get(&pool, &id).await.unwrap().unwrap().title.as_deref(),
            Some("sobre o orçamento")
        );

        // Clearing a name is a real intention, not a validation error — it puts the fallback back.
        rename(&pool, &id, Some("   ")).await.unwrap();
        assert_eq!(get(&pool, &id).await.unwrap().unwrap().title, None);
    }

    #[tokio::test]
    async fn a_conversation_with_no_row_has_no_brain_rather_than_a_default_one() {
        let pool = test_pool().await;
        let id = create(&pool, Brain::Local).await.unwrap();

        assert_eq!(brain_of(&pool, &id).await.unwrap(), Some(Brain::Local));
        // The `None` is what `send_message` reads to know nobody chose, so the origin rule still
        // decides. Defaulting to `Cloud` here would silently move every Telegram chat off the
        // local model.
        assert_eq!(brain_of(&pool, "-100200300").await.unwrap(), None);
    }
}
