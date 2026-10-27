//! Reading a conversation and the state around it (`/assistant/chats*`).

use super::*;

mod builtins;
mod builtins_config;
mod command;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/assistant/chats", get(list_chats).post(create_chat))
        .route(
            "/assistant/chats/{chat_id}",
            get(get_assistant_chat)
                .patch(patch_chat)
                .delete(delete_chat),
        )
        // The names an `@` in the composer completes against. A segment deeper than the chat
        // itself, and rooted at that chat's own directory rather than at anything the caller sends.
        .route("/assistant/chats/{chat_id}/files", get(get_chat_files))
        // And the commands a `/` completes against. Beside `/files` because it is the same gesture
        // at the same place, answered from a different part of the same directory.
        .route(
            "/assistant/chats/{chat_id}/commands",
            get(get_chat_commands),
        )
        // Running one: expanded here, so every engine receives the same prompt.
        .route(
            "/assistant/chats/{chat_id}/command",
            post(command::post_chat_command),
        )
        // What is different in this conversation's project. On demand and never polled, for the
        // reason the projects' own inspect readers give: re-asking it on a timer would walk
        // somebody's working tree in the background forever.
        .route("/assistant/chats/{chat_id}/diff", get(get_chat_diff))
        // Where a conversation runs, and whether that gives it tools. Its own read because it is a
        // filesystem question: answering it on the list would be a stat per conversation per poll,
        // for rows nobody is looking at.
        .route("/assistant/chats/{chat_id}/project", get(read_chat_project))
        // The work this conversation's turns launched, including what outlived them. Control/Admin
        // like every chat route: no `auth.rs` entry.
        .route("/assistant/chats/{chat_id}/tasks", get(get_chat_tasks))
        // Stop agent: one subagent or background shell, by the CLI's task id. See `assistant::stop_task`.
        .route(
            "/assistant/chats/{chat_id}/tasks/{task_id}/stop",
            post(post_stop_task),
        )
        // The act that turns a conversation with a directory into one with tools. The same thing
        // `wire_ide_session_tools` does before a pick-up, reached from the other side.
        .route("/assistant/chats/{chat_id}/tools", post(wire_chat_tools))
        // Copies the AI workflow into a conversation's worktree; only ever on an explicit confirm.
        .route(
            "/assistant/chats/{chat_id}/workflow",
            post(seed_chat_workflow),
        )
        // Taking back something that has not been sent. A segment deeper than the chat, and named
        // for the thing it removes rather than for the chat it removes it from.
        .route(
            "/assistant/chats/{chat_id}/queue/{queued_id}",
            delete(delete_queued),
        )
        .route("/assistant/chats/{chat_id}/seen", post(post_chat_seen))
        .route(
            "/assistant/chats/{chat_id}/restore",
            post(post_chat_restore),
        )
        // The two context gestures. Separate routes rather than one with a flag, because they are
        // separate decisions and a caller that got the flag backwards would silently throw away a
        // conversation's memory.
        .route(
            "/assistant/chats/{chat_id}/fresh-context",
            post(post_fresh_context),
        )
        .route("/assistant/chats/{chat_id}/clear", post(post_clear_context))
}

/// One exchange in a chat, as it is read back.
///
/// The reply is the run's stdout and the failure its stderr, which is what the shell was already
/// pulling out of the run detail — carried here so reopening a conversation does not mean one
/// request per turn.
#[derive(serde::Serialize, sqlx::FromRow)]
pub(super) struct AssistantTurn {
    pub(super) id: i64,
    pub(super) asked: String,
    pub(super) answer: Option<String>,
    pub(super) error: Option<String>,
    pub(super) status: String,
    pub(super) cost_usd: Option<f64>,
    /// Which model answered. Null on turns from before the column existed, and the window must keep
    /// that distinction: it is what stops a "changed model here" mark being drawn against a turn
    /// nothing knows the model of.
    pub(super) answered_by: Option<String>,
    /// The CLI session this turn ran in.
    ///
    /// Travels to the window so it can say where the conversation RESTARTED. `get_session` refuses
    /// to resume a session that read third-party text, and `POST /fresh-context` drops one on
    /// request; the next turn then mints a fresh id — so a change here is the moment the model
    /// stopped remembering what came before it. Without it, that happens and the transcript above
    /// and below looks like one unbroken conversation, which is the one thing it is not.
    ///
    /// It used to change on SIZE as well, which made this the ordinary fate of any long
    /// conversation. It is now the rare one: a full context is compacted, and `compacted` below is
    /// what says so.
    pub(super) session_id: Option<String>,
    pub(super) created_at: String,
    /// When the turn ended, in `created_at`'s format, or null while it runs. The Chats window
    /// counts the prompt cache down from here.
    pub(super) completed_at: Option<String>,
    /// The model the CLI reported answering with, or null when the stream never said and on
    /// every turn from before it was recorded. Distinct from `answered_by`, which says WHO.
    pub(super) model: Option<String>,
    /// How long this turn's prompt cache lives: `"5m"`, `"1h"`, or null when unknown.
    pub(super) cache_ttl: Option<String>,
    /// How much context this turn ran with, as an absolute token count. Null on a turn whose stream
    /// never reported one -- a turn that failed before the CLI said anything, and every turn from
    /// before the column existed.
    pub(super) context_fill: Option<i64>,
    /// What the turn ran, as the JSON `tools_used` holds. Not serialized: the window is given the
    /// parsed list below, so the shape of the column is this daemon's business and not a format
    /// two codebases have to agree on.
    #[serde(skip)]
    pub(super) tools_used: Option<String>,
    /// What the turn thought, as the JSON `thought` holds. Not serialized, for the reason above it.
    #[serde(skip)]
    pub(super) thought: Option<String>,
    /// Where the turn's pictures were kept, as the JSON `prompt_images` holds. Not serialized, for
    /// the reason above it.
    #[serde(skip)]
    pub(super) prompt_images: Option<String>,
    /// Roughly how many tokens the turn spent thinking, or null when it did not think and on every
    /// turn from before the column. Serialized as it stands: it is a number, not a private shape.
    pub(super) thought_tokens: Option<i64>,
    /// Whether the CLI summarised its own context while producing this turn.
    ///
    /// This is what replaced the restart mark for the ordinary case. A conversation no longer
    /// changes session when it fills up — it is compacted in place — so the fact the window has
    /// to draw moved from `session_id` to here, and it says a milder and truer thing: the older
    /// exchanges were summarised at this point, not forgotten at this point.
    pub(super) compacted: bool,
    /// The conversation that handed this turn its words, or null for the ordinary case — somebody
    /// typed them here.
    ///
    /// Without it the window draws a relayed turn under "you", which is the one thing it is not:
    /// the words arrived from another conversation, and a transcript that attributes them to the
    /// person reading it is not a cosmetic problem but a false record of who said what.
    ///
    /// Two columns and not one, because the pair answers two different questions and either can be
    /// absent on its own. The id is what the conversation IS and is always there when a turn was
    /// relayed at all; the title is what somebody called it and is null on every conversation
    /// nobody has named — which is most of them, since a title is written by the daemon only once
    /// a chat has been summarised. A window given only the title could not say anything about an
    /// unnamed conversation; given only the id it would print a uuid at a person.
    pub(super) relayed_from_chat_id: Option<String>,
    /// What that conversation is called, or null when it has no title yet. See above.
    pub(super) relayed_from_title: Option<String>,
    /// Who started this turn: `task` when the assistant woke on its own because a background task
    /// ended, null for everything a person or another conversation said.
    ///
    /// Without it the window draws the daemon's own `[background task … completed]` note under
    /// "you", which is the same false attribution a relayed turn would suffer.
    pub(super) origin: Option<String>,
}

