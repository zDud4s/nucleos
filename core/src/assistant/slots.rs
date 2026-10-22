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
    /// Armed by [`ChatSlot::drain_on_release`]: when the slot is released, what waited for this chat
    /// is sent from a task of its own.
    drain: Option<crate::state::AppState>,
    /// Taken by [`ChatSlot::acquire_for_warmup`]: the chat is being warmed, not answered.
    warmup: bool,
}

/// Chats whose slot is held by a warm-up rather than a turn, a subset of `BUSY_CHATS`.
///
/// A warm-up holds the slot only while it builds the process (a few reads and a spawn), so no turn
/// starts beside it. Nobody is answering in that window, and a person must not be told otherwise:
/// [`is_busy`] does not count it, and a send waits it out ([`ChatSlot::acquire_past_warmup`]) instead
/// of being queued behind a turn that does not exist.
static WARMUP_HELD: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// How long a send waits for a warm-up to let go of the chat before it is queued after all (the
/// warm-up's slot drains on release, so a message queued then is still sent).
const WARMUP_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

// Moved down with the process barrier it guards; kept nameable here for the rest of the module.
pub(super) use crate::assistant_types::relock;

/// Chats whose waiting messages must not be sent on their own: after a Stop, or after a send that
/// failed. Cleared when the person sends again.
static HELD_QUEUES: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Stops the chat's queue draining by itself until [`release_queue`].
pub(super) fn hold_queue(chat_id: &str) {
    relock(&HELD_QUEUES).insert(chat_id.to_string());
}

/// Lets the chat's queue drain again.
pub(super) fn release_queue(chat_id: &str) {
    relock(&HELD_QUEUES).remove(chat_id);
}

/// Whether the chat's queue is held.
pub(super) fn queue_is_held(chat_id: &str) -> bool {
    relock(&HELD_QUEUES).contains(chat_id)
}

impl ChatSlot {
    /// Claims the chat, or returns None if a turn is already in flight for it.
    pub(super) fn acquire(chat_id: &str) -> Option<Self> {
        relock(&BUSY_CHATS)
            .insert(chat_id.to_string())
            .then(|| Self {
                chat_id: chat_id.to_string(),
                drain: None,
                warmup: false,
            })
    }

    /// Claims the chat for a warm-up: held like a turn's slot, but not reported as busy.
    ///
    /// Both sets are written under the `BUSY_CHATS` lock, so [`is_busy`] (which takes that lock
    /// first) never sees the chat taken without also seeing who took it.
    pub(super) fn acquire_for_warmup(chat_id: &str) -> Option<Self> {
        let mut busy = relock(&BUSY_CHATS);
        if !busy.insert(chat_id.to_string()) {
            return None;
        }
        relock(&WARMUP_HELD).insert(chat_id.to_string());
        Some(Self {
            chat_id: chat_id.to_string(),
            drain: None,
            warmup: true,
        })
    }

