//! Warming a chat's process ahead of its first message: whether to, the one-at-a-time and
//! recently-declined guards, and the unbriefed mark the first turn after a warm-up reads.

use super::*;

/// Whether a turn should read the briefing: only a Claude turn that no kept process will serve. The
/// block travels as a per-spawn system flag, so a process already standing would discard it.
pub(in super::super) fn briefing_wanted(cli: &str, kept: bool) -> bool {
    cli == "claude" && !kept
}

/// Whether opening a chat should start its process ahead of the first message.
///
/// Only for a rooted conversation that may keep a process, has a session to resume (so the process
/// starts where the conversation is), is idle and has no process yet, is not already being warmed
/// (one at a time), and leaves room under the cap.
#[allow(clippy::too_many_arguments)]
pub(in super::super) fn should_prewarm(
    rooted: bool,
    cli: &str,
    policy: crate::execution::runner::ToolPolicy,
    has_session: bool,
    busy: bool,
    kept: bool,
    warming: bool,
    alive: usize,
) -> bool {
    rooted
        && may_keep_process(policy, cli)
        && has_session
        && !busy
        && !kept
        && !warming
        && alive < LIVE_CAP
}

/// Set while one chat's process is being warmed: opening several chats at once must not start
/// several processes at once.
static WARMING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Clears [`WARMING`] on every way out of a warm-up, including a panic.
struct Warming;

