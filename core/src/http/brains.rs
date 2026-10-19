//! Which models and tools a conversation can use (`/assistant/models*`, `/assistant/local-model*`, `/assistant/tools`, `/assistant/ide-sessions*`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        // Static segments ahead of `{turn_id}`; matchit prefers the literal, so a chat named like a
        // number cannot shadow a turn id.
        .route("/assistant/local-model", get(get_local_model))
        // Fetching one this machine does not have, and watching it arrive. Under the route above
        // rather than beside it because it is the same subject — what this machine can answer with
        // — and a person reading the table should see the two together.
        .route(
            "/assistant/local-model/pull",
            get(get_local_model_pull).post(post_local_model_pull),
        )
        // What it weighs, and whether this machine can carry it. Beside the pull because it is the
        // question asked immediately before one — and the one the pull itself asks to decide
        // whether to refuse.
        .route("/assistant/local-model/size", get(get_local_model_size))
        .route("/assistant/ide-sessions", get(list_ide_sessions))
        // The conversation behind one of them. A GET on the session itself rather than a
        // `/messages` under it: what a session IS, to anything outside this daemon, is what was
        // said in it — the id and the directory are how it is found, not what it holds.
        .route(
            "/assistant/ide-sessions/{session_id}",
            get(read_ide_session),
        )
        // Writing this daemon's classifier hook into the project a session was had in. POST
        // because it changes that project, and under the session because the session is what says
        // WHICH project — the request never names a directory.
        .route(
            "/assistant/ide-sessions/{session_id}/tools",
            post(wire_ide_session_tools),
        )
        // Which models a conversation may be moved to. Its own route and not a field on the chat,
        // because it is the same answer for every chat and a per-chat copy would be fetched once
        // per row in the list.
        .route("/assistant/models", get(get_assistant_models))
        .route("/assistant/models/groups", get(get_model_groups))
        .route("/assistant/tools", get(get_deniable_tools))
        .route("/assistant/commands", get(get_commands))
}

/// Whether this machine has a model that can answer a conversation.
///
/// Exists so a client can offer the choice honestly. Without it the window would show "local" as an
/// option, take the switch, and only discover on the next message that nothing on this machine can
/// answer — leaving the conversation set to a model that does not exist. An option that is not
/// there and an option that is unavailable today are different facts, and only one of them is
/// something the user can act on.
///
/// Reports what STARTUP resolved, not a live probe: `local_assistant` is `Some` only if the daemon
/// managed to build one, and a probe here would be a second, differently-timed opinion about the
/// same thing.
pub(super) async fn get_local_model(State(state): State<AppState>) -> Json<serde_json::Value> {
    // `state.local_assistant.is_some()` before the migration to the assistant factory: the field
    // this read no longer exists, so this call site is one of the lines that migration is allowed
    // to touch beyond the `AppState` literal.
    let available = state.assistants.serves(crate::chats::Brain::Local).is_ok();
    Json(serde_json::json!({ "available": available }))
}

/// One download of a local model: what it is, where it has got to, and how it ended.
///
/// `outcome` is `None` while it runs, and that single field is what "in flight" means here — there
/// is no separate flag, because two ways to say the same thing is two ways for them to disagree.
/// A finished pull is KEPT rather than cleared: the window polls, and clearing on completion would
/// mean the poll that arrives one tick after a successful download finds nothing and cannot tell
/// "it worked" from "nothing ever happened".
#[derive(Clone)]
pub(super) struct Pull {
    pub(super) model: String,
    /// Ollama's own word for what it is doing, carried through untranslated —
    /// `capabilities::PullProgress` explains why.
    pub(super) status: String,
    pub(super) completed: u64,
    pub(super) total: u64,
    pub(super) outcome: Option<Result<(), String>>,
}

