//! What `nucleos-core` needs to know about the assistant without owning it.
//!
//! The assistant itself (`assistant`, the turn machinery) lives in `nucleos-app`, a crate above this
//! one. The lib still names a handful of its vocabulary words (who sent a message, what became of
//! it, the refusals a turn can earn, the MCP config a launch is handed), reads the barrier of the
//! kept processes, and starts a turn from two places (voice, the browser panel). Those words and the
//! one hook live here; `assistant` re-exports them, so its own paths and the paths of everything
//! above it did not change.

use std::sync::{Arc, Mutex};

use sqlx::SqlitePool;

use crate::execution::runner::Attachment;
use crate::state::AppState;

/// Which client sent a chat message, as the client states it.
///
/// Stated rather than inferred, and that is why this type exists at all. A Telegram group id is
/// negative, so the origin is guessable from the shape of `chat_id` — and guessing would make a
/// routing decision depend on a numbering scheme Telegram owns and can change without telling
/// anybody. A client that says nothing is `Shell`, which keeps every existing caller where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Shell,
    /// Spoken into the microphone of the machine this daemon runs on.
    ///
    /// A variant and not a flag on `Shell`, because the two differ in what happens to the ANSWER:
    /// a shell turn is read, a voice turn is spoken. Recorded on the row for the same reason the
    /// other two are — a queued message must be sent as the thing it was.
    Voice,
    /// Typed in the panel of a NucleOS browser window on this machine; answered like `Shell`.
    BrowserPanel,
    /// A message that arrived through a channel sidecar, named by its registry id.
    ///
    /// The id is the manifest's `&'static str`, so the type stays `Copy`; an id that is not in the
    /// registry cannot be built from the wire (see `from_wire`).
    Channel(&'static str),
}

impl Origin {
    /// An unknown spelling resolves to `Shell` for the same reason absence does: this is a
    /// ship-dark feature, so anything not explicitly asking for the new path gets the old one.
    pub fn from_wire(value: Option<&str>) -> Self {
        match value {
            // Legacy spelling of the Telegram origin, kept so a message queued before the
            // channel registry still resolves; never built from any other bare wire string.
            Some("telegram") => Self::Channel(crate::channels::registry::TELEGRAM.id),
            Some("voice") => Self::Voice,
            Some("browser-panel") => Self::BrowserPanel,
            Some(wire) => wire
                .strip_prefix("channel:")
                .and_then(crate::channels::registry::by_id)
                .map_or(Self::Shell, |manifest| Self::Channel(manifest.id)),
            None => Self::Shell,
        }
    }

    /// Whether the sender is somebody on the far side of a network rather than at this machine.
    pub fn is_remote(self) -> bool {
        matches!(self, Self::Channel(_))
    }

    /// How this origin is written down, so a message that waits is sent as the thing it was.
    ///
    /// The inverse of `from_wire` and asserted against it: a queued message carries its origin
    /// through the database, and an origin that did not survive the round trip would route a
    /// Telegram turn back into the shell.
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Voice => "voice",
            Self::BrowserPanel => "browser-panel",
            Self::Shell => "shell",
            Self::Channel(id) => crate::channels::registry::by_id(id)
                .map_or("shell", |manifest| manifest.origin_wire),
        }
    }
}

/// Why a turn was refused before it cost anything: the chat says `local` and this machine has none.
///
/// A named constant rather than a sentence written twice, because `http.rs` turns it into the one
/// status code that tells this apart from a daemon that broke. Matched exactly there — a refusal
/// recognised by a substring is a refusal that stops being recognised when someone edits the words.
pub const NO_LOCAL_MODEL: &str = "this chat is set to the local model and none is configured";