/// One turn as the window receives it: the row, plus what the turn did.
///
/// The parse happens here rather than in the window for the reason it happens in `runner.rs` at
/// all: the column holds a serialisation this daemon chose, and a client re-deriving it would be a
/// second reader of a private shape. A column that will not parse reads as an empty list — the turn
/// is real and its reply is worth showing, and one unreadable field is not worth losing it over.
/// One relay a turn sent: where it went, and what it said.
///
/// Read off `chat_relays` rather than off the tool call the CLI reported, and the difference is not
/// cosmetic. The relay row is what the daemon ADMITTED and wrote; a tool call is what the model
/// asked for, and the two part company every time `relay::admit` refuses — a cycle, a chain too
/// deep, an owner who has gone away. A sender's transcript built from the asks would show messages
/// that never arrived, which is the one thing it must not do.
#[derive(serde::Serialize, sqlx::FromRow)]
pub(super) struct RelaySent {
    /// The conversation the message went to. Always present; the title beside it may not be.
    pub(super) chat_id: String,
    /// What that conversation is called, or null when nobody has named it yet.
    pub(super) title: Option<String>,
    /// The words that travelled, whole. The same string the destination's own turn was created
    /// with, so the two sides of one relay cannot disagree about what was said.
    pub(super) body: String,
    /// Which turn sent it. Not serialized — the window receives these already grouped under their
    /// turn, and repeating the key inside every value is a second copy of the same fact.
    #[serde(skip)]
    pub(super) sending_run_id: i64,
}

#[derive(serde::Serialize)]
pub(super) struct AssistantTurnOut {
    #[serde(flatten)]
    pub(super) turn: AssistantTurn,
    pub(super) did: Vec<crate::execution::runner::ToolCall>,
    /// The pictures this turn was sent with, as paths under the files root.
    ///
    /// Paths and not bytes, all the way to the window: it asks the files route for each one, which
    /// means a transcript of forty turns costs forty short strings rather than forty screenshots.
    pub(super) images: Vec<String>,
    /// What the turn thought before it answered, oldest first.
    ///
    /// Empty both for a turn that thought nothing and for a turn from before the column. The two
    /// are different facts and the row keeps them apart, but a window cannot act on the difference:
    /// either way there is nothing to draw.
    pub(super) thought: Vec<String>,
    /// The context window this conversation runs in, in tokens.
    ///
    /// It used to be `context_rotates_at`: the count past which the daemon stopped resuming and
    /// minted a fresh session. Nothing rotates now — the CLI compacts inside the session — so the
    /// number means the window rather than the cliff, and the name had to move with it.
    ///
    /// The same on every row of one conversation and NOT the same across conversations, which is
    /// why it still rides here rather than being a constant the window keeps its own copy of: a
    /// chat picked up from the editor runs in a wider window than the default, and a meter drawn
    /// against the default would be wrong for exactly the conversations nearest their limit.
    pub(super) context_window: i64,
    /// The relays this turn sent, oldest first. Empty for almost every turn.
    ///
    /// The mirror of `relayed_from_chat_id` on the receiving side, and it exists because that side
    /// shipped alone: the conversation certain to be watched by the person who caused a relay was
    /// the one that could not say what it had done.
    pub(super) relayed_to: Vec<RelaySent>,
    /// What was said INTO this turn while it ran ("Send now"), oldest first. Empty for almost every
    /// turn. The CLI folds such a line into the running turn, so the stream never shows it as
    /// typed and this is the only record.
    pub(super) said_now: Vec<crate::assistant::SaidDuring>,
}

