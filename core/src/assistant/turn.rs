//! Local turns, runner selection, deadlines, and assistant turn launching.

use super::*;

/// Every `runs.answered_by` wire word `spawn_local_turn` below ever writes.
///
/// One array, read by `answered_by_a_local_agent_loop` below and by nothing else — the single place
/// that has to grow the day a third `LocalChat` joins `runner::OllamaChat` and `openai_compatible::OpenAiCompatibleChat`,
/// because `spawn_local_turn`'s own `answered_by` parameter is generalised to accept whatever wire
/// word a caller passes it, and this is the one spot that says which words those calls actually use.
pub(super) const LOCAL_AGENT_ANSWERED_BY: &[&str] = &["local", "openrouter"];

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

/// Records and drives a turn answered by a `local_agent::LocalAssistant` tool-calling loop rather
/// than the agent CLI — the local model on this machine (`answered_by == "local"`) or the hosted
/// one reached over OpenRouter (`answered_by == "openrouter"`). One body for both: the two differ
/// only in which `LocalChat` the assistant was built with (`runner::OllamaChat` or
/// `openai_compatible::OpenAiCompatibleChat`) and in that one wire word, and a second copy of everything else
/// here is exactly the drift this module's map exists to prevent.
///
/// Deliberately NOT a variant inside `spawn_assistant_turn`. That body is almost entirely about
/// things this path does not have — an MCP config written to disk, a CLI session id arriving on a
/// channel, a resumable session, a cost in dollars, a stderr stream that explains an exit code.
/// Threading `Option`s through all of it to skip each in turn would make the CLI path harder to
/// read in order to describe a path that shares three lines with it.
///
/// History is replayed rather than resumed. There is no session to resume — neither Ollama's chat
/// endpoint nor OpenRouter's has a session protocol — so `recent_exchanges` rebuilds the
/// conversation from the run rows the turns already wrote, under the same barrier `get_session`
/// applies on the other path.
///
/// `relay_id` reaches here too, and is written into this INSERT for the same reason
/// `send_message_inner` writes its own: a conversation's brain is a property of the CHAT
/// (`chats::brain_of`), not of how the message that woke it up arrived, so a relay landing in a
/// conversation set to `Local` takes this path exactly as a person's own message would — and its
/// run must record where it came from just as reliably.
///
/// `origin` reaches here for the same reason and stops at the same place: this path writes its own
/// `runs` row, so a column wired only into `send_message_inner` would be NULL on every turn a
/// conversation set to `Local` ever answered. It is written, never READ here — the local path does
/// not route on it, having already been chosen by the time this is called — which is exactly what
/// makes forgetting it easy and invisible until `relay::admit` refuses a relay it should have
/// admitted.
pub(super) async fn spawn_local_turn(
    state: &crate::state::AppState,
    slot: ChatSlot,
    text: String,
    assistant: std::sync::Arc<crate::models::local_agent::LocalAssistant>,
    relay_id: Option<i64>,
    origin: Origin,
    // The `runs.answered_by` wire word for this turn: `"local"` or `"openrouter"`, matching
    // `chats::Brain::as_str()` for the brain that chose this path. A parameter and not a constant
    // baked into the query below, because that hardcoded `'local'` is exactly what made this
    // function local-only in the first place — generalising the call site, per this packet's own
    // instruction, means the one fact that differs travels in, not a second copy of the query.
    answered_by: &'static str,
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
        "INSERT INTO runs (prompt, status, mode, session_id, chat_id, answered_by, from_relay_id,
                           origin, created_at)
         VALUES (?, 'running', 'assistant', ?, ?, ?, ?, ?, ?)",
    )
    .bind(&text)
    .bind(&session_id)
    .bind(&slot.chat_id)
    .bind(answered_by)
    .bind(relay_id)
    .bind(origin.as_wire())
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&state.pool)
    .await
    .map_err(|e| e.to_string())?
    .last_insert_rowid();
    crate::chats::stamp_project(&state.pool, id, &slot.chat_id).await;

    // The relay learns which turn answered it, now that the turn has an id to learn.
    //
    // After the INSERT and not inside it, because the run's id does not exist until the run does —
    // which is exactly why this pointer and `runs.from_relay_id` are not equally trusted. That one
    // is written in the same statement as the run and is what `relay::chain_of` walks; this one is
    // a second write that can fail on its own, and 0122's header says plainly that nothing
    // security-critical may read it.
    //
    // A warning and not a refusal, therefore. The turn is already under way; undoing it because an
    // audit pointer would not write would be trading the thing somebody is waiting for against the
    // record of it.
    if let Some(relay_id) = relay_id
        && let Err(error) = crate::relay::mark_delivered(&state.pool, relay_id, id).await
    {
        tracing::warn!(%error, id, relay_id, "the relayed turn started but was not stamped onto its relay");
    }

    let prompt = text;

    let pool = state.pool.clone();
    let run_timeout = state.run_timeout;
    crate::execution::runs::spawn_registered(state, id, async move {
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
        // This turn's tools speak for this row, so a declaration names the run that taught it.
        let outcome = tokio::time::timeout(
            run_timeout,
            assistant.answer_as_run(id, &history, &prompt, &taint),
        )
        .await;
        let completed_at = chrono::Utc::now().to_rfc3339();

        // Marked before any status is written, whatever the ending. `hooks.rs` refuses the READ
        // when this fails, which is not available here — the reading already happened — so the
        // fail-closed move left is to refuse the ANSWER: the turn is recorded as failed and its
        // text is dropped rather than stored and handed to the next turn as history.
        let mut unmarked = false;
        if taint.load(std::sync::atomic::Ordering::SeqCst)
            && let Err(error) = crate::execution::runs::mark_untrusted_context(&pool, id).await
        {
            tracing::error!(
                run_id = id,
                %answered_by,
                %error,
                "could not mark a turn as having read untrusted text — dropping its answer"
            );
            unmarked = true;
        }

        // Guarded on `status = 'running'` for the reason the CLI path sets out: a `/cancel` that
        // already wrote its status can still be followed by one last wake-up here, and an unguarded
        // write would report a completed turn for one that was killed.
        let written = match outcome {
            // `timed_out`, not `failed`, matching the CLI path below. A wall-clock kill is a
            // distinct ending there and anything filtering runs by it would simply not see this
            // turn — the status is what the rest of the system reads, so it has to mean the same
            // thing whichever runner produced it.
            Err(_) => {
                tracing::warn!(run_id = id, %answered_by, "turn exceeded the wall clock");
                sqlx::query(
                    "UPDATE runs SET status = 'timed_out', completed_at = ? WHERE id = ? AND status = 'running'",
                )
                .bind(&completed_at)
                .bind(id)
                .execute(&pool)
                .await
            }
            Ok(Ok(turn)) => {
                if turn.ending != crate::models::local_agent::Ending::Answered {
                    // Worth a log line and not worth an error: the person gets a usable sentence
                    // either way, and this is the only place recording WHY it was that sentence.
                    tracing::warn!(
                        run_id = id,
                        %answered_by,
                        ending = ?turn.ending,
                        tool_calls = turn.tool_calls,
                        "turn ended without an answer of its own"
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
            // Transport failure: Ollama stopped, OpenRouter refused the request, or the model was
            // pulled out from under us. The chat is told rather than handed a silence it cannot
            // interpret.
            Ok(Err(error)) => {
                tracing::warn!(run_id = id, %answered_by, %error, "turn failed");
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
        crate::execution::runs::warn_on_terminal_write_err(&written, id, "completed");
    });

    Ok(id)
}

/// What tools a conversation's turn may reach.
///
/// Four conditions, and every one of them is load-bearing. Written as one pure function so the
/// rule has a single home and can be read whole; the caller supplies the filesystem answer.
///
/// **A directory.** A conversation continuing a session from the IDE is rooted somewhere and is
/// about the code there. Every conversation that predates this has no root, which is why nothing
/// changes for any of them: they take the first `None` arm and stay exactly as they were.
///
/// **The machine.** `Origin::Shell` means the person is sitting at this computer. Telegram stays on
/// `McpOnly` because the policy — not the MCP allowlist, which only grants — is the one thing that
/// keeps a message arriving over the network away from this machine's filesystem and shell.
///
/// `Origin::Voice` joins `Shell` here EXPLICITLY, and the explicitness is the point: letting it fall
/// through to `_` would be the same question answered with a weaker policy for having been spoken,
/// with nothing anywhere saying so. It belongs on this arm because the fact the arm turns on is
/// physical presence, and a microphone is a stricter proof of it than a keyboard — a spoken turn
/// requires being in the room, while a typed one only requires reaching the machine.
///
/// The argument against, which is real and loses: a transcript is a GUESS at what was said, and
/// whisper mishears. But the guess is shown to the person as the turn is sent, the answer is spoken
/// back to them, and the `PreToolUse` classifier this arm defers to is still the thing deciding. What
/// would not survive scrutiny is the alternative — trusting a message that crossed the network more
/// than one spoken into the machine's own microphone.
///
/// **A wired hook.** `Unrestricted` is not "ungoverned": it hands the decision to the `PreToolUse`
/// classifier. But that hook is COOPERATIVE — it runs only if the `.claude/settings.json` resolved
/// from the run's working directory registers it — so in a directory that never onboarded,
/// `Unrestricted` would mean a shell with nothing watching it. The transcripts on this machine span
/// sixty-nine directories and most were never NucleOS projects at all, so this is the common case
/// and not the edge one.
///
/// **A turn of this conversation's own, or the first clean hop out of one.** A relayed turn is one
/// another conversation handed over, and it arrives carrying that conversation's `origin`. The
/// owner chose (2026-10-11, the relay design's §13) to let it keep the tools, so a conversation can
/// delegate real work to another — but only as `Handover::FirstHop`: one hop from a turn a person
/// wrote, sent by a conversation that read no third-party text. Every other relayed turn is
/// `Handover::Withheld`, because past one hop the words were written by a turn nobody typed, and
/// after an untrusted read they may be somebody else's words altogether. The third brake, a spend
/// ceiling per chain, refuses the relay itself (`relay::chain_has_room`).
///
/// Every failing arm falls to `McpOnly`, which is what every chat turn has always used: the
/// conversation still continues and still resumes its session, and what it loses is the ability to
/// touch the machine.
pub fn tool_policy_for(
    cwd: Option<&str>,
    origin: Origin,
    hook_is_wired: bool,
    handover: crate::relay::Handover,
) -> crate::execution::runner::ToolPolicy {
    use crate::relay::Handover;
    match (cwd, origin, hook_is_wired, handover) {
        (
            Some(_),
            Origin::Shell | Origin::Voice | Origin::BrowserPanel,
            true,
            Handover::Typed | Handover::FirstHop,
        ) => crate::execution::runner::ToolPolicy::Unrestricted,
        _ => crate::execution::runner::ToolPolicy::McpOnly,
    }
}

/// Chooses the CLI that should answer a conversation turn.
pub fn answering_cli(config: &crate::config::ModelsConfig, pinned: Option<&str>) -> &'static str {
    pinned
        .and_then(|id| {
            config
                .runner_of(id)
                .or_else(|| crate::models::model_catalog::runner_by_id(id))
        })
        .unwrap_or_else(|| {
            if config.active_runner() == "codex" {
                "codex"
            } else {
                "claude"
            }
        })
}

/// Chooses a runner for a turn without replacing the daemon's default unnecessarily.
pub fn runner_for_turn(
    daemon: &std::sync::Arc<dyn crate::execution::runner::CommandRunner>,
    assistants: &dyn crate::assistants::Assistants,
    config: &crate::config::ModelsConfig,
    pinned: Option<&str>,
) -> (
    std::sync::Arc<dyn crate::execution::runner::CommandRunner>,
    &'static str,
) {
    let cli = answering_cli(config, pinned);
    if pinned.is_none() || cli == config.active_runner() {
        (daemon.clone(), cli)
    } else {
        (
            assistants
                .cli_runner(cli, pinned)
                .unwrap_or_else(|| daemon.clone()),
            cli,
        )
    }
}

/// Whether this turn may retain a live CLI process for a later turn.
pub fn may_keep_process(policy: crate::execution::runner::ToolPolicy, cli: &str) -> bool {
    matches!(policy, crate::execution::runner::ToolPolicy::Unrestricted) && cli == "claude"
}

/// How long a chat turn may run: `silence` is how long without a new stream event, `ceiling` the
/// total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TurnDeadlines {
    pub(super) silence: Option<std::time::Duration>,
    pub(super) ceiling: std::time::Duration,
}

/// A rooted turn (`Unrestricted`) dies on silence or a long ceiling; any other keeps the plain wall.
pub(super) fn turn_deadlines(
    run_timeout: std::time::Duration,
    progress_timeout: std::time::Duration,
    policy: crate::execution::runner::ToolPolicy,
) -> TurnDeadlines {
    match policy {
        crate::execution::runner::ToolPolicy::Unrestricted => TurnDeadlines {
            silence: Some(progress_timeout * crate::state::ROOTED_CHAT_PROGRESS_TIMEOUT_MULTIPLIER),
            ceiling: run_timeout * crate::state::ROOTED_CHAT_RUN_TIMEOUT_MULTIPLIER,
        },
        _ => TurnDeadlines {
            silence: None,
            ceiling: run_timeout,
        },
    }
}

pub(super) fn system_prompt_with_knowledge(
    cli: &str,
    instructions: Option<String>,
    block: Option<&str>,
) -> Option<String> {
    if cli != "claude" {
        return instructions;
    }
    let Some(block) = block else {
        return instructions;
    };
    match instructions {
        Some(instructions) => Some(format!("{instructions}{block}")),
        None => Some(block.trim_start().to_owned()),
    }
}

/// The request a conversation's CLI is launched with, built by the turn that asks for it and by the
/// process warmed ahead of that turn, so the two cannot come to disagree about how a conversation is
/// spawned (a kept process is compared against [`spawn_fingerprint`] of exactly these inputs).
#[allow(clippy::too_many_arguments)]
pub(super) fn chat_run_request(
    prompt: String,
    images: Vec<crate::execution::runner::Attachment>,
    env: Vec<(String, String)>,
    cwd: Option<std::path::PathBuf>,
    permission: crate::execution::runner::Permission,
    resume: Option<String>,
    session_id: String,
    mcp_config: std::path::PathBuf,
    tool_policy: crate::execution::runner::ToolPolicy,
    answering: crate::chats::Answering,
    doctrine: Option<String>,
    cli: &str,
    knowledge: Option<&str>,
) -> crate::execution::runner::RunRequest {
    // Read before `images` is moved into the request: this decides which door the run goes through.
    let steerable = !images.is_empty();
    crate::execution::runner::RunRequest {
        prompt,
        env,
        // Set for every rooted conversation, elevated or not: this is how the CLI finds
        // the session to resume in the first place. A conversation with no root keeps
        // `None`, because one quietly given a working directory is one whose relative paths
        // moved.
        cwd,
        permission,
        resume_session_id: resume,
        mcp_config: Some(mcp_config),
        mcp_job: None,
        mcp_team_run: None,
        // Decided by `tool_policy_for`, which is where the rule is written out. The
        // default remains what it always was — the orchestrator talks to NucleOS and to
        // nothing else, and the MCP allowlist does not enforce that on its own, because
        // an allowlist only grants.
        tool_policy,
        // Stays `None` in the request: `serve_turn` decides the silence rule per door (the
        // runner's own deadline one-shot, a per-event timeout on a living process).
        progress_timeout: None,
        // No ceiling, and the only production `None`. A chat turn is
        // watched by the person who asked for it, who can stop it — and a turn cut off
        // mid-answer by a limit nobody set reads as the app breaking rather than as a
        // brake working. The guard is the turn's deadlines: silence plus a 4 h ceiling for a rooted
        // turn, the plain 600 s wall for an McpOnly one.
        max_turns: None,
        // Always set. `cli_args` reads this only when there is no `--resume`, which is
        // exactly the first turn — the one that used to be launched with no session id
        // at all.
        session_id: Some(session_id),
        fork_session: false,
        // Asked for so the tail above carries the answer AS IT IS WRITTEN rather than a
        // paragraph at a time. It costs nothing when nobody is watching: these are more
        // events on a stream the daemon already reads line by line, and `extract_reply`
        // takes the reply from the `result` event either way.
        include_partial_messages: true,
        // An orchestrator turn is one message answered and closed; the next one arrives
        // as its own turn on the resumed session, which is where a Telegram reply
        // already goes. Nothing here needs a stdin, so it keeps a closed one.
        images,
        // Bytes only fit on stdin: an argument vector holds a string and there is
        // nowhere in it for a picture to go. So a turn carrying one takes the other
        // door — which is the same one-turn run either way, because `messages` is None
        // and stdin closes the moment the opening line is written.
        steerable,

        // An orchestrator turn is answered by a person watching a chat, so the CLI's
        // own permission surface is the right one: there IS somebody to approve. It is
        // `McpOnly` besides, so the surface being argued over is nearly empty.
        classifier_governs_tools: false,
        messages: None,
        // The owner opts in per conversation (decision 2026-10-05), and only a turn that
        // is not `McpOnly` can use it: `McpOnly` pushes the strict flag unconditionally,
        // so an unrooted, Telegram or withheld relayed turn never gets the ambient servers.
        ambient_mcp: ambient_mcp_for(tool_policy, answering.ambient_mcp),
        // What the conversation was pinned to, or `None` for the runner's configured model.
        //
        // This was `None` unconditionally, with a comment saying an orchestrator turn has no
        // role to route — true of JOB roles, and it read as though the field had no other use.
        // It had: `cli_args` turns it into `--model`, so the one kind of run a person actually
        // watches was the one kind that could not choose who answered it.
        model: answering.model,
        // `None` for every conversation that never asked, which leaves the CLI's own default in
        // place — the behaviour every chat had before the column existed.
        effort: answering.effort,
        // Who answers when the chosen model is overloaded. Empty for every conversation that
        // named nobody, which is the CLI's own behaviour: fail rather than quietly substitute.
        fallback_model: answering.fallback_model,
        // Beyond `cwd`, which is set just above and is where the turn actually runs. These only
        // ever GRANT reach, so a conversation that named none is exactly as confined as before.
        add_dirs: answering
            .extra_dirs
            .iter()
            .map(std::path::PathBuf::from)
            .collect(),
        // A ceiling on THIS answer, not on the conversation. The CLI stops the invocation; the
        // turn's deadlines around this call are still the other guard, and `max_turns` above is
        // deliberately `None` here for the reason its own comment gives.
        max_budget_usd: answering.turn_budget_usd,
        // The helpers this conversation defined, ADDED to whatever the CLI finds in the
        // project's own `.claude/agents/`. A conversation that defined none sends no flag, and
        // that is not the same as sending an empty one — see `agents` on `RunRequest`.
        agents: answering.agents.clone(),
        // On EVERY turn, because the flag is per invocation and this daemon spawns one per
        // turn. Instructions sent only on the first would govern the opening message and then
        // quietly stop mattering — wrong in the way that is hardest to see, since the first
        // answer is the right one.
        //
        // `.or(doctrine)`, not `.or_else`: the chat's own `system_prompt` — read moments ago
        // from `chats::answering`, where `origin` no longer exists — always wins when it is
        // there, and the configured Telegram doctrine only fills the slot when it is empty. A
        // person's own instructions are never replaced, only completed.
        append_system_prompt: system_prompt_with_knowledge(
            cli,
            answering.system_prompt.clone().or(doctrine),
            knowledge,
        ),
        // What this conversation is called, so the session it mints is findable in the CLI's
        // own `--resume` picker instead of being one more nameless timestamp there.
        session_name: answering.session_name.clone(),
        context_window: Some(crate::assistant::window_of(answering.context_window)),
        // Merged with whatever `tool_policy` denies by `cli_args`, into one flag. This can only
        // ever narrow: the allow-listing flag beside it grants rather than restricts, so there
        // is no widening version of this to get wrong.
        denied_tools: answering.denied_tools.clone(),
        // The wildcard, on purpose: an orchestrator turn acts for the person watching
        // the chat and carries the control token, so narrowing what it is offered would
        // only take away tools it is entitled to call.
        allowed_mcp_tools: None,
        // A person is watching, so a background task is worth launching even on the one-shot
        // path where it dies with the turn. The live path (`start_live_chat`) keeps its process
        // between turns and would have kept them anyway.
        background_tasks: true,
    }
}

/// Everything about how a conversation's CLI is spawned that a kept process cannot be told to change.
///
/// A process takes its model, effort, tool denials, directories and instructions as arguments once,
/// when it starts, so a conversation that changed any of them must get a new process rather than a
/// wrong one. [`serve_turn`] compares this against what the process was spawned with
/// (`LiveChat::spawned_with`).
///
/// Computed from the conversation's own settings and NEVER from the per-turn briefing: that block
/// differs on every turn, and if it entered this string every turn would respawn the process it
/// exists to keep.
pub(super) fn spawn_fingerprint(
    answering: &crate::chats::Answering,
    doctrine: Option<&str>,
    tool_policy: crate::execution::runner::ToolPolicy,
    ambient_mcp: bool,
) -> String {
    serde_json::json!({
        "model": answering.model,
        "effort": answering.effort,
        "fallback_model": answering.fallback_model,
        "extra_dirs": answering.extra_dirs,
        "turn_budget_usd": answering.turn_budget_usd,
        "agents": answering.agents,
        // The chat's own instructions win; the doctrine only fills the slot when it is empty, which
        // is exactly the rule the request applies.
        "system_prompt": answering.system_prompt.as_deref().or(doctrine),
        "denied_tools": answering.denied_tools,
        "context_window": answering.context_window,
        "tool_policy": format!("{tool_policy:?}"),
        "ambient_mcp": ambient_mcp,
    })
    .to_string()
}

/// Whether a turn should read the briefing: only a Claude turn that no kept process will serve. The
/// block travels as a per-spawn system flag, so a process already standing would discard it.
pub(super) fn briefing_wanted(cli: &str, kept: bool) -> bool {
    cli == "claude" && !kept
}

/// Whether opening a chat should start its process ahead of the first message.
///
/// Only for a rooted conversation that may keep a process, has a session to resume (so the process
/// starts where the conversation is), is idle and has no process yet, is not already being warmed
/// (one at a time), and leaves room under the cap.
#[allow(clippy::too_many_arguments)]
pub(super) fn should_prewarm(
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
const DECLINED_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// Chats [`warm_chat`] recently declined, and when.
static DECLINED: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
> = std::sync::LazyLock::new(Default::default);

/// Whether `chat_id` was declined less than [`DECLINED_TTL`] before `now`.
fn declined_within(
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
fn note_declined_in(
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
fn take_unbriefed(chat_id: &str) -> bool {
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

    // The chat's slot, held while the process is built, so no turn starts beside it. Armed to send
    // what waited when it is released: a message queued meanwhile must not wait for ever.
    let Some(mut slot) = ChatSlot::acquire(chat_id) else {
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
    if write_mcp_config(&mcp_path, &build_mcp_config(&exe.to_string_lossy())).is_err() {
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

    let fingerprint = spawn_fingerprint(
        &answering,
        None,
        tool_policy,
        ambient_mcp_for(tool_policy, answering.ambient_mcp),
    );
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
    let live = spawn_live_process(&runner, request, session_tx, chat_id, &fingerprint);
    keep_live(chat_id, live, LIVE_IDLE);
    // Marked while the slot is still held, so no turn can come between the process and its mark.
    mark_unbriefed(chat_id);
}

/// Everything one orchestrator turn is launched with.
///
/// A struct rather than a row of parameters, for the reason `RunRequest` gives about its own: at
/// this width, `resume` and `session_id` sit side by side and are both string-shaped, so swapping
/// them is one careless edit that the compiler would let through without a word — and the symptom
/// would be a conversation quietly resuming the id it was about to mint.
pub(super) struct TurnLaunch {
    pub(super) id: i64,
    pub(super) slot: ChatSlot,
    pub(super) text: String,
    /// The pictures this turn carries. Empty for almost every turn.
    ///
    /// Carried here rather than fetched later because they decide which door the run goes through:
    /// bytes only fit on stdin, so a turn with any of these is `steerable` and one without is not.
    pub(super) images: Vec<crate::execution::runner::Attachment>,
    /// The session this turn continues, or `None` to start on a fresh context.
    pub(super) resume: Option<String>,
    /// The id this turn is recorded under, which is `resume` when there is one.
    pub(super) session_id: String,
    pub(super) mcp_path: std::path::PathBuf,
    pub(super) cwd: Option<std::path::PathBuf>,
    pub(super) tool_policy: crate::execution::runner::ToolPolicy,
    /// The configured Telegram doctrine, already resolved against `origin` in `send_message` —
    /// `Some` only for an `Origin::Telegram` turn whose operator configured one, `None` otherwise.
    ///
    /// Carried on the struct rather than re-derived where it is used, because `origin` is gone by
    /// then: this is read inside the task `spawn_assistant_turn` spawns, where `chats::answering`
    /// is read and the doctrine fills its `system_prompt` only when that is empty — a person's own
    /// instructions must never be replaced, only completed.
    pub(super) doctrine: Option<String>,
}

pub(super) fn spawn_assistant_turn(state: &crate::state::AppState, launch: TurnLaunch) {
    let TurnLaunch {
        id,
        slot,
        text,
        images,
        resume,
        session_id,
        mcp_path,
        cwd,
        tool_policy,
        doctrine,
    } = launch;
    let pool = state.pool.clone();
    let daemon_runner = state.runner.clone();
    let assistants = state.assistants.clone();
    let deadlines = turn_deadlines(state.run_timeout, state.progress_timeout, tool_policy);
    let control_token = state.token.0.clone();
    // Built HERE, outside the task, and captured by the async block. A task aborted before its first
    // poll drops its captured state without ever running a line of the body, so a guard constructed
    // inside would simply never exist — and a `/cancel` racing a fresh message hits exactly that.
    let turn = TurnGuard { slot, mcp_path };
    // Kept for after the turn: the between-turns reader needs the whole state, and
    // `spawn_registered` takes ownership of it. The chat id is copied for the same reason — it lives
    // on the guard, which is dropped before the reader starts.
    let after = state.clone();
    let drained_chat = turn.slot.chat_id.clone();

    // The turn's stream, mirrored as the CLI writes it and published under the turn's own id — a
    // turn IS a run, so `GET /runs/{id}/tail` already serves this and needed nothing new.
    //
    // Published BEFORE the task is spawned, not inside it. `send_message` answers with this id and
    // the window starts asking immediately; registering from inside the task would leave a window
    // in which the tail does not exist yet, and "no live tail" is the same answer the endpoint
    // gives for a run that finished — so the first poll of every turn would read as already over.
    //
    // Taken out by `Registration`'s `Drop`, which `spawn_registered` builds, so completion, failure,
    // timeout, cancel and panic all remove it without a line here.
    let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    state
        .run_tails
        .lock()
        .unwrap()
        .insert(id, std::sync::Arc::clone(&transcript));

    crate::execution::runs::spawn_registered(state, id, async move {
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

        // Read here rather than carried in from the request that started the turn: it is a property
        // of the conversation at the moment it answers, and somebody who moved the selector while
        // reading the last reply means this turn.
        //
        // `unwrap_or(Auto)` and not a refusal, for the reason the fallback exists at all: `Auto` is
        // what a rooted conversation has always done, so a row that could not be read behaves the
        // way it behaved before there was a column. It is the safe direction only downward, which
        // is why the SNAPSHOT this turn writes cannot use it — see the run's own column.
        let mode = crate::chats::permission_mode_of(&pool, &turn.slot.chat_id)
            .await
            .unwrap_or(crate::chats::PermissionMode::Auto);
        let permission = crate::execution::runner::Permission::for_chat(mode);

        // The turn's SNAPSHOT, written before anything can observe it.
        //
        // The read above is a property of the conversation at this instant. The hook reads the same
        // fact again, once per tool call, minutes later — so it has to be written somewhere a tool
        // call can find it, and it cannot be the chat's row: moving the selector while a turn is
        // running would change the rules underneath a turn already running. That is the whole
        // reason `runs.permission_mode` exists, and what makes it a snapshot rather than a second
        // copy free to disagree — written once, here, and never read by the selector.
        //
        // **The ORDER gives the invariant for free.** This write, then the `RunRequest`, then the
        // runner. The CLI process does not exist until this row says what it is running under, so
        // no tool call of a cloud turn can observe a NULL.
        //
        // **A failed write refuses the turn.** It does not fall back to `auto`: somebody who chose
        // `manual` would silently be handed something WIDER than they asked for, and "unknown reads
        // as auto" is only safe downward. `fresh_chat_key` fails in the same direction and
        // for the same reason — of the two ways to be wrong here, only one leaves a run acting with
        // nobody watching.
        if let Err(error) = sqlx::query("UPDATE runs SET permission_mode = ? WHERE id = ?")
            .bind(mode.as_str())
            .bind(id)
            .execute(&pool)
            .await
        {
            tracing::warn!(
                run_id = id,
                chat_id = %turn.slot.chat_id,
                %error,
                "could not record this turn's permission mode — refusing the turn rather than running it wider than it was asked for"
            );
            let failed = sqlx::query(
                "UPDATE runs SET status = 'failed', stderr = ?, completed_at = ? WHERE id = ? AND status = 'running'",
            )
            .bind(format!(
                "could not record this turn's permission mode, so it was refused rather than run under a wider one: {error}"
            ))
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(id)
            .execute(&pool)
            .await;
            crate::execution::runs::warn_on_terminal_write_err(&failed, id, "failed");
            return;
        }

        // Who answers this turn, and how hard they are asked to think. Read HERE, beside `planning`
        // and for its reason: both are properties of the conversation at the moment it answers, and
        // somebody who changed the model while reading the last reply meant this turn and not the
        // next one.
        //
        // `unwrap_or_default` — `(None, None)` — rather than a refusal. A conversation that never
        // expressed a preference is the overwhelming majority of them and is not an error, and a
        // row that could not be read has a configured model to fall back on. Failing a turn over an
        // unreadable preference would break the chats that have none.
        let answering = crate::chats::answering(&pool, &turn.slot.chat_id)
            .await
            .unwrap_or_default();
        let config = crate::config::models_config_now();
        let (runner, cli) = runner_for_turn(
            &daemon_runner,
            assistants.as_ref(),
            &config,
            answering.model.as_deref(),
        );

        // Which door this turn goes through. Decided once and named, because the two are not
        // interchangeable and the reason is a security one before it is a speed one.
        //
        // A rooted turn carries a key scoped to its CONVERSATION, minted for the process that
        // carries it (below), which stays true as the turns change under it. An `McpOnly` turn carries the daemon's control token —
        // safe only because that policy leaves it no Bash, no Read and no Write to look at its own
        // environment with — and a process holding THAT key, kept alive and idle between turns, is
        // a different and much worse proposition. So only rooted Claude conversations keep a
        // process, and the barrier that makes it safe is the same one that earned it the tools. A
        // Codex turn has no stdin a later turn can arrive on.
        let may_live = may_keep_process(tool_policy, cli);

        let env = chat_key::turn_env(
            tool_policy,
            may_live,
            &pool,
            &turn.slot.chat_id,
            id,
            &control_token,
        )
        .await;

        // A turn with no `resume` is a conversation that was deliberately let go of — it read
        // third-party text, or somebody asked for a fresh context — so a process still holding the
        // old session has to go rather than be spoken to: answering down it would continue exactly
        // the conversation that was just ended.
        //
        // Pictures used to be here too, because `messages` carried a bare `String` and a later turn
        // was written with no attachments. `runner::LaterTurn` carries its own, so a screenshot
        // pasted into the second turn no longer costs the conversation its process.
        if resume.is_none() {
            evict_live(&turn.slot.chat_id);
        }

        // Section 6 gives conversations machine knowledge only. Claude receives it through the
        // per-invocation system flag so resumed history does not accumulate copies. Codex rejects
        // that flag, so it receives no block. A conversation is not an outcome, so D15 adds no
        // briefing trace here.
        // A turn a kept process will serve never reads the briefing: the block travels as a
        // per-spawn system flag, which a kept process ignores. Decided HERE, after the eviction
        // above, so a conversation let go of is never taken for one the process will answer.
        let ambient_mcp = ambient_mcp_for(tool_policy, answering.ambient_mcp);
        let fingerprint =
            spawn_fingerprint(&answering, doctrine.as_deref(), tool_policy, ambient_mcp);
        let kept = may_live
            && kept_process_fits(
                &turn.slot.chat_id,
                &fingerprint,
                cwd.as_deref(),
                permission,
                resume.as_deref(),
            );
        // A process warmed ahead of this turn was spawned before there was a message to choose a
        // briefing against (see `UNBRIEFED`): its first turn still asks for one, and when there is
        // one the warmed process gives way to a process that carries it.
        let warmed_unbriefed = take_unbriefed(&turn.slot.chat_id);
        let knowledge = if briefing_wanted(cli, kept && !warmed_unbriefed) {
            crate::brief::for_prompt(
                &pool,
                &crate::knowledge::Context::for_project(None),
                &text,
                "assistant",
            )
            .await
            .and_then(|briefing| briefing.block)
        } else {
            None
        };
        if kept && knowledge.is_some() {
            evict_live(&turn.slot.chat_id);
        }

        let request = chat_run_request(
            text,
            images,
            env,
            cwd,
            permission,
            resume,
            session_id,
            turn.mcp_path.clone(),
            tool_policy,
            answering,
            doctrine,
            cli,
            knowledge.as_deref(),
        );

        // What this turn itself wrote into the model's prompt, priced off the request that is about
        // to become an argument vector, and priced HERE because `request` is moved into `serve_turn`
        // on the very next statement and nothing downstream sees these values again.
        //
        // This is the launcher the measurement exists for. `runs::spawn_run` records the same thing
        // and sets `mcp_config: None`, so every row it writes carries a schema cost of zero — a
        // real zero, and correct, but it means the largest term in the sum was measured against the
        // one launcher that never pays it. A chat turn carries `--mcp-config` on every single turn,
        // and the schema block it pays for is the biggest thing the daemon puts in front of the
        // model.
        //
        // Best effort by way of `record_authored_prompt`, which writes nothing on `None` and
        // swallows its own database error: this is bookkeeping about a turn somebody is waiting
        // for, and it may not be the reason that turn fails. The runner decides whether there is
        // anything to say at all — a chat answered by a fake or by the Codex CLI authors no prompt
        // this daemon can price, and answers `None`, which leaves the column NULL rather than
        // claiming a zero.
        crate::execution::runs::record_authored_prompt(&pool, id, runner.authored_prompt(&request))
            .await;

        let result = serve_turn(
            &runner,
            request,
            session_tx,
            // The published buffer, so what the window watches is what the CLI is writing.
            //
            // Still not the turn's PRODUCT: the reply is what `extract_reply` pulls out of the
            // `result` event of a completed run, and a turn the turn's deadlines killed has no reply to
            // salvage. This is the same distinction as before — the stream is transport, the result
            // is the answer — with the transport now visible while it moves.
            &transcript,
            &turn.slot.chat_id,
            deadlines,
            may_live,
            // A rooted turn that may keep a process is re-keyed by `serve_turn` when it spawns one.
            may_live.then_some(&pool),
            // What this conversation's process must have been spawned with to serve this turn.
            &fingerprint,
        )
        .await;
        // The process this turn kept (if any) learns what it has now served, before anything can
        // read it between turns.
        // Only a turn that could have run in the kept process: a one-shot turn never touched it, and
        // its mode is not one that process served.
        if may_live {
            note_served_by_kept(&turn.slot.chat_id, id, mode.as_str(), &pool);
        }
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
            Ok(Ok(o)) => match settled_reply(&o.stdout) {
                Some((status, reply)) => {
                    // Read out of the same stream the reply came from, and stored beside it. The
                    // live tail is taken away the instant this turn ends, so without this the
                    // actions are visible while the turn runs and gone for ever afterwards.
                    //
                    // Serialised here rather than kept as rows: it is read only with the turn it
                    // belongs to, and a table would be a join for something no query ever asks
                    // about on its own. An empty list is stored as `[]`, which says "acted on
                    // nothing" — NULL is reserved for turns nobody asked.
                    let live = crate::execution::runner::live_from_stream(&o.stdout);
                    let tools_used =
                        serde_json::to_string(&live.did).unwrap_or_else(|_| "[]".to_string());
                    // The same argument as the line above, about the other half of the stream. The
                    // reasoning is discarded with the live tail the instant the turn ends, so
                    // without a column it is visible only while the turn runs — and a conversation
                    // reopened tomorrow shows a conclusion with nothing behind it.
                    let thought =
                        serde_json::to_string(&live.thought).unwrap_or_else(|_| "[]".to_string());
                    // And the measurement, which unlike the words is actually there. See
                    // `0093_runs_thought_tokens.sql` for what the CLI does and does not send.
                    let thought_tokens = live.thought_tokens;
                    // Read out of the same stream, and stored here because this is where an
                    // assistant turn ends. `runs.rs` does the equivalent at its own terminal write,
                    // and a chat turn never passes through it — so the column stayed null on every
                    // conversation, and the window meter under each turn had nothing to draw.
                    //
                    // Cache-read tokens count as context because they occupy the window exactly as
                    // fresh input does. A resumed conversation is nearly all cache: reading only
                    // `input_tokens` would report a session at 96k as sitting at 9k.
                    let context_fill = o.stdout.lines().fold(None, |fill, line| {
                        crate::execution::runner::context_fill_from_line(line, fill)
                    });
                    // Taken off the outcome and not re-derived from the transcript beside it: on the
                    // multi-turn path the transcript belongs to a PROCESS that may have served several
                    // answers, and this is a fact about ONE of them. `RunOutcome` is where the
                    // splitter already put the right turn's copy.
                    let compacted = o.compacted;
                    // What the Chats window needs for its cache countdown and its model label,
                    // read off the same stream.
                    let cache_ttl = crate::execution::runner::cache_ttl_from_stream(&o.stdout);
                    let model = crate::execution::runner::model_from_stream(&o.stdout);
                    // The tokens and the turn count too, which every other terminal write in the
                    // core already takes off the outcome and this one did not: a chat turn read back
                    // "none recorded" under numbers the runner had measured and handed it.
                    // The rows must exist before the turn stops being `running`, or a live task's next call finds no authority.
                    crate::chats::tasks::record_turn(&pool, &turn.slot.chat_id, id, &o.stdout)
                        .await;
                    let completed = sqlx::query(
                        "UPDATE runs SET status = ?, exit_code = ?, stdout = ?, stderr = ?, session_id = COALESCE(?, session_id), cost_usd = ?, input_tokens = ?, output_tokens = ?, cache_read_tokens = ?, cache_creation_tokens = ?, num_turns = ?, tools_used = ?, thought = ?, thought_tokens = ?, context_fill = ?, compacted = ?, cache_ttl = ?, model = COALESCE(?, model), completed_at = ? WHERE id = ? AND status = 'running'",
                    )
                    .bind(status)
                    .bind(o.exit_code)
                    .bind(&reply)
                    .bind(&o.stderr)
                    .bind(&o.session_id)
                    .bind(o.cost_usd)
                    .bind(o.input_tokens)
                    .bind(o.output_tokens)
                    .bind(o.cache_read_tokens)
                    .bind(o.cache_creation_tokens)
                    .bind(o.num_turns)
                    .bind(&tools_used)
                    .bind(&thought)
                    .bind(thought_tokens)
                    .bind(context_fill)
                    .bind(compacted)
                    .bind(cache_ttl)
                    .bind(&model)
                    .bind(&completed_at)
                    .bind(id)
                    .execute(&pool)
                    .await;
                    crate::execution::runs::warn_on_terminal_write_err(&completed, id, status);
                    if let Some(session_id) = o.session_id.as_deref() {
                        // `get_session` would refuse to resume this session anyway, by looking at the
                        // runs that produced it. Dropping the row here as well closes the one case that
                        // check cannot see: a session recorded against a run that never took the id has
                        // nothing pointing at it, so nothing marks it as having read anything.
                        match crate::execution::runs::read_untrusted_context(&pool, id).await {
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
                //
                // The measurements are kept for the reason the cost is: a turn that answered nothing
                // still spent what it spent, and one stopped at its ceiling has read the most.
                None => {
                    // A silence kill is a timeout, as `runs.rs` records it.
                    let status =
                        if o.exit_code == crate::execution::runner::PROGRESS_TIMEOUT_EXIT_CODE {
                            "timed_out"
                        } else {
                            "failed"
                        };
                    // The rows must exist before the turn stops being `running`, or a live task's next call finds no authority.
                    crate::chats::tasks::record_turn(&pool, &turn.slot.chat_id, id, &o.stdout)
                        .await;
                    let failed = sqlx::query(
                        "UPDATE runs SET status = ?, exit_code = ?, stderr = ?, session_id = COALESCE(?, session_id), cost_usd = ?, input_tokens = ?, output_tokens = ?, cache_read_tokens = ?, cache_creation_tokens = ?, num_turns = ?, completed_at = ? WHERE id = ? AND status = 'running'",
                    )
                    .bind(status)
                    .bind(o.exit_code)
                    .bind(&o.stderr)
                    .bind(&o.session_id)
                    .bind(o.cost_usd)
                    .bind(o.input_tokens)
                    .bind(o.output_tokens)
                    .bind(o.cache_read_tokens)
                    .bind(o.cache_creation_tokens)
                    .bind(o.num_turns)
                    .bind(&completed_at)
                    .bind(id)
                    .execute(&pool)
                    .await;
                    crate::execution::runs::warn_on_terminal_write_err(&failed, id, status);
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
                crate::execution::runs::warn_on_terminal_write_err(&failed, id, "failed");
            }
            Err(_) => {
                let timed_out = sqlx::query(
                    "UPDATE runs SET status = 'timed_out', completed_at = ? WHERE id = ? AND status = 'running'",
                )
                .bind(&completed_at)
                .bind(id)
                .execute(&pool)
                .await;
                crate::execution::runs::warn_on_terminal_write_err(&timed_out, id, "timed_out");
            }
        }

        // Dropped HERE rather than at the end of the block: dropping the guard releases the chat's
        // slot, and a slot armed by `drain_on_release` sends what waited from a task of its own, so
        // the next turn never starts inside this one's registered task (whose handle would still be
        // in `run_handles` and whose end would cut it short). Draining is the slot's job, not this
        // task's.
        drop(turn);
        // The process this turn kept is read from here on, so a background task's own answer is not
        // left waiting for the next person's turn.
        watch_between_turns(&after, &drained_chat);
    });
}

/// How a turn that came back from the CLI is recorded: its status and what it answered.
///
/// A `result` text is a completed turn. Without one, a turn the person interrupted is `cancelled`
/// and keeps what it had already said (`None` when it had said nothing); anything else answered
/// nothing, and the caller records it as the failure it is.
pub(super) fn settled_reply(stdout: &str) -> Option<(&'static str, Option<String>)> {
    if let Some(reply) = extract_reply(stdout) {
        return Some(("completed", Some(reply)));
    }
    if crate::execution::runner::interrupted_by_user(stdout) {
        let partial = crate::execution::runner::live_from_stream(stdout).text;
        return Some((
            "cancelled",
            Some(partial).filter(|text| !text.trim().is_empty()),
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::runner::FakeCommandRunner;
    use crate::state::AppState;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    async fn test_state(runner: Arc<FakeCommandRunner>) -> AppState {
        AppState {
            token: crate::auth::Token("t".into()),
            pool: crate::testdb::fresh_pool().await,
            telegram_doctrine: None,
            runner,
            triage_runner: None,
            local_triage_disabled: None,
            assistants: Arc::new(crate::assistants::NoAssistants),
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_tails: Default::default(),
            files_root: None,
            files_trash: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: Arc::new(crate::secrets::InMemorySecrets::default()),
            email: Arc::new(crate::state::EmailRuntime::default()),
            voice: Arc::new(crate::integrations::voice::VoiceRuntime::default()),
            browser: Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: Arc::new(crate::integrations::github::GithubRuntime::default()),
            web: Arc::new(crate::integrations::web::WebRuntime::disabled()),
            quota: Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: Arc::new(crate::integrations::calendar::CalendarRuntime::default()),
            council: Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    /// A declined chat is not re-evaluated on every poll: it is left alone for the TTL, and only it.
    #[test]
    fn a_declined_prewarm_is_not_re_evaluated_inside_the_ttl() {
        let mut declined = std::collections::HashMap::new();
        let at = Instant::now();

        note_declined_in(&mut declined, "declined-chat", at);

        assert!(declined_within(
            &declined,
            "declined-chat",
            at + Duration::from_secs(10)
        ));
        assert!(
            !declined_within(&declined, "another-chat", at + Duration::from_secs(10)),
            "only the chat that was declined is left alone"
        );
        assert!(
            !declined_within(&declined, "declined-chat", at + DECLINED_TTL),
            "past the TTL the chat is evaluated again"
        );

        note_declined_in(&mut declined, "later-chat", at + DECLINED_TTL);
        assert!(
            !declined.contains_key("declined-chat"),
            "an expired entry is dropped rather than kept for ever"
        );
    }

    /// A process warmed before there was a message carries no briefing; when the first turn has one
    /// to give, that turn is answered by a process that carries it instead of losing it.
    #[tokio::test]
    async fn a_warmed_process_gives_way_when_the_first_turn_has_a_briefing() {
        let fake = Arc::new(FakeCommandRunner::default());
        let state = test_state(fake.clone()).await;
        let chat = "turn-warm-briefed";
        let root = tempfile::TempDir::new().unwrap();
        crate::governance::autopilot::wire_classifier_hook(root.path()).unwrap();
        sqlx::query(
            "INSERT INTO chats (chat_id, brain, created_at, cwd)
             VALUES (?, 'cloud', '2026-01-01T00:00:00Z', ?)",
        )
        .bind(chat)
        .bind(root.path().to_str().unwrap())
        .execute(&state.pool)
        .await
        .unwrap();
        upsert_session(&state.pool, chat, "sess-briefed", "2026-01-01T00:00:00Z")
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('semantic', 'machine', NULL, 'owner', 'memory',
                     'zanzibar house rule', 'body', 'active',
                     '2026-08-19T00:00:00+00:00')",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        prewarm_open_chat(&state, chat);
        let mut warmed = false;
        for _ in 0..500 {
            warmed = *fake.calls.lock().unwrap() == 1 && has_kept_process(chat);
            if warmed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(warmed, "opening the chat did not warm its process");
        let at_warm = fake.last_append_system_prompt.lock().unwrap().clone();
        assert!(
            !matches!(at_warm, Some(Some(ref prompt)) if prompt.contains("zanzibar house rule")),
            "a warmed process has no message to choose a briefing against: {at_warm:?}"
        );

        let id = send_message(&state, chat, "zanzibar?", Origin::Shell)
            .await
            .unwrap();
        for _ in 0..500 {
            let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            if status != "running" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(
            *fake.calls.lock().unwrap(),
            2,
            "the briefed first turn was spoken down the unbriefed warmed process"
        );
        let sent = fake.last_append_system_prompt.lock().unwrap().clone();
        assert!(
            matches!(sent, Some(Some(ref prompt)) if prompt.contains("zanzibar house rule")),
            "the first turn after a warm-up lost its briefing: {sent:?}"
        );
        evict_live(chat);
    }
}
