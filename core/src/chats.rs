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
    /// Where this conversation's turns run, or `None` for the daemon's own directory.
    ///
    /// Set at creation for a conversation picked up from the editor, and by `set_cwd` for one that
    /// is told afterwards which project it is about. It used to be the first of those alone, which
    /// is why a conversation opened in the window could never have tools: `tool_policy_for` grants
    /// them on a directory, and there was no way to give it one.
    ///
    /// It travels to the list because the window has to show it: two conversations continued from
    /// two worktrees of the same repository are otherwise indistinguishable by anything a person
    /// can read.
    pub cwd: Option<String>,
    /// Which conversation had in the editor this one was picked up from, or `None` when it was
    /// opened here.
    ///
    /// Travels to the list because the window draws what was already said in that conversation
    /// above the turns the daemon ran. Read off this row rather than off `assistant_sessions`,
    /// which names the session the NEXT turn resumes and is replaced the first time a context
    /// rotates — see 0082.
    pub ide_session_id: Option<String>,
    /// The fallback title. Read from the turns rather than copied into `title` at creation, so it
    /// cannot go stale.
    pub first_message: Option<String>,
    pub last_activity: Option<String>,
    /// How many answers landed in this conversation since it was last opened.
    ///
    /// Waiting for YOU, not for the model: a turn still being written is the chat waiting on the
    /// model, and the list already has its own word for that. Counted at read time from the
    /// watermark rather than stored, so it is right after a crash without anything having been
    /// written when the turn ended.
    pub waiting: i64,
}

/// Opens a conversation. The id is minted HERE, not accepted from the caller.
///
/// `chat_id` becomes part of a filename in the temporary MCP config, and `assistant.rs` encodes it
/// precisely because it arrives from a sidecar and cannot be trusted. That encoding stays; this
/// simply declines to open a second door for arbitrary strings.
///
/// `picked_up` is the session this conversation continues, and it is named by every caller rather
/// than defaulted — for the reason `RunRequest` gives about its own fields: it decides both where
/// the turn runs AND how much it may do, and a parameter with a default is a parameter nobody chose.
///
/// The whole session and not its two facts separately. The directory and the id come from one file
/// and mean nothing apart: a row carrying one session's directory and another's id would resume a
/// conversation somewhere it was never had, which the CLI does not refuse — it quietly starts a new
/// session instead. Taking the pair as one value is what makes that pairing unable to be wrong.
pub async fn create(
    pool: &SqlitePool,
    brain: Brain,
    picked_up: Option<&crate::sessions::IdeSession>,
) -> sqlx::Result<String> {
    let chat_id = crate::auth::generate_uuid_v4();
    sqlx::query(
        "INSERT INTO chats (chat_id, title, brain, created_at, cwd, ide_session_id)
         VALUES (?, NULL, ?, ?, ?, ?)",
    )
    .bind(&chat_id)
    .bind(brain.as_str())
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(picked_up.map(|session| session.cwd.as_str()))
    .bind(picked_up.map(|session| session.session_id.as_str()))
    .execute(pool)
    .await?;
    Ok(chat_id)
}

/// Whether this conversation plans without acting.
///
/// Read on the turn path rather than carried on the summary, for the reason `cwd_of` is read there:
/// it decides what the run is LAUNCHED with, and a value that travelled through the window and back
/// would be a second copy of it free to disagree.
pub async fn plans_only(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar::<_, i64>("SELECT plan_only FROM chats WHERE chat_id = ?")
        .bind(chat_id)
        .fetch_optional(pool)
        .await
        .map(|found| found.unwrap_or(0) != 0)
}