/// Why a turn was refused before it cost anything: the chat says `openrouter` and this daemon has
/// no hosted model wired up to answer it.
///
/// `NO_LOCAL_MODEL`'s sibling and not its synonym, and the two have to stay apart because the
/// failure they each guard is aimed at a different wallet. A chat that says `local` and finds no
/// local model falls back to the CLI today when the choice was only inferred (`origin ==
/// Origin::Channel(_)`) and refuses only when a person SAID `local` outright — because
/// the CLI has always answered an unmarked Telegram message, so refusing there would take the bot
/// off the air to enforce a promise nobody made. `openrouter` has no such history: nothing before
/// this brain existed could ever have chosen it, so there is no old behaviour to preserve and no
/// argument for falling through at all. Every turn that reaches this brain is refused, full stop,
/// until the hosted route is actually wired — the placeholder this constant replaces the silence
/// of (`(None, Some(crate::chats::Brain::OpenRouter)) => false` in `send_message_with`) is exactly
/// what let that turn go to the cloud CLI instead: silently, and on the bill.
///
/// A named constant for the same reason `NO_LOCAL_MODEL` is one: `http.rs` will need to turn this
/// into a status code a client can act on, and a refusal recognised by a fragment of its wording
/// stops being recognised the day somebody improves the sentence.
///
pub const NO_HOSTED_MODEL: &str = "this chat is set to the hosted model and none is configured";

/// What became of a message somebody sent: a turn, or a place in the queue.
///
/// Two outcomes rather than an `Option<i64>`, because the caller has something different to say
/// about each and a null id says neither. A turn is answered by watching it; a queued message is
/// answered by showing it waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    Turn(i64),
    Queued,
}

/// Why a turn was refused before it cost anything: this conversation is already answering one.
///
/// A constant because two callers now have to recognise it and act differently on it. `http.rs`
/// turns it into the one status a client can retry on, and `scheduler.rs` reads it as "nothing was
/// started" and hands the window back so the rule tries again on the next tick.
///
/// Matched by value at both, never by substring. A refusal recognised by a fragment of its wording
/// stops being recognised the moment somebody improves the sentence, and the two behaviours that
/// depend on it would fail apart and silently.
pub const TURN_IN_PROGRESS: &str = "a turn is already in progress for this chat";

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

/// The throwaway MCP config a turn is launched with.
pub fn build_mcp_config(exe_path: &str) -> serde_json::Value {
    let args = vec!["--mcp-tools".to_string()];
    serde_json::json!({
        "mcpServers": {
            "nucleos": {
                "type": "stdio",
                "command": exe_path,
                "args": args
            }
        }
    })
}

/// The throwaway MCP config a job node is launched with.
pub fn build_job_node_mcp_config(exe_path: &str, job_id: i64) -> serde_json::Value {
    let args = vec![
        "--mcp-tools".to_string(),
        "--box".to_string(),
        "job-node".to_string(),
        "--job".to_string(),
        job_id.to_string(),
    ];
    serde_json::json!({
        "mcpServers": {
            "nucleos": {
                "type": "stdio",
                "command": exe_path,
                "args": args
            }
        }
    })
}

/// The throwaway MCP config a team agent's node run is launched with.
pub fn build_team_mcp_config(exe_path: &str, run_id: i64) -> serde_json::Value {
    let args = vec![
        "--mcp-tools".to_string(),
        "--box".to_string(),
        "team".to_string(),
        "--run".to_string(),
        run_id.to_string(),
    ];
    serde_json::json!({
        "mcpServers": {
            "nucleos": {
                "type": "stdio",
                "command": exe_path,
                "args": args
            }
        }
    })
}

/// Every `runs.answered_by` wire word `spawn_local_turn` ever writes.
///
/// One array, read by `answered_by_a_local_agent_loop` below and by nothing else — the single place
/// that has to grow the day a third `LocalChat` joins `runner::OllamaChat` and `openai_compatible::OpenAiCompatibleChat`,
/// because `spawn_local_turn`'s own `answered_by` parameter is generalised to accept whatever wire
/// word a caller passes it, and this is the one spot that says which words those calls actually use.
pub const LOCAL_AGENT_ANSWERED_BY: &[&str] = &["local", "openrouter"];