/// The one download this machine is doing, or the last one it did.
///
/// A process-global rather than a field on `AppState`, for the reason `sidecar::SIDECARS` and
/// `assistant::LIVE_CHATS` are: this is not per-request state that a handler was handed, it is a
/// fact about the machine — there is one Ollama on it, one disk, and at most one download being
/// watched. It also sits beside `OLLAMA_TAGS_CLIENT`, which is a static in this module for exactly
/// the same argument.
///
/// **One at a time, deliberately.** Two multi-gigabyte downloads sharing one disk finish later than
/// the same two in sequence, and the window has one progress bar; a second request while one runs
/// is refused rather than queued, because a queue nobody can see or cancel is worse than a refusal
/// somebody can retry.
///
/// The one way this can wedge: a task that panicked between its last progress frame and writing its
/// outcome would leave `outcome: None` forever, and every later pull would be refused until the
/// daemon restarts. What it runs has no panic path — `pull_local_model` returns its failures — and
/// the alternative (keeping the `JoinHandle` to ask `is_finished`) costs this type its `Clone` and
/// the readout its pure test, which is a worse trade for a case that needs a bug to reach.
pub(super) static LOCAL_PULL: std::sync::LazyLock<std::sync::Mutex<Option<Pull>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

/// PURE: a pull -> what the window is told about it.
///
/// `percent` is `null` and not `0` when nothing has said how big the download is. Every pull opens
/// on `pulling manifest`, which carries no counts, and a bar sitting at 0% says a download is
/// stuck — which one that has not started measuring itself is not.
///
/// `state` collapses to three words because three is what a window can draw. `done` and `failed`
/// stay apart, though both mean "not running": collapsing them would either swallow the reason a
/// download failed or leave a spinner up after one that worked.
pub(super) fn pull_readout(pull: &Pull) -> serde_json::Value {
    serde_json::json!({
        "model": pull.model,
        "state": match &pull.outcome {
            None => "running",
            Some(Ok(())) => "done",
            Some(Err(_)) => "failed",
        },
        "status": pull.status,
        "percent": (pull.total > 0).then(|| pull.completed * 100 / pull.total),
        "error": match &pull.outcome {
            Some(Err(reason)) => serde_json::Value::String(reason.clone()),
            _ => serde_json::Value::Null,
        },
    })
}

/// The client a download runs on, built once and shared.
///
/// Emphatically NOT `ollama_tags_client()`: that one is built with `OLLAMA_TAGS_TIMEOUT` so a
/// wedged Ollama cannot hold up a menu, and a whole-request timeout of a couple of seconds would
/// abort every download that was working. A pull runs for minutes by design. The connect timeout
/// stays short, because failing to DIAL is still a fast failure and the person is watching.
/// The client the model REGISTRY is read over, built once and shared.
///
/// Also emphatically not `ollama_tags_client()`, and this one is a correction rather than a
/// precaution: the size route shipped reusing that client, which is built for a localhost probe
/// with a two-second whole-request budget. `registry.ollama.ai` is across the internet and behind
/// a TLS handshake. It measured 0.37s from this machine so two seconds was not actually the bug —
/// the bug was the missing TLS backend, and this reuse was simply wrong for a reason that would
/// have surfaced later, on a slower connection, as a size that was sometimes unknown.
///
/// Ten seconds, and a short connect timeout for the same reason the pull client keeps one: failing
/// to dial is a fast failure, and somebody is looking at the row while it happens.
pub(super) fn model_registry_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("HTTP client for the model registry (check TLS and proxy environment)")
    })
}

pub(super) fn ollama_pull_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("HTTP client for the /api/pull stream (check TLS and proxy environment)")
    })
}

#[derive(serde::Deserialize)]
pub(super) struct PullRequest {
    /// A choice id from `GET /assistant/models`, and checked against that list — see the handler.
    pub(super) model: String,
}

#[derive(serde::Deserialize)]
pub(super) struct SizeQuery {
    pub(super) model: String,
}

/// PURE: what the window draws under a model this machine does not have yet.
///
/// Both numbers travel, not just the verdict. "17.4 GB, and this machine has 15.8" is something a
/// person can act on — a smaller quantisation, a different model, more memory — while a bare
/// `too_big` is a wall with no door in it. `null` rather than `0` on either side for the reason
/// `capabilities::interpret_manifest` returns `None`: a zero is a real number that grades as the
/// most permissive verdict there is, which is the last thing a failed read should produce.
pub(super) fn size_readout(
    model: &str,
    bytes: Option<u64>,
    memory: Option<u64>,
) -> serde_json::Value {
    let fit = crate::capabilities::model_fit(bytes.unwrap_or(0), memory.unwrap_or(0));
    serde_json::json!({
        "model": model,
        "bytes": bytes,
        "memory": memory,
        "fit": match fit {
            crate::capabilities::Fit::Comfortable => "comfortable",
            crate::capabilities::Fit::Tight => "tight",
            crate::capabilities::Fit::TooBig => "too_big",
            crate::capabilities::Fit::Unknown => "unknown",
        },
        "error": serde_json::Value::Null,
    })
}