/// A conversation as it is read back: its turns, and whatever it was handed before the first one.
///
/// An object rather than the bare array this used to be, because a transcript is not only its
/// turns. A chat picked up from a session too large to resume begins with the verbatim tail of that
/// session in front of it, the model answers from that tail — and nothing in the window said so.
/// You asked, it replied knowing a past it never lived, and the reason sat in a column.
///
/// `handoff.rs` states the rule this serves: context pressure must leave an auditable record rather
/// than quietly erase how work continued. A compaction that is stored and never shown is still an
/// erasure from where the person is standing.
#[derive(serde::Serialize)]
pub(super) struct TranscriptOut {
    /// The exchanges this conversation was handed, oldest first. Empty for an ordinary chat, which
    /// is most of them, and empty rather than absent so a reader never has to branch on missing.
    pub(super) handed: Vec<(String, String)>,
    pub(super) turns: Vec<AssistantTurnOut>,
    /// What was said to this conversation while it was busy, and has not been sent yet, each with
    /// the name it can be taken back by.
    ///
    /// Here rather than on its own route because it belongs to the same picture and moves on the
    /// same poll: the window draws it under the last turn, where the answer will land.
    pub(super) queued: Vec<crate::chats::Waiting>,
    /// What this conversation is waiting to be allowed to do, which is nearly always nothing.
    ///
    /// Here for the reason `queued` is, and one more: this is polled at a turn's own cadence while a
    /// turn is live, which is exactly when a question can appear — a route of its own would need a
    /// second poll at the same speed to say "nothing" almost every time.
    pub(super) asks: Vec<crate::hooks::Ask>,
    /// Whether there are turns older than the oldest one in `turns`.
    ///
    /// A conversation is read from its recent end and cut at `ASSISTANT_TRANSCRIPT_LIMIT`, and
    /// until this existed the cut was silent: the hundred-and-first turn simply was not there, and
    /// nothing on the page distinguished a conversation that started where you were looking from
    /// one whose first afternoon had been dropped off the top. This is what lets the window offer
    /// to go and get the rest.
    pub(super) more: bool,
    /// What departments said in this conversation, oldest first. Empty for almost every chat.
    ///
    /// A list of its own beside `turns` rather than folded into them, because a notice is not a
    /// turn: nothing was asked, nothing ran, nothing was spent, and there is no answer. Giving it a
    /// turn's shape would mean inventing a prompt nobody typed. The window interleaves the two by
    /// `created_at`, which is the only place the two orders have to meet.
    pub(super) notices: Vec<crate::chats::notices::ChatNotice>,
}

/// Which slice of a conversation to read back.
#[derive(serde::Deserialize)]
pub(super) struct TranscriptQuery {
    /// Read the turns immediately BEFORE this one, rather than the most recent.
    ///
    /// Absent is the ordinary case and means the recent end. A page walks backwards by handing
    /// back the id of the oldest turn it has, which is stable in a way an offset is not: turns are
    /// only ever appended, so an offset from the end shifts under a conversation that answers
    /// while somebody is reading it and a page boundary would repeat or skip a turn.
    pub(super) before: Option<i64>,
    /// Read only the turns AFTER this one — the poll's incremental read.
    ///
    /// A page that already holds every settled turn up to this id asks only for what lies past it,
    /// rather than having the newest hundred turns, answers and thoughts included, re-sent every
    /// second and a half. `more` keeps its meaning: there were more rows in the window than the
    /// limit returned, which on this read tells the page its watermark is too far behind to catch
    /// up incrementally and a full read is the answer.
    pub(super) after: Option<i64>,
}

/// How many turns of a conversation are read back. A chat is read from its recent end.
pub(super) const ASSISTANT_TRANSCRIPT_LIMIT: i64 = 100;

/// The names a conversation offers, and whether it had anywhere to look at all.
///
/// `rooted` is the distinction an empty list cannot draw: "nothing here matches what you typed" and
/// "this conversation has no directory" look identical to a caller and are entirely different
/// facts. Only the second is worth a sentence in the window.
#[derive(serde::Serialize)]
pub(super) struct MentionsOut {
    pub(super) rooted: bool,
    #[serde(flatten)]
    pub(super) found: crate::mentions::Found,
}

/// What a conversation offers for a `/`.
///
/// No `rooted` here, unlike the file route beside it, and the difference is real: a conversation
/// with no directory still has the person's own commands and every installed plugin's. Nowhere to
/// look is a thing that can only be true of files.
#[derive(serde::Serialize)]
pub(super) struct CommandsOut {
    pub(super) commands: Vec<crate::commands::Command>,
}