impl Drop for Warming {
    fn drop(&mut self) {
        WARMING.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// How long a chat [`warm_chat`] declined is left alone. `prewarm_open_chat` runs on every poll of
/// an open transcript, and each declined attempt re-read the database and the settings file to come
/// to the same answer. A chat whose state changes meanwhile (a session appears, a process is
/// evicted) waits this out.
pub(super) const DECLINED_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// Chats [`warm_chat`] recently declined, and when.
static DECLINED: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
> = std::sync::LazyLock::new(Default::default);

/// Whether `chat_id` was declined less than [`DECLINED_TTL`] before `now`.
pub(super) fn declined_within(
    declined: &std::collections::HashMap<String, std::time::Instant>,
    chat_id: &str,
    now: std::time::Instant,
) -> bool {
    declined
        .get(chat_id)
        .is_some_and(|at| now.saturating_duration_since(*at) < DECLINED_TTL)
}

/// Records `chat_id` as declined at `now`, dropping entries already past the TTL so the map stays
/// as small as the set of chats open in the last half minute.
pub(super) fn note_declined_in(
    declined: &mut std::collections::HashMap<String, std::time::Instant>,
    chat_id: &str,
    now: std::time::Instant,
) {
    declined.retain(|_, at| now.saturating_duration_since(*at) < DECLINED_TTL);
    declined.insert(chat_id.to_owned(), now);
}

fn declined_recently(chat_id: &str) -> bool {
    let declined = DECLINED.lock().unwrap_or_else(|e| e.into_inner());
    declined_within(&declined, chat_id, std::time::Instant::now())
}

fn note_declined(chat_id: &str) {
    let mut declined = DECLINED.lock().unwrap_or_else(|e| e.into_inner());
    note_declined_in(&mut declined, chat_id, std::time::Instant::now());
}

/// Chats whose kept process was warmed with no briefing and has served no turn yet.
///
/// The briefing is chosen against the message (`brief::of` ranks by the query; nothing passes
/// `MIN_RELEVANCE` against an empty one), so a process started before there is a message cannot be
/// given it. The first turn such a process would serve therefore still asks for the briefing, and
/// when there is one it lets the warmed process go and starts one that carries it: no latency is
/// lost when nothing relevant is known, and nothing relevant is silently dropped when it is.
static UNBRIEFED: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(Default::default);

fn mark_unbriefed(chat_id: &str) {
    UNBRIEFED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(chat_id.to_owned());
}

/// Whether `chat_id`'s kept process was warmed unbriefed, clearing the mark: only the first turn
/// after a warm-up asks, whichever process ends up serving it.
pub(super) fn take_unbriefed(chat_id: &str) -> bool {
    UNBRIEFED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(chat_id)
}

/// Starts the open chat's process before its first message, so that message does not pay for it.
///
/// Called when a visible view opens the chat. Synchronous and cheap: it only looks at what is
/// already in memory and hands the rest to a task of its own, which decides again with the facts it
/// has to read (see [`should_prewarm`]).
pub fn prewarm_open_chat(state: &crate::state::AppState, chat_id: &str) {
    if is_busy(chat_id)
        || has_kept_process(chat_id)
        || WARMING.load(std::sync::atomic::Ordering::SeqCst)
        || declined_recently(chat_id)
    {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let state = state.clone();
    let chat_id = chat_id.to_owned();
    runtime.spawn(async move { warm_chat(&state, &chat_id).await });
}

/// The body of [`prewarm_open_chat`]: builds the request a turn of this chat would build, with
/// nothing to say yet, and keeps the process it spawns.
async fn warm_chat(state: &crate::state::AppState, chat_id: &str) {
    let pool = &state.pool;
    let Ok(cwd) = crate::chats::cwd_of(pool, chat_id).await else {
        return;
    };
    // Unrooted conversations never keep a process; no point reading anything else.
    if cwd.is_none() {
        note_declined(chat_id);
        return;
    }
    // Read before anything costlier: with no session there is nothing to resume, and this runs on
    // every poll of an open transcript.
    let resume = get_session(pool, chat_id).await.ok().flatten();
    if resume.is_none() {
        note_declined(chat_id);
        return;
    }
    let answering = crate::chats::answering(pool, chat_id)
        .await
        .unwrap_or_default();
    let config = crate::config::models_config_now();
    let (runner, cli) = runner_for_turn(
        &state.runner,
        state.assistants.as_ref(),
        &config,
        answering.model.as_deref(),
    );
    // Reads the conversation's settings file: off the runtime thread, as `send_message_inner` does,
    // and a join failure reads as "not wired".
    let hook_wired = match cwd.clone() {
        Some(dir) => tokio::task::spawn_blocking(move || {
            crate::governance::autopilot::classifier_hook_is_wired(std::path::Path::new(&dir))
        })
        .await
        .unwrap_or(false),
        None => false,
    };
    // The policy a person's own message at this machine would get: the origin that opened the window.
    let tool_policy = tool_policy_for(
        cwd.as_deref(),
        Origin::Shell,
        hook_wired,
        crate::relay::handover_of(pool, None).await,
    );
    // The cap is not enforced under test (`ENFORCED_CAP`): `LIVE_PROCESSES` is one counter shared by
    // every test running in parallel, and counting it here would make a warm-up depend on them.
    let alive = if ENFORCED_CAP == usize::MAX {
        0
    } else {
        LIVE_PROCESSES.load(std::sync::atomic::Ordering::SeqCst)
    };
    let busy = is_busy(chat_id);
    let warming = WARMING.load(std::sync::atomic::Ordering::SeqCst);
    if !should_prewarm(
        true,
        cli,
        tool_policy,
        resume.is_some(),
        busy,
        has_kept_process(chat_id),
        warming,
        alive,
    ) {
        // A turn in flight or another chat being warmed is over in moments; only a refusal that
        // comes from the conversation itself is worth remembering.
        if !busy && !warming {
            note_declined(chat_id);
        }
        return;
    }
    let Some(resume) = resume else {
        return;
    };
    // Claimed only now, with the decision made, and released by `Warming`'s drop.
    if WARMING
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .is_err()
    {
        return;
    }
    let _warming = Warming;

    // The chat's slot, held while the process is built, so no turn starts beside it. Taken as a
    // warm-up's: the chat does not read as busy, and a send waits these moments out instead of being
    // queued behind a turn that does not exist. Armed to send what waited when it is released: a
    // message queued meanwhile must not wait for ever.
    let Some(mut slot) = ChatSlot::acquire_for_warmup(chat_id) else {
        return;
    };
    slot.drain_on_release(state);
    // A turn may have kept a process between the check above and the slot. Minting a key now would
    // retire the one that process still holds.
    if has_kept_process(chat_id) {
        return;
    }

    let mode = crate::chats::permission_mode_of(pool, chat_id)
        .await
        .unwrap_or(crate::chats::PermissionMode::Auto);
    let permission = crate::execution::runner::Permission::for_chat(mode);
    // No turn owns this process yet, so no run id; the conversation's key is what identifies it.
    let mut env = chat_key::turn_env(tool_policy, true, pool, chat_id, 0, &state.token.0).await;
    // A process is keyed where it is spawned, and this is where one is.
    chat_key::rekey(&mut env, pool, chat_id).await;

    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // Its own file, not the one a turn writes: a turn starting meanwhile must not have its config
    // removed from under it.
    let mcp_path = mcp_config_path(&format!("{chat_id}.warm"));
    let active = crate::chats::mcp_servers::active_for(tool_policy, &answering.mcp_servers);
    let config = build_chat_mcp_config(&exe.to_string_lossy(), active);
    if write_mcp_config(&mcp_path, &config).is_err() {
        return;
    }
    {
        // The CLI reads the config as it starts; a minute is far more than that needs.
        let doomed = mcp_path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let _ = std::fs::remove_file(doomed);
        });
    }

    let fingerprint = spawn_fingerprint(&answering, None, tool_policy);
    let request = chat_run_request(
        String::new(),
        Vec::new(),
        env,
        cwd.map(std::path::PathBuf::from),
        permission,
        Some(resume.clone()),
        resume,
        mcp_path,
        tool_policy,
        answering,
        None,
        cli,
        None,
    );
    // Nobody is waiting for the session id: the process announces it with its first answer, and the
    // turn that speaks to it records it then.
    let (session_tx, _) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut live = spawn_live_process(&runner, request, session_tx, chat_id, &fingerprint);
    // Known by the name a turn's own config has, or the first turn would never fit this process.
    live.shape = live
        .shape
        .clone()
        .with_mcp_config(Some(mcp_config_path(chat_id)));
    keep_live(chat_id, live, LIVE_IDLE);
    // Marked while the slot is still held, so no turn can come between the process and its mark.
    mark_unbriefed(chat_id);
}