/// Whether `answered_by` names a turn `spawn_local_turn` drove, as opposed to the CLI path.
///
/// Exists so a reader OUTSIDE this module — `voice.rs`'s `answer_so_far` is the one today — never
/// has to spell `"local"` or `"openrouter"` out for itself to answer a question that is really about
/// `spawn_local_turn`'s own behaviour: every turn it drives writes `stdout` as the plain finished
/// answer and streams nothing while running, where a CLI turn streams JSONL into both. A caller that
/// compared `answered_by` against `"local"` alone — literally the bug this replaces — quietly mis-
/// classified every hosted turn as a CLI one instead, which is exactly the failure a third route
/// added here without a matching edit at every call site would repeat. Checking against
/// `LOCAL_AGENT_ANSWERED_BY` instead of the two literals directly means a future third entry needs
/// only ONE new line, not a search for every place someone once wrote `"local"`.
pub fn answered_by_a_local_agent_loop(answered_by: &str) -> bool {
    LOCAL_AGENT_ANSWERED_BY.contains(&answered_by)
}

/// Takes a lock a panic poisoned anyway: the data is still there and the next caller needs it.
pub fn relock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether a kept process of `chat_id`, alive now, served run `run_id`. This, not the
/// `chat_tasks` row, is what lets a task act after its turn: a row can be written `running` after
/// its process is gone.
pub fn live_process_served(chat_id: &str, run_id: i64) -> bool {
    relock(&PROCESS_BARRIERS)
        .values()
        .any(|p| p.chat_id == chat_id && p.served.contains(&run_id))
}

/// Test-only stand-in for a kept process that served `run_id`; dropping it ends the vouching.
#[cfg(any(test, feature = "testkit"))]
pub struct HeldProcess(usize);

#[cfg(any(test, feature = "testkit"))]
impl Drop for HeldProcess {
    fn drop(&mut self) {
        relock(&PROCESS_BARRIERS).remove(&self.0);
    }
}

/// Keys are odd, so they never equal a real (aligned) `process_key`.
#[cfg(any(test, feature = "testkit"))]
pub fn hold_process_serving(chat_id: &str, run_id: i64) -> HeldProcess {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
    let key = NEXT.fetch_add(2, std::sync::atomic::Ordering::SeqCst);
    relock(&PROCESS_BARRIERS).insert(
        key,
        ProcessBarrier {
            mode: "auto",
            served: vec![run_id],
            chat_id: chat_id.to_owned(),
            pool: None,
        },
    );
    HeldProcess(key)
}

/// What the turns a kept process has served say about the barrier it runs under.
///
/// The mode is the one STAMPED on the latest turn this process served; `served` is every run id it
/// served, so `read_untrusted` can be the strictest of them. A spontaneous turn takes its barrier
/// from here and not from the chat's newest row: that row may be a turn that never reached this
/// process (stopped before it stamped its mode), and copying it would hand the hook a NULL it reads
/// as `auto`.
pub struct ProcessBarrier {
    pub mode: &'static str,
    pub served: Vec<i64>,
    /// The conversation this process belongs to.
    pub chat_id: String,
    /// To close its tasks when it ends. `None` only for the test holder.
    pub pool: Option<SqlitePool>,
}

/// Barriers of the kept processes, by `LiveChat::process_key`. Cleared by `LiveChat`'s `Drop`.
pub static PROCESS_BARRIERS: std::sync::LazyLock<
    Mutex<std::collections::HashMap<usize, ProcessBarrier>>,
> = std::sync::LazyLock::new(Default::default);

/// Records that the process behind `key` served run `run_id` under `mode`.
pub fn note_served(key: usize, chat_id: &str, run_id: i64, mode: &'static str, pool: &SqlitePool) {
    let mut barriers = relock(&PROCESS_BARRIERS);
    let entry = barriers.entry(key).or_insert_with(|| ProcessBarrier {
        mode,
        served: Vec::new(),
        chat_id: chat_id.to_owned(),
        pool: Some(pool.clone()),
    });
    entry.mode = mode;
    entry.served.push(run_id);
}

