use sqlx::SqlitePool;
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use crate::runner::extract_reply;

static BUSY_CHATS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Owns a chat's one turn slot for as long as it is held, releasing it on every way out.
///
/// A guard rather than a matching pair of calls, because there is no point in the turn's life where
/// a trailing statement is reliable. Cancelling a run calls `abort()`
/// (`runs::finalize_termination`), which drops the task's future mid-await; abandoning the HTTP
/// request that started the turn drops that future the same way. Anything written after an `.await`
/// then never runs, and a chat left behind in `BUSY_CHATS` rejects every later message with 409
/// until the daemon restarts — "the bot stopped answering" points nowhere near either cause.
struct ChatSlot {
    chat_id: String,
}

impl ChatSlot {
    /// Claims the chat, or returns None if a turn is already in flight for it.
    fn acquire(chat_id: &str) -> Option<Self> {
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
struct TurnGuard {
    slot: ChatSlot,
    mcp_path: std::path::PathBuf,
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        // `slot` releases the chat by being dropped with the rest of this struct.
        let _ = std::fs::remove_file(&self.mcp_path);
    }
}

/// How much context a chat's session may have occupied before resuming it stops paying for itself.
///
/// `runs.context_fill` is an ABSOLUTE token count, not a fraction: runner.rs writes
/// `input_tokens + cache_read_input_tokens` from the live assistant events, so this compares against
/// tokens directly. 140k is ≈0.7 of the 200k window the runner assumes as its conservative floor —
/// past there a resume mostly re-buys prior turns whose useful part was the last exchange.
const CONTEXT_ROTATION_TOKENS: i64 = 140_000;

/// The session a chat's next turn resumes, or `None` when it must start clean.
///
/// The `NOT EXISTS` is the second half of the barrier `hooks.rs` opens. That one refuses to let a
/// turn act after it has read third-party text; this one stops the text outliving the turn. Without
/// it the barrier holds for one message and no longer: `--resume` hands the next turn the same
/// context, that turn's own row is clean, and so the `approve_proposal` a mail body asked for is
/// simply made in the message after the one that read it.
///
/// Expressed as a condition on the READ rather than as a delete when the turn ends, deliberately.
/// A turn ends by completing, by failing, by timing out, by being cancelled, and by the daemon
/// being killed underneath it — five paths, of which the last runs no cleanup code at all. A
/// session that is unresumable because of what the database says about it is unresumable on every
/// one of them, including across a restart.
///
/// The second `NOT EXISTS` is context rotation, and it is on the READ for that same reason. It needs
/// no counterpart anywhere else: `send_message` mints a fresh session id whenever this returns
/// `None`, and `upsert_session` replaces the chat's row rather than adding one, so refusing to
/// resume IS the whole rotation.
pub async fn get_session(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Option<String>> {
    let session_id: Option<Option<String>> = sqlx::query_scalar(
        "SELECT s.session_id FROM assistant_sessions s
          WHERE s.chat_id = ?
            AND NOT EXISTS (SELECT 1 FROM runs r
                             WHERE r.session_id = s.session_id
                               AND r.read_untrusted = 1)
            AND NOT EXISTS (SELECT 1 FROM runs r
                             WHERE r.session_id = s.session_id
                               AND r.context_fill > ?)",
    )
    .bind(chat_id)
    .bind(CONTEXT_ROTATION_TOKENS)
    .fetch_optional(pool)
    .await?;

    Ok(session_id.flatten())
}

/// Leaves a chat with nothing to resume, so its next turn starts on a fresh context.
///
/// Not an error path: this is what a turn that read third-party text is supposed to leave behind.
pub async fn forget_session(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM assistant_sessions WHERE chat_id = ?")
        .bind(chat_id)
        .execute(pool)
        .await
        .map(|_| ())
}

pub async fn upsert_session(
    pool: &SqlitePool,
    chat_id: &str,
    session_id: &str,
    updated_at: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO assistant_sessions (chat_id, session_id, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(chat_id) DO UPDATE SET
             session_id = excluded.session_id,
             updated_at = excluded.updated_at",
    )
    .bind(chat_id)
    .bind(session_id)
    .bind(updated_at)
    .execute(pool)
    .await?;

    Ok(())
}

/// Where a chat's throwaway MCP config lives, with `chat_id` encoded rather than interpolated.
///
/// `chat_id` arrives from a sidecar and is an opaque string to us, so it cannot be trusted to be a
/// filename. Interpolated raw it reached `Path::join`, which **discards the base** when the joined
/// component is absolute: a `chat_id` of `C:/Windows/System32/x` wrote the config there instead of
/// in the temp directory, and `TurnGuard::drop` then removed whatever it had landed on — an
/// arbitrary write and delete for anything holding the daemon token.
///
/// Encoding, not validating: the id stays opaque (chats are not required to look like numbers, and
/// the tests rely on that), and the mapping stays injective, so two chats differing only in an
/// escaped character cannot collide onto one file and clobber each other's config mid-turn.
fn mcp_config_path(chat_id: &str) -> std::path::PathBuf {
    let mut safe = String::with_capacity(chat_id.len());
    for byte in chat_id.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => safe.push(byte as char),
            // `%` itself lands here, which is what keeps the encoding reversible.
            other => safe.push_str(&format!("%{other:02x}")),
        }
    }
    std::env::temp_dir().join(format!("nucleos-mcp-{safe}.json"))
}

fn write_mcp_config(path: &std::path::Path, config: &serde_json::Value) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(config).map_err(std::io::Error::other)?;
    crate::storage::write_atomic(path, &bytes)
}

pub fn build_mcp_config(exe_path: &str) -> serde_json::Value {
    serde_json::json!({
        "mcpServers": {
            "nucleos": {
                "type": "stdio",
                "command": exe_path,
                "args": ["--mcp-tools"]
            }
        }
    })
}

/// Which client sent a chat message, as the client states it.
///
/// Stated rather than inferred, and that is why this type exists at all. A Telegram group id is
/// negative, so the origin is guessable from the shape of `chat_id` — and guessing would make a
/// routing decision depend on a numbering scheme Telegram owns and can change without telling
/// anybody. A client that says nothing is `Shell`, which keeps every existing caller where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Shell,
    Telegram,
}

impl Origin {
    /// An unknown spelling resolves to `Shell` for the same reason absence does: this is a
    /// ship-dark feature, so anything not explicitly asking for the new path gets the old one.
    pub fn from_wire(value: Option<&str>) -> Self {
        match value {
            Some("telegram") => Self::Telegram,
            _ => Self::Shell,
        }
    }
}

/// Why a turn was refused before it cost anything: the chat says `local` and this machine has none.
///
/// A named constant rather than a sentence written twice, because `http.rs` turns it into the one
/// status code that tells this apart from a daemon that broke. Matched exactly there — a refusal
/// recognised by a substring is a refusal that stops being recognised when someone edits the words.
pub const NO_LOCAL_MODEL: &str = "this chat is set to the local model and none is configured";

