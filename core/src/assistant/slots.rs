//! Per-chat turn slots, steering, stopping, and immediate answers.

use super::*;

pub(super) static BUSY_CHATS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Owns a chat's one turn slot for as long as it is held, releasing it on every way out.
///
/// A guard rather than a matching pair of calls, because there is no point in the turn's life where
/// a trailing statement is reliable. Cancelling a run calls `abort()`
/// (`runs::finalize_termination`), which drops the task's future mid-await; abandoning the HTTP
/// request that started the turn drops that future the same way. Anything written after an `.await`
/// then never runs, and a chat left behind in `BUSY_CHATS` rejects every later message with 409
/// until the daemon restarts — "the bot stopped answering" points nowhere near either cause.
pub(super) struct ChatSlot {
    pub(super) chat_id: String,
}

impl ChatSlot {
    /// Claims the chat, or returns None if a turn is already in flight for it.
    pub(super) fn acquire(chat_id: &str) -> Option<Self> {
        BUSY_CHATS
            .lock()
            .unwrap()
            .insert(chat_id.to_string())
            .then(|| Self {
                chat_id: chat_id.to_string(),
            })
    }
}

impl Drop for ChatSlot {
    fn drop(&mut self) {
        BUSY_CHATS.lock().unwrap().remove(&self.chat_id);
    }
}

/// Every conversation's living CLI, so the next turn does not pay to start one.
///
/// A static beside `BUSY_CHATS`, and for the same reason: this is runtime state about a chat, keyed
/// by chat, that no request and no row owns. It is not in `AppState` because nothing outside this
/// module has any business speaking into a conversation's stdin.
pub(super) static LIVE_CHATS: LazyLock<Mutex<HashMap<String, LiveChat>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// When a visible view last polled each chat's transcript: chat id to that instant. Runtime state
/// beside `LIVE_CHATS`, for the same reason. It anchors the idle clock and spares open chats when
/// the cap forces an eviction; entries older than `DEV_IDLE` are dropped by `reap_now`.
pub(super) static SEEN_CHATS: LazyLock<Mutex<HashMap<String, std::time::Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Records that a visible view just read this chat's transcript.
pub fn mark_open(chat_id: &str) {
    SEEN_CHATS
        .lock()
        .unwrap()
        .insert(chat_id.to_owned(), std::time::Instant::now());
}

/// Whether a visible view polled this chat within [`OPEN_WINDOW`].
pub fn chat_is_open(chat_id: &str) -> bool {
    let seen = SEEN_CHATS.lock().unwrap().get(chat_id).copied();
    is_open(seen, std::time::Instant::now())
}

pub(super) fn is_open(seen: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    seen.is_some_and(|s| now.saturating_duration_since(s) < OPEN_WINDOW)
}

/// The way into a conversation's process WHILE a turn is running in it: chat id to a clone of
/// `LiveChat.messages`.
///
/// `LIVE_CHATS` holds a process only between turns (a turn takes it out), so a Stop or a Send now
/// arriving mid-turn has nothing to find there. An entry here is opened by a [`SteerGuard`] for the
/// length of one turn's gathering and is gone with it. Interrupt is not measured with a foreground
/// tool running or a background task alive (spike "verify before building"); the grace period in
/// [`stop_turn`] covers whatever that turns out to be.
pub(super) static STEERING: LazyLock<
    Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<crate::execution::runner::LaterTurn>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Keeps a chat's entry in [`STEERING`] for as long as it is held, on every way out (an aborted
/// task drops it too, which is why it is a guard and not a trailing call).
pub(super) struct SteerGuard {
    pub(super) chat_id: String,
}

impl SteerGuard {
    pub(super) fn open(
        chat_id: &str,
        messages: &tokio::sync::mpsc::UnboundedSender<crate::execution::runner::LaterTurn>,
    ) -> Self {
        STEERING
            .lock()
            .unwrap()
            .insert(chat_id.to_owned(), messages.clone());
        Self {
            chat_id: chat_id.to_owned(),
        }
    }
}

impl Drop for SteerGuard {
    fn drop(&mut self) {
        STEERING.lock().unwrap().remove(&self.chat_id);
    }
}

