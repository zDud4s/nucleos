//! Questions a hook puts to a person and waits on: the ask window, the pending table and its answers.

/// How long a tool call waits for somebody to answer for it.
///
/// Thirty days less an hour, and the number is a ceiling, not a preference. The CLI holds the hook
/// call open for as long as the settings entry's `timeout` says (`HOOK_TIMEOUT_SECONDS`, thirty
/// days), so a question can stand for as long as the CLI lives and be answered hours later from
/// another machine. The hook's own wait (`ask_daemon.py`) outlasts this by half an hour, and the
/// CLI's timeout outlasts that, so the daemon, not the CLI cutting the hook off, gives the answer.
/// A hook the CLI cuts does not block the tool, it falls through to the ordinary permission flow.
///
/// It bounds a question a hook is waiting on. A question no hook ever started waiting on is bounded
/// by `ASK_WAIT_GRACE` instead.
pub const ASK_WINDOW: std::time::Duration =
    std::time::Duration::from_secs(30 * 24 * 60 * 60 - 60 * 60);

/// How long a question may stand before a hook has started waiting on it.
///
/// The gate call registers the question and the hook comes back to `/hooks/ask-wait` a moment later.
/// A hook that died in between leaves a question nobody will ever collect, while its CLI lives on,
/// so neither the hook's connection (`TakeDown`) nor the process's end (`LiveChat` drop) takes it
/// down. Past this grace such a question is a ghost and goes; one a hook waits on never expires
/// here, because its connection ends it.
pub const ASK_WAIT_GRACE: std::time::Duration = std::time::Duration::from_secs(120);

/// One tool call a conversation is waiting to be allowed.
///
/// In memory and nowhere else, deliberately. A proposal is a durable row because the run it belongs
/// to is not being watched; this exists only while a hook call is blocked on it, and a daemon that
/// restarts has killed the turn that was asking. A question outliving the turn that asked it is a
/// question about nothing.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Ask {
    pub id: String,
    pub chat_id: String,
    pub run_id: i64,
    pub tool: String,
    /// The one argument that says what this is about, or `None` when none of them does.
    ///
    /// Deliberately not the whole input, for the reason `runner::ToolCall` gives about its own: a
    /// `Write` carries the file it is writing, and a window that printed that argument would print
    /// the file.
    pub detail: Option<String>,
}

/// What the ladder needs to decide an open question again: the call itself and where it ran.
///
/// Kept beside the `Ask` and never inside it. `Ask` is what the window is shown and what goes over
/// the wire, and its `detail` is deliberately one argument for the reason given there; this carries
/// the WHOLE input (a `Write`'s content included) and so must never be serialised. It exists so that
/// moving the permission selector can re-run the same ladder over a question already standing,
/// instead of leaving it open until the window refuses it.
#[derive(Clone, Debug)]
pub struct AskedCall {
    pub tool_input: serde_json::Value,
    pub root: String,
    pub project_id: Option<String>,
    /// The rung the turn was SPAWNED on. `effective_permission` needs it to tell a turn that is on
    /// `plan` (fixed by the CLI's own flag) from one that merely was asked on another rung.
    pub snapshot: crate::chats::PermissionMode,
}

pub(super) struct Pending {
    pub(super) ask: Ask,
    /// `None` for a question opened without a call to re-decide (`ask_about`), which a rung change
    /// then leaves for a person.
    pub(super) call: Option<AskedCall>,
    /// Handed to whoever answers, and taken when they do — so a second answer finds nothing rather
    /// than overwriting the first.
    pub(super) answer: Option<tokio::sync::oneshot::Sender<bool>>,
    /// Handed to the hook call that waits, and taken for the same reason.
    pub(super) heard: Option<tokio::sync::oneshot::Receiver<bool>>,
    /// When it was asked, so a question no hook came to wait on can expire (`ASK_WAIT_GRACE`).
    pub(super) asked_at: std::time::Instant,
}

/// Takes down every question whose hook never started waiting (`heard` still there) within
/// `ASK_WAIT_GRACE`. Called wherever the map is read or written to decide something, which is
/// exactly where a ghost would otherwise be seen, kept alive by `chat_has_open_ask`, or answered.
fn forget_ghosts(asks: &mut std::collections::HashMap<String, Pending>) {
    asks.retain(|_, pending| {
        !(pending.heard.is_some() && pending.asked_at.elapsed() >= ASK_WAIT_GRACE)
    });
}

/// Makes a question look `by` older than it is, so a test can cross the grace without sleeping.
#[cfg(test)]
pub(super) fn backdate_ask(id: &str, by: std::time::Duration) {
    let mut asks = ASKS.lock().unwrap();
    let pending = asks.get_mut(id).expect("the question to age is standing");
    pending.asked_at = std::time::Instant::now()
        .checked_sub(by)
        .expect("the clock has run long enough to age a question");
}

pub(super) static ASKS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, Pending>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Records that this turn is waiting to be allowed something, and returns the question's name.
pub fn ask_about(chat_id: &str, run_id: i64, tool: &str, detail: Option<String>) -> String {
    register_ask(chat_id, run_id, tool, detail, None)
}

/// `ask_about` for a question that remembers its call, so a change of rung can decide it again.
pub fn ask_about_call(
    chat_id: &str,
    run_id: i64,
    tool: &str,
    detail: Option<String>,
    call: AskedCall,
) -> String {
    register_ask(chat_id, run_id, tool, detail, Some(call))
}

