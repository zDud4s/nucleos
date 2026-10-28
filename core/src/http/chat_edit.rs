//! Creating and editing a conversation and its groups (`POST /assistant/chats`, `PATCH /assistant/chats/{chat_id}`, `/assistant/chat-groups*`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/assistant/chats/{chat_id}/title", post(post_chat_title))
        .route(
            "/assistant/chat-groups",
            get(list_chat_groups).post(create_chat_group),
        )
        .route(
            "/assistant/chat-groups/{group_id}",
            axum::routing::patch(rename_chat_group).delete(delete_chat_group),
        )
        .route(
            "/assistant/chats/{chat_id}/group",
            axum::routing::put(put_chat_group),
        )
}

#[derive(serde::Deserialize)]
pub(super) struct CreateChatRequest {
    /// Absent means cloud, matching the column default and every caller written before this.
    pub(super) brain: Option<String>,
    /// The id of a session already had in the IDE, which this conversation continues.
    ///
    /// The ID and nothing else. The directory it runs in is looked up from the transcript, never
    /// accepted here — a caller that could name its own working directory could name one where the
    /// classifier hook is wired and collect the tools that come with it.
    pub(super) continue_session: Option<String>,
    /// Which model answers it, as a choice id from `GET /assistant/models`.
    ///
    /// Here as well as on the PATCH because the front door has no conversation to PATCH yet: a
    /// person picks a model, types, and sends, and without this the window would have to open a
    /// chat on the default and correct it a round trip later — visibly, and wrongly if the second
    /// call failed.
    ///
    /// A plain `Option` and not the PATCH's `Option<Option<_>>`: there is no conversation here to
    /// unpin, so absent and null both mean the same thing and the third state does not exist.
    pub(super) model: Option<String>,
    /// How hard it is asked to think, on the same footing.
    pub(super) effort: Option<String>,
    /// What the conversation may do without asking, from its first message.
    ///
    /// Here for the same reason `model` is -- the front door has nothing to PATCH yet -- and,
    /// unlike `model`, it is written INTO the row rather than applied a step later. A rung that
    /// failed to take would leave the conversation on `auto`, which is wider than what was asked
    /// for; see `chats::create_on`.
    ///
    /// Absent means `auto`, which is the column's own default and what every conversation opened
    /// before this field existed already holds. A spelling outside the five fails the request
    /// rather than falling back, on the PATCH's reasoning: the reader's leniency exists for rows
    /// written before the column did, and applying it to a request would let a typo widen what a
    /// conversation may do.
    pub(super) permission_mode: Option<crate::chats::PermissionMode>,
    /// Who answers when the chosen model is unavailable, as choice ids in the order to try them.
    #[serde(default)]
    pub(super) fallback_model: Vec<String>,
    /// Absolute paths this conversation's tools may also reach.
    #[serde(default)]
    pub(super) extra_dirs: Vec<String>,
    /// The most one turn may spend, in dollars.
    pub(super) turn_budget_usd: Option<f64>,
    /// The helpers it may hand work to, added to the ones the CLI finds in the project itself.
    #[serde(default)]
    pub(super) agents: Vec<crate::execution::runner::Subagent>,
    /// Standing instructions appended to its system prompt.
    pub(super) system_prompt: Option<String>,
    /// Built-in tools it may not reach for.
    #[serde(default)]
    pub(super) denied_tools: Vec<String>,
}

/// How many exchanges a conversation too large to resume is handed.
///
/// The same count `recent_exchanges` replays for a rotated conversation, and deliberately: this is
/// the rotation's mechanism reaching a conversation whose past happens to live in somebody else's
/// file rather than in our runs. A different number here would be a second policy about the same
/// question.
pub(super) const HANDOVER_EXCHANGES: usize = 6;