pub async fn send_message(
    state: &crate::state::AppState,
    chat_id: &str,
    text: &str,
    origin: Origin,
) -> Result<i64, String> {
    // Held from here on: every early return, error, and dropped future below releases the chat by
    // dropping this, which is why none of them needs a cleanup statement of its own.
    let slot = ChatSlot::acquire(chat_id)
        .ok_or("a turn is already in progress for this chat".to_string())?;

    // Who answers this conversation. The chat's own row decides when there is one; when there is
    // none — every Telegram conversation, and everything that predates the `chats` table — the
    // origin rule that has always been here decides, unchanged.
    //
    // Precedence and not a combination, because the two facts are not the same kind of fact. A row
    // is a choice somebody made about THIS conversation; the origin is a guess about the sender,
    // and a guess must not outrank a choice.
    let chosen = crate::chats::brain_of(&state.pool, chat_id)
        .await
        .map_err(|e| e.to_string())?;
    let wants_local = match chosen {
        Some(crate::chats::Brain::Local) => true,
        Some(crate::chats::Brain::Cloud) => false,
        None => origin == Origin::Telegram,
    };

    if wants_local {
        match state.local_assistant.clone() {
            Some(assistant) => {
                return spawn_local_turn(state, slot, text.to_string(), assistant).await;
            }
            // A conversation that SAYS `local` and has no local model refuses. Falling through to
            // the cloud would be the worst possible way to find that out: on the bill, for a chat
            // that said it was staying on the machine. The refusal comes before any row is
            // inserted, so nothing was spent and nothing has to be explained away afterwards.
            None if chosen == Some(crate::chats::Brain::Local) => {
                return Err(NO_LOCAL_MODEL.to_string());
            }
            // The origin path keeps its old shape on purpose: a Telegram chat with no local model
            // has always simply gone to the cloud, and has never claimed otherwise. Refusing here
            // would take the bot off the air to enforce a promise nobody made.
            None => {}
        }
    }

    let resume = get_session(&state.pool, chat_id)
        .await
        .map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy().to_string();
    let config = build_mcp_config(&exe);
    let mcp_path = mcp_config_path(chat_id);
    write_mcp_config(&mcp_path, &config).map_err(|e| e.to_string())?;

    // Assigned by the daemon and persisted with the row, exactly as `runs::create_run_inner` does
    // it, so an assistant turn is not the one kind of run that can exist without a session id. A
    // first turn had neither `--resume` nor `--session-id`, so its id existed only if the CLI's
    // stream happened to announce one — and `budget.rs` keys spend on `session_id`, so a turn whose
    // stream carried no `init` event was money charged against nothing. Continuing a chat keeps the
    // session being resumed rather than minting a rival id for the same conversation.
    //
    // Writing it at INSERT rather than waiting for the stream also closes the read-untrusted
    // barrier's blind spot: `get_session` refuses to resume a session any run READ mail in, and a
    // turn killed before its stream reported an id used to leave a row with no session to match on.
    let session_id = resume.clone().unwrap_or_else(crate::auth::generate_uuid_v4);
    // `chat_id` alongside the session, because they answer different questions and diverge on
    // purpose. The session is what the NEXT turn resumes, and this module drops it whenever a turn
    // read third-party text — so a conversation that has read mail once is spread across several
    // sessions, and no amount of joining on `session_id` reassembles it. The chat is the thread.
    //
    // `answered_by` alongside them, written at INSERT for the same reason `session_id` is: a turn
    // that is cancelled before it produces a word still has to say who was answering it. It is a
    // literal here rather than a parameter because everything that reaches this line is on the CLI
    // path — the branch above is where the other answer is given.
    let id = sqlx::query(
        "INSERT INTO runs (prompt, status, mode, session_id, chat_id, answered_by, created_at)
         VALUES (?, 'running', 'assistant', ?, ?, 'cloud', ?)",
    )
    .bind(text)
    .bind(&session_id)
    .bind(chat_id)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&state.pool)
    .await
    .map_err(|e| e.to_string())?
    .last_insert_rowid();

    spawn_assistant_turn(
        state,
        id,
        slot,
        text.to_string(),
        resume,
        session_id,
        mcp_path,
    );
    Ok(id)
}

/// How many past exchanges a local turn is shown.
///
/// Small, and bounded again by characters below, because every one of these is re-sent on every
/// round of the tool loop — so a generous history is multiplied by `MAX_TOOL_ROUNDS` before it
/// reaches `TURN_NUM_CTX`. Six is enough for "and the second one?" and for the follow-up after that.
const HISTORY_TURNS: i64 = 6;

/// The character budget for replayed history, counted newest-first.
///
/// A ceiling on turns alone is not a ceiling: one pasted stack trace answered by a long reply is a
/// single exchange and thousands of characters. What overflows the window is length, so length is
/// what is bounded.
const HISTORY_CHARS: usize = 6_000;

/// The exchanges a local turn may be shown, oldest first.
///
/// The `id >` clause is the same barrier `get_session` applies to the CLI path, stated for a path
/// that has no session to refuse. `hooks.rs` stops a turn acting after it has read third-party
/// text; without this, a local turn would be handed that text as history — and a local turn can
/// call `create_run`. So a conversation resumes only from the point after anything read mail, which
/// is exactly what "a chat that has read mail is spread across several sessions" already means on
/// the other path.
///
/// Only `completed` turns, and only ones with a reply: a failed turn's row has no answer, and
/// replaying a question that was never answered invites the model to answer it now, out of order.
pub(crate) async fn recent_exchanges(
    pool: &SqlitePool,
    chat_id: &str,
) -> sqlx::Result<Vec<(String, String)>> {
    let mut rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT prompt, stdout FROM runs
          WHERE chat_id = ?
            AND mode = 'assistant'
            AND status = 'completed'
            AND stdout IS NOT NULL
            AND stdout <> ''
            AND id > (SELECT COALESCE(MAX(id), 0) FROM runs
                       WHERE chat_id = ? AND read_untrusted = 1)
          ORDER BY id DESC
          LIMIT ?",
    )
    .bind(chat_id)
    .bind(chat_id)
    .bind(HISTORY_TURNS)
    .fetch_all(pool)
    .await?;

    // Trimmed newest-first, then flipped, so the exchanges that survive a tight budget are the
    // recent ones. Dropping from the other end would keep the oldest and answer a question about
    // "the second one" with the conversation from an hour ago.
    let mut budget = HISTORY_CHARS;
    rows.retain(|(asked, answered)| {
        let cost = asked.chars().count() + answered.chars().count();
        match budget.checked_sub(cost) {
            Some(left) => {
                budget = left;
                true
            }
            None => false,
        }
    });
    rows.reverse();
    Ok(rows)
}