fn register_ask(
    chat_id: &str,
    run_id: i64,
    tool: &str,
    detail: Option<String>,
    call: Option<AskedCall>,
) -> String {
    let id = crate::auth::generate_uuid_v4();
    let (answer, heard) = tokio::sync::oneshot::channel();
    let mut asks = ASKS.lock().unwrap();
    forget_ghosts(&mut asks);
    // **One question per run, the newest.** A run has one tool call in flight (see `wait_for_run`),
    // so an older question of the same run is a ghost: its hook died between asking and waiting.
    // Left standing, the judge could still answer it `true`, and `wait_for_run` — which finds a
    // question by its run — could hand that yes to THIS call, which nobody judged.
    asks.retain(|_, pending| pending.ask.run_id != run_id);
    asks.insert(
        id.clone(),
        Pending {
            ask: Ask {
                id: id.clone(),
                chat_id: chat_id.to_owned(),
                run_id,
                tool: tool.to_owned(),
                detail,
            },
            call,
            answer: Some(answer),
            heard: Some(heard),
            asked_at: std::time::Instant::now(),
        },
    );
    id
}

/// Whether this question is still standing, answered or not yet.
pub fn ask_is_open(id: &str) -> bool {
    let mut asks = ASKS.lock().unwrap();
    forget_ghosts(&mut asks);
    asks.contains_key(id)
}

/// Whether this run has a tool call parked on a question.
pub fn run_is_parked(run_id: i64) -> bool {
    let mut asks = ASKS.lock().unwrap();
    forget_ghosts(&mut asks);
    asks.values().any(|pending| pending.ask.run_id == run_id)
}

/// Whether anything of this conversation is waiting on a person, which is work for the process
/// that holds the CLI: reaping it would cut the hook's call and let the tool fall through.
pub fn chat_has_open_ask(chat_id: &str) -> bool {
    let mut asks = ASKS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    forget_ghosts(&mut asks);
    asks.values().any(|pending| pending.ask.chat_id == chat_id)
}

/// Takes down every question of a conversation whose process is gone. The hook's connection
/// (`TakeDown`) ends a question being waited on, and `ASK_WAIT_GRACE` one whose hook never arrived
/// while its process lives; this is for a CLI that died with its hook, which nothing else would
/// ever clear.
pub fn forget_asks_for_chat(chat_id: &str) {
    ASKS.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|_, pending| pending.ask.chat_id != chat_id);
}

/// What this conversation is waiting to be allowed, which is nearly always nothing or one thing.
pub fn asks_for(chat_id: &str) -> Vec<Ask> {
    let mut asks = ASKS.lock().unwrap();
    forget_ghosts(&mut asks);
    asks.values()
        .filter(|pending| pending.ask.chat_id == chat_id)
        .map(|pending| pending.ask.clone())
        .collect()
}

/// The standing questions of one conversation that can be decided again: id, run, tool and call.
///
/// Only those still unanswered (`answer` not yet taken) and only those that kept their call, so a
/// question somebody just answered, or one opened without a call, is never handed back.
pub(super) fn open_calls(chat_id: &str) -> Vec<(String, i64, String, AskedCall)> {
    let mut asks = ASKS.lock().unwrap();
    forget_ghosts(&mut asks);
    asks.values()
        .filter(|pending| pending.ask.chat_id == chat_id && pending.answer.is_some())
        .filter_map(|pending| {
            let call = pending.call.clone()?;
            Some((
                pending.ask.id.clone(),
                pending.ask.run_id,
                pending.ask.tool.clone(),
                call,
            ))
        })
        .collect()
}

/// Answers one question. `false` when there was nothing to answer, which is a race a person loses
/// harmlessly: the turn moved on, or somebody answered a moment sooner.
pub fn answer_ask(id: &str, allow: bool) -> bool {
    let answer = {
        let mut asks = ASKS.lock().unwrap();
        forget_ghosts(&mut asks);
        asks.get_mut(id).and_then(|pending| pending.answer.take())
    };
    match answer {
        Some(answer) => answer.send(allow).is_ok(),
        None => false,
    }
}

/// Waits for this RUN's question to be answered. `None` means refuse, whatever the reason.
///
/// Keyed on the run and not on the question's name, which is what keeps it honest without a second
/// check: a run has at most one tool call in flight, because the hook that asks is synchronous and
/// the CLI is sitting on it. So "this run's ask" names exactly one thing, and no run can name
/// another's.
///
/// The question is taken down either way. A turn whose call was refused has moved on, and a
/// question still standing in the window would be about something that is no longer happening.
/// "Either way" includes this future being dropped mid-wait — the hook's connection cut — which is
/// why the taking down is a drop guard and not a line after the `await`.
pub async fn wait_for_run(run_id: i64, window: std::time::Duration) -> Option<bool> {
    let (id, heard) = {
        let mut asks = ASKS.lock().unwrap();
        forget_ghosts(&mut asks);
        let (id, pending) = asks
            .iter_mut()
            .find(|(_, pending)| pending.ask.run_id == run_id)?;
        (id.clone(), pending.heard.take()?)
    };
    // Built only once the lock above is released: its `drop` takes the same lock.
    let taken_down = TakeDown(id);
    let answer = tokio::time::timeout(window, heard)
        .await
        .ok()
        .and_then(|heard| heard.ok());
    drop(taken_down);
    answer
}

/// Removes a question when the wait on it ends, however it ends.
struct TakeDown(String);

impl Drop for TakeDown {
    fn drop(&mut self) {
        ASKS.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.0);
    }
}