/// Opens a conversation, and answers with the id it was given.
///
/// The id is minted by the daemon rather than accepted from the body: `chat_id` reaches a filename
/// in `assistant.rs`'s temporary MCP config, and while that path encodes what it is handed, there is
/// no reason to open a second door for arbitrary strings when this one can simply not exist.
pub(super) async fn create_chat(
    State(state): State<AppState>,
    Json(body): Json<CreateChatRequest>,
) -> Result<Json<serde_json::Value>, ChatRefusal> {
    // A named model decides the route, and outranks any `brain` sent beside it — the same
    // precedence `patch_chat` applies, for the same reason: a row saying `local` while naming a
    // cloud model would be sent to Ollama under a name it has never heard.
    let brain = match body.model.as_deref() {
        Some(id) => chosen_brain(&state, id, Asking::Chat).await?,
        None => crate::chats::Brain::from_wire(body.brain.as_deref().unwrap_or("cloud")),
    };
    // Checked before the row exists, so a bad level leaves no conversation behind to explain. The
    // three below are checked here for the same reason and in the same breath.
    if let Some(level) = body.effort.as_deref()
        && !crate::config::is_effort_level(&models_config(), level)
    {
        return Err(StatusCode::BAD_REQUEST.into());
    }
    checked_fallback(&body.fallback_model)?;
    checked_dirs(&body.extra_dirs).await?;
    if let Some(amount) = body.turn_budget_usd {
        checked_budget(amount)?;
    }
    checked_agents(&body.agents)?;
    if let Some(text) = body.system_prompt.as_deref() {
        checked_instructions(text)?;
    }
    checked_denials(&body.denied_tools)?;

    // Resolved BEFORE the row is written, so a session the daemon cannot find leaves nothing behind
    // — rather than a conversation that looks continued and starts a fresh context on its first turn.
    let continued = match body.continue_session.clone() {
        None => None,
        Some(session_id) => {
            let root = crate::sessions::default_root().ok_or(StatusCode::NOT_FOUND)?;
            let found =
                tokio::task::spawn_blocking(move || crate::sessions::find(&root, &session_id))
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            Some(found.ok_or(StatusCode::NOT_FOUND)?)
        }
    };

    let chat_id = crate::chats::create_on(
        &state.pool,
        brain,
        continued.as_ref(),
        body.permission_mode
            .unwrap_or(crate::chats::PermissionMode::Auto),
    )
    .await
    .map_err(|error| {
        tracing::warn!(%error, "opening a chat failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // After the row exists rather than as arguments to `create`, which takes the facts a
    // conversation cannot be opened without. These two are preferences: a conversation with neither
    // is the ordinary case and has been since the table was made, and threading them through the
    // constructor would make every existing caller name a choice it does not have.
    //
    // A failure here is logged and not fatal. The conversation is open and usable on the configured
    // model; refusing the whole request would throw away a chat that exists, and answering 500
    // would tell the window nothing was created when something was.
    if body.model.is_some() || body.effort.is_some() {
        if let Err(error) =
            crate::chats::set_model(&state.pool, &chat_id, body.model.as_deref()).await
        {
            tracing::warn!(%error, chat_id = %chat_id, "the new conversation kept the configured model");
        }
        if let Err(error) =
            crate::chats::set_effort(&state.pool, &chat_id, body.effort.as_deref()).await
        {
            tracing::warn!(%error, chat_id = %chat_id, "the new conversation kept the default effort");
        }
    }
    // Each of these reads "the caller sent one, AND writing it failed" — the guard short-circuits,
    // so a caller who sent nothing still writes nothing, exactly as the nesting these replaced did.
    if !body.fallback_model.is_empty()
        && let Err(error) =
            crate::chats::set_fallback(&state.pool, &chat_id, &body.fallback_model).await
    {
        tracing::warn!(%error, chat_id = %chat_id, "the new conversation kept no fallback");
    }
    if !body.extra_dirs.is_empty()
        && let Err(error) =
            crate::chats::set_extra_dirs(&state.pool, &chat_id, &body.extra_dirs).await
    {
        tracing::warn!(%error, chat_id = %chat_id, "the new conversation reaches only its own directory");
    }
    if body.turn_budget_usd.is_some()
        && let Err(error) =
            crate::chats::set_turn_budget(&state.pool, &chat_id, body.turn_budget_usd).await
    {
        tracing::warn!(%error, chat_id = %chat_id, "the new conversation kept no ceiling");
    }
    if !body.agents.is_empty()
        && let Err(error) = crate::chats::set_agents(&state.pool, &chat_id, &body.agents).await
    {
        tracing::warn!(%error, chat_id = %chat_id, "the new conversation defined no helpers");
    }
    if body.system_prompt.is_some()
        && let Err(error) =
            crate::chats::set_system_prompt(&state.pool, &chat_id, body.system_prompt.as_deref())
                .await
    {
        tracing::warn!(%error, chat_id = %chat_id, "the new conversation kept the default instructions");
    }
    if !body.denied_tools.is_empty()
        && let Err(error) =
            crate::chats::set_denied_tools(&state.pool, &chat_id, &body.denied_tools).await
    {
        tracing::warn!(%error, chat_id = %chat_id, "the new conversation denied no tools of its own");
    }

    // Measured once, here, where the file is read anyway and where the answer can still change what
    // happens: a picked-up session is the one case where a conversation arrives already full, and
    // the window it is given has to be wide enough to hold what it is carrying.
    //
    // This measurement used to decide whether to pick the session up AT ALL — anything over 140k
    // was refused and handed six exchanges instead — and that ceiling was written after a real
    // incident: about 180k of context, re-sent uncached as fresh input, $1.72 for a one-word
    // answer. What it bought was not what it looked like. The expense there is a COLD CACHE on a
    // large context, which a window ceiling does not prevent and a stump does not either; what a
    // stump prevented was the conversation. The bound that does hold is the one the CLI applies
    // for itself: it never sends more than its window, and it compacts to stay under it. So the
    // number below is now a window to widen rather than a line to refuse at, and the only refusal
    // left is for a session larger than any window a model has — which is arithmetic, not policy.
    if let Some(session) = &continued {
        let read = {
            let session_id = session.session_id.clone();
            match crate::sessions::default_root() {
                Some(root) => tokio::task::spawn_blocking(move || {
                    crate::sessions::conversation(&root, &session_id)
                })
                .await
                .ok()
                .flatten(),
                None => None,
            }
        };
        let carries = read.as_ref().and_then(|read| read.context_estimate);
        // Against the LARGEST window less the CLI's own compaction headroom, because that is the
        // point at which the CLI would refuse the context outright rather than summarise it. An
        // unreadable session file answers `None` and is picked up: a file this daemon could not
        // measure is not evidence that it is too big, and the CLI reads the same file for itself.
        let ceiling =
            crate::assistant::LARGEST_WINDOW_TOKENS - crate::assistant::COMPACTION_HEADROOM;
        let resumable = carries.is_none_or(|carries| carries <= ceiling);

        if resumable {
            // Widened to fit BEFORE the session is attached, so the first turn already runs in a
            // window that can hold what it inherited. Only upward, and clamped by `window_of` when
            // it is read back: an ordinary conversation picked up small keeps the default and
            // compacts at the default, which is the cheaper of the two and the right one for it.
            if let Some(carries) = carries
                && carries > crate::assistant::CONTEXT_WINDOW_TOKENS
                && let Err(error) = crate::chats::widen_window(
                    &state.pool,
                    &chat_id,
                    (carries + crate::assistant::COMPACTION_HEADROOM)
                        .min(crate::assistant::LARGEST_WINDOW_TOKENS),
                )
                .await
            {
                tracing::warn!(
                    %error,
                    chat_id = %chat_id,
                    "the conversation was picked up but kept the default window; it will compact sooner"
                );
            }
            // Written after the chat exists, because it is what `get_session` reads to decide the
            // first turn resumes rather than starts clean. A failure here is not a failed request:
            // the conversation is real and usable, it simply begins a context of its own — so it is
            // logged as the thing it is rather than rolled back into a 500 nobody can act on.
            if let Err(error) = crate::assistant::upsert_session(
                &state.pool,
                &chat_id,
                &session.session_id,
                &chrono::Utc::now().to_rfc3339(),
            )
            .await
            {
                tracing::warn!(
                    %error,
                    chat_id = %chat_id,
                    session_id = %session.session_id,
                    "the conversation was opened but could not be attached to its session; it will start clean"
                );
            }
        } else if let Some(read) = &read {
            // Larger than any window a model has, so there is nothing to resume INTO and it is
            // handed the tail instead. Rare now, and no longer the ordinary fate of a long
            // conversation. Stored rather than re-read: the file can be tens of megabytes, the
            // turn path must not go near it, and a handover that lives in a row is one somebody
            // can read afterwards.
            let tail = crate::sessions::exchanges(&read.said, HANDOVER_EXCHANGES);
            if !tail.is_empty()
                && let Ok(stored) = serde_json::to_string(&tail)
                && let Err(error) = crate::chats::set_handover(&state.pool, &chat_id, &stored).await
            {
                tracing::warn!(
                    %error,
                    chat_id = %chat_id,
                    "the conversation was opened but could not be handed its predecessor's tail"
                );
            }
        }
    }

    Ok(Json(serde_json::json!({ "chat_id": chat_id })))
}

/// Every named model is on the menu AND is one the agent CLI could take over, or a refusal.
///
/// The same allowlist a pinned model goes through, for the same reason: a fallback naming something
/// the daemon would not run is a turn that dies at spawn on the day the primary is overloaded —
/// which is to say, on the worst day, and only then.
///
/// Cloud only, which the pinned model deliberately is not. `--fallback-model` is a flag on the
/// agent CLI; the local route is a different process that has never heard of it, so a local choice
/// here is a name handed to a CLI that cannot resolve it — the exact failure this check exists to
/// prevent, arriving from the one direction the id check alone let through.
pub(super) fn checked_fallback(names: &[String]) -> Result<(), StatusCode> {
    let config = models_config();
    for name in names {
        cloud_choice(&config, name).ok_or(StatusCode::BAD_REQUEST)?;
    }
    Ok(())
}

/// The catalogue entry a name refers to, if it is one the agent CLI could be handed.
///
/// Takes the config rather than reading it, which is what makes the rule testable: `models_config`
/// reads a fixed path, so a test running from the crate root gets the built-in defaults and those
/// have no local model in them — the exact case this exists to refuse would be unreachable.
///
/// "Cloud" here means the agent CLI, not "not on this machine". Both `--fallback-model` and the
/// per-helper `model` inside `--agents` are flags on that CLI; the local route is a different
/// process that has never heard of either, so a local choice in one of those places is a name
/// handed to something that cannot resolve it. The conversation's OWN model is deliberately not
/// filtered this way — picking it is how a conversation moves onto the local route at all.
pub(super) fn cloud_choice(
    config: &crate::config::ModelsConfig,
    id: &str,
) -> Option<crate::config::AssistantChoice> {
    config
        .catalogue()
        .into_iter()
        .find(|choice| choice.id == id)
        .filter(|choice| {
            crate::chats::Brain::from_wire(&choice.brain) == crate::chats::Brain::Cloud
        })
}

/// The largest `--agents` value this daemon will write, in characters.
///
/// A helper set travels as ONE argv element, and Windows caps an entire command line at 32767
/// characters — the prompt, every flag and this. Refused at the door rather than at spawn, because
/// over the line `CreateProcess` fails with an error about nothing in particular and the turn looks
/// broken rather than too big. Somebody who needs more has the CLI's own answer: files in the
/// project's `.claude/agents/`, which this merges with rather than replaces.
///
/// A helper's `tools` list counts toward this same total — it is serialised inside the same object,
/// on the way to the same argv element — but does not move the number. Measured against 49 built-in
/// names at up to 22 characters each, granting a helper every one of them costs roughly 1,200
/// characters; the ceiling was sized for prompts, which dwarf that, so it stays unchanged. A
/// conversation defining enough helpers with a full list each to approach 8,000 hits the refusal
/// this constant already gives, which is the ceiling doing its one job, not a new failure mode.
pub(super) const AGENTS_JSON_CEILING: usize = 8_000;

/// The longest standing instructions this daemon will write, in characters.
///
/// The same command-line ceiling `AGENTS_JSON_CEILING` guards, counted separately because they are
/// separate argv elements and a person hitting one should not be told about the other. Windows caps
/// a whole command line at 32767: these two together are 16000 of it, and the message itself is
/// also on that line — which is a limit this daemon has always had and does not introduce here.
pub(super) const INSTRUCTIONS_CEILING: usize = 8_000;

/// Standing instructions that will fit on a command line, or a refusal.
///
/// Length is the whole of it. There is nothing else to check: the text becomes ONE element of an
/// argument vector, never a shell word, so no character in it means anything to anything but the
/// model reading it.
pub(super) fn checked_instructions(text: &str) -> Result<(), StatusCode> {
    if text.len() > INSTRUCTIONS_CEILING {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

/// Every named tool is one this daemon knows how to deny, or a refusal.
///
/// Names only, never the CLI's `Bash(git *)` patterns. Not because a pattern is dangerous — it can
/// only ever deny — but because a pattern is a rule language, and a rule that matches nothing is
/// reported as one line on stderr that nobody using this app will ever read. A name off the list is
/// a refusal somebody can act on; a pattern with a typo is a restriction that silently is not one.
pub(super) fn checked_denials(names: &[String]) -> Result<(), StatusCode> {
    for name in names {
        if !crate::execution::runner::BUILTIN_TOOLS.contains(&name.as_str()) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    Ok(())
}

/// Every helper is one the CLI would actually build, or a refusal.
///
/// The door checks this because the CLI does NOT. Measured against 2.1.198, it parses the flag
/// inside a try/catch and answers a throw with an EMPTY agent list: a single bad definition costs
/// you every helper you defined, silently, with the run continuing as though you had asked for
/// none. There is no message and no exit code to notice. So what could not survive is refused here,
/// where there is somebody to tell.
pub(super) fn checked_agents(
    agents: &[crate::execution::runner::Subagent],
) -> Result<(), StatusCode> {
    let config = models_config();
    let mut seen: Vec<&str> = Vec::new();

    for agent in agents {
        // A name the CLI would reject, or one that is not a name. `-` first is its own documented
        // rule — a leading dash reads as a flag. The rest is this daemon's: the name is what a model
        // types to delegate, so a space or a quote in it is a helper nothing can call by hand.
        let name = agent.name.trim();
        if name.is_empty()
            || name.len() > 64
            || name.starts_with('-')
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        // Two helpers of one name collapse into one on the way into the object the flag takes, and
        // the person watches the other vanish without being told. Refused here so nothing is lost
        // quietly. Case-sensitive, because that is what actually collapses: the CLI keys on the
        // exact string, so `Reviewer` and `reviewer` are genuinely two helpers, and refusing them
        // would be this daemon inventing a rule the CLI does not have.
        if seen.contains(&name) {
            return Err(StatusCode::BAD_REQUEST);
        }
        seen.push(name);

        // Both are `min(1)` in the CLI's own schema. The description is load-bearing beyond that:
        // it is the whole of what the parent model reads to decide whether to delegate, so a helper
        // without one is defined, listed, and never used.
        if agent.description.trim().is_empty() || agent.prompt.trim().is_empty() {
            return Err(StatusCode::BAD_REQUEST);
        }

        // Reuses `BUILTIN_TOOLS` for a different job than `checked_denials` puts it to: there it is
        // the vocabulary of what a CONVERSATION may be denied, here it is the vocabulary of what a
        // HELPER may be granted — same list, because both are naming the one set of tools this
        // daemon knows about. Names only, never the CLI's `Bash(git *)` patterns, for the reason
        // `checked_denials` already gives: a pattern is a rule language, and a typo in one is a
        // restriction that silently is not one. `Some(vec![])` is checked and accepted here, not
        // skipped — a helper granted no tools at all is a coherent, explicit thing to ask for, and
        // treating it like `None` would make that restriction vanish.
        if let Some(tools) = &agent.tools {
            checked_denials(tools)?;
        }

        // A helper runs INSIDE the agent CLI, so its model has to be one that CLI can take — the
        // same reason the fallback is cloud-only, and not the same question as which route the
        // conversation itself is on.
        let named = match &agent.model {
            None => None,
            Some(id) => Some(cloud_choice(&config, id).ok_or(StatusCode::BAD_REQUEST)?),
        };

        // Against the levels the helper's OWN model takes when it named one, and against the union
        // when it did not. This is the one place the narrow check is honest: a helper's model and
        // its effort arrive in the same object, in one request, so they cannot drift apart between
        // two calls the way a conversation's can — which is exactly why `is_effort_level` checks the
        // union and this does not.
        if let Some(level) = agent.effort.as_deref() {
            let takes = match &named {
                Some(choice) => choice.efforts.iter().any(|have| have == level),
                None => crate::config::is_effort_level(&config, level),
            };
            if !takes {
                return Err(StatusCode::BAD_REQUEST);
            }
        }
    }

    if crate::execution::runner::agents_json(agents).len() > AGENTS_JSON_CEILING {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

/// Every path is absolute and is a directory that is there, or a refusal.
///
/// Checked here rather than at the first turn, matching `cwd`: a conversation pointed at a typo
/// looks exactly like one pointed at a project until somebody asks it to read a file, and the
/// answer comes back as a model's confusion rather than as a refusal anybody can act on.
///
/// Absolute because a relative path resolves against the DAEMON's working directory, which is not a
/// place the caller knows or meant — and this grants tool access, so a path nobody verified is a
/// directory nobody chose. Stored as given rather than canonicalised, for the reason `cwd` gives:
/// on Windows a canonical path carries a `\\?\` prefix that a raw one does not.
pub(super) async fn checked_dirs(paths: &[String]) -> Result<(), StatusCode> {
    for raw in paths {
        let path = std::path::Path::new(raw);
        let is_directory = tokio::fs::metadata(path)
            .await
            .map(|found| found.is_dir())
            .unwrap_or(false);
        if !path.is_absolute() || !is_directory {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    Ok(())
}

/// A ceiling that is a real amount of money, or a refusal.
///
/// `is_finite` and not just a range check: JSON carries infinities and NaN through some encoders,
/// and `format!("{ceiling}")` would hand the CLI the word `inf` as a dollar amount. Zero is refused
/// as well — a ceiling of nothing is a conversation that cannot answer, which is what clearing it
/// is for and says so plainly.
pub(super) fn checked_budget(amount: f64) -> Result<(), StatusCode> {
    if !amount.is_finite() || amount <= 0.0 {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

#[derive(serde::Deserialize)]
pub(super) struct PatchChatRequest {
    pub(super) title: Option<String>,
    pub(super) brain: Option<String>,
    /// What this conversation may do without being asked, from here on.
    ///
    /// Five rungs where there used to be a checkbox with two states. The old `plan_only` field is
    /// gone rather than kept as an alias: the only client of this API is this application's own
    /// window, and an alias would be a second way to say one thing, maintained for nobody.
    ///
    /// A spelling outside the five fails the whole request rather than falling to a default. The
    /// reader's "unknown reads as auto" rule exists for rows written before the column did, and
    /// applying it to a request would let a typo quietly widen what a conversation may do.
    pub(super) permission_mode: Option<crate::chats::PermissionMode>,
    /// The project this conversation is about, as an absolute path to a directory.
    ///
    /// The only way a conversation opened in the window ever gets tools: `tool_policy_for` grants
    /// them on a directory plus a wired classifier hook, and without this there was no directory to
    /// give it.
    pub(super) cwd: Option<String>,
    /// Which model answers this conversation from here on.
    ///
    /// `Option<Option<_>>` and not `Option<_>`, because three states have to be distinguishable and
    /// two of them look identical to the simpler type: the field ABSENT means "leave it alone", an
    /// explicit `null` means "unpin, follow the configured model again", and a string pins. With a
    /// single `Option` the unpin is unsayable — it arrives as absence, which is the one thing it is
    /// not.
    ///
    /// The string is a choice id from `GET /assistant/models` and is checked against that list. Not
    /// because an unknown one is dangerous — it becomes one element of an argument vector, never a
    /// shell word — but because it is a turn that dies at spawn, on the person's next message, for a
    /// reason the window could have given them here.
    #[serde(default, deserialize_with = "sent_even_if_null")]
    pub(super) model: Option<Option<String>>,
    /// How hard the model is asked to think: `low | medium | high | xhigh | max`, or `null` for the
    /// CLI's own default. Same three states as `model`, for the same reason.
    #[serde(default, deserialize_with = "sent_even_if_null")]
    pub(super) effort: Option<Option<String>>,
    /// Who answers when the chosen model is unavailable, as choice ids in the order to try them.
    /// `null` or an empty list clears it. Same three states as `model`, for the same reason.
    #[serde(default, deserialize_with = "sent_even_if_null")]
    pub(super) fallback_model: Option<Option<Vec<String>>>,
    /// Absolute paths this conversation's tools may also reach. `null` or empty clears them.
    #[serde(default, deserialize_with = "sent_even_if_null")]
    pub(super) extra_dirs: Option<Option<Vec<String>>>,
    /// The most one TURN may spend, in dollars. `null` clears the ceiling.
    ///
    /// Per turn, not per conversation: the CLI's flag bounds one invocation and this daemon spawns
    /// one per turn. The field is named for what it bounds so nobody reads it as a total.
    #[serde(default, deserialize_with = "sent_even_if_null")]
    pub(super) turn_budget_usd: Option<Option<f64>>,
    /// The helpers this conversation may hand work to. `null` or an empty list clears them.
    ///
    /// The WHOLE set every time, not one helper added or removed. A conversation's helpers are read
    /// and drawn as a list, and a client that could add one without naming the others would have to
    /// be told what happens when two requests cross — this way the last writer wins and says so.
    #[serde(default, deserialize_with = "sent_even_if_null")]
    pub(super) agents: Option<Option<Vec<crate::execution::runner::Subagent>>>,
    /// Standing instructions appended to this conversation's system prompt. `null` or blank clears
    /// them.
    ///
    /// Appended and never substituted: the flag that REPLACES the CLI's system prompt exists and is
    /// deliberately not reachable from here, because it would drop the tool descriptions and the
    /// safety framing with it.
    #[serde(default, deserialize_with = "sent_even_if_null")]
    pub(super) system_prompt: Option<Option<String>>,
    /// Built-in tools this conversation may not reach for. `null` or an empty list clears them.
    ///
    /// The whole set every time, like `agents`, and for the same reason.
    #[serde(default, deserialize_with = "sent_even_if_null")]
    pub(super) denied_tools: Option<Option<Vec<String>>>,
    /// Removed (slash spec §8.1): kept only so a PATCH that still sends it is refused, never stored.
    /// `Some(_)` whenever the key is present, `null` included.
    #[serde(default, deserialize_with = "sent_even_if_null")]
    pub(super) ambient_mcp: Option<Option<serde_json::Value>>,
}

/// What the two doors onto a conversation refuse with. Every path that had no body keeps none
/// (`Status`); the one path that has something to say — a model this machine cannot serve — carries
/// the reason in the body (`WithDetail`), the `{"detail": ...}` shape this crate already uses for a
/// refusal with a reason (see the `"detail"` sites around invalid ownership claims and unreadable
/// pins). One enum rather than widening every existing `StatusCode` return in this function to a
/// body-carrying tuple: every one of those keeps compiling unchanged through the `From<StatusCode>`
/// impl below, so this stays the smallest change that gets that one path its body.
///
/// `create_chat` answered a bare 503 here for one release, because the packet that added the
/// check had no test asking for the body and would not widen a response without one. Opening a
/// conversation on a model the machine cannot run is the FIRST place somebody meets this
/// refusal, though, so it is the last place a bare status is any use.
///
/// `Coded` is a refusal a client must tell apart by machine, answered as `{"error", "detail"}`.
pub(super) enum ChatRefusal {
    Status(StatusCode),
    WithDetail(StatusCode, String),
    Coded(StatusCode, &'static str, String),
}

impl IntoResponse for ChatRefusal {
    fn into_response(self) -> axum::response::Response {
        match self {
            ChatRefusal::Status(status) => status.into_response(),
            ChatRefusal::WithDetail(status, detail) => {
                (status, Json(serde_json::json!({ "detail": detail }))).into_response()
            }
            ChatRefusal::Coded(status, code, detail) => (
                status,
                Json(serde_json::json!({ "error": code, "detail": detail })),
            )
                .into_response(),
        }
    }
}

impl From<StatusCode> for ChatRefusal {
    fn from(status: StatusCode) -> Self {
        ChatRefusal::Status(status)
    }
}

impl From<BrainRefusal> for ChatRefusal {
    fn from(refusal: BrainRefusal) -> Self {
        match refusal {
            BrainRefusal::UnknownModel => ChatRefusal::Status(StatusCode::BAD_REQUEST),
            // Not a 500, for the same reason `BrainRefusal`'s own `From<StatusCode>` impl gives:
            // nothing broke, this machine simply cannot run what was asked for.
            BrainRefusal::CannotServe(reason) => {
                ChatRefusal::WithDetail(StatusCode::SERVICE_UNAVAILABLE, reason)
            }
        }
    }
}

/// Renames a conversation, changes which model answers it, or both.
///
/// The two halves are deliberately not symmetric. A rename touches nothing but the row; a model
/// change also drops the chat's resumable session, because the model taking over has not seen the
/// turns the other one answered and resuming across that gap would hand it a context missing them.
/// That is done here rather than asked of the caller: a client that forgets the step poisons the
/// conversation for the next model, and the shell will not be the only client.
pub(super) async fn patch_chat(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    Json(body): Json<PatchChatRequest>,
) -> Result<StatusCode, ChatRefusal> {
    // Answered before anything is written. Without it a PATCH against a chat that was never opened
    // — or was archived — reports `204 No Content` for an UPDATE that matched no row, which is the
    // API saying "done" about something it did not do.
    if crate::chats::get(&state.pool, &chat_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .is_none()
    {
        return Err(StatusCode::NOT_FOUND.into());
    }

    // Refused before anything is written, whatever the value: a stored flag nothing reads would
    // be a promise nothing keeps.
    if body.ambient_mcp.is_some() {
        return Err(ChatRefusal::Coded(
            StatusCode::BAD_REQUEST,
            "ambient_mcp_removed",
            "ambient_mcp is gone: switch MCP servers per chat with /mcp or the MCP panel".into(),
        ));
    }

    // All three move something a turn in flight is already using, and nothing else records any of
    // them. `answered_by` is written when a turn's row is born, so changing the brain under a live
    // turn makes that column lie about who answered it; the chat's `cwd` is the ONLY record of where
    // a turn ran — the assistant path never writes `runs.cwd` — so moving it under a live turn makes
    // the row the wrong answer to "where did this happen". 409 rather than a queue: the same answer
    // `POST /assistant/message` gives for the same reason.
    //
    // A rename moves neither and is left alone.
    if (body.brain.is_some()
        || body.model.is_some()
        || body.effort.is_some()
        || body.cwd.is_some()
        || body.permission_mode.is_some())
        && crate::assistant::is_busy(&chat_id)
    {
        return Err(StatusCode::CONFLICT.into());
    }

    // Every one of these is fixed on a process's command line when it is spawned, so a kept process
    // goes on with the old value for as long as it lives — and it can answer on its own between
    // turns, with no person's turn to notice the mismatch (`serve_turn` compares the rest of them
    // when a person speaks). Stopped once the edit is written, below. The rung is not here: three of
    // them are the hook's alone, and the spontaneous path checks the other two itself.
    let respawns = body.brain.is_some()
        || body.cwd.is_some()
        || body.model.is_some()
        || body.effort.is_some()
        || body.fallback_model.is_some()
        || body.extra_dirs.is_some()
        || body.turn_budget_usd.is_some()
        || body.agents.is_some()
        || body.system_prompt.is_some()
        || body.denied_tools.is_some();

    // Every check first, every write after. This used to validate and write field by field, so a
    // body with one good field and one bad one answered 400 with the good one already stored — a
    // refusal that had half-happened. Nothing below this block's end touches the database until
    // each field has been judged.
    if let Some(cwd) = body.cwd.as_deref() {
        // Absolute, and a directory that is there. Checked HERE rather than at the first turn: a
        // conversation pointed at a typo would look exactly like one pointed at a project until
        // somebody asked it to read a file, and the answer would arrive as a model's confusion
        // rather than as a refusal anybody could act on.
        //
        // Absolute because a relative path would be resolved against the DAEMON's working
        // directory, which is not a place the caller knows or meant. Stored as given rather than
        // canonicalised: on Windows a canonical path carries a `\\?\` prefix that a raw one does
        // not, and this string is handed to the CLI as its working directory and compared against a
        // living process's own.
        let path = std::path::Path::new(cwd);
        let is_directory = tokio::fs::metadata(path)
            .await
            .map(|found| found.is_dir())
            .unwrap_or(false);
        if !path.is_absolute() || !is_directory {
            return Err(StatusCode::BAD_REQUEST.into());
        }
    }

    // A choice names its own route, so the brain a model implies is worked out here, before any
    // write, and applied after the plain `brain` field below.
    let model_brain = match &body.model {
        Some(Some(id)) => Some(chosen_brain(&state, id, Asking::Chat).await?),
        // Unpinning says nothing about the route. The conversation goes back to following the
        // configured model, and `brain` keeps whatever it already had — changing it here would
        // be this route inventing a decision nobody expressed.
        _ => None,
    };

    // None of the checks below joins the 409 guard above, and that is a per-column answer rather
    // than an oversight. `cwd` and the brain are guarded because a live turn's record would come to
    // disagree with them — the chat row is the only note of where a turn ran, and `answered_by` is
    // written when the row is born. Nothing records a run's fallback, its extra directories, its
    // ceiling or its helpers, and all of them are read at LAUNCH, so a turn already running has its
    // own copy and cannot be made to lie by changing them. Refusing here would only stop somebody
    // setting up their next turn while waiting for this one.
    let fallback_names = match &body.fallback_model {
        Some(fallback) => {
            let names = fallback.clone().unwrap_or_default();
            checked_fallback(&names)?;
            Some(names)
        }
        None => None,
    };
    let extra_dirs = match &body.extra_dirs {
        Some(dirs) => {
            let paths = dirs.clone().unwrap_or_default();
            checked_dirs(&paths).await?;
            Some(paths)
        }
        None => None,
    };
    if let Some(Some(amount)) = &body.turn_budget_usd {
        checked_budget(*amount)?;
    }
    let helpers = match &body.agents {
        Some(agents) => {
            let defined = agents.clone().unwrap_or_default();
            checked_agents(&defined)?;
            Some(defined)
        }
        None => None,
    };
    if let Some(Some(text)) = &body.system_prompt {
        checked_instructions(text)?;
    }
    let denials = match &body.denied_tools {
        Some(denied) => {
            let names = denied.clone().unwrap_or_default();
            checked_denials(&names)?;
            Some(names)
        }
        None => None,
    };
    if let Some(Some(level)) = &body.effort
        && !crate::config::is_effort_level(&models_config(), level)
    {
        return Err(StatusCode::BAD_REQUEST.into());
    }
    fn failed(step: &'static str) -> impl FnOnce(sqlx::Error) -> StatusCode {
        move |error| {
            tracing::warn!(%error, step, "editing a conversation failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }

    // One transaction: either every field lands or none does.
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(failed("opening the edit"))?;
    let mut forget = false;

    if let Some(cwd) = body.cwd.as_deref() {
        crate::chats::set_cwd(&mut *tx, &chat_id, cwd)
            .await
            .map_err(failed("pointing a conversation at a project"))?;
        // For the reason the brain does, and one more. The CLI keeps its transcripts as
        // `<root>/<project>/<session>.jsonl` — one directory per project — so a session had in one
        // tree is not somewhere a run in another tree would look for it. And its context is about
        // the old tree regardless: continuing it here would answer questions about this project out
        // of the last one's files.
        forget = true;
    }

    if let Some(brain) = body.brain.as_deref() {
        crate::chats::set_brain(&mut *tx, &chat_id, crate::chats::Brain::from_wire(brain))
            .await
            .map_err(failed("changing a chat's model"))?;
        forget = true;
    }

    // After the brain block above, and that order is the whole point: a choice names its own route,
    // so when a client sends both and they disagree, the one that cannot be wrong is the one that
    // lands last. A row saying `local` while naming a cloud model would be sent to Ollama under a
    // name it has never heard.
    if let Some(model) = &body.model {
        crate::chats::set_model(&mut *tx, &chat_id, model.as_deref())
            .await
            .map_err(failed("pinning a conversation to a model"))?;
        if let Some(brain) = model_brain {
            crate::chats::set_brain(&mut *tx, &chat_id, brain)
                .await
                .map_err(failed("routing a conversation to its model's brain"))?;
        }
        // For the reason the brain block gives: the model taking over has not seen the turns the
        // other one answered, and resuming across that gap hands it a context missing them.
        forget = true;
    }

    if let Some(names) = &fallback_names {
        crate::chats::set_fallback(&mut *tx, &chat_id, names)
            .await
            .map_err(failed("naming a conversation's fallback model"))?;
    }

    if let Some(paths) = &extra_dirs {
        crate::chats::set_extra_dirs(&mut *tx, &chat_id, paths)
            .await
            .map_err(failed("widening what a conversation may reach"))?;
    }

    if let Some(ceiling) = &body.turn_budget_usd {
        crate::chats::set_turn_budget(&mut *tx, &chat_id, *ceiling)
            .await
            .map_err(failed("setting what a turn may spend"))?;
    }

    if let Some(defined) = &helpers {
        crate::chats::set_agents(&mut *tx, &chat_id, defined)
            .await
            .map_err(failed("defining a conversation's helpers"))?;
    }

    if let Some(instructions) = &body.system_prompt {
        crate::chats::set_system_prompt(&mut *tx, &chat_id, instructions.as_deref())
            .await
            .map_err(failed("writing a conversation's standing instructions"))?;
    }

    if let Some(names) = &denials {
        crate::chats::set_denied_tools(&mut *tx, &chat_id, names)
            .await
            .map_err(failed("narrowing what a conversation may reach for"))?;
    }

    // No `forget_session`. Effort changes how hard the same model thinks, not who is thinking — the
    // context the session holds is still that model's own, and dropping it would make a dial nobody
    // considers destructive silently restart the conversation.
    if let Some(effort) = &body.effort {
        crate::chats::set_effort(&mut *tx, &chat_id, effort.as_deref())
            .await
            .map_err(failed("changing how hard a conversation thinks"))?;
    }

    if let Some(permission_mode) = body.permission_mode {
        crate::chats::set_permission_mode(&mut *tx, &chat_id, permission_mode)
            .await
            .map_err(failed("changing what a conversation may do without asking"))?;
    }

    if let Some(title) = body.title.as_deref() {
        crate::chats::rename(&mut *tx, &chat_id, Some(title))
            .await
            .map_err(failed("renaming a chat"))?;
    }

    tx.commit().await.map_err(failed("committing the edit"))?;

    // Only now, once the rows say what the person asked for: forgetting a session on the strength
    // of a write that was then rolled back would lose a context for nothing.
    if forget {
        crate::assistant::forget_session(&state.pool, &chat_id)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "forgetting a changed conversation's session failed");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
    }

    if let Some(permission_mode) = body.permission_mode {
        // The running turn now decides with this rung; asks it allows are answered here.
        crate::hooks::reconsider_asks(&state, &chat_id, permission_mode).await;
    }

    if respawns {
        // The next turn respawns.
        crate::assistant::evict_live(&chat_id);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// How much of a model's answer may become a title.
///
/// A model asked for five words can answer with a paragraph, and the answer goes straight into a
/// sidebar. Cut here rather than in CSS: what is stored is what other clients will read, and a
/// paragraph in that column is a paragraph everywhere.
pub(super) const TITLE_LIMIT: usize = 80;

/// The first thing the model said that could be a name, bounded.
pub(super) fn title_from(reply: &str) -> String {
    reply
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .chars()
        .take(TITLE_LIMIT)
        .collect()
}

/// Names a conversation using the local model.
///
/// Local only, and 503 rather than a cloud fallback when there is none: a title is decoration, and
/// decoration is not worth a billed cloud call — every turn on the other path is a run with a price
/// on it. The same 503 covers a model that answered with nothing usable, because from the caller's
/// side "the local model could not name this" is one fact either way.
///
/// The history comes from `recent_exchanges`, so this reads the conversation under the same barrier
/// a local turn does: nothing from before the last turn that read third-party text. A title drawn
/// from a stranger's mail would be that mail choosing what this conversation is called.
pub(super) async fn post_chat_title(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    // `state.local_assistant.clone()` before the migration to the assistant factory: the field this
    // read no longer exists, so this call site is one of the lines that migration is allowed to
    // touch beyond the `AppState` literal.
    let assistant = state
        .assistants
        .assistant_for(crate::chats::Brain::Local, None)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let history = crate::assistant::recent_exchanges(&state.pool, &chat_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "reading a chat before naming it failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    // Nothing has been said, so there is nothing to name it after. Asking anyway would be the model
    // guessing about a conversation that has not happened.
    if history.is_empty() {
        return Err(StatusCode::CONFLICT);
    }

    // The prompt asks for no tools; the model still has them. This is what says whether it used one
    // to read a stranger's words on the way to an answer.
    let taint = std::sync::atomic::AtomicBool::new(false);
    let turn = assistant
        .answer(
            &history,
            "Name this conversation in at most five words, in the language it is being had in. \
             Answer with the name alone, on one line, and call no tools — everything you need is \
             already above.",
            &taint,
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, "the local model could not name a chat");
            StatusCode::SERVICE_UNAVAILABLE
        })?;

    // Naming a conversation is NOT a run, so there is no row to mark and nothing downstream that
    // would refuse this answer later — `mark_untrusted_context`, which is what a local turn does
    // here, has nothing to write against. The fail-closed move left is to drop the title: one drawn
    // from a mail body would be its sender naming this conversation, in the sidebar, for good.
    //
    // Reported as the same 503 as a model that could not answer, because from the caller's side
    // both are "the local model could not name this". The distinction that matters is for whoever
    // reads the log, and it is in the line below.
    if taint.load(std::sync::atomic::Ordering::SeqCst) {
        tracing::warn!(chat_id = %chat_id, "a chat's proposed name read third-party text; dropping it");
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    let title = title_from(&turn.answer);
    if title.is_empty() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    crate::chats::rename(&state.pool, &chat_id, Some(&title))
        .await
        .map_err(|error| {
            tracing::warn!(%error, "storing a chat's name failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// Chat groups (P4): the list, create, rename, delete and assign handlers.
pub(super) async fn list_chat_groups(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::chats::groups::ChatGroup>>, StatusCode> {
    crate::chats::groups::list(&state.pool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[derive(serde::Deserialize)]
pub(super) struct ChatGroupName {
    pub(super) name: String,
}

pub(super) fn group_refusal(
    error: crate::chats::groups::GroupError,
) -> (StatusCode, Json<serde_json::Value>) {
    use crate::chats::groups::GroupError;
    match error {
        GroupError::Blank => refusal(StatusCode::BAD_REQUEST, "blank_name"),
        GroupError::TooLong => refusal(StatusCode::BAD_REQUEST, "name_too_long"),
        GroupError::NoGroup | GroupError::NoChat => refusal(StatusCode::NOT_FOUND, "not_found"),
        GroupError::Db(error) => {
            tracing::warn!(%error, "chat group operation failed");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
    }
}

pub(super) async fn create_chat_group(
    State(state): State<AppState>,
    Json(body): Json<ChatGroupName>,
) -> Result<
    (StatusCode, Json<crate::chats::groups::ChatGroup>),
    (StatusCode, Json<serde_json::Value>),
> {
    crate::chats::groups::create(&state.pool, &body.name)
        .await
        .map(|group| (StatusCode::CREATED, Json(group)))
        .map_err(group_refusal)
}

pub(super) async fn rename_chat_group(
    State(state): State<AppState>,
    Path(group_id): Path<i64>,
    Json(body): Json<ChatGroupName>,
) -> Result<Json<crate::chats::groups::ChatGroup>, (StatusCode, Json<serde_json::Value>)> {
    crate::chats::groups::rename(&state.pool, group_id, &body.name)
        .await
        .map(Json)
        .map_err(group_refusal)
}

pub(super) async fn delete_chat_group(
    State(state): State<AppState>,
    Path(group_id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    match crate::chats::groups::delete(&state.pool, group_id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[derive(serde::Deserialize)]
pub(super) struct ChatGroupAssignment {
    pub(super) group_id: Option<i64>,
}

pub(super) async fn put_chat_group(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    Json(body): Json<ChatGroupAssignment>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    crate::chats::groups::assign(&state.pool, &chat_id, body.group_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(group_refusal)
}