/// Records and drives a turn answered by the model on this machine.
///
/// Deliberately NOT a variant inside `spawn_assistant_turn`. That body is almost entirely about
/// things a local turn does not have — an MCP config written to disk, a CLI session id arriving on
/// a channel, a resumable session, a cost in dollars, a stderr stream that explains an exit code.
/// Threading `Option`s through all of it to skip each in turn would make the CLI path harder to
/// read in order to describe a path that shares three lines with it.
///
/// History is replayed rather than resumed. There is no session to resume — Ollama's chat endpoint
/// has no session protocol — so `recent_exchanges` rebuilds the conversation from the run rows the
/// turns already wrote, under the same barrier `get_session` applies on the other path.
async fn spawn_local_turn(
    state: &crate::state::AppState,
    slot: ChatSlot,
    text: String,
    assistant: std::sync::Arc<crate::local_agent::LocalAssistant>,
) -> Result<i64, String> {
    // A session id even though nothing resumes it, because `budget.rs` keys spend on this column
    // and a run row that is the one kind without one is a special case every reader downstream has
    // to know about. It costs a uuid.
    // Read BEFORE this turn's own row is inserted, so the turn cannot appear in its own history.
    // An empty history on a database error, not a failure: a bot that answers without remembering
    // is worse than one that remembers, and better than one that refuses to answer.
    let history = recent_exchanges(&state.pool, &slot.chat_id)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "could not read chat history; answering without it");
            Vec::new()
        });

    let session_id = crate::auth::generate_uuid_v4();
    let id = sqlx::query(
        "INSERT INTO runs (prompt, status, mode, session_id, chat_id, answered_by, created_at)
         VALUES (?, 'running', 'assistant', ?, ?, 'local', ?)",
    )
    .bind(&text)
    .bind(&session_id)
    .bind(&slot.chat_id)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&state.pool)
    .await
    .map_err(|e| e.to_string())?
    .last_insert_rowid();

    let pool = state.pool.clone();
    let run_timeout = state.run_timeout;
    crate::runs::spawn_registered(state, id, async move {
        // Holds the chat for the length of the turn and releases it however this ends, including by
        // being aborted mid-await — the same guarantee `TurnGuard` gives the CLI path.
        let _slot = slot;
        // Wrapped for the same reason the CLI path is: the slot is released by this task ending,
        // so a turn that never ends is a chat that answers nothing ever again — every later message
        // refused with 409 until the daemon restarts. The HTTP client has its own per-exchange
        // timeout; this one bounds the whole turn, including a loop that keeps making progress
        // slowly.
        // Outside the future on purpose. A wall-clock timeout DROPS the turn and a transport error
        // propagates out of it, and in both cases the turn may already have read a mail body with
        // no `Turn` left to say so — so the run row would be written clean and the next turn in this
        // chat would start with a stranger's words in its history and an open latch.
        let taint = std::sync::atomic::AtomicBool::new(false);
        let outcome =
            tokio::time::timeout(run_timeout, assistant.answer(&history, &text, &taint)).await;
        let completed_at = chrono::Utc::now().to_rfc3339();

        // Marked before any status is written, whatever the ending. `hooks.rs` refuses the READ
        // when this fails, which is not available here — the reading already happened — so the
        // fail-closed move left is to refuse the ANSWER: the turn is recorded as failed and its
        // text is dropped rather than stored and handed to the next turn as history.
        let mut unmarked = false;
        if taint.load(std::sync::atomic::Ordering::SeqCst)
            && let Err(error) = crate::runs::mark_untrusted_context(&pool, id).await
        {
            tracing::error!(
                run_id = id,
                %error,
                "could not mark a local turn as having read untrusted text — dropping its answer"
            );
            unmarked = true;
        }

        // Guarded on `status = 'running'` for the reason the CLI path sets out: a `/cancel` that
        // already wrote its status can still be followed by one last wake-up here, and an unguarded
        // write would report a completed turn for one that was killed.
        let written = match outcome {
            // `timed_out`, not `failed`, matching the CLI path below. A wall-clock kill is a
            // distinct ending there and anything filtering runs by it would simply not see a local
            // turn — the status is what the rest of the system reads, so it has to mean the same
            // thing whichever runner produced it.
            Err(_) => {
                tracing::warn!(run_id = id, "local turn exceeded the wall clock");
                sqlx::query(
                    "UPDATE runs SET status = 'timed_out', completed_at = ? WHERE id = ? AND status = 'running'",
                )
                .bind(&completed_at)
                .bind(id)
                .execute(&pool)
                .await
            }
            Ok(Ok(turn)) => {
                if turn.ending != crate::local_agent::Ending::Answered {
                    // Worth a log line and not worth an error: the person gets a usable sentence
                    // either way, and this is the only place recording WHY it was that sentence.
                    tracing::warn!(
                        run_id = id,
                        ending = ?turn.ending,
                        tool_calls = turn.tool_calls,
                        "local turn ended without an answer of its own"
                    );
                }
                // The turn read mail and the row could not be made to say so, so the answer is not
                // stored. Anything else writes a chat message quoting a stranger's words onto a run
                // marked clean, which `recent_exchanges` would then hand to the next turn with the
                // latch open — the one state every refusal in this file assumes does not exist.
                if unmarked {
                    sqlx::query(
                        "UPDATE runs SET status = 'failed', stderr = ?, completed_at = ? WHERE id = ? AND status = 'running'",
                    )
                    .bind("this turn read third-party text and the daemon could not record that; its answer was dropped rather than stored unmarked")
                    .bind(&completed_at)
                    .bind(id)
                    .execute(&pool)
                    .await
                } else {
                    sqlx::query(
                        "UPDATE runs SET status = 'completed', exit_code = 0, stdout = ?, cost_usd = 0, completed_at = ? WHERE id = ? AND status = 'running'",
                    )
                    .bind(&turn.answer)
                    .bind(&completed_at)
                    .bind(id)
                    .execute(&pool)
                    .await
                }
            }
            // Transport failure: Ollama stopped, or the model was pulled out from under us. The
            // chat is told rather than handed a silence it cannot interpret.
            Ok(Err(error)) => {
                tracing::warn!(run_id = id, %error, "local turn failed");
                sqlx::query(
                    "UPDATE runs SET status = 'failed', stderr = ?, completed_at = ? WHERE id = ? AND status = 'running'",
                )
                .bind(error.to_string())
                .bind(&completed_at)
                .bind(id)
                .execute(&pool)
                .await
            }
        };
        crate::runs::warn_on_terminal_write_err(&written, id, "completed");
    });

    Ok(id)
}

