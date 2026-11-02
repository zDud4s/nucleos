//! Moving a chat to another machine in the middle of work: the state of the move on THIS machine.
//!
//! The order, driven by the app: `preview`, the person confirms, `begin` freezes the origin,
//! `package`, `import` on the destination, `finish` at the origin. A failure before `finish` is an
//! `abort` and the chat goes on working here. This file is the part that lives in the database
//! (`chats.moved_to`, `chats.frozen_for`, `chats.frozen_at`, `chat_transfers_seen`, migration 0266)
//! and the wait `begin` does lives in `assistant/chat_move.rs`; packaging and importing are the next part's.
//!
//! A frozen chat refuses to START turns (`assistant::send`): a message typed meanwhile joins the
//! queue, which travels in the package. A moved chat refuses them with a sentence naming the
//! destination. A freeze older than [`FREEZE_TTL_SECS`] is read as gone, so an app that vanishes
//! between `begin` and `finish` cannot trap a chat.

use sqlx::SqlitePool;

/// How long a freeze lives without being confirmed alive ([`touch`]).
pub const FREEZE_TTL_SECS: i64 = 15 * 60;

/// What a message typed into a frozen chat is told. It is also the refusal `send_message_inner`
/// returns, which `send_or_queue` turns into a queued message.
pub const CHAT_FROZEN: &str =
    "this chat is being moved to another machine; the message is waiting and goes with it";

/// What a new turn in a moved chat is told.
pub fn moved_sentence(to: &str) -> String {
    format!("This chat was moved to {to}. Continue it there; this copy is read-only.")
}

/// Whether a chat may start a turn here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Open,
    /// A move is in progress; messages queue.
    Frozen,
    /// The chat lives on the named machine.
    Moved(String),
}

/// Why a step of the move was refused.
#[derive(Debug)]
pub enum Refusal {
    NoSuchChat,
    /// Frozen for another transfer.
    AlreadyFrozen,
    /// Already moved to the named machine.
    Moved(String),
    /// `finish` or a wait found no freeze for this transfer: it expired or was aborted.
    NotFrozenForThis,
    /// An import found the chat alive on this machine: it is not a chat that moved away.
    AlreadyHere,
    Store(sqlx::Error),
}

impl Refusal {
    pub fn message(&self) -> String {
        match self {
            Self::NoSuchChat => "no such chat".to_string(),
            Self::AlreadyFrozen => "this chat is already being moved".to_string(),
            Self::Moved(to) => moved_sentence(to),
            Self::NotFrozenForThis => {
                "this move is no longer in progress (it was aborted or expired)".to_string()
            }
            Self::AlreadyHere => "this chat already lives on this machine".to_string(),
            Self::Store(error) => format!("could not read the chat: {error}"),
        }
    }
}

impl From<sqlx::Error> for Refusal {
    fn from(error: sqlx::Error) -> Self {
        Self::Store(error)
    }
}

/// Unix seconds, the clock every function below defaults to.
pub fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Drops a freeze nobody confirmed within [`FREEZE_TTL_SECS`]. Called by every read of the freeze.
async fn expire(pool: &SqlitePool, chat_id: &str, now: i64) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE chats SET frozen_for = NULL, frozen_at = NULL
          WHERE chat_id = ? AND frozen_for IS NOT NULL
            AND (frozen_at IS NULL OR frozen_at <= ?)",
    )
    .bind(chat_id)
    .bind(now - FREEZE_TTL_SECS)
    .execute(pool)
    .await?;
    Ok(())
}

/// [`gate_at`] on the real clock; a store error reads as open, because refusing every turn of a
/// chat over a failed read would be worse than the race it closes.
pub async fn gate(pool: &SqlitePool, chat_id: &str) -> Gate {
    match gate_at(pool, chat_id, unix_now()).await {
        Ok(gate) => gate,
        Err(error) => {
            tracing::warn!(%error, chat_id, "could not read the transfer state of a chat");
            Gate::Open
        }
    }
}

/// Whether the chat may start a turn at `now`. A chat with no row is open.
pub async fn gate_at(pool: &SqlitePool, chat_id: &str, now: i64) -> sqlx::Result<Gate> {
    expire(pool, chat_id, now).await?;
    let row: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT moved_to, frozen_for FROM chats WHERE chat_id = ?")
            .bind(chat_id)
            .fetch_optional(pool)
            .await?;
    Ok(match row {
        Some((Some(to), _)) => Gate::Moved(to),
        Some((None, Some(_))) => Gate::Frozen,
        _ => Gate::Open,
    })
}