/// Writes a line into the process running a chat's turn, if one is. False when nothing is steerable.
pub(super) fn steer(chat_id: &str, turn: crate::execution::runner::LaterTurn) -> bool {
    // Cloned out so the lock is not held across the send.
    let sender = STEERING.lock().unwrap().get(chat_id).cloned();
    sender.is_some_and(|sender| sender.send(turn).is_ok())
}

/// Whether a turn is running in this chat's process right now, so a line written would land in it.
pub(super) fn is_steerable(chat_id: &str) -> bool {
    STEERING.lock().unwrap().contains_key(chat_id)
}

/// How long an interrupt is waited on before the kill path runs.
///
/// The CLI ends an interrupted turn in about 30 ms (spike CLI 2.1.280). A line written before the CLI
/// has started the turn (its start-up takes ~14 s) is never answered, and neither is one the
/// process cannot hear, so the wait is bounded and ends in what Stop always did.
pub(super) const INTERRUPT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// What Stop did to a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// The CLI was asked to stop and did: the turn is `cancelled`, the process and session live on.
    Interrupted,
    /// The turn's task was aborted and its process taken down, as `/runs/{id}/cancel` does.
    Killed,
    /// There was no running turn to stop.
    NotRunning,
}

/// Stops a chat turn: an interrupt when its process can be spoken to, the kill otherwise.
pub async fn stop_turn(state: &crate::state::AppState, turn_id: i64) -> Stopped {
    stop_turn_within(state, turn_id, INTERRUPT_GRACE).await
}

/// [`stop_turn`] with the wait for the interrupt's answer given, so a test need not wait five seconds.
pub(super) async fn stop_turn_within(
    state: &crate::state::AppState,
    turn_id: i64,
    grace: std::time::Duration,
) -> Stopped {
    let Ok(Some((chat_id, status))) = sqlx::query_as::<_, (Option<String>, String)>(
        "SELECT chat_id, status FROM runs WHERE id = ? AND mode = 'assistant'",
    )
    .bind(turn_id)
    .fetch_optional(&state.pool)
    .await
    else {
        return Stopped::NotRunning;
    };

    let interrupted = status == "running"
        && chat_id.as_deref().is_some_and(|chat| {
            steer(
                chat,
                crate::execution::runner::LaterTurn {
                    interrupt: true,
                    ..Default::default()
                },
            )
        });
    if interrupted {
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            let status: Option<String> = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(turn_id)
                .fetch_optional(&state.pool)
                .await
                .ok()
                .flatten();
            if status.as_deref() != Some("running") {
                return Stopped::Interrupted;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    if crate::execution::runs::finalize_termination(state, turn_id, "cancelled").await {
        Stopped::Killed
    } else {
        Stopped::NotRunning
    }
}

/// What became of a request to stop one agent of a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoppedTask {
    /// The running process was asked to stop it; the CLI reports the task ended on its stream.
    Asked,
    /// No turn of the conversation is running, so there is no process listening to ask.
    NotRunning,
}

/// Asks the conversation's running process to stop ONE task — a subagent or a background shell —
/// by the CLI's own `task_id`, leaving the turn and every other task to carry on.
///
/// Only while a turn runs: that is when the process reads its stdin. A task outliving its turn
/// cannot be reached this way, and the answer says so rather than pretending.
pub fn stop_task(chat_id: &str, task_id: &str) -> StoppedTask {
    let asked = steer(
        chat_id,
        crate::execution::runner::LaterTurn {
            stop_task: Some(task_id.to_owned()),
            ..Default::default()
        },
    );
    if asked {
        StoppedTask::Asked
    } else {
        StoppedTask::NotRunning
    }
}

/// What became of text said NOW, into a turn that was running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaidNow {
    /// It went down the running process's stdin and was recorded under the turn.
    Injected,
    /// There was nothing to steer, so it went the ordinary way: a turn, or the queue.
    Sent(Sent),
}

/// Something said while a turn was running, kept because the CLI folds it into the turn and the
/// stream never shows it as typed.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct SaidDuring {
    #[serde(skip)]
    pub run_id: i64,
    pub text: String,
    pub created_at: String,
}

