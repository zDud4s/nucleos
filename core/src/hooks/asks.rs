//! Questions a hook puts to a person and waits on: the ask window, the pending table and its answers.

/// How long a tool call waits for somebody to answer for it.
///
/// Forty-five seconds, and the number is not a preference. The CLI holds the hook call open while
/// this waits and kills it at its own ceiling — sixty seconds unless the settings entry says
/// otherwise, and the entries already written into people's projects do not. Raising that would
/// help only the projects wired after the change and leave every existing one being cut off
/// mid-question, so the window is chosen to fit the ceiling that is actually out there.
///
/// Long enough for somebody looking at the window to read a command and decide; short enough that
/// stepping away costs one refused tool call rather than a conversation that hangs.
pub const ASK_WINDOW: std::time::Duration = std::time::Duration::from_secs(45);

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
/// instead of leaving it open until the 45-second window refuses it.
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
    ASKS.lock().unwrap().insert(
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
        },
    );
    id
}

/// What this conversation is waiting to be allowed, which is nearly always nothing or one thing.
pub fn asks_for(chat_id: &str) -> Vec<Ask> {
    ASKS.lock()
        .unwrap()
        .values()
        .filter(|pending| pending.ask.chat_id == chat_id)
        .map(|pending| pending.ask.clone())
        .collect()
}

/// The standing questions of one conversation that can be decided again: id, run, tool and call.
///
/// Only those still unanswered (`answer` not yet taken) and only those that kept their call, so a
/// question somebody just answered, or one opened without a call, is never handed back.
pub(super) fn open_calls(chat_id: &str) -> Vec<(String, i64, String, AskedCall)> {
    ASKS.lock()
        .unwrap()
        .values()
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
    let answer = ASKS
        .lock()
        .unwrap()
        .get_mut(id)
        .and_then(|pending| pending.answer.take());
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
pub async fn wait_for_run(run_id: i64, window: std::time::Duration) -> Option<bool> {
    let (id, heard) = {
        let mut asks = ASKS.lock().unwrap();
        let (id, pending) = asks
            .iter_mut()
            .find(|(_, pending)| pending.ask.run_id == run_id)?;
        (id.clone(), pending.heard.take()?)
    };
    let answer = tokio::time::timeout(window, heard)
        .await
        .ok()
        .and_then(|heard| heard.ok());
    ASKS.lock().unwrap().remove(&id);
    answer
}