/// Puts a conversation into planning, or takes it out.
pub async fn set_plan_only(pool: &SqlitePool, chat_id: &str, planning: bool) -> sqlx::Result<()> {
    sqlx::query("UPDATE chats SET plan_only = ? WHERE chat_id = ?")
        .bind(i64::from(planning))
        .bind(chat_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Points a conversation at the project it is about.
///
/// The second writer this column has ever had. The first is the pick-up, at creation, and until now
/// it was the only one — so a conversation opened in the window had no directory and no way to be
/// given one, which `tool_policy_for` reads as `McpOnly` for as long as it exists.
///
/// The caller has already checked that this is a directory. Here it is a string going into a column,
/// and a second check would be a second answer to a question the filesystem can change between them.
pub async fn set_cwd(pool: &SqlitePool, chat_id: &str, cwd: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE chats SET cwd = ? WHERE chat_id = ?")
        .bind(cwd)
        .bind(chat_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Records what a conversation was handed in place of the session it could not resume.
pub async fn set_handover(pool: &SqlitePool, chat_id: &str, handover: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE chats SET handover = ? WHERE chat_id = ?")
        .bind(handover)
        .bind(chat_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// What this conversation was handed when it could not be resumed, or `None`.
///
/// The verbatim tail of the editor session it was picked up from, taken once at pick-up and stored
/// as JSON pairs. Its own query rather than a field off `get`, for the reason `cwd_of` is: it is
/// read on the turn path, and `get` walks the whole list to answer.
pub async fn handover_of(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Option<String>> {
    let handover: Option<Option<String>> =
        sqlx::query_scalar("SELECT handover FROM chats WHERE chat_id = ?")
            .bind(chat_id)
            .fetch_optional(pool)
            .await?;
    Ok(handover.flatten())
}

/// Where this conversation's turns run, or `None` for the daemon's own directory.
///
/// Its own query rather than a field off `get`, matching `brain_of`: this is read on the hot path of
/// every single turn, and `get` walks the whole list to answer.
pub async fn cwd_of(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Option<String>> {
    opened_in(pool, chat_id).await.map(Option::flatten)
}

/// One message waiting to be said, and the name it can be taken back by.
///
/// An id and not a position: the drain removes the front of the queue while a person is looking at
/// it, so "the second one" means something different a moment later — and taking one back by
/// position would take back a message nobody pointed at.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct Waiting {
    pub id: i64,
    pub text: String,
}

/// What is waiting to be said to this conversation, oldest first.
///
/// Ordered by `id` and never by `created_at`: two messages typed in the same second must not swap
/// places, and a queue that reorders itself is one nobody can predict.
pub async fn queued(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Vec<Waiting>> {
    sqlx::query_as("SELECT id, text FROM chat_queue WHERE chat_id = ? ORDER BY id")
        .bind(chat_id)
        .fetch_all(pool)
        .await
}

/// Takes a waiting message back off the queue, answering whether there was one to take.
///
/// The chat is part of the WHERE and not merely checked first. A delete that finds the row by id
/// alone and trusts the caller about whose it is has no defence at all, and the two-step version —
/// read it, check the chat, delete it — has a window between the check and the delete.
///
/// `false` rather than an error when nothing matched: the drain may have sent that message a moment
/// ago, and losing that race is a thing a person does harmlessly, not a fault to report.
pub async fn drop_queued(pool: &SqlitePool, chat_id: &str, id: i64) -> sqlx::Result<bool> {
    sqlx::query("DELETE FROM chat_queue WHERE id = ? AND chat_id = ?")
        .bind(id)
        .bind(chat_id)
        .execute(pool)
        .await
        .map(|done| done.rows_affected() > 0)
}

/// Keeps a message, and whatever was attached to it, until the conversation has a turn free.
///
/// The pictures travel as bytes here, unlike on a run, which keeps paths. The two rows have
/// opposite lives: a run is read on every poll and lives for ever, a queued message is read once by
/// the drain that sends it and is deleted in the same statement. Keeping the words and losing the
/// screenshot would be losing half of what somebody sent, without saying so.
pub async fn enqueue(
    pool: &SqlitePool,
    chat_id: &str,
    text: &str,
    origin: &str,
    images: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO chat_queue (chat_id, text, origin, images, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(chat_id)
    .bind(text)
    .bind(origin)
    .bind(images)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await
    .map(|_| ())
}

/// Takes the oldest waiting message off this conversation's queue, or `None` when there is none.
///
/// Deleted as it is read, in one statement, rather than read and then deleted after it has been
/// sent. Two drains racing the same row is the failure that matters here — the same words sent
/// twice, billed twice — and `RETURNING` makes the row belong to exactly one of them. The other
/// order would be safer against a message lost to a crash mid-send, and that is the wrong trade:
/// one lost message is a person retyping a sentence, one duplicated message is a turn nobody asked
/// for acting on a conversation twice.
pub async fn take_queued(
    pool: &SqlitePool,
    chat_id: &str,
) -> sqlx::Result<Option<(String, Option<String>, Option<String>)>> {
    sqlx::query_as(
        "DELETE FROM chat_queue
          WHERE id = (SELECT id FROM chat_queue WHERE chat_id = ? ORDER BY id LIMIT 1)
      RETURNING text, origin, images",
    )
    .bind(chat_id)
    .fetch_optional(pool)
    .await
}

/// Where this conversation runs, keeping "no such conversation" apart from "no directory".
///
/// `cwd_of` flattens the two into one `None` because the turn path cannot act on the difference: a
/// chat with no directory and a chat that is gone both mean "do not set a working directory". A
/// caller that answers a person can act on it — one is a 404 and the other is a sentence — so the
/// unflattened answer lives here and `cwd_of` is written in terms of it, rather than the two
/// queries drifting apart.
pub async fn opened_in(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Option<Option<String>>> {
    sqlx::query_scalar("SELECT cwd FROM chats WHERE chat_id = ?")
        .bind(chat_id)
        .fetch_optional(pool)
        .await
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
        "SELECT c.chat_id, c.title, c.brain, c.created_at, c.cwd, c.ide_session_id,
                (SELECT r.prompt FROM runs r
                  WHERE r.chat_id = c.chat_id AND r.mode = 'assistant'
                  ORDER BY r.id ASC LIMIT 1) AS first_message,
                (SELECT r.created_at FROM runs r
                  WHERE r.chat_id = c.chat_id AND r.mode = 'assistant'
                  ORDER BY r.id DESC LIMIT 1) AS last_activity,
                -- Answers that landed since this chat was last opened. `status NOT IN` and not
                -- `= 'completed'`: a turn that failed, timed out or was cancelled has stopped
                -- moving and is something to come back to, and `runs.status` is free-form TEXT
                -- (0002) — naming the two live states is the list that stays right when a new
                -- terminal one is added.
                (SELECT COUNT(*) FROM runs r
                  WHERE r.chat_id = c.chat_id AND r.mode = 'assistant'
                    AND r.status NOT IN ('running', 'pending')
                    AND r.id > COALESCE(c.last_seen_turn_id, 0)) AS waiting
           FROM chats c
          WHERE c.archived_at IS NULL
          ORDER BY COALESCE(last_activity, c.created_at) DESC",
    )
    .fetch_all(pool)
    .await
}

/// One conversation, or `None` when it is not one of the app's — never opened here, or archived.
///
/// Read through `list` rather than with a query of its own, so there is exactly one definition of
/// what a listed chat is and of where its fallback title comes from. A second SELECT saying almost
/// the same thing is how the two drift.
pub async fn get(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Option<ChatSummary>> {
    Ok(list(pool)
        .await?
        .into_iter()
        .find(|chat| chat.chat_id == chat_id))
}

/// Every conversation had in the editor that this daemon has already picked up.
///
/// Two columns, and neither is the other's fallback. `chats.ide_session_id` is where a conversation
/// CAME FROM and never moves; `assistant_sessions.session_id` is what its next turn RESUMES and is
/// replaced the first time a context rotates or a turn reads third-party text. A session is spoken
/// for if either names it: without the first, a rotated conversation puts its own origin back on
/// offer and picking it up again would put two threads on one context; without the second, the
/// sessions the daemon minted here would be missing from the answer this has always given.
///
/// Archiving does not release one, which is the behaviour that was already there — the
/// `assistant_sessions` row outlives the archive — and is stated the same way for both halves.
pub async fn picked_up(pool: &SqlitePool) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT ide_session_id FROM chats WHERE ide_session_id IS NOT NULL
          UNION
         SELECT session_id FROM assistant_sessions WHERE session_id IS NOT NULL",
    )
    .fetch_all(pool)
    .await
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

/// Records that this conversation has been read up to its last turn that had LANDED.
///
/// The watermark is chosen here rather than accepted from the caller, and it is the last SETTLED
/// turn rather than simply the last one. Both halves matter:
///
/// A client that named its own watermark could mark a turn it had not drawn yet — a list read that
/// overtook the transcript would silently swallow the answer it was meant to announce. And taking
/// the last turn of any kind would swallow one still being written: opening a chat mid-turn would
/// mark the answer seen seconds before it arrived, which is exactly the case this whole thing is
/// for.
///
/// `MAX(id)` over no rows is NULL, and `COALESCE` keeps that from clearing a watermark already set —
/// a chat whose turns were all still live would otherwise be marked back to unread by being opened.
pub async fn mark_seen(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE chats
            SET last_seen_turn_id = COALESCE(
                  (SELECT MAX(r.id) FROM runs r
                    WHERE r.chat_id = chats.chat_id
                      AND r.mode = 'assistant'
                      AND r.status NOT IN ('running', 'pending')),
                  last_seen_turn_id)
          WHERE chat_id = ?",
    )
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
mod queue_tests {
    use super::*;

    async fn pool_with_a_queue() -> SqlitePool {
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

    /// A message that waits must be nameable, or nothing can take it back.
    ///
    /// By id and not by position: the drain removes the front of the queue while a person is
    /// looking at it, so "the second one" means something different a moment later — and deleting
    /// by position would take back a message somebody never pointed at.
    #[tokio::test]
    async fn what_waits_can_be_named_and_taken_back() {
        let pool = pool_with_a_queue().await;
        for text in ["primeiro", "segundo"] {
            enqueue(&pool, "c-1", text, "shell", "[]").await.unwrap();
        }

        let waiting = queued(&pool, "c-1").await.unwrap();
        assert_eq!(waiting.len(), 2);
        assert_eq!(waiting[0].text, "primeiro");

        assert!(drop_queued(&pool, "c-1", waiting[0].id).await.unwrap());

        let left = queued(&pool, "c-1").await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].text, "segundo");
    }

    /// One conversation must not be able to take a message out of another's queue.
    ///
    /// The chat is part of the WHERE and not merely checked first: a delete that finds the row by
    /// id alone and trusts the caller about whose it is has no defence at all, and the two-step
    /// version has a window between the check and the delete.
    #[tokio::test]
    async fn a_conversation_cannot_take_a_message_out_of_another_ones_queue() {
        let pool = pool_with_a_queue().await;
        enqueue(&pool, "mine", "meu", "shell", "[]").await.unwrap();
        let mine = queued(&pool, "mine").await.unwrap()[0].id;

        assert!(!drop_queued(&pool, "somebody-else", mine).await.unwrap());

        assert_eq!(queued(&pool, "mine").await.unwrap().len(), 1);
    }

    /// Taking back something already gone is `false`, never an error: the drain may have sent it a
    /// moment ago, and that is a race a person loses harmlessly rather than a fault.
    #[tokio::test]
    async fn taking_back_something_already_gone_says_so_without_failing() {
        let pool = pool_with_a_queue().await;

        assert!(!drop_queued(&pool, "c-1", 999).await.unwrap());
    }
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

        let id = create(&pool, Brain::Cloud, None).await.unwrap();
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
        let id = create(&pool, Brain::Cloud, None).await.unwrap();
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
        let id = create(&pool, Brain::Cloud, None).await.unwrap();
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
        let id = create(&pool, Brain::Cloud, None).await.unwrap();

        rename(&pool, &id, Some("sobre o orçamento")).await.unwrap();
        assert_eq!(
            get(&pool, &id).await.unwrap().unwrap().title.as_deref(),
            Some("sobre o orçamento")
        );

        // Clearing a name is a real intention, not a validation error — it puts the fallback back.
        rename(&pool, &id, Some("   ")).await.unwrap();
        assert_eq!(get(&pool, &id).await.unwrap().unwrap().title, None);
    }

    /// Records a turn in a chat and answers with its id, so a test can talk about "up to here".
    async fn turn_in(pool: &SqlitePool, chat_id: &str, status: &str) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, chat_id, created_at)
             VALUES ('olá', ?, 'assistant', ?, '2026-08-11T10:00:00+00:00')",
        )
        .bind(status)
        .bind(chat_id)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn waiting_in(pool: &SqlitePool, chat_id: &str) -> i64 {
        get(pool, chat_id).await.unwrap().unwrap().waiting
    }

    #[tokio::test]
    async fn a_turn_still_thinking_is_not_something_to_come_back_to() {
        let pool = test_pool().await;
        let id = create(&pool, Brain::Cloud, None).await.unwrap();

        turn_in(&pool, &id, "running").await;

        // "Waiting" means waiting for YOU. A turn still being written is the chat waiting on the
        // model, which the list already says with its own word.
        assert_eq!(waiting_in(&pool, &id).await, 0);
    }

    #[tokio::test]
    async fn an_answer_that_landed_is_waiting_until_the_chat_is_opened() {
        let pool = test_pool().await;
        let id = create(&pool, Brain::Cloud, None).await.unwrap();
        turn_in(&pool, &id, "completed").await;

        assert_eq!(waiting_in(&pool, &id).await, 1);

        mark_seen(&pool, &id).await.unwrap();

        assert_eq!(waiting_in(&pool, &id).await, 0);
    }

    #[tokio::test]
    async fn a_turn_that_failed_is_waiting_too() {
        let pool = test_pool().await;
        let id = create(&pool, Brain::Cloud, None).await.unwrap();
        turn_in(&pool, &id, "failed").await;

        // Knowing the answer never came matters at least as much as knowing it did — and a failed
        // turn is a billed run either way.
        assert_eq!(waiting_in(&pool, &id).await, 1);
    }

    #[tokio::test]
    async fn only_what_landed_after_the_last_look_counts() {
        let pool = test_pool().await;
        let id = create(&pool, Brain::Cloud, None).await.unwrap();
        turn_in(&pool, &id, "completed").await;
        mark_seen(&pool, &id).await.unwrap();

        turn_in(&pool, &id, "completed").await;
        turn_in(&pool, &id, "completed").await;

        assert_eq!(waiting_in(&pool, &id).await, 2);
    }

    /// The mark is a watermark over turns that have LANDED, so a turn still in flight cannot be
    /// swallowed by opening the chat while it is being written.
    #[tokio::test]
    async fn opening_a_chat_mid_turn_does_not_mark_the_answer_still_coming() {
        let pool = test_pool().await;
        let id = create(&pool, Brain::Cloud, None).await.unwrap();
        turn_in(&pool, &id, "completed").await;
        let live = turn_in(&pool, &id, "running").await;

        mark_seen(&pool, &id).await.unwrap();
        // The live turn now lands.
        sqlx::query("UPDATE runs SET status = 'completed' WHERE id = ?")
            .bind(live)
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(waiting_in(&pool, &id).await, 1);
    }

    #[tokio::test]
    async fn a_conversation_with_no_row_has_no_brain_rather_than_a_default_one() {
        let pool = test_pool().await;
        let id = create(&pool, Brain::Local, None).await.unwrap();

        assert_eq!(brain_of(&pool, &id).await.unwrap(), Some(Brain::Local));
        // The `None` is what `send_message` reads to know nobody chose, so the origin rule still
        // decides. Defaulting to `Cloud` here would silently move every Telegram chat off the
        // local model.
        assert_eq!(brain_of(&pool, "-100200300").await.unwrap(), None);
    }

    /// A conversation picked up from the editor remembers WHICH conversation it was picked up from,
    /// on its own row.
    ///
    /// Not read back off `assistant_sessions`. That row holds the session the NEXT turn resumes,
    /// and the daemon replaces it whenever a context rotates past its ceiling or a turn reads
    /// third-party text — so within a message or two it names a session the CLI minted here, not
    /// the one this conversation came from. The window needs the original every time the chat is
    /// opened, to draw what was already said in it, and the original never changes.
    #[tokio::test]
    async fn a_chat_picked_up_from_the_editor_remembers_which_conversation_it_came_from() {
        let pool = test_pool().await;
        let picked_up = crate::sessions::IdeSession {
            session_id: "aaaa-1111".into(),
            cwd: "C:/Projects/nucleos".into(),
            title: Some("arranja o parser".into()),
            last_activity: "2026-08-11T10:00:00+00:00".into(),
        };

        let id = create(&pool, Brain::Cloud, Some(&picked_up)).await.unwrap();

        let chat = get(&pool, &id).await.unwrap().unwrap();
        assert_eq!(chat.ide_session_id.as_deref(), Some("aaaa-1111"));
        // The two travel together because they came from one place, which is the whole reason
        // `create` takes the session rather than the two facts separately.
        assert_eq!(chat.cwd.as_deref(), Some("C:/Projects/nucleos"));
    }

    #[tokio::test]
    async fn a_chat_opened_here_came_from_no_conversation_at_all() {
        let pool = test_pool().await;

        let id = create(&pool, Brain::Cloud, None).await.unwrap();

        let chat = get(&pool, &id).await.unwrap().unwrap();
        assert_eq!(chat.ide_session_id, None);
        assert_eq!(chat.cwd, None);
    }

    /// A conversation stays picked up after the session it resumes has moved on.
    ///
    /// This is the case the column exists for. The daemon rotates a context past its ceiling by
    /// minting a fresh session and replacing the row that named the old one — so a filter reading
    /// only `assistant_sessions` puts the conversation's ORIGIN back on the list of things to pick
    /// up, and picking it up a second time puts two threads on one context.
    #[tokio::test]
    async fn a_conversation_stays_picked_up_after_the_session_it_resumes_has_rotated() {
        let pool = test_pool().await;
        let id = create(
            &pool,
            Brain::Cloud,
            Some(&crate::sessions::had_in("C:/Projects/nucleos", "aaaa-1111")),
        )
        .await
        .unwrap();
        crate::assistant::upsert_session(&pool, &id, "aaaa-1111", "2026-08-11T10:00:00+00:00")
            .await
            .unwrap();

        // The context fills, and the next turn runs in a session the daemon minted here.
        crate::assistant::upsert_session(&pool, &id, "minted-here", "2026-08-11T11:00:00+00:00")
            .await
            .unwrap();

        let taken = picked_up(&pool).await.unwrap();
        assert!(taken.contains(&"aaaa-1111".to_string()), "{taken:?}");
        // And the one it resumes now, which is what kept the old filter honest before rotation.
        assert!(taken.contains(&"minted-here".to_string()), "{taken:?}");
    }
}