/// Whether this daemon KNOWS a model will not run here — never merely suspects it.
///
/// The name says `known` because the answer is asymmetric and the asymmetry is the whole point.
/// `true` requires two successful reads: a size the registry gave and a memory figure Windows gave.
/// Every other outcome is `false` — an unreachable registry, a manifest that did not parse, a
/// machine that could not be measured — because refusing on a failure to MEASURE would hand a third
/// party on the far side of the internet the power to switch downloads off on this machine.
///
/// Its own function rather than a condition inline: the caller is a guard clause whose reader wants
/// to know what is being refused, not how it was determined.
pub(super) async fn known_too_big(model: &str) -> bool {
    let Ok(bytes) = crate::capabilities::registry_model_size(
        model_registry_client(),
        crate::capabilities::OLLAMA_REGISTRY_URL,
        model,
    )
    .await
    else {
        return false;
    };
    let Some(memory) = crate::capabilities::total_memory_bytes() else {
        return false;
    };
    crate::capabilities::model_fit(bytes, memory) == crate::capabilities::Fit::TooBig
}

/// What a model weighs, and whether this machine can carry it.
///
/// Answers the question the download confirmation needs and cannot ask any other way: the local
/// `/api/tags` knows the size of models already here, which is the case already settled. This reads
/// the public registry — no key, no account — over the same short-timeout client the `/api/tags`
/// probes use, because like them it runs to decorate a menu somebody is looking at.
///
/// Two failures, answered differently on purpose. A name `registry_path` refuses is a `400`: it can
/// never work and will not start working on its own. A registry that could not be reached is a
/// `200` carrying `unknown`, because it is transient and a picker that broke whenever a third party
/// on the far side of the internet was slow would be a feature that breaks the app it decorates.
///
/// Reachable by the control token and an admin key alone — it is in no scope table, and
/// `auth::the_pull_routes_are_out_of_an_agents_reach` is where that is held.
pub(super) async fn get_local_model_size(
    Query(query): Query<SizeQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if crate::capabilities::registry_path(&query.model).is_none() {
        return Err(refusal(StatusCode::BAD_REQUEST, "unknown_model"));
    }
    let memory = crate::capabilities::total_memory_bytes();
    match crate::capabilities::registry_model_size(
        model_registry_client(),
        crate::capabilities::OLLAMA_REGISTRY_URL,
        &query.model,
    )
    .await
    {
        Ok(bytes) => Ok(Json(size_readout(&query.model, Some(bytes), memory))),
        Err(reason) => {
            let mut readout = size_readout(&query.model, None, memory);
            readout["error"] = serde_json::Value::String(reason);
            Ok(Json(readout))
        }
    }
}