/// Freezes the chat for `transfer_id`. Repeating the same id is fine (and confirms it alive);
/// another id on a frozen chat, or any id on a moved one, is refused.
pub async fn freeze_at(
    pool: &SqlitePool,
    chat_id: &str,
    transfer_id: &str,
    now: i64,
) -> Result<(), Refusal> {
    expire(pool, chat_id, now).await?;
    let changed = sqlx::query(
        "UPDATE chats SET frozen_for = ?, frozen_at = ?
          WHERE chat_id = ? AND moved_to IS NULL
            AND (frozen_for IS NULL OR frozen_for = ?)",
    )
    .bind(transfer_id)
    .bind(now)
    .bind(chat_id)
    .bind(transfer_id)
    .execute(pool)
    .await?
    .rows_affected();
    if changed == 1 {
        return Ok(());
    }
    Err(match gate_at(pool, chat_id, now).await? {
        Gate::Moved(to) => Refusal::Moved(to),
        Gate::Frozen => Refusal::AlreadyFrozen,
        Gate::Open => Refusal::NoSuchChat,
    })
}

/// Confirms the freeze of `transfer_id` alive, answering whether it still stands.
pub async fn touch_at(
    pool: &SqlitePool,
    chat_id: &str,
    transfer_id: &str,
    now: i64,
) -> sqlx::Result<bool> {
    expire(pool, chat_id, now).await?;
    let changed =
        sqlx::query("UPDATE chats SET frozen_at = ? WHERE chat_id = ? AND frozen_for = ?")
            .bind(now)
            .bind(chat_id)
            .bind(transfer_id)
            .execute(pool)
            .await?
            .rows_affected();
    Ok(changed == 1)
}

/// Lifts the freeze of `transfer_id`. Safe to repeat and safe for a freeze that is not this one's:
/// it only ever clears its own. Answers whether it cleared one.
pub async fn abort(pool: &SqlitePool, chat_id: &str, transfer_id: &str) -> sqlx::Result<bool> {
    let changed = sqlx::query(
        "UPDATE chats SET frozen_for = NULL, frozen_at = NULL
          WHERE chat_id = ? AND frozen_for = ?",
    )
    .bind(chat_id)
    .bind(transfer_id)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(changed == 1)
}