/// The slash commands this conversation can run.
///
/// Three sources, read on the request rather than cached: a command is a file somebody just wrote,
/// and a picker that needed a daemon restart to notice it would be a picker people stop trusting.
/// Measured on this machine at about 7ms for the whole sweep, which is a keystroke's worth.
pub(super) async fn get_chat_commands(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    Query(query): Query<MentionQuery>,
) -> Result<Json<CommandsOut>, StatusCode> {
    let cwd = crate::chats::opened_in(&state.pool, &chat_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "reading where a conversation runs failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let typed: String = query.q.chars().take(MENTION_QUERY_LIMIT).collect();
    // Off the async runtime, as the file walk is: this reads several directories and every command
    // file's front matter, which is short but is still disk on a request thread.
    let commands = tokio::task::spawn_blocking(move || {
        // The registry's listing and not `available`: it adds the built-ins and drops the
        // entries a higher-precedence name shadows, so the picker offers only what would run.
        let listed = crate::assistant::slash::registry::listing(
            cwd.as_deref().map(std::path::Path::new),
            crate::commands::home().as_deref(),
        );
        let mut found = crate::commands::matching(&listed, &typed);
        // Built-ins lead whatever was typed; the sort is stable, so the match order holds inside
        // each group.
        found.sort_by_key(|command| command.source != crate::commands::Source::Builtin);
        found
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(CommandsOut { commands }))
}

/// What is different in a conversation's project, as `git diff` writes it.
///
/// The question a person has after a coding turn is "what changed", and answering it used to mean
/// leaving the app: the transcript names the tool and the file and stops there.
///
/// **Not what the turn did, and never labelled as such.** The daemon takes no snapshot before a
/// turn, so this is what is different NOW — the same thing after one turn, and not after three. The
/// honest claim is the one this makes.
///
/// `inspect::diff` and not a `git diff` of its own: that one already turns off every setting a
/// target repository could use to run a command string as the daemon user, and carries the deadline
/// and output ceiling this needs for exactly the same reasons.
pub(super) async fn get_chat_diff(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
) -> Result<String, StatusCode> {
    let cwd = crate::chats::opened_in(&state.pool, &chat_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "reading a conversation's project failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?
        // 409 and not an empty answer. An empty diff is a claim — "nothing has changed" — and a
        // conversation with no project is not in a position to make it.
        .ok_or(StatusCode::CONFLICT)?;

    let root = std::path::PathBuf::from(cwd);
    tokio::task::spawn_blocking(move || inspect::diff(&root))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        // A directory that is not a repository has nothing to say, which is an empty answer rather
        // than a refusal: the conversation is fine, its project simply is not under git.
        .or_else(|error| match error {
            inspect::InspectError::Io(_) => Ok(String::new()),
            other => Err(inspect_status(other)),
        })
}

/// `GET /assistant/chats/{chat_id}/tasks`. The subagents and background tasks this conversation's
/// turns launched, oldest first; 404 for a conversation that does not exist.
pub(super) async fn get_chat_tasks(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
) -> Result<Json<Vec<crate::chats::tasks::ChatTask>>, StatusCode> {
    crate::chats::opened_in(&state.pool, &chat_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "reading a conversation failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;
    crate::chats::tasks::for_chat(&state.pool, &chat_id)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "reading a conversation's tasks failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// Stops one agent of a running conversation. 404 when no turn of it is running to ask.
pub(super) async fn post_stop_task(
    Path((chat_id, task_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // The CLI's ids are short and plain; anything else is not one of them, and is not written into
    // the process's stdin.
    if task_id.is_empty()
        || task_id.len() > 128
        || !task_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(refusal(StatusCode::BAD_REQUEST, "task_id"));
    }
    match crate::assistant::stop_task(&chat_id, &task_id) {
        crate::assistant::StoppedTask::Asked => Ok(Json(serde_json::json!({ "stopped": "asked" }))),
        crate::assistant::StoppedTask::NotRunning => {
            Err(refusal(StatusCode::NOT_FOUND, "not_running"))
        }
    }
}

/// Where a conversation runs, and whether that gives its turns tools.
#[derive(serde::Serialize)]
pub(super) struct ChatProjectOut {
    /// The project this conversation is about, or `null` for one that has none.
    pub(super) cwd: Option<String>,
    /// The session a terminal standing in `cwd` could carry this conversation on in, or `null`.
    ///
    /// `claude --resume <this>` from that directory continues it — measured, with a word said only
    /// to the daemon coming back out of the CLI. The way back was always there and nothing said so.
    ///
    /// `null` when the daemon itself would not resume it: a rotated conversation, or one that has
    /// read a stranger's text. Offering an id the daemon has refused would be sending somebody
    /// somewhere it will not go.
    pub(super) session: Option<String>,
    /// What this conversation may do without being asked.
    ///
    /// Beside the tools and not beside the title, because it is the same question in the other
    /// direction: one says what this conversation CAN do, the other says what it will choose not to.
    ///
    /// The remembered position of the selector — what the NEXT message runs under. Never the
    /// snapshot on the run, which is a different fact with a different lifetime: that one says what
    /// a turn already in flight started with, and nothing in the window reads it.
    pub(super) permission_mode: crate::chats::PermissionMode,
    /// Whether a turn here would actually get Bash, Read and Write.
    ///
    /// Not the same fact as having a directory, which is why both travel. `tool_policy_for` wants
    /// the classifier hook wired in that directory too — every fresh worktree lacks it, since
    /// `.claude/` is not committed — so a conversation pointed at one still cannot open a file, and
    /// a window that reported only the directory would be telling the truth and misleading at once.
    pub(super) tools: bool,
    /// Whether this conversation's turns use the user's ambient MCP servers. Off until somebody
    /// opts in.
    pub(super) ambient_mcp: bool,
    /// Whether `cwd` is a linked worktree of a project that pins a workflow which was never
    /// materialized into it — the case "Set up workflow" answers.
    pub(super) workflow_missing: bool,
}

/// Where a conversation runs, and whether that gives it tools.
pub(super) async fn read_chat_project(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
) -> Result<Json<ChatProjectOut>, StatusCode> {
    // `opened_in` and not `cwd_of`, because the two absences are different answers: a chat that is
    // not there is a 404, and one with no directory is a conversation this route has something to
    // say about.
    let opened_in = crate::chats::opened_in(&state.pool, &chat_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "reading a conversation's project failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Read for both answers below, because a conversation with no directory can still be carried
    // on somewhere — the terminal just has to be standing where it was had.
    let session = crate::assistant::get_session(&state.pool, &chat_id)
        .await
        .unwrap_or(None);
    let permission_mode = crate::chats::permission_mode_of(&state.pool, &chat_id)
        .await
        .unwrap_or(crate::chats::PermissionMode::Auto);
    let ambient_mcp = crate::chats::ambient_mcp_of(&state.pool, &chat_id)
        .await
        .unwrap_or(false);

    let Some(cwd) = opened_in else {
        return Ok(Json(ChatProjectOut {
            cwd: None,
            session,
            permission_mode,
            tools: false,
            ambient_mcp,
            workflow_missing: false,
        }));
    };

    // An unreadable roster offers nothing: the setup writes files, so it fails closed.
    let roster = crate::governance::autopilot::rostered_projects(&state.pool)
        .await
        .unwrap_or_default();
    let dir = std::path::PathBuf::from(&cwd);
    let machine_root = state.machine_config_root.clone();
    let (wired, workflow_missing) = tokio::task::spawn_blocking(move || {
        (
            crate::governance::autopilot::classifier_hook_is_wired(&dir),
            crate::governance::autopilot::workflow_missing(&dir, &roster, machine_root.as_deref()),
        )
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Asked of the same function the turn path asks, rather than restated here. A second copy of
    // this rule would be a second thing to keep true, and the one that answers the window is the
    // one that must agree with the one that launches the run.
    // `Typed`: this answers what the person's OWN next message would get, which is the only thing
    // the window can offer them. A relayed turn's policy is not a property of the conversation and
    // is not previewable here — it is decided per turn, by the turn that carries a relay id.
    let tools = crate::assistant::tool_policy_for(
        Some(cwd.as_str()),
        crate::assistant::Origin::Shell,
        wired,
        crate::relay::Handover::Typed,
    ) == crate::execution::runner::ToolPolicy::Unrestricted;

    Ok(Json(ChatProjectOut {
        cwd: Some(cwd),
        session,
        permission_mode,
        tools,
        ambient_mcp,
        workflow_missing,
    }))
}

/// Wires the classifier hook in a conversation's project, which is what gives its turns tools.
pub(super) async fn wire_chat_tools(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let cwd = crate::chats::opened_in(&state.pool, &chat_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "reading a conversation's project failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?
        // Nothing to wire and nothing to guess. A silent success here would be this route reporting
        // "done" about a directory nobody has named.
        .ok_or(StatusCode::CONFLICT)?;

    let dir = std::path::PathBuf::from(&cwd);
    tokio::task::spawn_blocking(move || crate::governance::autopilot::wire_classifier_hook(&dir))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|error| {
            tracing::warn!(%error, cwd = %cwd, "could not wire a conversation's project");
            StatusCode::CONFLICT
        })?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
pub(super) struct SeedWorkflowIn {
    #[serde(default)]
    pub(super) confirm: bool,
}

/// Materializes the project's pinned workflows into a conversation's worktree, then wires the
/// hook. Writes into the worktree, so it runs only on an explicit `confirm: true`.
pub(super) async fn seed_chat_workflow(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    Json(body): Json<SeedWorkflowIn>,
) -> Result<StatusCode, StatusCode> {
    // Checked first: nothing below runs on anything but an explicit yes.
    if !body.confirm {
        return Err(StatusCode::BAD_REQUEST);
    }
    let cwd = crate::chats::opened_in(&state.pool, &chat_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "reading a conversation's project failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?
        .ok_or(StatusCode::CONFLICT)?;

    let roster = crate::governance::autopilot::rostered_projects(&state.pool)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "reading the project roster failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    match crate::governance::autopilot::seed_workflow(
        std::path::Path::new(&cwd),
        &roster,
        state.machine_config_root.as_deref(),
        state.workflow_library.as_deref(),
    )
    .await
    {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(
            crate::governance::autopilot::SeedRefusal::NotAWorktree
            | crate::governance::autopilot::SeedRefusal::NoPinnedWorkflow
            | crate::governance::autopilot::SeedRefusal::MainNotRostered,
        ) => Err(StatusCode::CONFLICT),
        // Same answer `wire_chat_tools` gives when the hook cannot be written.
        Err(crate::governance::autopilot::SeedRefusal::NotWired(error)) => {
            tracing::warn!(%error, cwd = %cwd, "could not wire a conversation's project after seeding it");
            Err(StatusCode::CONFLICT)
        }
        Err(crate::governance::autopilot::SeedRefusal::Failed(error)) => {
            tracing::warn!(%error, cwd = %cwd, "could not set up the workflow in a conversation's worktree");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Takes a message back off a conversation's queue before it is sent.
///
/// 404 when there was nothing to take, which covers both of the ways that happens: a message the
/// drain sent a moment ago, and one that belongs to a different conversation. They are the same
/// answer on purpose — a caller learning which of the two it was would be learning something about
/// somebody else's queue.
pub(super) async fn delete_queued(
    State(state): State<AppState>,
    Path((chat_id, queued_id)): Path<(String, i64)>,
) -> Result<StatusCode, StatusCode> {
    match crate::chats::drop_queued(&state.pool, &chat_id, queued_id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(%error, "taking a message off a queue failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Names under this conversation's own working directory, for completing an `@'`.
///
/// The root comes from the chat's row and never from the caller. A route that took a directory
/// would be a route that reads any directory — and there is nothing to gain by it: the only
/// defensible root is where this conversation's turns already run, because the model can open those
/// files anyway and naming them discloses nothing it could not read.
///
/// A directory that is gone reads as `rooted: false` rather than as an empty search. It is the same
/// fact as having none: there is nowhere to look, and saying "no matches" would send somebody
/// hunting for a spelling mistake.
pub(super) async fn get_chat_files(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    Query(query): Query<MentionQuery>,
) -> Result<Json<MentionsOut>, StatusCode> {
    let cwd = crate::chats::opened_in(&state.pool, &chat_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "reading where a conversation runs failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let Some(cwd) = cwd else {
        return Ok(Json(MentionsOut {
            rooted: false,
            found: crate::mentions::Found {
                hits: Vec::new(),
                truncated: false,
            },
        }));
    };

    let typed: String = query.q.chars().take(MENTION_QUERY_LIMIT).collect();
    // Off the async runtime: this is a directory walk on a keystroke, and holding a runtime thread
    // for it would stall every other request sharing that thread.
    let answered = tokio::task::spawn_blocking(move || {
        let root = std::path::Path::new(&cwd);
        root.is_dir()
            .then(|| crate::mentions::matching(root, &typed))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(match answered {
        Some(found) => MentionsOut {
            rooted: true,
            found,
        },
        None => MentionsOut {
            rooted: false,
            found: crate::mentions::Found {
                hits: Vec::new(),
                truncated: false,
            },
        },
    }))
}

/// Every relay sent by any of `turns`, in one read.
///
/// An `IN` list built from the turn ids rather than a query per turn: this runs on every transcript
/// poll, which is every second and a half while a turn is live.
///
/// Empty turns answer empty without touching the database — an `IN ()` is not valid SQLite, and
/// building the statement anyway to discover that would be a round trip to learn something already
/// known.
pub(super) async fn relays_sent_by(
    pool: &sqlx::SqlitePool,
    turns: &[AssistantTurn],
) -> sqlx::Result<Vec<RelaySent>> {
    if turns.is_empty() {
        return Ok(Vec::new());
    }
    // `QueryBuilder` rather than a formatted string, which is the shape this codebase already uses
    // for runtime assembly (`feed.rs`, `notes.rs`). `push_bind` writes the placeholder AND binds the
    // value in one step, so the count of `?` and the count of arguments cannot drift apart — the one
    // mistake a hand-built `IN` list invites.
    let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT rel.to_chat_id AS chat_id, dst.title AS title, rel.body AS body,
                rel.sending_run_id AS sending_run_id
           FROM chat_relays rel
           LEFT JOIN chats dst ON dst.chat_id = rel.to_chat_id
          WHERE rel.sending_run_id IN (",
    );
    // LEFT JOIN above for the reason the transcript's own join gives: a destination nobody has
    // named has no title and must still resolve, and one archived since still has its row.
    let mut separated = query.separated(", ");
    for turn in turns {
        separated.push_bind(turn.id);
    }
    query.push(") ORDER BY rel.id");
    query.build_query_as::<RelaySent>().fetch_all(pool).await
}

/// A chat's turns, oldest first.
///
/// The daemon has always kept these — a turn is a run — but nothing on the row said which chat it
/// belonged to, so the shell's transcript could only live in the window that made it and died with
/// a reload. Reconstructing it from `/runs?mode=assistant` was never an option: that is every
/// chat's turns at once, the Telegram sidecar's included.
///
/// Ordered by id rather than by `created_at`: two turns of the same conversation can share a
/// timestamp to the second, and a transcript that reorders itself is one you lose your place in.
/// The limit takes the LAST turns and then puts them back in order, so a long conversation opens on
/// its recent end rather than on its beginning.
pub(super) async fn get_assistant_chat(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    Query(query): Query<TranscriptQuery>,
    headers: axum::http::HeaderMap,
) -> Result<Json<TranscriptOut>, StatusCode> {
    // 404 for an id nothing ever used: a `200` with an empty transcript reads as a conversation
    // that exists and is quiet. A `chats` row, an assistant run OR a queued message is enough, so
    // an archived chat (its row stays), a Telegram chat (it never had one) and a chat whose only
    // trace is a message still waiting all stay readable.
    let known: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM chats WHERE chat_id = ?)
             OR EXISTS (SELECT 1 FROM runs WHERE chat_id = ? AND mode = 'assistant')
             OR EXISTS (SELECT 1 FROM chat_queue WHERE chat_id = ?)",
    )
    .bind(&chat_id)
    .bind(&chat_id)
    .bind(&chat_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|error| {
        tracing::warn!(%error, "looking up an assistant chat failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    if !known {
        return Err(StatusCode::NOT_FOUND);
    }
    // Only the visible transcript poll sends this header (`useChatTranscript`); voice reads and
    // `?before=` pages do not, so a read that is not somebody looking at the chat never keeps its
    // process alive.
    if headers.get("x-nucleos-chat-open").is_some_and(|v| v == "1") {
        crate::assistant::mark_open(&chat_id);
        // Warm the opened chat's process so the first turn does not pay the spawn.
        crate::assistant::prewarm_open_chat(&state, &chat_id);
    }
    // `?` on a missing bound reads as no bound: `i64::MAX` is above every id this table will ever
    // hold, so one query serves both the recent end and a page above it. Two queries differing by
    // a single clause is how the two come to disagree about ordering.
    let before = query.before.unwrap_or(i64::MAX);
    // Absent reads as 0, below every id, for the same reason `before` reads as `i64::MAX`.
    let after = query.after.unwrap_or(0);
    // One more than asked for, and it is never returned. Whether there is anything above this page
    // is a question about the row after the last one, and asking for it here answers it for the
    // price of a row rather than with a second COUNT over the same index.
    //
    // Two LEFT JOINs, and left rather than inner for the obvious reason and a less obvious one:
    // almost every turn has no relay behind it, and an inner join would return only the relayed
    // ones — but also, a relay row whose source conversation has since been ARCHIVED still has a
    // `chats` row and still resolves, which is what keeps a transcript readable after the far side
    // of a conversation is put away. `src.title` being null is an unnamed conversation, not a
    // missing one; the id beside it always resolves.
    let mut turns = sqlx::query_as::<_, AssistantTurn>(
        "SELECT r.id, r.prompt AS asked, r.stdout AS answer, r.stderr AS error, r.status,
                r.cost_usd, r.answered_by, r.session_id, r.created_at, r.completed_at,
                r.model, r.cache_ttl, r.context_fill,
                r.tools_used, r.thought, r.thought_tokens, r.prompt_images, r.compacted,
                rel.from_chat_id AS relayed_from_chat_id,
                src.title        AS relayed_from_title,
                r.origin
           FROM runs r
           LEFT JOIN chat_relays rel ON rel.id = r.from_relay_id
           LEFT JOIN chats src ON src.chat_id = rel.from_chat_id
          WHERE r.chat_id = ? AND r.mode = 'assistant' AND r.id < ? AND r.id > ?
          ORDER BY r.id DESC
          LIMIT ?",
    )
    .bind(&chat_id)
    .bind(before)
    .bind(after)
    .bind(ASSISTANT_TRANSCRIPT_LIMIT + 1)
    .fetch_all(&state.pool)
    .await
    .map_err(|error| {
        tracing::warn!(%error, "reading an assistant chat failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let more = turns.len() as i64 > ASSISTANT_TRANSCRIPT_LIMIT;
    turns.truncate(ASSISTANT_TRANSCRIPT_LIMIT as usize);
    turns.reverse();
    // One query for the whole transcript, not one per turn. This route is polled every second and a
    // half while a turn is live, so a per-turn read here would be a hidden multiplier on the most
    // frequent request this daemon serves — for a list that is empty on almost every turn.
    //
    // Empty on a failure rather than a 500, the same trade the queue read below makes: a transcript
    // is the point of this request, and losing the note about what a turn forwarded is a smaller
    // loss than a conversation that will not open.
    let sent = relays_sent_by(&state.pool, &turns)
        .await
        .unwrap_or_default();
    // Empty on a failure, for the same trade: losing the note about what was said mid-turn is a
    // smaller loss than a conversation that will not open.
    //
    // Bounded to the page: only what was said into a turn at or above the page's lowest one can
    // belong to a turn this response carries, and this route is polled while a turn is live.
    let said = match turns.first() {
        Some(oldest) => sqlx::query_as::<_, crate::assistant::SaidDuring>(
            "SELECT run_id, text, created_at FROM chat_said_now
              WHERE chat_id = ? AND run_id >= ? ORDER BY id",
        )
        .bind(&chat_id)
        .bind(oldest.id)
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default(),
        None => Vec::new(),
    };
    // The notices from the page's own time onward, or all of them when the page is the whole
    // history — a notice older than the oldest turn shown has nothing on the page to sit beside.
    let notices = match turns.first() {
        Some(oldest) if more => {
            crate::chats::notices::for_chat_since(&state.pool, &chat_id, &oldest.created_at).await
        }
        _ => crate::chats::notices::for_chat(&state.pool, &chat_id).await,
    }
    .unwrap_or_default();
    // Read through `assistant::handed_over` rather than parsed here: that function is already the
    // one reader of the column's shape, and a second one is a second thing to change the day the
    // shape does. A chat that was handed nothing comes back empty, which is the honest answer for
    // every ordinary conversation.
    let handed = crate::assistant::handed_over(&state.pool, &chat_id).await;
    // Read once for the whole transcript rather than per turn: it is a property of the
    // conversation, and forty rows asking the same question of the same row is thirty-nine
    // round trips nobody needs. A failure reads as the default, which is what every conversation
    // that never touched this column runs in anyway.
    let context_window = crate::assistant::window_of(
        crate::chats::answering(&state.pool, &chat_id)
            .await
            .map(|answering| answering.context_window)
            .unwrap_or_default(),
    );
    // Empty on a failure rather than a 500: the transcript is the point of this request, and a
    // conversation nobody can read because its queue would not load is a worse answer than one
    // drawn without a note about what is waiting.
    let queued = crate::chats::queued(&state.pool, &chat_id)
        .await
        .unwrap_or_default();
    Ok(Json(TranscriptOut {
        handed,
        queued,
        asks: crate::hooks::asks_for(&chat_id),
        more,
        // Empty on a failure rather than a 500, the same trade the two reads above make: a
        // transcript is the point of this request, and losing a department's aside is a smaller loss
        // than a conversation that will not open.
        notices,
        turns: turns
            .into_iter()
            .map(|turn| {
                // Without their answers. See `ToolCall::result`: this list rides a route that is
                // polled once a second while a turn is live, and the answers are fetched per turn
                // by `get_turn_tools` when somebody actually opens one.
                let did: Vec<crate::execution::runner::ToolCall> = turn
                    .tools_used
                    .as_deref()
                    .and_then(|json| {
                        serde_json::from_str::<Vec<crate::execution::runner::ToolCall>>(json).ok()
                    })
                    .unwrap_or_default()
                    .into_iter()
                    .map(crate::execution::runner::ToolCall::without_result)
                    .collect();
                let thought = turn
                    .thought
                    .as_deref()
                    .and_then(|json| serde_json::from_str(json).ok())
                    .unwrap_or_default();
                let images = turn
                    .prompt_images
                    .as_deref()
                    .and_then(|json| serde_json::from_str(json).ok())
                    .unwrap_or_default();
                let relayed_to = sent
                    .iter()
                    .filter(|relay| relay.sending_run_id == turn.id)
                    .map(|relay| RelaySent {
                        chat_id: relay.chat_id.clone(),
                        title: relay.title.clone(),
                        body: relay.body.clone(),
                        sending_run_id: relay.sending_run_id,
                    })
                    .collect();
                let said_now = said
                    .iter()
                    .filter(|said| said.run_id == turn.id)
                    .cloned()
                    .collect();
                AssistantTurnOut {
                    turn,
                    did,
                    images,
                    thought,
                    context_window,
                    relayed_to,
                    said_now,
                }
            })
            .collect(),
    }))
}

#[derive(serde::Deserialize)]
pub(super) struct ListChatsQuery {
    /// Archived conversations instead of the live list.
    #[serde(default)]
    pub(super) archived: bool,
}

/// The conversations the app opened, most recently active first.
///
/// The Telegram sidecar's chats are absent from this, and no line here says so. They are absent
/// because nothing ever created a row for them — the only door into `chats` is `create_chat` below.
/// A filter naming Telegram would have to be kept correct as clients are added; an absence needs no
/// maintenance.
pub(super) async fn list_chats(
    State(state): State<AppState>,
    Query(query): Query<ListChatsQuery>,
) -> Result<Json<Vec<crate::chats::ChatSummary>>, StatusCode> {
    let rows = if query.archived {
        crate::chats::list_archived(&state.pool).await
    } else {
        crate::chats::list(&state.pool).await
    };
    rows.map(|rows| {
        Json(
            rows.into_iter()
                .map(|row| {
                    let asks = crate::hooks::asks_for(&row.chat_id).len();
                    crate::chats::settle(row, asks)
                })
                .collect(),
        )
    })
    .map_err(|error| {
        tracing::warn!(%error, "listing chats failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// Starts this conversation's next turn on a fresh window, with a replay of what was recently said.
///
/// This app's `/clear` more than its `/compact`, and the distinction sharpened when the CLI took
/// over compaction: the CLI SUMMARISES a full context and carries on in the same session, which is
/// what a `/compact` means and what now happens on its own. This drops the session entirely — the
/// next turn starts clean and is handed a few hundred tokens of recent exchanges — which is what
/// somebody means when the conversation has gone somewhere they do not want it to follow.
///
/// It stays because that is a real thing to want and nothing else offers it. What it is no longer
/// is the escape hatch from an expensive context: a conversation that grows now compacts on its
/// own, so nobody has to throw one away to stop paying for it.
pub(super) async fn post_fresh_context(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    chat_must_exist(&state, &chat_id).await?;
    // A live turn writes its session back when it ends (`upsert_session`), so forgetting one now
    // would be undone in a minute by the turn that is still running — the request would report
    // success and change nothing. 409 is the same answer `POST /assistant/message` gives.
    if crate::assistant::is_busy(&chat_id) {
        return Err(StatusCode::CONFLICT);
    }

    crate::assistant::forget_session(&state.pool, &chat_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "starting a conversation on a fresh context failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// Starts the next turn clean and tells it nothing about what came before — this app's `/clear`.
///
/// The stronger of the two, and the difference is the whole point: `/fresh-context` moves the
/// conversation onto a new window and replays what was recently said; this moves the floor of that
/// replay to now, so there is nothing to replay. It also hides the tail an editor session was
/// picked up with, which is older than every turn here and would otherwise be the one thing a clear
/// did not clear.
///
/// The turns are not deleted. Every one of them stays readable in the window and stays in `runs`
/// costing what it cost — clearing decides what the MODEL is shown, not what happened. That is the
/// same position `handoff.rs` takes about rewriting history.
pub(super) async fn post_clear_context(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    chat_must_exist(&state, &chat_id).await?;
    // For the reason above, and one more: the floor is the id of the LAST run, and a turn in flight
    // already has a row. Clearing now would put the floor above a turn that has not answered yet,
    // so its answer would arrive into a conversation that had already forgotten the question.
    if crate::assistant::is_busy(&chat_id) {
        return Err(StatusCode::CONFLICT);
    }

    crate::chats::clear_context(&state.pool, &chat_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "clearing what a conversation is shown failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    // Both, and in this order. The cut decides what a fresh turn is TOLD; forgetting the session is
    // what makes the next turn fresh at all. Without the second, the next turn would resume the old
    // window and the cut would have changed nothing anybody could see.
    crate::assistant::forget_session(&state.pool, &chat_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "forgetting a cleared conversation's session failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// A 404 for a conversation this daemon does not have — never opened here, or archived.
///
/// Answered before anything is written, for the reason `patch_chat` gives at length: without it a
/// route reports success for an UPDATE that matched no row, which is the API saying "done" about
/// something it did not do.
pub(super) async fn chat_must_exist(state: &AppState, chat_id: &str) -> Result<(), StatusCode> {
    match crate::chats::get(&state.pool, chat_id).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// Records that this conversation has been read.
///
/// Its own route rather than a field on `PATCH`, and the reason is the 409 that one answers. A
/// model cannot move under a live turn, so `PATCH` refuses while a chat is busy — and reading a
/// conversation while it is mid-turn is the ordinary case: you sent the message and you are
/// watching it. Folded together, the answer you were looking straight at would come back marked
/// unread.
///
/// Takes no body. Where the watermark lands is `chats::mark_seen`'s to decide, because a client
/// naming its own could mark a turn it has not drawn yet — a list read that overtook the transcript
/// would silently swallow the very answer it was meant to announce.
pub(super) async fn post_chat_seen(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    // Answered before anything is written, for the reason `patch_chat` gives: `204` over an UPDATE
    // that matched no row is the API saying "done" about something it did not do.
    if crate::chats::get(&state.pool, &chat_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .is_none()
    {
        return Err(StatusCode::NOT_FOUND);
    }

    crate::chats::mark_seen(&state.pool, &chat_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|error| {
            tracing::warn!(%error, "marking a chat read failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// Puts an archived conversation back on the list: 204, or 404 when it was not archived.
pub(super) async fn post_chat_restore(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    match crate::chats::restore(&state.pool, &chat_id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(%error, "restoring a chat failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Takes a conversation off the list, and leaves every turn of it in place.
///
/// Archive rather than delete, because every turn is a billed run: removing the rows would hide
/// money spent from the table that records it. The transcript stays readable to anything that asks
/// for the chat by id.
pub(super) async fn delete_chat(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
) -> Result<axum::response::Response, StatusCode> {
    let failed = |error: sqlx::Error| {
        tracing::warn!(%error, "archiving a chat failed");
        StatusCode::INTERNAL_SERVER_ERROR
    };

    // `204` for an archive that matched nothing is the API saying "done" about something it did
    // not do.
    if crate::chats::get(&state.pool, &chat_id)
        .await
        .map_err(failed)?
        .is_none()
    {
        return Ok(refusal_with_detail(
            StatusCode::NOT_FOUND,
            "chat_not_found",
            "There is no such conversation, or it is already archived.".to_string(),
        )
        .into_response());
    }

    // Putting away a conversation that is answering would hide a turn that is still being paid
    // for, and its answer would land in a chat nobody can see.
    let answering: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM runs
                         WHERE chat_id = ? AND mode = 'assistant'
                           AND status IN ('running', 'pending'))",
    )
    .bind(&chat_id)
    .fetch_one(&state.pool)
    .await
    .map_err(failed)?;
    if answering || crate::assistant::is_busy(&chat_id) {
        return Ok(refusal_with_detail(
            StatusCode::CONFLICT,
            "chat_busy",
            "This conversation is answering right now. Stop it or wait for the answer, then archive it."
                .to_string(),
        )
        .into_response());
    }

    // Messages still waiting would be sent into a conversation nobody can see.
    if !crate::chats::queued(&state.pool, &chat_id)
        .await
        .map_err(failed)?
        .is_empty()
    {
        return Ok(refusal_with_detail(
            StatusCode::CONFLICT,
            "chat_queued",
            "This conversation has messages waiting to be sent. Remove them or let them go, then archive it."
                .to_string(),
        )
        .into_response());
    }

    crate::chats::archive(&state.pool, &chat_id)
        .await
        .map_err(failed)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