/// Fetch a local model this machine does not have yet.
///
/// Answers `202` the moment the download starts rather than when it finishes, because it finishes
/// in minutes and no HTTP client on either end would wait. The body is the same readout `GET` on
/// this path returns, so the window has a first frame to draw without waiting for its own first
/// poll.
///
/// **`menu()` is the allowlist, not the request body.** This is the one route in this file that
/// makes the machine fetch gigabytes from a name somebody sent, so the name has to be one that was
/// already written into `~/.nucleos/nucleos-models.yaml` or that Ollama already has. Refused with the same
/// `400` a model the catalogue does not offer gets from `patch_chat`, and for the same reason:
/// it is the person's name that is wrong, not this daemon's state.
///
/// Reachable by the control token and an admin key alone — it is in no scope table, and
/// `auth::the_pull_routes_are_out_of_an_agents_reach` is where that is held.
pub(super) async fn post_local_model_pull(
    Json(body): Json<PullRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let (_, choices) = menu(Asking::Daemon).await;
    if !choices
        .iter()
        .any(|choice| choice.brain == "local" && choice.id == body.model)
    {
        return Err(refusal(StatusCode::BAD_REQUEST, "unknown_model"));
    }

    // Refused BEFORE the download rather than discovered after it. A model larger than this machine
    // does not become runnable by arriving, so the gigabytes would be spent to prove a thing the
    // registry will say in one small request.
    //
    // Fails OPEN, and that asymmetry is deliberate: only a size this daemon actually read and
    // actually graded `TooBig` stops anything. An unreachable registry, a manifest it could not
    // parse, a machine whose memory it could not measure — all of those proceed, because refusing
    // on a failure to measure would make a third party on the far side of the internet able to
    // switch off downloads here.
    if known_too_big(&body.model).await {
        return Err(refusal(StatusCode::BAD_REQUEST, "model_too_big"));
    }

    let started = {
        // The whole check-and-claim under one lock, so two requests arriving together cannot both
        // find the slot free. Nothing is awaited inside it.
        let mut slot = LOCAL_PULL
            .lock()
            .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "pull_state_poisoned"))?;
        if slot.as_ref().is_some_and(|pull| pull.outcome.is_none()) {
            return Err(refusal(StatusCode::CONFLICT, "pull_in_flight"));
        }
        let started = Pull {
            model: body.model.clone(),
            status: "starting".to_owned(),
            completed: 0,
            total: 0,
            outcome: None,
        };
        *slot = Some(started.clone());
        started
    };

    let model = body.model;
    // The download in a task of its own, awaited by this one, so a panic inside it still closes
    // the slot. A slot left with no outcome reads as `pull_in_flight` to every later request, and
    // nothing short of a restart would ever clear it.
    tokio::spawn(async move {
        let outcome = tokio::spawn(async move {
            crate::capabilities::pull_local_model(
                ollama_pull_client(),
                crate::execution::runner::OLLAMA_BASE_URL,
                &model,
                |frame| {
                    if let Ok(mut slot) = LOCAL_PULL.lock()
                        && let Some(pull) = slot.as_mut()
                    {
                        pull.status = frame.status;
                        pull.completed = frame.completed;
                        pull.total = frame.total;
                    }
                },
            )
            .await
        })
        .await
        .unwrap_or_else(|error| {
            tracing::error!(%error, "the local model download's task ended without a result");
            Err(format!("the download ended without a result: {error}"))
        });
        // A panic inside the progress callback poisons the lock while holding it; the slot it
        // guards is still whole, so it is taken back and the poison cleared rather than leaving
        // every later request on `pull_state_poisoned`.
        let mut slot = LOCAL_PULL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pull) = slot.as_mut() {
            pull.outcome = Some(outcome);
        }
        drop(slot);
        LOCAL_PULL.clear_poison();
    });

    Ok((StatusCode::ACCEPTED, Json(pull_readout(&started))))
}

/// How the download is going.
///
/// `204` when this daemon has not been asked to fetch anything since it started — the same way
/// `get_assistant_live` above answers a turn nothing is writing. It is emphatically not "the
/// download failed": a window that read it as one would put an error up on every fresh install.
pub(super) async fn get_local_model_pull() -> Result<Json<serde_json::Value>, StatusCode> {
    let slot = LOCAL_PULL
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let pull = slot.as_ref().ok_or(StatusCode::NO_CONTENT)?;
    Ok(Json(pull_readout(pull)))
}

/// One session as it is OFFERED: what it is, plus what continuing it would be able to do.
///
/// The second half is not decoration. A conversation continued in a directory with no classifier
/// hook runs on the MCP server alone — no `Read`, no `Edit`, no `Bash` — so picking up a coding
/// session there gets a model that cannot open the file being discussed. That was invisible until
/// the first turn came back empty-handed, and it is the single thing that made continuing a
/// session feel like nothing had happened.
#[derive(serde::Serialize)]
pub(super) struct OfferedSession {
    #[serde(flatten)]
    pub(super) session: crate::sessions::IdeSession,
    /// Whether a turn continued here would get the project's tools.
    ///
    /// Answered by asking `tool_policy_for` — the same function the turn itself asks — rather than
    /// by restating the rule here. A second opinion about this would be a window promising tools
    /// the turn then does not get.
    pub(super) tools: bool,
}