/// Completes the move: the chat now lives at `to`, the freeze ends, and the waiting messages (which
/// travelled in the package) leave this machine's queue. Needs the freeze of `transfer_id` to
/// still stand; repeating a finish that already happened for the same `to` is a no-op.
pub async fn finish_at(
    pool: &SqlitePool,
    chat_id: &str,
    transfer_id: &str,
    to: &str,
    now: i64,
) -> Result<(), Refusal> {
    expire(pool, chat_id, now).await?;
    let mut tx = pool.begin().await?;
    let changed = sqlx::query(
        "UPDATE chats SET moved_to = ?, frozen_for = NULL, frozen_at = NULL
          WHERE chat_id = ? AND frozen_for = ? AND moved_to IS NULL",
    )
    .bind(to)
    .bind(chat_id)
    .bind(transfer_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if changed == 0 {
        drop(tx);
        let moved: Option<Option<String>> =
            sqlx::query_scalar("SELECT moved_to FROM chats WHERE chat_id = ?")
                .bind(chat_id)
                .fetch_optional(pool)
                .await?;
        return match moved {
            None => Err(Refusal::NoSuchChat),
            Some(Some(already)) if already == to => Ok(()),
            _ => Err(Refusal::NotFrozenForThis),
        };
    }
    sqlx::query("DELETE FROM chat_queue WHERE chat_id = ?")
        .bind(chat_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Records that this machine imported `transfer_id`, answering whether it is the first time.
pub async fn record_import(
    pool: &SqlitePool,
    transfer_id: &str,
    chat_id: &str,
    now: i64,
) -> sqlx::Result<bool> {
    let changed = sqlx::query(
        "INSERT OR IGNORE INTO chat_transfers_seen (transfer_id, chat_id, seen_at) VALUES (?, ?, ?)",
    )
    .bind(transfer_id)
    .bind(chat_id)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(changed.rows_affected() == 1)
}

/// What the dialog shows before the person confirms.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Preview {
    /// What dies if the move is `now`: the turn in flight and the tasks it launched.
    pub will_stop: Vec<String>,
    /// Ignored files that stay behind.
    pub left_behind: Vec<String>,
    /// A turn is in flight here.
    pub running: bool,
    /// The project and the code can be recreated on the destination.
    pub target_ok: bool,
    /// Why not, when `target_ok` is false.
    pub target_problem: Option<String>,
}

/// What the code check found.
#[derive(Debug, Clone, Default)]
pub struct CodeCheck {
    pub ok: bool,
    pub problem: Option<String>,
    pub left_behind: Vec<String>,
}

/// Whether the chat's code can be bundled here: its folder is a git repository of a project on the
/// roster, has a commit, is not a shallow clone and has no unresolved merge conflict. The ignored
/// files that would stay behind are listed. A chat with no folder has no code and moves freely.
/// Looks only; nothing is frozen or built.
pub async fn check_code(pool: &SqlitePool, chat_id: &str) -> CodeCheck {
    let cwd = match crate::chats::cwd_of(pool, chat_id).await {
        Ok(Some(cwd)) if !cwd.trim().is_empty() => cwd,
        Ok(_) => {
            return CodeCheck {
                ok: true,
                ..CodeCheck::default()
            };
        }
        Err(error) => {
            return CodeCheck {
                ok: false,
                problem: Some(format!("could not read the chat: {error}")),
                left_behind: Vec::new(),
            };
        }
    };
    match crate::chats::transfer_code::probe(pool, std::path::Path::new(&cwd)).await {
        Ok(probe) => CodeCheck {
            ok: true,
            problem: None,
            left_behind: probe.left_behind,
        },
        Err(problem) => CodeCheck {
            ok: false,
            problem: Some(problem),
            left_behind: Vec::new(),
        },
    }
}

/// Whether this machine already imported `transfer_id`.
pub async fn was_imported(pool: &SqlitePool, transfer_id: &str) -> sqlx::Result<bool> {
    let seen: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM chat_transfers_seen WHERE transfer_id = ?")
            .bind(transfer_id)
            .fetch_optional(pool)
            .await?;
    Ok(seen.is_some())
}

/// Words for a task that a `now` move ends.
fn task_line(task: &crate::chats::tasks::ChatTask) -> String {
    let what = match task.kind.as_str() {
        "subagent" => "A subagent",
        "background_bash" => "A background command",
        "background_agent" => "A background agent",
        _ => "A task",
    };
    match task.subagent_type.as_deref() {
        Some(kind) => format!("{what} ({kind}) is still running and ends."),
        None => format!("{what} is still running and ends."),
    }
}

/// What is knowable without packaging anything. Refuses a chat that is moved or already frozen.
/// `parked` is the chat's open questions, read by the caller from the hook store (which lives in
/// the crate above this one).
pub async fn preview(
    pool: &SqlitePool,
    chat_id: &str,
    running: bool,
    parked: &[super::transfer_package::ParkedQuestion],
) -> Result<Preview, Refusal> {
    match gate_at(pool, chat_id, unix_now()).await? {
        Gate::Moved(to) => return Err(Refusal::Moved(to)),
        Gate::Frozen => return Err(Refusal::AlreadyFrozen),
        Gate::Open => {}
    }
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM chats WHERE chat_id = ?")
        .bind(chat_id)
        .fetch_optional(pool)
        .await?;
    if exists.is_none() {
        return Err(Refusal::NoSuchChat);
    }
    let mut will_stop = Vec::new();
    if running {
        will_stop.push(
            "The turn in progress is cut off; what it has not written yet is lost.".to_string(),
        );
    }
    for task in crate::chats::tasks::for_chat(pool, chat_id).await? {
        if task.status == "running" {
            will_stop.push(task_line(&task));
        }
    }
    // A tool call parked on a question is lost with the turn; the destination asks it again.
    for ask in parked {
        will_stop.push(format!(
            "A question about {} is waiting for your answer; it is asked again on the other machine.",
            ask.tool
        ));
    }
    let code = check_code(pool, chat_id).await;
    Ok(Preview {
        will_stop,
        left_behind: code.left_behind,
        running,
        target_ok: code.ok,
        target_problem: code.problem,
    })
}

/// When a move freezes the chat relative to a turn in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum When {
    /// Wait for the turn to end; new messages queue and travel.
    AfterTurn,
    /// Cut the turn now.
    Now,
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn a_chat(pool: &SqlitePool, chat_id: &str) {
        sqlx::query("INSERT INTO chats (chat_id, brain, created_at) VALUES (?, 'cloud', ?)")
            .bind(chat_id)
            .bind("2026-01-01T00:00:00Z")
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_second_move_of_a_frozen_chat_is_refused() {
        let pool = crate::testdb::fresh_pool().await;
        a_chat(&pool, "c1").await;
        freeze_at(&pool, "c1", "t-1", 1_000).await.unwrap();

        let second = freeze_at(&pool, "c1", "t-2", 1_001).await;

        assert!(matches!(second, Err(Refusal::AlreadyFrozen)), "{second:?}");
        // The first move is untouched, and repeating its own id is fine.
        assert_eq!(gate_at(&pool, "c1", 1_002).await.unwrap(), Gate::Frozen);
        freeze_at(&pool, "c1", "t-1", 1_003).await.unwrap();
        // A move already finished refuses another one too.
        finish_at(&pool, "c1", "t-1", "laptop", 1_004)
            .await
            .unwrap();
        let third = freeze_at(&pool, "c1", "t-3", 1_005).await;
        assert!(
            matches!(third, Err(Refusal::Moved(ref to)) if to == "laptop"),
            "{third:?}"
        );
    }

    #[tokio::test]
    async fn a_dropped_connection_leaves_the_chat_working_at_the_origin() {
        let pool = crate::testdb::fresh_pool().await;
        a_chat(&pool, "c1").await;
        a_chat(&pool, "c2").await;

        // An abort: the chat is open again, and the queue it gathered is still there.
        freeze_at(&pool, "c1", "t-1", 1_000).await.unwrap();
        crate::chats::enqueue(&pool, "c1", "typed meanwhile", "shell", "[]")
            .await
            .unwrap();
        assert!(abort(&pool, "c1", "t-1").await.unwrap());
        assert_eq!(gate_at(&pool, "c1", 1_001).await.unwrap(), Gate::Open);
        assert_eq!(crate::chats::queued(&pool, "c1").await.unwrap().len(), 1);
        // An abort for another transfer's id clears nothing, and a repeat is harmless.
        freeze_at(&pool, "c1", "t-2", 1_002).await.unwrap();
        assert!(!abort(&pool, "c1", "t-1").await.unwrap());
        assert_eq!(gate_at(&pool, "c1", 1_003).await.unwrap(), Gate::Frozen);

        // The app vanished: the freeze expires by itself, read at the 15 minute mark, and a finish
        // that arrives too late is refused instead of moving a chat nobody confirmed.
        freeze_at(&pool, "c2", "t-9", 5_000).await.unwrap();
        let just_before = 5_000 + FREEZE_TTL_SECS - 1;
        assert_eq!(
            gate_at(&pool, "c2", just_before).await.unwrap(),
            Gate::Frozen
        );
        let late = 5_000 + FREEZE_TTL_SECS;
        let finished = finish_at(&pool, "c2", "t-9", "laptop", late).await;
        assert!(
            matches!(finished, Err(Refusal::NotFrozenForThis)),
            "{finished:?}"
        );
        assert_eq!(gate_at(&pool, "c2", late).await.unwrap(), Gate::Open);
        // Confirming the freeze alive keeps it from expiring.
        freeze_at(&pool, "c2", "t-10", late).await.unwrap();
        assert!(touch_at(&pool, "c2", "t-10", late + 800).await.unwrap());
        assert_eq!(
            gate_at(&pool, "c2", late + 800 + FREEZE_TTL_SECS - 1)
                .await
                .unwrap(),
            Gate::Frozen
        );
    }

    #[tokio::test]
    async fn finish_hands_the_queue_over_and_a_repeat_changes_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        a_chat(&pool, "c1").await;
        freeze_at(&pool, "c1", "t-1", 1_000).await.unwrap();
        crate::chats::enqueue(&pool, "c1", "waiting", "shell", "[]")
            .await
            .unwrap();

        finish_at(&pool, "c1", "t-1", "laptop", 1_001)
            .await
            .unwrap();
        finish_at(&pool, "c1", "t-1", "laptop", 1_002)
            .await
            .unwrap();

        assert!(crate::chats::queued(&pool, "c1").await.unwrap().is_empty());
        assert_eq!(
            gate_at(&pool, "c1", 1_003).await.unwrap(),
            Gate::Moved("laptop".to_string())
        );
        assert!(record_import(&pool, "t-1", "c1", 1_004).await.unwrap());
        assert!(!record_import(&pool, "t-1", "c1", 1_005).await.unwrap());
    }

    #[tokio::test]
    async fn the_preview_lists_what_a_move_now_would_end() {
        let pool = crate::testdb::fresh_pool().await;
        a_chat(&pool, "c1").await;
        sqlx::query(
            "INSERT INTO chat_tasks (chat_id, launched_by_run_id, tool_use_id, kind, status, started_at)
             VALUES ('c1', 1, 'tu-1', 'background_bash', 'running', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let idle = preview(&pool, "c1", false, &[]).await.unwrap();
        let busy = preview(&pool, "c1", true, &[]).await.unwrap();

        assert!(!idle.running && busy.running);
        assert_eq!(idle.will_stop.len(), 1, "{idle:?}");
        assert!(idle.will_stop[0].contains("background command"), "{idle:?}");
        assert_eq!(busy.will_stop.len(), 2, "{busy:?}");
        assert!(busy.will_stop[0].contains("turn in progress"), "{busy:?}");
        assert!(busy.target_ok);
        freeze_at(&pool, "c1", "t-1", unix_now()).await.unwrap();
        assert!(matches!(
            preview(&pool, "c1", false, &[]).await,
            Err(Refusal::AlreadyFrozen)
        ));
    }
}