fn spawn_assistant_turn(
    state: &crate::state::AppState,
    id: i64,
    slot: ChatSlot,
    text: String,
    resume: Option<String>,
    session_id: String,
    mcp_path: std::path::PathBuf,
) {
    let pool = state.pool.clone();
    let runner = state.runner.clone();
    let run_timeout = state.run_timeout;
    // The one agent that carries the control token, and the only one that can: an orchestrator turn
    // runs under `ToolPolicy::McpOnly`, so it has no Bash, no Read and no Write — no way to look at
    // its own environment. It needs the full surface because approving a proposal or disengaging the
    // kill switch on the user's word is its job, and a scoped key would make it useless for that.
    let env = crate::runs::run_env(&state.token.0, id, None);
    // Built HERE, outside the task, and captured by the async block. A task aborted before its first
    // poll drops its captured state without ever running a line of the body, so a guard constructed
    // inside would simply never exist — and a `/cancel` racing a fresh message hits exactly that.
    let turn = TurnGuard { slot, mcp_path };

    crate::runs::spawn_registered(state, id, async move {
        let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        {
            let pool = pool.clone();
            let chat_id = turn.slot.chat_id.clone();
            tokio::spawn(async move {
                if let Some(session_id) = session_rx.recv().await {
                    // The run takes the id first, and the chat's resumable session is recorded only
                    // if it did. `get_session` decides whether a session may be resumed by looking
                    // at the runs that produced it, so the two writes are not independent: a
                    // session recorded against a run that never took the id has nothing pointing at
                    // it, and would stay resumable no matter what that turn went on to read.
                    let stored = sqlx::query("UPDATE runs SET session_id = ? WHERE id = ?")
                        .bind(&session_id)
                        .bind(id)
                        .execute(&pool)
                        .await;
                    if let Err(error) = stored {
                        tracing::warn!(
                            run_id = id,
                            %error,
                            "could not record the turn's session id; the chat will start its next turn clean"
                        );
                    } else {
                        let _ = upsert_session(
                            &pool,
                            &chat_id,
                            &session_id,
                            &chrono::Utc::now().to_rfc3339(),
                        )
                        .await;
                    }
                }
            });
        }

        let result = tokio::time::timeout(
            run_timeout,
            runner.run_prompt(
                crate::runner::RunRequest {
                    prompt: text,
                    env,
                    cwd: None,
                    plan_only: false,
                    resume_session_id: resume,
                    mcp_config: Some(turn.mcp_path.clone()),
                    // The orchestrator talks to NucleOS and to nothing else. The MCP allowlist below
                    // does not enforce that on its own — an allowlist only grants — so the policy is
                    // what actually keeps a Telegram turn away from the filesystem and the shell.
                    tool_policy: crate::runner::ToolPolicy::McpOnly,
                    progress_timeout: None,
                    // Always set. `cli_args` reads this only when there is no `--resume`, which is
                    // exactly the first turn — the one that used to be launched with no session id
                    // at all.
                    session_id: Some(session_id),
                    fork_session: false,
                    include_partial_messages: false,
                    // An orchestrator turn is one message answered and closed; the next one arrives
                    // as its own turn on the resumed session, which is where a Telegram reply
                    // already goes. Nothing here needs a stdin, so it keeps a closed one.
                    steerable: false,
                    // An orchestrator turn is answered by a person watching a chat, so the CLI's
                    // own permission surface is the right one: there IS somebody to approve. It is
                    // `McpOnly` besides, so the surface being argued over is nearly empty.
                    classifier_governs_tools: false,
                    messages: None,
                    // `McpOnly` already pushes the strict flag unconditionally, so this changes
                    // nothing here — it is the same answer said in the request rather than inferred.
                    ambient_mcp: false,
                    // An orchestrator turn is not a job node, so it has no role to route.
                    model: None,
                },
                session_tx,
                // Unread here, deliberately. An assistant turn's product is the reply that
                // `extract_reply` pulls out of a completed run; a turn the wall clock killed has no
                // reply to salvage, and `assistant_sessions` has nowhere to keep a partial one.
                // `runs.rs` reads its copy because a run's trajectory is worth keeping even when
                // the run is not — that difference is in the tables, not an oversight here.
                std::sync::Arc::new(std::sync::Mutex::new(String::new())),
            ),
        )
        .await;
        let completed_at = chrono::Utc::now().to_rfc3339();

        // Each terminal write below is guarded on the turn still being `running`. A `/cancel` aborts
        // this task, but the abort lands only where this future is next dropped — so a cancel that
        // already wrote its status can still be followed by one last wake-up here, and an unguarded
        // write would report a completed turn for a CLI that was killed. First writer wins; no rows
        // means the turn was finalised elsewhere, which is an outcome, not an error.
        match result {
            // A turn's product is the `result` event, and a CLI that exited without one answered
            // nothing. That is a failed turn, not a completed one — and emphatically not a turn
            // whose raw stream can stand in for the reply it never wrote. The stream is transport:
            // `init` events, session ids, and whatever the `SessionStart` hook injected as
            // `additionalContext`. Handing it to a chat as the answer published the whole hook body
            // to Telegram — internal context, delivered under `status = 'completed'`, so nothing
            // downstream had any reason to treat it as the breakage it was.
            //
            // The reader gets the stderr instead, which is where the CLI says why it stopped. When
            // the tool-policy barrier kills a turn that line names the offending tools, so the chat
            // shows the actual fault rather than a wall of JSON.
            Ok(Ok(o)) => match extract_reply(&o.stdout) {
                Some(reply) => {
                    let completed = sqlx::query(
                        "UPDATE runs SET status = 'completed', exit_code = ?, stdout = ?, stderr = ?, session_id = COALESCE(?, session_id), cost_usd = ?, completed_at = ? WHERE id = ? AND status = 'running'",
                    )
                    .bind(o.exit_code)
                    .bind(&reply)
                    .bind(&o.stderr)
                    .bind(&o.session_id)
                    .bind(o.cost_usd)
                    .bind(&completed_at)
                    .bind(id)
                    .execute(&pool)
                    .await;
                    crate::runs::warn_on_terminal_write_err(&completed, id, "completed");
                    if let Some(session_id) = o.session_id.as_deref() {
                        // `get_session` would refuse to resume this session anyway, by looking at the
                        // runs that produced it. Dropping the row here as well closes the one case that
                        // check cannot see: a session recorded against a run that never took the id has
                        // nothing pointing at it, so nothing marks it as having read anything.
                        match crate::runs::read_untrusted_context(&pool, id).await {
                            Ok(false) => {
                                let _ = upsert_session(
                                    &pool,
                                    &turn.slot.chat_id,
                                    session_id,
                                    &completed_at,
                                )
                                .await;
                            }
                            // Including the error: a turn whose record cannot be read is not a turn
                            // that can be shown to be clean.
                            _ => {
                                let _ = forget_session(&pool, &turn.slot.chat_id).await;
                            }
                        }
                    }
                }
                // `stdout` is left NULL rather than filled with the stream: there is no reply, and a
                // column that says so is honest.
                //
                // Nothing is done about the session here, matching the failure and timeout arms
                // below. Not an omission — the id was already recorded when the CLI announced it,
                // by the task above, and it stays recorded on every path that does not complete. A
                // turn killed at the tool-policy barrier died before it could call anything, so the
                // session it leaves has read nothing and is safe to resume; the read-side check in
                // `get_session` is what decides that, and it decides it the same way here.
                None => {
                    let failed = sqlx::query(
                        "UPDATE runs SET status = 'failed', exit_code = ?, stderr = ?, session_id = COALESCE(?, session_id), cost_usd = ?, completed_at = ? WHERE id = ? AND status = 'running'",
                    )
                    .bind(o.exit_code)
                    .bind(&o.stderr)
                    .bind(&o.session_id)
                    .bind(o.cost_usd)
                    .bind(&completed_at)
                    .bind(id)
                    .execute(&pool)
                    .await;
                    crate::runs::warn_on_terminal_write_err(&failed, id, "failed");
                }
            },
            Ok(Err(e)) => {
                let failed = sqlx::query(
                    "UPDATE runs SET status = 'failed', stderr = ?, completed_at = ? WHERE id = ? AND status = 'running'",
                )
                .bind(e.to_string())
                .bind(&completed_at)
                .bind(id)
                .execute(&pool)
                .await;
                crate::runs::warn_on_terminal_write_err(&failed, id, "failed");
            }
            Err(_) => {
                let timed_out = sqlx::query(
                    "UPDATE runs SET status = 'timed_out', completed_at = ? WHERE id = ? AND status = 'running'",
                )
                .bind(&completed_at)
                .bind(id)
                .execute(&pool)
                .await;
                crate::runs::warn_on_terminal_write_err(&timed_out, id, "timed_out");
            }
        }

        // No cleanup here on purpose: `turn` drops it, on every path including an abort.
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Token;
    use crate::runner::FakeCommandRunner;
    use crate::state::AppState;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

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

    async fn test_state() -> AppState {
        AppState {
            token: Token("t".into()),
            pool: test_pool().await,
            runner: Arc::new(FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            local_assistant: None,
            run_handles: Arc::new(Mutex::new(HashMap::new())),
            run_messages: Arc::new(Mutex::new(HashMap::new())),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            web: std::sync::Arc::new(crate::web::WebRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::calendar::CalendarRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    /// A local assistant that answers one fixed sentence and calls no tools.
    fn fake_local_assistant(answer: &'static str) -> Arc<crate::local_agent::LocalAssistant> {
        struct OneLiner(&'static str);
        #[async_trait::async_trait]
        impl crate::local_agent::LocalChat for OneLiner {
            async fn exchange(
                &self,
                _messages: Vec<serde_json::Value>,
                _tools: Option<Vec<serde_json::Value>>,
            ) -> std::io::Result<serde_json::Value> {
                Ok(serde_json::json!({"role": "assistant", "content": self.0}))
            }
        }

        struct NoTools;
        #[async_trait::async_trait]
        impl crate::local_agent::ToolBox for NoTools {
            fn schemas(&self) -> Vec<serde_json::Value> {
                Vec::new()
            }
            async fn call(&self, _name: &str, _arguments: &serde_json::Value) -> String {
                unreachable!("this assistant answers without calling tools")
            }
        }

        Arc::new(crate::local_agent::LocalAssistant::new(
            Box::new(OneLiner(answer)),
            Box::new(NoTools),
        ))
    }

    /// Polls until the turn leaves `running`, the way every other test in this module waits for a
    /// spawned turn, and returns its status and reply.
    async fn settled_turn(pool: &SqlitePool, id: i64) -> (String, Option<String>) {
        for _ in 0..100 {
            let row: (String, Option<String>) =
                sqlx::query_as("SELECT status, stdout FROM runs WHERE id = ?")
                    .bind(id)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            if row.0 != "running" {
                return row;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("turn {id} never left running");
    }

    /// What `FakeCommandRunner::default` answers, so a test can say "this went down the CLI path"
    /// without asserting on a string whose meaning is not obvious at the call site.
    const CLI_FAKE_REPLY: &str = "fake output";

    #[test]
    fn only_an_explicit_telegram_origin_is_telegram() {
        assert_eq!(Origin::from_wire(Some("telegram")), Origin::Telegram);
        // Everything else is the shell, which is what makes this ship dark: a client that has not
        // been taught the field keeps the behaviour it has today.
        for value in [None, Some("shell"), Some("Telegram"), Some(""), Some("tg")] {
            assert_eq!(Origin::from_wire(value), Origin::Shell, "{value:?}");
        }
    }

    /// The ship-dark guarantee, and the test most likely to be needed later: with no model
    /// configured, a Telegram turn is answered exactly as it was before any of this existed.
    #[tokio::test]
    async fn a_telegram_turn_uses_the_cli_when_no_local_model_is_configured() {
        let state = test_state().await;
        assert!(state.local_assistant.is_none());

        let id = send_message(&state, "tg-no-local", "hello", Origin::Telegram)
            .await
            .unwrap();
        let (status, reply) = settled_turn(&state.pool, id).await;
        assert_eq!(status, "completed");
        assert_eq!(
            reply.as_deref(),
            Some(CLI_FAKE_REPLY),
            "expected the CLI fake's reply, so this turn took the CLI path"
        );
    }

    #[tokio::test]
    async fn a_telegram_turn_is_answered_on_this_machine_when_a_model_is_configured() {
        let mut state = test_state().await;
        state.local_assistant = Some(fake_local_assistant("três corridas a andar"));

        let id = send_message(&state, "tg-local", "o que está a correr?", Origin::Telegram)
            .await
            .unwrap();
        let (status, reply) = settled_turn(&state.pool, id).await;
        assert_eq!(status, "completed");
        assert_eq!(reply.as_deref(), Some("três corridas a andar"));
    }

    /// The other half of the routing rule. Configuring a local model must not quietly move the
    /// desktop app's chat onto it — the shell is where the long, tool-heavy conversations happen.
    #[tokio::test]
    async fn a_shell_turn_stays_on_the_cli_even_with_a_local_model_configured() {
        let mut state = test_state().await;
        state.local_assistant = Some(fake_local_assistant("never asked"));

        let id = send_message(&state, "shell-with-local", "hello", Origin::Shell)
            .await
            .unwrap();
        let (_, reply) = settled_turn(&state.pool, id).await;
        assert_eq!(
            reply.as_deref(),
            Some(CLI_FAKE_REPLY),
            "a configured local model must not move the shell's chat onto it"
        );
    }

    async fn answered_by(pool: &SqlitePool, id: i64) -> Option<String> {
        sqlx::query_scalar("SELECT answered_by FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Written when the row is BORN, not when the turn ends: a turn that is cancelled still has to
    /// say who was answering it, and the transcript draws its memory cut from this column.
    #[tokio::test]
    async fn a_cloud_turn_records_that_the_cloud_answered_it() {
        let state = test_state().await;

        let id = send_message(&state, "who-answered", "hello", Origin::Shell)
            .await
            .unwrap();

        assert_eq!(answered_by(&state.pool, id).await.as_deref(), Some("cloud"));
    }

    /// The half that closes a hole predating this work: until now a Telegram turn answered on this
    /// machine was indistinguishable from a cloud one in the runs table.
    #[tokio::test]
    async fn a_local_turn_records_that_the_local_model_answered_it() {
        let mut state = test_state().await;
        state.local_assistant = Some(fake_local_assistant("aqui mesmo"));

        let id = send_message(&state, "tg-who-answered", "olá", Origin::Telegram)
            .await
            .unwrap();

        assert_eq!(answered_by(&state.pool, id).await.as_deref(), Some("local"));
    }

    #[tokio::test]
    async fn a_chat_marked_local_is_answered_locally_even_from_the_shell() {
        let mut state = test_state().await;
        state.local_assistant = Some(fake_local_assistant("na máquina"));
        let id = crate::chats::create(&state.pool, crate::chats::Brain::Local)
            .await
            .unwrap();

        let turn = send_message(&state, &id, "olá", Origin::Shell)
            .await
            .unwrap();

        assert_eq!(
            answered_by(&state.pool, turn).await.as_deref(),
            Some("local")
        );
    }

    #[tokio::test]
    async fn a_chat_marked_cloud_is_answered_in_the_cloud_even_from_telegram() {
        let mut state = test_state().await;
        state.local_assistant = Some(fake_local_assistant("never asked"));
        let id = crate::chats::create(&state.pool, crate::chats::Brain::Cloud)
            .await
            .unwrap();

        let turn = send_message(&state, &id, "olá", Origin::Telegram)
            .await
            .unwrap();

        // The chat's own row wins over the sender. Anything else would make the app unable to say
        // "answer this one in the cloud" for a conversation it opened.
        assert_eq!(
            answered_by(&state.pool, turn).await.as_deref(),
            Some("cloud")
        );
    }

    /// The guard on this whole change. No `chats` row anywhere is every Telegram conversation, and
    /// every conversation that predates the table — the old rule, unchanged.
    #[tokio::test]
    async fn a_conversation_with_no_row_routes_exactly_as_it_did_before() {
        let mut state = test_state().await;
        state.local_assistant = Some(fake_local_assistant("na máquina"));

        let from_telegram = send_message(&state, "-100200300", "olá", Origin::Telegram)
            .await
            .unwrap();
        let from_shell = send_message(&state, "no-row-shell", "olá", Origin::Shell)
            .await
            .unwrap();

        assert_eq!(
            answered_by(&state.pool, from_telegram).await.as_deref(),
            Some("local")
        );
        assert_eq!(
            answered_by(&state.pool, from_shell).await.as_deref(),
            Some("cloud")
        );
    }

    #[tokio::test]
    async fn a_local_chat_with_no_local_model_refuses_instead_of_quietly_costing_money() {
        // No local model configured: `state.local_assistant` is None.
        let state = test_state().await;
        let id = crate::chats::create(&state.pool, crate::chats::Brain::Local)
            .await
            .unwrap();

        let outcome = send_message(&state, &id, "olá", Origin::Shell).await;

        assert!(
            outcome.is_err(),
            "a chat that says local must not fall through to the cloud"
        );
        // And it must not have spent anything trying: no row, no bill.
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE chat_id = ?")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(runs, 0);
    }

    /// The other side of the refusal above, and the reason it is stated on the CHAT and not on the
    /// origin: a Telegram conversation with no local model has always simply gone to the cloud, and
    /// has never claimed otherwise. Breaking that would take the bot off the air.
    #[tokio::test]
    async fn a_telegram_conversation_with_no_local_model_still_falls_through_to_the_cloud() {
        let state = test_state().await;

        let turn = send_message(&state, "-100200301", "olá", Origin::Telegram)
            .await
            .unwrap();

        assert_eq!(
            answered_by(&state.pool, turn).await.as_deref(),
            Some("cloud")
        );
    }

    /// A local turn holds the chat's one slot like any other, and releases it. Without this the
    /// second message to a bot answered locally would be rejected with 409 for ever.
    #[tokio::test]
    async fn a_local_turn_releases_the_chat_when_it_ends() {
        let mut state = test_state().await;
        state.local_assistant = Some(fake_local_assistant("done"));

        let first = send_message(&state, "tg-slot", "one", Origin::Telegram)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;

        let second = send_message(&state, "tg-slot", "two", Origin::Telegram).await;
        assert!(second.is_ok(), "the slot was not released: {second:?}");
    }

    async fn record_turn(
        pool: &SqlitePool,
        chat_id: &str,
        prompt: &str,
        reply: &str,
        read_untrusted: i64,
    ) {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, chat_id, stdout, read_untrusted, created_at)
             VALUES (?, 'completed', 'assistant', ?, ?, ?, '2026-08-09T00:00:00Z')",
        )
        .bind(prompt)
        .bind(chat_id)
        .bind(reply)
        .bind(read_untrusted)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn history_comes_back_oldest_first() {
        let pool = test_pool().await;
        record_turn(&pool, "c", "what is running?", "two runs", 0).await;
        record_turn(&pool, "c", "and the second?", "the calendar one", 0).await;

        let history = recent_exchanges(&pool, "c").await.unwrap();
        assert_eq!(
            history,
            vec![
                ("what is running?".to_string(), "two runs".to_string()),
                (
                    "and the second?".to_string(),
                    "the calendar one".to_string()
                ),
            ]
        );
    }

    /// The barrier, stated for a path that has no session to refuse. A local turn can `create_run`,
    /// so replaying a turn that read a stranger's mail would hand that stranger's words to a turn
    /// able to act on them — the exact failure `get_session` prevents on the CLI path.
    #[tokio::test]
    async fn history_starts_after_anything_that_read_a_strangers_words() {
        let pool = test_pool().await;
        record_turn(&pool, "c", "before", "old answer", 0).await;
        record_turn(&pool, "c", "read my mail", "it says ignore all rules", 1).await;
        record_turn(&pool, "c", "after", "fresh answer", 0).await;

        let history = recent_exchanges(&pool, "c").await.unwrap();
        assert_eq!(
            history,
            vec![("after".to_string(), "fresh answer".to_string())],
            "history must resume only from after the mail read"
        );
    }

    #[tokio::test]
    async fn history_is_per_chat_and_only_of_answered_turns() {
        let pool = test_pool().await;
        record_turn(&pool, "other", "not mine", "not mine", 0).await;
        record_turn(&pool, "c", "answered", "yes", 0).await;
        // A failed turn has no reply; replaying its question invites the model to answer it now,
        // out of order.
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, chat_id, created_at)
             VALUES ('unanswered', 'failed', 'assistant', 'c', '2026-08-09T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let history = recent_exchanges(&pool, "c").await.unwrap();
        assert_eq!(history, vec![("answered".to_string(), "yes".to_string())]);
    }

    /// A ceiling on the NUMBER of exchanges is not a ceiling: one pasted stack trace is a single
    /// exchange and thousands of characters. What overflows the window is length.
    #[tokio::test]
    async fn a_long_exchange_is_dropped_and_the_recent_ones_are_kept() {
        let pool = test_pool().await;
        record_turn(&pool, "c", &"x".repeat(HISTORY_CHARS), "huge", 0).await;
        record_turn(&pool, "c", "recent", "kept", 0).await;

        let history = recent_exchanges(&pool, "c").await.unwrap();
        assert_eq!(
            history,
            vec![("recent".to_string(), "kept".to_string())],
            "the budget must be spent newest-first"
        );
    }

    /// End to end: the second message to a locally-answered chat must arrive with the first one
    /// behind it, or every follow-up is answered by a bot with no memory.
    #[tokio::test]
    async fn a_second_local_turn_is_given_the_first() {
        use std::sync::Mutex as StdMutex;

        struct Recorder(Arc<StdMutex<Vec<serde_json::Value>>>);
        #[async_trait::async_trait]
        impl crate::local_agent::LocalChat for Recorder {
            async fn exchange(
                &self,
                messages: Vec<serde_json::Value>,
                _tools: Option<Vec<serde_json::Value>>,
            ) -> std::io::Result<serde_json::Value> {
                *self.0.lock().unwrap() = messages;
                Ok(serde_json::json!({"role": "assistant", "content": "ok"}))
            }
        }
        struct NoTools;
        #[async_trait::async_trait]
        impl crate::local_agent::ToolBox for NoTools {
            fn schemas(&self) -> Vec<serde_json::Value> {
                Vec::new()
            }
            async fn call(&self, _: &str, _: &serde_json::Value) -> String {
                unreachable!()
            }
        }

        let seen = Arc::new(StdMutex::new(Vec::new()));
        let mut state = test_state().await;
        state.local_assistant = Some(Arc::new(crate::local_agent::LocalAssistant::new(
            Box::new(Recorder(seen.clone())),
            Box::new(NoTools),
        )));

        let first = send_message(&state, "tg-memory", "primeira", Origin::Telegram)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;
        let second = send_message(&state, "tg-memory", "segunda", Origin::Telegram)
            .await
            .unwrap();
        settled_turn(&state.pool, second).await;

        let messages = seen.lock().unwrap().clone();
        let contents: Vec<String> = messages
            .iter()
            .map(|m| m["content"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            contents,
            vec![
                crate::local_agent::SYSTEM_PROMPT.to_string(),
                "primeira".to_string(),
                "ok".to_string(),
                "segunda".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn unknown_chat_has_no_session() {
        let pool = test_pool().await;

        assert_eq!(get_session(&pool, "unknown").await.unwrap(), None);
    }

    #[tokio::test]
    async fn upsert_stores_session() {
        let pool = test_pool().await;

        upsert_session(&pool, "chat-1", "session-1", "2026-07-21T10:00:00Z")
            .await
            .unwrap();

        assert_eq!(
            get_session(&pool, "chat-1").await.unwrap(),
            Some("session-1".to_string())
        );
    }

    #[tokio::test]
    async fn upsert_updates_existing_session() {
        let pool = test_pool().await;

        upsert_session(&pool, "chat-1", "session-1", "2026-07-21T10:00:00Z")
            .await
            .unwrap();
        upsert_session(&pool, "chat-1", "session-2", "2026-07-21T11:00:00Z")
            .await
            .unwrap();

        assert_eq!(
            get_session(&pool, "chat-1").await.unwrap(),
            Some("session-2".to_string())
        );
    }

    /// The half of the barrier that has to survive the daemon dying.
    ///
    /// `hooks.rs` refuses to let a turn act after it has read third-party text, and that refusal is
    /// recorded against the RUN. A session outlives the run: `--resume` hands the next turn the same
    /// context, and the next turn's own row is clean, so a mail body refused once would simply be
    /// obeyed one message later. The check therefore lives on the read, where no cleanup code has to
    /// have run for it to hold — this test writes the rows directly for that reason, standing in for
    /// a turn that was cancelled, timed out, or killed with the daemon.
    #[tokio::test]
    async fn a_session_a_turn_read_mail_in_is_never_resumed() {
        let pool = test_pool().await;
        let chat_id = "assistant-untrusted-session-chat";

        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, read_untrusted, created_at)
             VALUES ('x', 'completed', 'assistant', 'sess-mail', 1, '2026-07-29T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        upsert_session(&pool, chat_id, "sess-mail", "2026-07-29T00:00:00Z")
            .await
            .unwrap();

        assert_eq!(
            get_session(&pool, chat_id).await.unwrap(),
            None,
            "a session whose turn read a stranger's words must not be resumable"
        );
    }

    /// The contrast, so the test above cannot pass by refusing everything: an ordinary turn is what
    /// makes the bot conversational, and it keeps its session.
    #[tokio::test]
    async fn a_session_no_turn_read_mail_in_is_resumed_as_before() {
        let pool = test_pool().await;
        let chat_id = "assistant-clean-session-chat";

        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, created_at)
             VALUES ('x', 'completed', 'assistant', 'sess-clean', '2026-07-29T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        upsert_session(&pool, chat_id, "sess-clean", "2026-07-29T00:00:00Z")
            .await
            .unwrap();

        assert_eq!(
            get_session(&pool, chat_id).await.unwrap(),
            Some("sess-clean".to_string())
        );
    }

    /// A chat is a conversation that never ends, and `--resume` hands every turn the whole of it.
    /// Past the point where the window is mostly prior turns, the resume stops buying continuity and
    /// starts buying the same tokens again on every message — the CLI re-reads a context whose useful
    /// part is the last exchange, and the chat pays for the rest.
    ///
    /// So the resume has a ceiling. `runs.context_fill` is an absolute token count, not a fraction,
    /// and once any turn on a session recorded more than `CONTEXT_ROTATION_TOKENS` of it, that
    /// session stops being resumable and the next message starts clean.
    ///
    /// Written as rows rather than driven through a turn, for the same reason the mail-read test
    /// above is: the condition lives on the READ, so it must hold for a session whose turn died
    /// without running any cleanup.
    #[tokio::test]
    async fn a_chat_whose_session_filled_the_context_starts_clean() {
        let pool = test_pool().await;
        let chat_id = "assistant-rotated-session-chat";

        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, context_fill, created_at)
             VALUES ('x', 'completed', 'assistant', 'sess-full', ?, '2026-08-08T00:00:00Z')",
        )
        .bind(CONTEXT_ROTATION_TOKENS + 1)
        .execute(&pool)
        .await
        .unwrap();
        upsert_session(&pool, chat_id, "sess-full", "2026-08-08T00:00:00Z")
            .await
            .unwrap();

        assert_eq!(
            get_session(&pool, chat_id).await.unwrap(),
            None,
            "a session that filled the context must not be resumed into again"
        );
    }

    /// The contrast that stops the rotation from simply ending every conversation: a session still
    /// inside the ceiling keeps being resumed, and a turn that never reported a fill at all is not
    /// treated as though it had overflowed.
    #[tokio::test]
    async fn a_chat_below_the_rotation_threshold_still_resumes() {
        let pool = test_pool().await;
        let chat_id = "assistant-roomy-session-chat";

        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, context_fill, created_at)
             VALUES ('x', 'completed', 'assistant', 'sess-roomy', ?, '2026-08-08T00:00:00Z'),
                    ('y', 'completed', 'assistant', 'sess-roomy', NULL, '2026-08-08T00:01:00Z')",
        )
        .bind(CONTEXT_ROTATION_TOKENS - 1)
        .execute(&pool)
        .await
        .unwrap();
        upsert_session(&pool, chat_id, "sess-roomy", "2026-08-08T00:01:00Z")
            .await
            .unwrap();

        assert_eq!(
            get_session(&pool, chat_id).await.unwrap(),
            Some("sess-roomy".to_string()),
            "a chat with room left is still one conversation, and an unreported fill is not an \
             overflow"
        );
    }

    /// What a Telegram user actually received when the tool-policy barrier killed a turn: the CLI's
    /// own stream, `SessionStart` hook payload and all, delivered as though it were the answer.
    ///
    /// The shape below is the real one — three `system` events and no `result`, because the run was
    /// killed at `init`. The assertion that matters is the negative one: whatever the chat is shown,
    /// it must not be the stream. `status` carries the rest of the fix; the sidecar renders a
    /// `failed` turn as its stderr, which is where the CLI says which tools it objected to.
    #[tokio::test]
    async fn a_turn_with_no_result_event_fails_instead_of_replying_with_its_own_stream() {
        let mut state = test_state().await;
        let hook_body = r#"{"type":"system","subtype":"hook_response","hook_name":"SessionStart:resume","output":"You have superpowers. If you think there is even a 1% chance a skill might apply"}"#;
        let stream = format!(
            "{}\n{}\n{}\n",
            r#"{"type":"system","subtype":"hook_started","hook_name":"SessionStart:resume"}"#,
            hook_body,
            r#"{"type":"system","subtype":"init","session_id":"s","tools":["TaskCreate"]}"#,
        );
        state.runner = Arc::new(FakeCommandRunner {
            canned: Mutex::new(Some(crate::runner::RunOutcome {
                exit_code: -1,
                stdout: stream.clone(),
                stderr: "nucleos: ToolPolicy::McpOnly violated by CLI-advertised tools: TaskCreate"
                    .to_string(),
                session_id: None,
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                num_turns: None,
            })),
            ..Default::default()
        });
        let chat_id = "assistant-no-result-event-chat";

        let id = send_message(
            &state,
            chat_id,
            "Le me o ultimo mail que recebi",
            Origin::Shell,
        )
        .await
        .unwrap();

        let mut row = None;
        for _ in 0..500 {
            let (status, stdout, stderr): (String, Option<String>, Option<String>) =
                sqlx::query_as("SELECT status, stdout, stderr FROM runs WHERE id = ?")
                    .bind(id)
                    .fetch_one(&state.pool)
                    .await
                    .unwrap();
            if status != "running" {
                row = Some((status, stdout, stderr));
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let (status, stdout, stderr) = row.expect("the turn must reach a terminal status");

        assert_eq!(
            status, "failed",
            "a turn that answered nothing is not a completed turn"
        );
        assert_eq!(
            stdout, None,
            "the reply column must stay empty rather than carry the stream: {stdout:?}"
        );
        assert!(
            stderr.is_some_and(|e| e.contains("TaskCreate")),
            "the reader gets the CLI's reason instead of its transport"
        );
    }

    /// End to end, through the turn machinery rather than through hand-written rows: a turn reads
    /// mail while it runs, and the message after it starts the CLI with no `--resume` at all.
    ///
    /// The session is recorded early — the CLI announces it in its first event, long before any tool
    /// call — so "do not store it" was never available as a fix. What the turn can do is not leave it
    /// behind, and what the read can do is refuse it anyway.
    #[tokio::test]
    async fn the_message_after_a_mail_read_starts_a_fresh_conversation() {
        let mut state = test_state().await;
        let runner = Arc::new(FakeCommandRunner {
            delay: Mutex::new(Some(Duration::from_millis(150))),
            ..Default::default()
        });
        state.runner = runner.clone();
        let chat_id = "assistant-forget-after-mail-chat";

        let first = send_message(&state, chat_id, "what is in my mail?", Origin::Shell)
            .await
            .unwrap();

        // The session id is announced before the simulated delay, so this is the window in which a
        // real turn calls `get_email` and `hooks.rs` marks the run.
        for _ in 0..500 {
            if get_session(&state.pool, chat_id).await.unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(
            get_session(&state.pool, chat_id).await.unwrap(),
            Some("fake-session-id".to_string()),
            "the turn should have recorded its session before reading anything"
        );
        crate::runs::mark_untrusted_context(&state.pool, first)
            .await
            .unwrap();

        for _ in 0..500 {
            let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(first)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            if status == "completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        assert_eq!(
            get_session(&state.pool, chat_id).await.unwrap(),
            None,
            "a turn that read mail must leave the chat nothing to resume"
        );

        *runner.last_resume.lock().unwrap() = Some("not-cleared".to_string());
        send_message(&state, chat_id, "approve proposal 4", Origin::Shell)
            .await
            .unwrap();
        for _ in 0..500 {
            if runner.last_resume.lock().unwrap().as_deref() != Some("not-cleared") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(
            *runner.last_resume.lock().unwrap(),
            None,
            "the next message must start on a context no mail body has spoken into"
        );
    }

    #[test]
    fn builds_mcp_config() {
        let config = build_mcp_config("C:/x/nucleos-core.exe");

        assert_eq!(config["mcpServers"]["nucleos"]["type"], "stdio");
        assert_eq!(
            config["mcpServers"]["nucleos"]["command"],
            "C:/x/nucleos-core.exe"
        );
        assert_eq!(
            config["mcpServers"]["nucleos"]["args"],
            serde_json::json!(["--mcp-tools"])
        );
    }

    #[test]
    fn a_configuracao_mcp_e_escrita_por_inteiro() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let long_path = "C:/um/caminho/deliberadamente/muito/comprido/para/nucleos-core.exe";
        let short_path = "C:/n.exe";

        write_mcp_config(&path, &build_mcp_config(long_path)).unwrap();
        write_mcp_config(&path, &build_mcp_config(short_path)).unwrap();

        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            config["mcpServers"]["nucleos"]["command"],
            serde_json::json!(short_path)
        );
    }

    #[test]
    fn only_one_turn_can_be_in_flight_per_chat() {
        let chat_id = "busy-test-chat";

        let slot = ChatSlot::acquire(chat_id).expect("the chat starts free");
        assert!(ChatSlot::acquire(chat_id).is_none());
        drop(slot);
        assert!(ChatSlot::acquire(chat_id).is_some());
    }

    /// The chat is marked busy synchronously, but the work that follows — reading the session,
    /// writing the MCP config, inserting the run row — spans awaits, and the Telegram sidecar's HTTP
    /// client can give up inside that span. A cancelled request drops this future exactly the way
    /// `abort()` drops a turn's, so a chat slot released only by a trailing statement is never
    /// released at all. That is the same 409-forever symptom as a cancelled turn, reached through a
    /// different door: the bot simply stops answering until the daemon restarts.
    #[tokio::test]
    async fn a_dropped_message_request_frees_the_chat() {
        use std::future::Future;

        let state = test_state().await;
        let chat_id = "assistant-dropped-request-chat";

        let mut request = Box::pin(send_message(&state, chat_id, "hello", Origin::Shell));

        // One poll is all it takes to claim the chat; the future then parks on the session lookup.
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(
            request.as_mut().poll(&mut context).is_pending(),
            "the request ran to completion before it could be dropped"
        );
        assert!(
            BUSY_CHATS.lock().unwrap().contains(chat_id),
            "the turn should have claimed the chat"
        );
        drop(request);

        assert!(
            !BUSY_CHATS.lock().unwrap().contains(chat_id),
            "an abandoned request must not leave the chat busy forever"
        );
    }

    #[tokio::test]
    async fn send_message_creates_assistant_run_and_upserts_session() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let chat_id = "assistant-send-test-chat";

        let id = send_message(&state, chat_id, "hello", Origin::Shell)
            .await
            .unwrap();
        let mode: String = sqlx::query_scalar("SELECT mode FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(mode, "assistant");

        let mut status = String::new();
        let mut session = None;
        for _ in 0..50 {
            status = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
            if status == "completed" {
                session = get_session(&pool, chat_id).await.unwrap();
                if session.is_some() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(status, "completed");
        assert_eq!(session, Some("fake-session-id".to_string()));
    }

    /// Every run carries a daemon-assigned session id, and an assistant turn is a run. Its first
    /// turn had nothing to resume, so it was launched with neither `--resume` nor `--session-id`:
    /// the run had an id only if the CLI's stream volunteered one. `budget.rs` deduplicates spend by
    /// `session_id`, so a first turn whose stream carried no `init` event — the case `runner.rs`
    /// already has a test for — was money charged against nothing at all.
    ///
    /// Asserted on the row and on what the runner was handed, because either alone is satisfiable
    /// without the other: a row written and never passed to the CLI leaves the two disagreeing about
    /// which session the spend belongs to.
    #[tokio::test]
    async fn an_assistant_first_turn_is_assigned_a_session_id() {
        let mut state = test_state().await;
        let runner = Arc::new(FakeCommandRunner {
            // Silent about its session, like a stream that never emits an init event. Whatever the
            // turn ends up carrying is therefore the daemon's own doing, not the CLI's.
            canned: Mutex::new(Some(crate::runner::RunOutcome {
                exit_code: 0,
                stdout: "hello back".into(),
                stderr: String::new(),
                session_id: None,
                cost_usd: Some(0.0),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                num_turns: None,
            })),
            ..Default::default()
        });
        state.runner = runner.clone();
        let pool = state.pool.clone();

        let id = send_message(&state, "assistant-first-turn-chat", "hello", Origin::Shell)
            .await
            .unwrap();

        let stored: Option<String> = sqlx::query_scalar("SELECT session_id FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let stored =
            stored.expect("a first turn's row must carry the session its spend is billed to");
        assert_eq!(
            stored.len(),
            36,
            "the assigned id must be a v4 uuid, which is what the CLI accepts: {stored}"
        );

        for _ in 0..500 {
            if runner.last_session_id.lock().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(
            *runner.last_session_id.lock().unwrap(),
            Some(stored),
            "the CLI must be told the same session the row was written with"
        );
        assert_eq!(
            *runner.last_resume.lock().unwrap(),
            None,
            "a first turn has nothing to resume, so the id has to be assigned rather than inherited"
        );
    }

    /// The orchestrator is supposed to reach NucleOS and nothing else, and for a long time the
    /// only thing standing between a Telegram message and the filesystem was an `--allowedTools`
    /// line that does not restrict anything (measured against CLI 2.1.198: an allowlist grants,
    /// it never revokes). The restriction is the tool policy, so the policy is what is asserted.
    #[tokio::test]
    async fn a_turn_launches_the_cli_restricted_to_the_nucleos_server() {
        let mut state = test_state().await;
        let runner = Arc::new(FakeCommandRunner::default());
        state.runner = runner.clone();

        send_message(&state, "assistant-tool-policy-chat", "hello", Origin::Shell)
            .await
            .unwrap();
        for _ in 0..500 {
            if runner.last_tool_policy.lock().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }

        assert_eq!(
            *runner.last_tool_policy.lock().unwrap(),
            Some(crate::runner::ToolPolicy::McpOnly),
            "a Telegram turn must not be launched with the built-in tools available"
        );
    }

    /// A cancelled turn's future is dropped where it is parked, but `abort()` reaches it only at
    /// that drop — so a cancel that has already written `cancelled` can be followed by the turn
    /// waking up once more and writing its own `completed`, reply text and all, over a turn whose
    /// CLI was killed. The status a chat reports must be the first one written, not the last.
    #[tokio::test]
    async fn a_turn_completion_never_overwrites_a_finalised_status() {
        let mut state = test_state().await;
        let runner = Arc::new(FakeCommandRunner {
            delay: Mutex::new(Some(Duration::from_secs(1))),
            ..Default::default()
        });
        state.runner = runner.clone();
        let chat_id = "assistant-finalised-status-chat";

        let id = send_message(&state, chat_id, "take your time", Origin::Shell)
            .await
            .unwrap();
        // The fake runner counts the call before it sleeps, so this parks the turn inside the CLI
        // call: past the point of no return for its terminal write, and short of running it.
        for _ in 0..500 {
            if *runner.calls.lock().unwrap() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(
            *runner.calls.lock().unwrap(),
            1,
            "the turn should have started"
        );

        // What `finalize_termination` writes when it gets there first — written directly, because
        // aborting the task would drop the very future whose last write is the thing under test.
        sqlx::query("UPDATE runs SET status = 'cancelled', completed_at = ? WHERE id = ?")
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();

        // The handle is released by the guard the task captured, so an empty map is proof the turn
        // reached the end — its terminal write included — rather than proof that time passed.
        for _ in 0..500 {
            if !state.run_handles.lock().unwrap().contains_key(&id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !state.run_handles.lock().unwrap().contains_key(&id),
            "the turn never finished"
        );

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            status, "cancelled",
            "a killed turn must not report itself completed"
        );
    }

    #[test]
    fn a_hostile_chat_id_cannot_escape_the_temp_directory() {
        // `chat_id` arrives from a sidecar and is opaque to us. Interpolated raw it reached
        // `Path::join`, which DISCARDS the base when the joined component is absolute — so the
        // config landed on an arbitrary path, and `TurnGuard::drop` then deleted whatever was there.
        let temp = std::env::temp_dir();
        for chat_id in [
            "C:/Windows/System32/nucleos",
            r"C:\Windows\System32\nucleos",
            "../../../evil",
            r"..\..\..\evil",
            "/etc/passwd",
            r"\\server\share\evil",
        ] {
            let path = mcp_config_path(chat_id);
            assert_eq!(
                path.parent(),
                Some(temp.as_path()),
                "{chat_id:?} escaped the temp directory"
            );
        }
    }

    #[test]
    fn distinct_chats_never_share_a_config_path() {
        // Encoding rather than stripping, so the mapping stays injective: two chats that differ
        // only in an escaped character must not collide onto one file and clobber each other.
        assert_ne!(mcp_config_path("a/b"), mcp_config_path("a-b"));
        assert_ne!(mcp_config_path("a/b"), mcp_config_path(r"a\b"));
        assert_ne!(mcp_config_path("a%2fb"), mcp_config_path("a/b"));

        // The ordinary case stays readable rather than being hex soup: a real Telegram group id.
        assert!(
            mcp_config_path("-1001234567890")
                .to_string_lossy()
                .ends_with("nucleos-mcp--1001234567890.json")
        );
    }

    #[tokio::test]
    async fn cancelling_a_turn_frees_the_chat_and_removes_its_mcp_config() {
        let mut state = test_state().await;
        // A slow runner keeps the turn parked on an await, which is where a real `/cancel` lands.
        let runner = Arc::new(FakeCommandRunner {
            delay: Mutex::new(Some(Duration::from_secs(30))),
            ..Default::default()
        });
        state.runner = runner.clone();
        let chat_id = "assistant-cancel-test-chat";
        let mcp_path = mcp_config_path(chat_id);

        let id = send_message(&state, chat_id, "take your time", Origin::Shell)
            .await
            .unwrap();
        // Wait until the CLI is actually under way; cancelling a turn still queued would exercise a
        // different (and easier) path than the one a user hits.
        for _ in 0..50 {
            if *runner.calls.lock().unwrap() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            *runner.calls.lock().unwrap(),
            1,
            "the turn should have started"
        );

        assert!(
            crate::runs::finalize_termination(&state, id, "cancelled").await,
            "the turn should still have been in flight"
        );

        // `finalize_termination` aborts the task, which drops its future mid-await. Cleanup that
        // lives in trailing statements never runs — and a chat left in BUSY_CHATS rejects every
        // later message with 409 until the daemon restarts, which is indistinguishable from the bot
        // having died.
        for _ in 0..50 {
            if !mcp_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            !mcp_path.exists(),
            "the temp mcp config should not outlive a cancelled turn"
        );
        assert!(
            ChatSlot::acquire(chat_id).is_some(),
            "a cancelled turn must free the chat for the next message"
        );
    }
}