/// The conversations already had in the IDE that this daemon could continue.
///
/// Ones already continued are dropped: a second conversation resuming the same session would put
/// two threads on one context, and the window would show them as unrelated.
pub(super) async fn list_ide_sessions(
    State(state): State<AppState>,
) -> Result<Json<Vec<OfferedSession>>, StatusCode> {
    let Some(root) = crate::sessions::default_root() else {
        return Ok(Json(Vec::new()));
    };
    let taken = crate::chats::picked_up(&state.pool)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "listing the sessions already continued failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Read on the request rather than kept warm in memory. The store is the CLI's, it changes
    // whenever a session is typed into, and a cache would be a second copy of somebody else's truth.
    let found = tokio::task::spawn_blocking(move || {
        crate::sessions::discover(&root, IDE_SESSIONS_SHOWN)
            .into_iter()
            .filter(|session| !taken.contains(&session.session_id))
            .map(|session| {
                // Read here and never cached: the hook can be wired between two openings of this
                // list, including by the route below, and a stale `false` would go on offering to
                // fix what is already fixed.
                let wired = crate::governance::autopilot::classifier_hook_is_wired(
                    std::path::Path::new(&session.cwd),
                );
                // `Typed` for the reason the route above gives: an offered session is something
                // the person would type into themselves.
                let policy = crate::assistant::tool_policy_for(
                    Some(session.cwd.as_str()),
                    crate::assistant::Origin::Shell,
                    wired,
                    crate::relay::Handover::Typed,
                );
                OfferedSession {
                    session,
                    tools: policy == crate::execution::runner::ToolPolicy::Unrestricted,
                }
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(found))
}

/// Puts this daemon's classifier hook into the project a session was had in.
///
/// The most consequential thing this daemon does to a directory it does not own: afterwards every
/// session had there — this one, and the ones opened in the editor — has its tool calls routed
/// through the classifier, and conversations continued there get the CLI's whole tool surface
/// instead of the MCP server alone. So it is a POST somebody presses, never something inferred
/// from picking a session up.
///
/// The directory comes from the transcript by way of `sessions::find`, exactly as `create_chat`
/// gets it and for a sharper version of the same reason: a caller that could name the directory
/// could have this daemon write an executable hook into any folder on the machine.
///
/// A 409 for a settings file this cannot parse. That is not the daemon failing — it is the project
/// saying no — and the one thing the caller can do about it is go and look at that file.
pub(super) async fn wire_ide_session_tools(
    Path(session_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let root = crate::sessions::default_root().ok_or(StatusCode::NOT_FOUND)?;
    let session = tokio::task::spawn_blocking(move || crate::sessions::find(&root, &session_id))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let dir = std::path::PathBuf::from(&session.cwd);
    tokio::task::spawn_blocking(move || crate::governance::autopilot::wire_classifier_hook(&dir))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|error| {
            tracing::warn!(%error, cwd = %session.cwd, "could not wire the classifier hook");
            StatusCode::CONFLICT
        })?;

    Ok(StatusCode::NO_CONTENT)
}

/// What was said in one conversation had in the editor, oldest first.
///
/// Read off disk on the request, like the list above and for the same reason: the store is the
/// CLI's and changes whenever a session is typed into, so anything kept here would be a second copy
/// of somebody else's truth. This one reads a whole file rather than its head, which is why it is
/// on the blocking pool.
///
/// Not behind a chat id, though the window only ever asks about sessions it holds one for. The
/// session is the thing that has a conversation in it; a chat merely points at one, and two routes
/// for the same bytes would be two places for the bounds to differ.
///
/// A transcript this machine does not have is a 404. An empty conversation is a 200 with nothing in
/// it, and the window says different things about the two — "nothing was said here" is a claim, and
/// it must not be made about a file that was never found.
pub(super) async fn read_ide_session(
    Path(session_id): Path<String>,
) -> Result<Json<IdeConversationOut>, StatusCode> {
    let root = crate::sessions::default_root().ok_or(StatusCode::NOT_FOUND)?;
    let found =
        tokio::task::spawn_blocking(move || crate::sessions::conversation(&root, &session_id))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    found
        .map(|conversation| {
            Json(IdeConversationOut {
                conversation,
                largest_window: crate::assistant::LARGEST_WINDOW_TOKENS
                    - crate::assistant::COMPACTION_HEADROOM,
            })
        })
        .ok_or(StatusCode::NOT_FOUND)
}

/// A conversation had in the editor, and the largest one this daemon can pick up whole.
///
/// The number rides with it for the reason it rides with a turn: it is a property of this daemon
/// and not of the session, and the window keeping its own copy of a rule this side owns is a second
/// source of truth that drifts silently the day the constant changes. Here it also answers the
/// question the estimate is being asked for — whether picking this up continues it or hands it on.
///
/// It is the LARGEST window minus the CLI's own compaction headroom, and not the default window,
/// because a picked-up conversation is given a window wide enough to hold it. What it cannot be
/// given is a window wider than the model has, and that is the line this names.
#[derive(serde::Serialize)]
pub(super) struct IdeConversationOut {
    #[serde(flatten)]
    pub(super) conversation: crate::sessions::Conversation,
    pub(super) largest_window: i64,
}

/// How many past sessions the list offers.
///
/// There are hundreds on this machine and they are ordered by recency, so this is a question about
/// how far back a person reaches for a conversation they mean to continue — not about completeness.
pub(super) const IDE_SESSIONS_SHOWN: usize = 40;

/// Query parameters for the assistant model menu.
#[derive(serde::Deserialize)]
pub(super) struct ModelsQuery {
    pub(super) chat: Option<String>,
}

/// Which models a conversation may be moved to, and how hard each can be asked to think.
///
/// Built from the same `menu()` `chosen_brain` uses, above — installed models included, over
/// `OLLAMA_TAGS_CLIENT` — rather than a second copy of the merge; see `menu()`'s doc comment for the
/// fail-closed reasoning. Always pays for the installed-models read, unlike `chosen_brain`'s fast
/// path: this route serves the WHOLE menu, so there is no cheap subset it could answer from instead.
///
/// Each choice is then marked (`config::marked_with`) with what `state.assistants.declared_for`
/// found — grouped by route because that trait method answers for one `Brain` at a time, and this
/// menu mixes cloud, local and hosted choices in one list. A route with no choices on the menu
/// (the common case for the hosted route, and for local when nothing is configured) is skipped
/// rather than asked with an empty slice, so an untouched install costs this route nothing beyond
/// the network read `menu()` already pays for.
pub(super) async fn get_assistant_models(
    State(state): State<AppState>,
    Query(query): Query<ModelsQuery>,
) -> Json<serde_json::Value> {
    let asking = match query.chat {
        Some(_) => Asking::Chat,
        None => Asking::Daemon,
    };
    let (config, choices) = menu(asking).await;

    let mut declared: std::collections::HashMap<String, crate::capabilities::Declared> =
        std::collections::HashMap::new();
    for brain in [
        crate::chats::Brain::Cloud,
        crate::chats::Brain::Local,
        crate::chats::Brain::OpenRouter,
    ] {
        let ids: Vec<String> = choices
            .iter()
            .filter(|choice| crate::chats::Brain::from_wire(&choice.brain) == brain)
            .map(|choice| choice.id.clone())
            .collect();
        if ids.is_empty() {
            continue;
        }
        declared.extend(state.assistants.declared_for(brain, &ids).await);
    }
    let choices = crate::config::marked_with(choices, &declared);

    Json(serde_json::json!({
        // Each choice carries its OWN effort levels, which is what the picker draws: they differ
        // per model, and a menu built from the union below would offer levels that die at spawn.
        "choices": choices,
        // What an unpinned conversation runs on, so the window can NAME that state rather than
        // showing an empty selection and letting a person guess.
        "configured": config.configured_model(),
        // The same model as a person reads it, from the daemon's one naming function.
        "configured_label": crate::models::model_catalog::display_name(config.configured_model()),
        // The union, for the one case with no model chosen yet — the front door before anybody
        // picks. Also what the door validates against. Unaffected by the installed merge: an
        // installed model carries no effort levels of its own, `effort_levels()` keeps calling the
        // sync `catalogue()`, and there is nothing for the network read to add here.
        "efforts": config.effort_levels(),
        // Why `nucleos-models.yaml` is not the file in force, or null. A typo on the first load
        // serves the built-in catalogue, and the log line alone leaves the owner wondering where
        // their local model went. Read after `menu()` above, which is what refreshes it.
        "config_problem": crate::config::models_config_problem(),
    }))
}

/// The picker as groups: provider and family, newest first, named as a person reads them.
///
/// The config's cloud choices come first and win; the discovered models (live vendor lists, or the
/// built-in catalogue when there is no key or the vendor does not answer) are appended when the
/// runner may answer them. Never carries a key.
pub(super) async fn get_model_groups() -> Json<serde_json::Value> {
    // Only the chat picker calls this, and a chat of any kind may use either CLI. A request with no
    // `chat` is a new conversation, which `create_chat` validates as a chat.
    let asking = Asking::Chat;
    let (config, choices) = menu(asking).await;
    let mut cloud: Vec<crate::config::AssistantChoice> = choices
        .into_iter()
        .filter(|choice| choice.brain == "cloud")
        .map(|mut choice| {
            if choice.label == choice.id {
                choice.label = crate::models::model_catalog::display_name(&choice.id);
            }
            choice
        })
        .collect();
    let snapshot = crate::models::model_catalog::current().await;
    let mut created = std::collections::HashMap::new();
    for found in &snapshot.models {
        if let Some(at) = found.created {
            created.insert(found.id.clone(), at);
        }
        if cloud.iter().any(|choice| choice.id == found.id) {
            continue;
        }
        if crate::models::model_catalog::admits(
            config.active_runner(),
            rooted_of(asking),
            found.runner,
        ) {
            cloud.push(crate::models::model_catalog::as_choice(found));
        }
    }
    Json(serde_json::json!({
        "groups": crate::models::model_catalog::group(cloud, &created),
        "source": snapshot.source,
        "sources": snapshot.sources,
        "catalogue_version": crate::models::model_catalog::CATALOGUE_VERSION,
        "fetched_at": snapshot.fetched_at,
    }))
}

/// The commands a slash offers before there is a conversation to offer them for.
///
/// The front door has a box you type into and no chat behind it yet, so the chat-scoped route
/// cannot answer — and until this, a slash there did nothing at all. Typing `/` where you land is
/// the first thing anybody does.
///
/// The same `available` the chat route calls, with no project: personal commands and installed
/// plugins' travel wherever the conversation ends up, and a project's belong to a directory this
/// conversation does not have yet. So the front door offers exactly what will still be true after
/// the first message, and never a command that stops existing the moment the chat is opened.
pub(super) async fn get_commands(Query(query): Query<MentionQuery>) -> Json<serde_json::Value> {
    let typed: String = query.q.chars().take(MENTION_QUERY_LIMIT).collect();
    // Off the async runtime like its chat-scoped twin: this reads several directories and every
    // command file's front matter, which is short and is still disk on a request thread.
    let commands = tokio::task::spawn_blocking(move || {
        let available = crate::commands::available(None, crate::commands::home().as_deref());
        crate::commands::matching(&available, &typed)
    })
    .await
    .unwrap_or_default();

    Json(serde_json::json!({ "commands": commands }))
}

/// Which built-in tools a conversation can be told not to reach for.
///
/// Served rather than hardcoded in the window, because there is exactly one list and it is the one
/// `cli_args` writes into the flag. A second copy in the UI would offer a name the door refuses, or
/// — worse — stop offering one the daemon can still deny, so a restriction somebody set becomes
/// invisible and unremovable.
///
/// The list is deliberately WIDER than any single CLI version's tool set: see `BUILTIN_TOOLS`,
/// where the margin is the point. Denying a name this CLI does not have costs one line on stderr;
/// missing one it does have costs the restriction.
pub(super) async fn get_deniable_tools() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "tools": crate::execution::runner::BUILTIN_TOOLS }))
}