    /// [`ChatSlot::acquire`] for a turn, waiting out a warm-up that holds the chat: the warm-up
    /// lets go within moments, and the turn then finds the process it just warmed. `None` only when
    /// a turn holds the chat, or the warm-up outlasts [`WARMUP_WAIT`].
    pub(super) async fn acquire_past_warmup(chat_id: &str) -> Option<Self> {
        let deadline = tokio::time::Instant::now() + WARMUP_WAIT;
        loop {
            if let Some(slot) = Self::acquire(chat_id) {
                return Some(slot);
            }
            if !relock(&WARMUP_HELD).contains(chat_id) || tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// Arms the slot: once released, it sends what waited for the chat, in a detached task of its
    /// own (never inside a registered run task, whose abort would cut it short).
    pub(super) fn drain_on_release(&mut self, state: &crate::state::AppState) {
        self.drain = Some(state.clone());
    }
}

impl Drop for ChatSlot {
    fn drop(&mut self) {
        relock(&BUSY_CHATS).remove(&self.chat_id);
        // After the busy set: in between, the chat reads as free, which it is.
        if self.warmup {
            relock(&WARMUP_HELD).remove(&self.chat_id);
        }
        if let Some(state) = self.drain.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let chat = self.chat_id.clone();
            runtime.spawn(async move { drain_queued(&state, &chat).await });
        }
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
    relock(&SEEN_CHATS).insert(chat_id.to_owned(), std::time::Instant::now());
}

/// Whether a visible view polled this chat within [`OPEN_WINDOW`].
pub fn chat_is_open(chat_id: &str) -> bool {
    let seen = relock(&SEEN_CHATS).get(chat_id).copied();
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
        relock(&STEERING).insert(chat_id.to_owned(), messages.clone());
        Self {
            chat_id: chat_id.to_owned(),
        }
    }
}

impl Drop for SteerGuard {
    fn drop(&mut self) {
        relock(&STEERING).remove(&self.chat_id);
    }
}

/// Writes a line into the process running a chat's turn, if one is. False when nothing is steerable.
pub(super) fn steer(chat_id: &str, turn: crate::execution::runner::LaterTurn) -> bool {
    // Cloned out so the lock is not held across the send.
    let sender = relock(&STEERING).get(chat_id).cloned();
    sender.is_some_and(|sender| sender.send(turn).is_ok())
}

/// Whether a turn is running in this chat's process right now, so a line written would land in it.
pub(super) fn is_steerable(chat_id: &str) -> bool {
    relock(&STEERING).contains_key(chat_id)
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
    let outcome = stop_turn_within_inner(state, turn_id, grace).await;
    // The final reply goes out first and the pause notice after it, so the outbox reads in the order
    // the person should. `reply_now` is the same compare-and-set the replies loop uses, so it does
    // not answer twice. `NotRunning` stopped nothing and announces nothing.
    if matches!(outcome, Stopped::Interrupted | Stopped::Killed)
        && let Some(chat) = stopped_chat(state, turn_id).await
    {
        crate::channels::replies::reply_now(state, turn_id).await;
        if let Err(error) = crate::channels::replies::announce_queue_paused(state, &chat).await {
            tracing::warn!(%error, chat, "could not announce the paused queue");
        }
    }
    outcome
}

/// The conversation a turn belongs to, if it has one.
async fn stopped_chat(state: &crate::state::AppState, turn_id: i64) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>("SELECT chat_id FROM runs WHERE id = ?")
        .bind(turn_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
        .flatten()
}

async fn stop_turn_within_inner(
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
    // A turn that already ended has nothing to stop; finalizing it would abort whatever handle is
    // registered under its id.
    if status != "running" {
        return Stopped::NotRunning;
    }
    // Stop means "and do not carry on with what waited": the queue stays put until the person sends.
    if let Some(chat) = chat_id.as_deref() {
        hold_queue(chat);
    }

    let interrupted = chat_id.as_deref().is_some_and(|chat| {
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

/// Stops the turn running in a conversation, found by chat id: [`stop_turn`] on its newest running
/// assistant run, with the id it stopped.
///
/// `None` when no turn is running, and then nothing is touched: the queue is neither held nor
/// released, so a hold that was there stays and a free queue stays free. Stopping a conversation
/// that is not answering does nothing, which keeps it distinct from killing.
pub async fn stop_chat(state: &crate::state::AppState, chat_id: &str) -> Option<(i64, Stopped)> {
    let turn_id: i64 = sqlx::query_scalar(
        "SELECT id FROM runs WHERE chat_id = ? AND mode = 'assistant' AND status = 'running'
         ORDER BY id DESC LIMIT 1",
    )
    .bind(chat_id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()?;
    Some((turn_id, stop_turn(state, turn_id).await))
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
///
/// A warm-up holding the slot is not a turn ([`WARMUP_HELD`]): nothing is answering, so a chat
/// being warmed is not busy for an archive, a PATCH or a fresh context.
pub fn is_busy(chat_id: &str) -> bool {
    // The busy set first and released before the second lock, the order `acquire_for_warmup`
    // writes them in.
    let taken = relock(&BUSY_CHATS).contains(chat_id);
    taken && !relock(&WARMUP_HELD).contains(chat_id)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A chat id no other test in this process is using: the queue holds are process-global.
    fn a_chat(prefix: &str) -> String {
        static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);
        format!(
            "{prefix}:{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )
    }

    async fn a_running_turn(state: &crate::state::AppState, chat: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, chat_id, created_at)
             VALUES ('slow', 'running', 'assistant', ?, '2026-10-20T00:00:00Z') RETURNING id",
        )
        .bind(chat)
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn stop_chat_stops_the_running_turn_and_holds_the_queue() {
        let state = crate::channels::testkit::state().await;
        let chat = a_chat("stop-chat");
        let turn = a_running_turn(&state, &chat).await;
        // No process is registered for the chat, so Stop takes the kill path, which needs a handle.
        let task = tokio::spawn(tokio::time::sleep(std::time::Duration::from_secs(60)));
        state
            .run_handles
            .lock()
            .unwrap()
            .insert(turn, task.abort_handle());
        assert!(!queue_is_held(&chat));

        let stopped = stop_chat(&state, &chat).await;

        assert_eq!(stopped, Some((turn, Stopped::Killed)));
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(turn)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "cancelled");
        assert!(queue_is_held(&chat));
        release_queue(&chat);
        task.abort();
    }

    #[tokio::test]
    async fn stop_chat_with_nothing_running_is_none_and_leaves_the_queue_hold_as_it_was() {
        let state = crate::channels::testkit::state().await;
        let free = a_chat("stop-chat-free");
        let held = a_chat("stop-chat-held");
        hold_queue(&held);

        assert_eq!(stop_chat(&state, &free).await, None);
        assert_eq!(stop_chat(&state, &held).await, None);

        assert!(!queue_is_held(&free), "a free queue must stay free");
        assert!(queue_is_held(&held), "a held queue must stay held");
        release_queue(&held);
    }
}