/// What the lib asks of the assistant, which lives in a crate above it: start (or queue) a turn.
///
/// Two callers, both in the lib and both below the assistant in the crate graph: the voice loop
/// (`integrations::voice::converse`) and the browser panel (`browser::panel::deliver`). The daemon
/// installs the real implementation at startup ([`AppState::turns`]); a state that never did, as in
/// the lib's own tests, answers [`NOT_WIRED`] instead of starting a turn.
#[async_trait::async_trait]
pub trait Turns: Send + Sync {
    /// `assistant::send_or_queue`: sends the message, or keeps it until the chat has a turn free.
    async fn send_or_queue(
        &self,
        state: &AppState,
        chat_id: &str,
        text: &str,
        images: &[Attachment],
        origin: Origin,
    ) -> Result<Sent, String>;

    /// `assistant::say_now_from`, with its outcome dropped: the panel only needs to know whether the
    /// text was taken.
    async fn say_now_from(
        &self,
        state: &AppState,
        chat_id: &str,
        text: &str,
        origin: Origin,
    ) -> Result<(), String>;
}

/// What a caller without a wired [`Turns`] reports as the reason.
pub const NOT_WIRED: &str = "assistant turns are not wired in this build";

/// The concrete type [`AppState::turns`] stores, since a `LateSlot` holds one sized type.
pub struct TurnsHandle {
    pub turns: Arc<dyn Turns>,
    /// The same object when it is a [`RecordingTurns`], so a test can read it back from a state.
    #[cfg(test)]
    recorder: Option<Arc<RecordingTurns>>,
}

/// Fills the slot. Called once by the crate that owns the assistant; a second call keeps the first.
pub fn install_turns(state: &AppState, turns: Arc<dyn Turns>) {
    state.turns.set(Arc::new(TurnsHandle {
        turns,
        #[cfg(test)]
        recorder: None,
    }));
}

/// The installed [`Turns`], or `None` when nothing installed one.
pub fn turns(state: &AppState) -> Option<Arc<dyn Turns>> {
    state
        .turns
        .get::<TurnsHandle>()
        .map(|handle| handle.turns.clone())
}

/// A [`Turns`] that starts nothing and remembers what it was asked, for the lib's own tests of
/// the two callers: what reaches the assistant is theirs to get right, what the assistant then
/// does with it is tested beside the assistant.
#[cfg(test)]
#[derive(Default)]
pub struct RecordingTurns {
    /// `(chat_id, text, origin)` per call, in order.
    pub calls: std::sync::Mutex<Vec<(String, String, Origin)>>,
}

#[cfg(test)]
impl RecordingTurns {
    /// Installs a fresh recorder into `state` and returns it.
    pub fn install(state: &AppState) -> Arc<RecordingTurns> {
        let recorder = Arc::new(RecordingTurns::default());
        state.turns.set(Arc::new(TurnsHandle {
            turns: recorder.clone(),
            recorder: Some(recorder.clone()),
        }));
        recorder
    }

    /// The recorder [`RecordingTurns::install`] put into `state`.
    pub fn of(state: &AppState) -> Arc<RecordingTurns> {
        state
            .turns
            .get::<TurnsHandle>()
            .and_then(|handle| handle.recorder.clone())
            .expect("RecordingTurns::install ran on this state")
    }

    pub fn calls(&self) -> Vec<(String, String, Origin)> {
        self.calls.lock().expect("recorder lock").clone()
    }

    fn record(&self, chat_id: &str, text: &str, origin: Origin) -> usize {
        let mut calls = self.calls.lock().expect("recorder lock");
        calls.push((chat_id.to_owned(), text.to_owned(), origin));
        calls.len()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl Turns for RecordingTurns {
    async fn send_or_queue(
        &self,
        _state: &AppState,
        chat_id: &str,
        text: &str,
        _images: &[Attachment],
        origin: Origin,
    ) -> Result<Sent, String> {
        Ok(Sent::Turn(self.record(chat_id, text, origin) as i64))
    }

    async fn say_now_from(
        &self,
        _state: &AppState,
        chat_id: &str,
        text: &str,
        origin: Origin,
    ) -> Result<(), String> {
        self.record(chat_id, text, origin);
        Ok(())
    }
}