/// Everything said into a chat's turns, oldest first.
pub async fn said_during(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Vec<SaidDuring>> {
    sqlx::query_as::<_, SaidDuring>(
        "SELECT run_id, text, created_at FROM chat_said_now WHERE chat_id = ? ORDER BY id",
    )
    .bind(chat_id)
    .fetch_all(pool)
    .await
}

/// Says `text` into the turn that is running, or sends it the ordinary way when there is none to say
/// it into.
///
/// Never touches `chat_queue` on the steering path: a queued copy would be sent a second time when
/// the turn ended. Only from the shell and text only, and only while the chat still holds the
/// permission mode the running turn was launched under — words written into it after the person moved
/// the chat to another rung would be acted on under the old one.
pub async fn say_now(
    state: &crate::state::AppState,
    chat_id: &str,
    text: &str,
) -> Result<SaidNow, String> {
    say_now_from(state, chat_id, text, Origin::Shell).await
}

/// `say_now` for a client that is not the shell: the origin is recorded on the steering row and
/// carried to `send_or_queue` when there is no running turn.
pub async fn say_now_from(
    state: &crate::state::AppState,
    chat_id: &str,
    text: &str,
    origin: Origin,
) -> Result<SaidNow, String> {
    let running: Option<(i64, Option<String>)> = sqlx::query_as(
        "SELECT id, permission_mode FROM runs
         WHERE chat_id = ? AND mode = 'assistant' AND status = 'running'
         ORDER BY id DESC LIMIT 1",
    )
    .bind(chat_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|error| error.to_string())?;
    let mode = crate::chats::permission_mode_of(&state.pool, chat_id).await;

    if let (Some((run_id, snapshot)), Ok(mode)) = (running, mode)
        && is_steerable(chat_id)
        && Some(mode.as_str()) == snapshot.as_deref()
    {
        let row = sqlx::query(
            "INSERT INTO chat_said_now (chat_id, run_id, text, origin, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(chat_id)
        .bind(run_id)
        .bind(text)
        .bind(origin.as_wire())
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&state.pool)
        .await
        .map_err(|error| error.to_string())?;
        let delivered = steer(
            chat_id,
            crate::execution::runner::LaterTurn {
                text: text.to_owned(),
                images: Vec::new(),
                interrupt: false,
                stop_task: None,
            },
        );
        if delivered {
            return Ok(SaidNow::Injected);
        }
        // The turn ended between the check and the write: forget the record and fall through.
        let _ = sqlx::query("DELETE FROM chat_said_now WHERE id = ?")
            .bind(row.last_insert_rowid())
            .execute(&state.pool)
            .await;
    }

    send_or_queue(state, chat_id, text, &[], origin)
        .await
        .map(SaidNow::Sent)
}

/// Takes a chat's turn slot and holds it until dropped, for the tests of modules that need one
/// taken.
///
/// Beside the guard rather than reached for through a second copy of `BUSY_CHATS`: what makes the
/// slot mean anything is that there is exactly one set of busy chats, and a test that inserted into
/// its own would be testing a set nothing reads.
#[cfg(any(test, feature = "testkit"))]
pub fn take_the_slot_for_testing(chat_id: &str) -> impl Drop {
    ChatSlot::acquire(chat_id).expect("the chat should have been free")
}

/// Whether a turn is in flight for this chat.
///
/// Reads the same set the slot is taken from, so it answers about the LIVE turn rather than about
/// what the database happened to record. Asking `runs` instead would be wrong in both directions: a
/// row still marked `running` after the daemon was killed says busy when nothing is, and the window
/// between `ChatSlot::acquire` and the INSERT says free when the chat is already taken.
pub fn is_busy(chat_id: &str) -> bool {
    BUSY_CHATS.lock().unwrap().contains(chat_id)
}

/// Owns a turn's chat slot and its temp MCP config for the length of the turn, releasing both when
/// it ends — by completing, by failing, or by being aborted.
pub(super) struct TurnGuard {
    pub(super) slot: ChatSlot,
    pub(super) mcp_path: std::path::PathBuf,
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        // `slot` releases the chat by being dropped with the rest of this struct.
        let _ = std::fs::remove_file(&self.mcp_path);
    }
}
