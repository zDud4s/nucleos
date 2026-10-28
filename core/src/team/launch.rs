//! Team director and specialist launch machinery.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DirectorNode {
    Planning,
    Replanning,
    Delivering,
}

impl DirectorNode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Replanning => "replanning",
            Self::Delivering => "delivering",
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Launching
// ---------------------------------------------------------------------------------------------

/// The row a launch writes before anything is spawned, so the run exists for the hook to resolve a
/// mode from and for reconciliation to find if the daemon dies here.
///
/// **`read_untrusted` is set in THIS statement and never in a second one**, for the reason 0122
/// gives about `runs.from_relay_id`: a run born holding a colleague's words cannot be allowed to
/// exist without saying so. The gap between an INSERT and a follow-up UPDATE is a window in which a
/// node carrying a stranger's text, quoted into its own prompt, reads as a turn that has read
/// nothing — and every refusal in `hooks::team_decision` depends on that state not existing.
///
/// This is the receiving half of the trade `team_notes.rs` documents. Writing a note is `WritesOwn`,
/// so a specialist that read the web can still tell a colleague what it found; the taint travels
/// with the words and lands here. A node that receives a note may read on and may no longer ask —
/// which costs a department its alçada for that node, and buys the conversation.
pub(super) async fn open_run(
    state: &AppState,
    team_run_id: &str,
    prompt: &str,
    read_untrusted: bool,
) -> Result<(i64, String), sqlx::Error> {
    let session_id = crate::auth::generate_uuid_v4();
    let id = sqlx::query(
        "INSERT INTO runs (prompt, status, mode, session_id, team_run_id, read_untrusted,
                           created_at)
         VALUES (?, 'running', ?, ?, ?, ?, ?)",
    )
    .bind(prompt)
    .bind(TEAM_MODE)
    .bind(&session_id)
    .bind(team_run_id)
    .bind(i64::from(read_untrusted))
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&state.pool)
    .await?
    .last_insert_rowid();
    Ok((id, session_id))
}

/// The MCP config one team agent's invocation is launched with, and the file's lifetime.
///
/// One per NODE rather than one per team run: the server it starts is the Team box and it is named
/// by the node's own run (`--box team --run <id>`), because the box decides what to serve from that
/// run's `run_loadout` row. Every node therefore has a config no other node shares, and the guard
/// removes it when the node's task ends — a daemon that runs for months would otherwise leave one
/// in the temp directory for every invocation of every department it ever ran.
///
/// **The process id is in the name, and it is not decoration.** The temp directory is shared by
/// every process on the machine, so a name built only from the id is the SAME path in two of them
/// — and both write it and both delete it. On this machine that is not hypothetical: the daemon
/// runs while suites run, and several checkouts run suites at once, each with tests that use fixed
/// ids. One deleting the other's config mid-turn is a failure with no cause visible anywhere near
/// it. `transcribe.rs` already names its recordings this way, for the same reason.
struct TeamNodeMcp {
    path: std::path::PathBuf,
}

impl TeamNodeMcp {
    fn write(team_run_id: &str, node_run_id: i64) -> std::io::Result<Self> {
        let safe: String = team_run_id
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
            .collect();
        let path = std::env::temp_dir().join(format!(
            "nucleos-team-{}-{safe}-{node_run_id}.json",
            std::process::id()
        ));
        let exe = std::env::current_exe()?.to_string_lossy().into_owned();
        // What narrows a department's surface is `--allowedTools`, taken from the node's loadout.
        let body = serde_json::to_vec(&crate::assistant_types::build_team_mcp_config(
            &exe,
            node_run_id,
        ))
        .map_err(std::io::Error::other)?;
        crate::storage::write_atomic(&path, &body)?;
        Ok(Self { path })
    }
}

impl Drop for TeamNodeMcp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The key this run's agents authenticate with: `team:<id>.<secret>`, rebuilt from the stored half.
pub(super) async fn team_token(
    pool: &sqlx::SqlitePool,
    team_run_id: &str,
) -> Result<String, sqlx::Error> {
    let secret: String = sqlx::query_scalar("SELECT token FROM team_runs WHERE id = ?")
        .bind(team_run_id)
        .fetch_one(pool)
        .await?;
    Ok(format!("team:{team_run_id}.{secret}"))
}

pub(super) async fn launch_director(
    state: &AppState,
    run: &TeamRun,
    node: DirectorNode,
) -> Result<(), sqlx::Error> {
    let Some((team, director)) = team_and_director(state, run).await? else {
        return finish(
            state,
            run,
            "failed",
            "this team's director is no longer in the catalogue",
        )
        .await;
    };
    let mut prompt = match node {
        DirectorNode::Delivering => delivery_prompt(state, run, &team).await,
        _ => director_prompt(state, run, &team).await,
    };

    // The director is addressable like anybody else — `teams.director_agent_id` is an `agents.id`,
    // and it is on the roster every specialist is shown. That is deliberate and is the highest-value
    // message this feature will ever carry: "I found X, it is worth replanning" reaches the node that
    // decides what the next round does, instead of waiting for a folder somebody has to read.
    let waiting = crate::team_notes::pending(&state.pool, &run.id, &director.id)
        .await
        .unwrap_or_default();
    if let Some(block) = crate::team_notes::render(&waiting) {
        prompt.push_str(&block);
    }

    let (run_id, session_id) = open_run(state, &run.id, &prompt, !waiting.is_empty()).await?;

    // AFTER the run exists, which is `notes.rs`' hard-won rule and not a detail: a node that failed
    // to start never read the words, and a note consumed by it would be lost in silence — the one
    // failure the owner cannot see and cannot repeat.
    let ids: Vec<i64> = waiting.iter().map(|note| note.id).collect();
    if let Err(error) = crate::team_notes::mark_delivered(&state.pool, &ids, run_id).await {
        tracing::warn!(
            team_run = %run.id,
            run_id,
            %error,
            "the director was given its colleagues' notes and they were not marked delivered; they \
             will arrive again"
        );
    }

    // The marker is written BEFORE the spawn and cleared by the ingestion, which is what makes it a
    // marker and not a derived condition — the lesson migration 0051 bought, where a derived
    // `replanning` cost one wasted node per round.
    sqlx::query(
        "UPDATE team_runs SET director_node = ?, director_run_id = ?, updated_at = ? WHERE id = ?",
    )
    .bind(node.as_str())
    .bind(run_id)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(&run.id)
    .execute(&state.pool)
    .await?;

    spawn_agent(state, run, &director, run_id, session_id, prompt).await;
    Ok(())
}

pub(super) async fn launch_specialist(
    state: &AppState,
    run: &TeamRun,
    item: &TeamItem,
) -> Result<(), sqlx::Error> {
    let Some(agent) = crate::agent::get(&state.pool, &item.agent_id)
        .await
        .ok()
        .flatten()
    else {
        // The agent was deleted between the plan and the launch. One item fails; the run goes on.
        sqlx::query("UPDATE team_items SET state = 'failed' WHERE team_run_id = ? AND ordinal = ?")
            .bind(&run.id)
            .bind(item.ordinal)
            .execute(&state.pool)
            .await?;
        return Ok(());
    };

    let team = sqlx::query_scalar::<_, String>("SELECT mission FROM teams WHERE id = ?")
        .bind(&run.team_id)
        .fetch_one(&state.pool)
        .await?;
    let mut prompt = specialist_prompt(state, run, &team, item).await;

    // Read BEFORE the run is opened and stamped AFTER, and never both in one breath. A read taken
    // and claimed together looks identical from here right up until this launch fails — and a
    // colleague's words would then be marked delivered to a node that never existed.
    let waiting = crate::team_notes::pending(&state.pool, &run.id, &item.agent_id)
        .await
        .unwrap_or_default();
    if let Some(block) = crate::team_notes::render(&waiting) {
        prompt.push_str(&block);
    }

    let (run_id, session_id) = open_run(state, &run.id, &prompt, !waiting.is_empty()).await?;

    let ids: Vec<i64> = waiting.iter().map(|note| note.id).collect();
    if let Err(error) = crate::team_notes::mark_delivered(&state.pool, &ids, run_id).await {
        tracing::warn!(
            team_run = %run.id,
            run_id,
            %error,
            "a specialist was given its colleagues' notes and they were not marked delivered; they \
             will arrive again"
        );
    }

    sqlx::query(
        "UPDATE team_items SET state = 'running', run_id = ? WHERE team_run_id = ? AND ordinal = ?",
    )
    .bind(run_id)
    .bind(&run.id)
    .bind(item.ordinal)
    .execute(&state.pool)
    .await?;

    spawn_agent(state, run, &agent, run_id, session_id, prompt).await;
    Ok(())
}

/// Resolves and records what one team node is given: the knowledge block, the tools and the trace
/// of what it was shown. Returns the run's tools and the block to append to its prompt.
///
/// Spec 2026-10-08 section 6: a department agent reads the machine's knowledge, its team's and its
/// own, and no project's. The block is appended to the prompt because `state.runner` may be Codex.
/// Since R3 a team agent leaves a trace of what it was shown (this supersedes D15 for
/// departments), so a later outcome can credit the rows that were in front of it.
///
/// Shared by both engines, and that is the point: the local loop's `LocalToolBox` is derived from
/// the same `run_loadout` row the Claude path's Team box is served from, so the two cannot disagree
/// about which loadout tools a node holds.
async fn resolve_node_loadout(
    state: &AppState,
    run: &TeamRun,
    agent: &crate::agent::Agent,
    run_id: i64,
    with_tools: bool,
    prompt: &str,
) -> (crate::loadout::RunTools, String) {
    let loadout = crate::loadout::resolve(
        &state.pool,
        &crate::loadout::LoadoutInput {
            agent: Some(&agent.id),
            team: Some(&run.team_id),
            project: None,
            job: None,
            task_text: prompt,
            node: None,
            files: &[],
            // The tools and the context come out of the same resolution as the memory. A member
            // with tools runs in the Team box; one without holds none, and "none" is recorded as
            // an empty set like any other loadout. There is no working directory, so a folder ref
            // is listed in the index but never added.
            equipment: Some(crate::loadout::Equipment {
                tool_policy: if with_tools { "mcp_only" } else { "none" },
                base: crate::mcp_tools::TEAM_BASE,
                extras: crate::mcp_tools::TEAM_EXTRAS,
                managed_root: state.files_root.as_deref(),
                has_cwd: false,
            }),
        },
    )
    .await;
    if let Some(brief) = loadout.brief.as_ref()
        && let Err(error) = crate::brief::record(&state.pool, run_id, None, &brief.trace).await
    {
        tracing::warn!(team_run = %run.id, run_id, %error, "could not record what the team agent was shown");
    }
    // The row is written before the CLI starts: the route and the hook decide an extra from it, and
    // a run that cannot prove it holds a tool does not hold it.
    let run_tools = loadout.run.clone().unwrap_or_default();
    if let Err(error) = crate::loadout::record(&state.pool, run_id, &run_tools).await {
        tracing::warn!(team_run = %run.id, run_id, %error, "could not record the team agent's loadout");
    }
    (run_tools, loadout.block)
}

/// Spawns one invocation and lets it write its own terminal `runs` row.
///
/// Nothing awaits the task: the tick discovers the answer by reading the row, which is what makes a
/// restart mid-flight cost nothing. `spawn_registered` is what makes the run cancellable.
pub(super) async fn spawn_agent(
    state: &AppState,
    run: &TeamRun,
    agent: &crate::agent::Agent,
    run_id: i64,
    session_id: String,
    prompt: String,
) {
    let token = match team_token(&state.pool, &run.id).await {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(team_run = %run.id, %error, "a team run has no key; its agent cannot call a tool");
            // Nothing was spawned, so nothing else would ever close the row `open_run` wrote.
            fail_run(&state.pool, run_id, "the team run has no key").await;
            return;
        }
    };
    let with_tools = agent.tool_policy() == "mcp_only";
    let pool = state.pool.clone();

    // Spec 2026-10-08 section 6: see `resolve_node_loadout`. Resolved before the engine is known,
    // because the engine decides how the prompt is run and never what the node is shown, so every
    // engine holds the same tools.
    let (run_tools, block) =
        resolve_node_loadout(state, run, agent, run_id, with_tools, &prompt).await;

    // Spec 2026-10-20 sections 6 and 8: the effective engine launches. `resolve_agent_launch` reads
    // the pin from the agent's own row (a team pins nothing of its own) and, with none, says which
    // runners the router may choose from; it does no HTTP.
    let launch = match crate::agents::launch::resolve_agent_launch(
        state,
        agent,
        crate::agents::launch::Pins::default(),
        &prompt,
        &block,
        None,
    )
    .await
    {
        Ok(launch) => launch,
        Err(error) => {
            // Nothing was spawned, so nothing else would ever close the row `open_run` wrote.
            fail_run(&pool, run_id, &error.to_string()).await;
            return;
        }
    };
    let prompt = launch.prompt.clone();

    if let Some((crate::agents::capabilities::Engine::Local, model)) = &launch.pin {
        if model.is_empty() {
            fail_run(&pool, run_id, "a local agent with no model").await;
            return;
        }
        let model = model.clone();
        // The FACTORY's chat, where this built its own `runner::OllamaChat` against
        // `runner::OLLAMA_BASE_URL`. This was the last reader in the daemon still doing so: the
        // chat route and the council seat both ask `assistants::Assistants::local_chat`, which is
        // the one place that decides which server a local model gets. On an install whose
        // `local_engine` is `openai_compatible`, a team member left here reached an Ollama that may not be
        // running, may not hold the model, and may not be the machine that was paid for -- while
        // the owner's own chat reached the server they configured. Half a daemon on each engine.
        let chat = match state.assistants.local_chat(&model) {
            Ok(chat) => chat,
            Err(refusal) => {
                // The refusal's OWN sentence, for the reason `council.rs` gives at its seat:
                // `Refusal::message` is already what the chat route shows an operator for this
                // misconfiguration, and a second wording invented here would describe one problem
                // in two voices depending on which door somebody came through. The row is closed
                // rather than left `running` -- nothing was spawned, so nothing else ever closes it.
                fail_run(&pool, run_id, &refusal.message(crate::chats::Brain::Local)).await;
                return;
            }
        };
        // The TEAM's box, never `LocalToolBox::new`: `LOCAL_TOOLS` carries `create_run` and
        // `create_job`, and the local path never passes through `hooks.rs` at all.
        //
        // Its loadout was resolved above, BEFORE the loop starts: the loadout row is what the
        // daemon's routes admit `request_tool` and `read_context` by, and the same tools then
        // decide which of those two this box offers. A member without tools holds none
        // (`NoTools` below) and its row is recorded empty, like any other loadout. The block is in
        // `prompt` for the reason it is on the Claude path -- it is what lists the context files
        // `read_context` can open.
        let tools =
            crate::mcp_tools::LocalToolBox::for_team(daemon_url(), token, pool.clone(), run_id)
                .with_loadout(&run_tools.tools);
        let timeout = state.run_timeout;
        crate::execution::runs::spawn_registered(state, run_id, async move {
            let taint = std::sync::atomic::AtomicBool::new(false);
            let empty = crate::models::local_agent::NoTools;
            let tool_box: &dyn crate::models::local_agent::ToolBox =
                if with_tools { &tools } else { &empty };
            let turn = tokio::time::timeout(
                timeout,
                crate::models::local_agent::run_turn(
                    // `as_ref`, because what the factory hands back is a `Box<dyn LocalChat>`
                    // and the loop takes the trait object itself -- the box is the seam, not
                    // the value.
                    chat.as_ref(),
                    tool_box,
                    crate::models::local_agent::SYSTEM_PROMPT,
                    &[],
                    &prompt,
                    &taint,
                ),
            )
            .await;
            if taint.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = crate::execution::runs::mark_untrusted_context(&pool, run_id).await;
            }
            let completed_at = chrono::Utc::now().to_rfc3339();
            match turn {
                Ok(Ok(turn)) => record_local_turn(&pool, run_id, &turn, &completed_at).await,
                Ok(Err(error)) => fail_run(&pool, run_id, &error.to_string()).await,
                Err(_) => {
                    crate::storage::settle_write("team local run (timed out)", run_id, || {
                        sqlx::query(
                            "UPDATE runs SET status = 'timed_out', cost_usd = 0, completed_at = ?
                         WHERE id = ? AND status = 'running'",
                        )
                        .bind(&completed_at)
                        .bind(run_id)
                        .execute(&pool)
                    })
                    .await;
                }
            }
        });
        stop_if_the_run_ended(state, &run.id, run_id).await;
        return;
    }

    // The node's own config, held by a guard that goes into the launch task below so the file lives
    // exactly as long as the node does.
    let mcp_guard = if with_tools {
        match TeamNodeMcp::write(&run.id, run_id) {
            Ok(guard) => Some(guard),
            Err(error) => {
                tracing::warn!(team_run = %run.id, run_id, %error, "could not write the team agent's MCP config");
                None
            }
        }
    } else {
        None
    };
    let mcp_config = mcp_guard.as_ref().map(|guard| guard.path.clone());

    // What the session is told it may use. An unreadable row tells it the narrowest thing, a team
    // of one at `normal` — the direction that cannot overstate what `round_width` will open.
    let capacity = run_capacity(state, run).await.unwrap_or_else(|error| {
        tracing::warn!(team_run = %run.id, %error, "could not read the run's speed; the session is told normal");
        crate::speed::Capacity::solo()
    });

    // The run's money, held to the seat as well as between seats. Only for a runner that enforces
    // it: `CodexCliRunner` refuses any request carrying a ceiling, so on a Codex daemon a seat with
    // one would never launch at all.
    let max_budget_usd = if state.runner.honours_max_budget() {
        seat_budget_of(state, run).await
    } else {
        None
    };

    let mcp_team_run = mcp_guard.as_ref().map(|_| run_id);
    // A closure, because the same request is built twice when an answer fails its output shape:
    // `RunRequest` owns channel ends and is not `Clone`.
    let build =
        move |prompt: String,
              session_id: String,
              model: Option<String>,
              effort: Option<String>,
              tool_policy: crate::execution::runner::ToolPolicy,
              max_budget_usd: Option<f64>| crate::execution::runner::RunRequest {
            prompt,
            // The RUN's own key, never `state.token`. `auth::TEAM_ROUTES` is what it reaches.
            env: crate::execution::runs::run_env(&token, run_id, None, capacity),
            // No working directory, exactly as a council seat has none — which is also why the
            // `PreToolUse` hook may never fire and why the route table has to hold alone.
            cwd: None,
            permission: crate::execution::runner::Permission::Default,
            resume_session_id: None,
            mcp_config: mcp_config.clone(),
            mcp_job: None,
            // The node's own run id, the one `TeamNodeMcp::write` put on `--run`; priced as the Team box.
            mcp_team_run,
            // A member with tools is offered the Team box (`--box team --run <node>`), which serves
            // only what its loadout row lists plus the box's base. `allowed_mcp_tools` below is that
            // same set handed to the CLI as `--allowedTools`.
            tool_policy,
            progress_timeout: None,
            max_turns: Some(crate::execution::runner::DEFAULT_MAX_TURNS),
            session_id: Some(session_id),
            fork_session: false,
            include_partial_messages: false,
            // `steerable` with no `messages`, for the reason `council.rs` documents at length: it is the
            // only way to keep the prompt off the command line, and a director's prompt carries the
            // whole folder index. Windows caps a command line at 32 767 characters, and the first real
            // council died exactly there.
            images: Vec::new(),
            steerable: true,
            classifier_governs_tools: false,
            messages: None,
            request_tx: None,
            mcp_servers: Vec::new(),
            model,
            effort,
            fallback_model: Vec::new(),
            add_dirs: Vec::new(),
            max_budget_usd,
            agents: Vec::new(),
            append_system_prompt: None,
            denied_tools: Vec::new(),
            session_name: None,
            context_window: None,
            // The loadout's effective set: the box's base plus what the owner approved for this agent
            // or its team. Empty for a member without tools.
            allowed_mcp_tools: Some(run_tools.tools.clone()),
            background_tasks: false,
        };

    // The model the row pins, else the one the agent names beside no engine (which the router may
    // still replace under `apply`). Spec 2026-10-20 section 6: a model alone pins no engine.
    let pinned_model = launch
        .pin
        .as_ref()
        .map(|(_, model)| model.clone())
        .filter(|model| !model.is_empty())
        .or_else(|| agent.model.clone());
    let pin_kind = launch
        .pin
        .as_ref()
        .and_then(|(engine, _)| runner_kind_of(*engine));
    let runners = launch.runners.clone();
    // Only a package agent has an output shape its answer is held to; a user agent's is free.
    let shape = agent.is_package().then_some(launch.output);
    let first_policy = if with_tools {
        crate::execution::runner::ToolPolicy::McpOnly
    } else {
        crate::execution::runner::ToolPolicy::None
    };
    let request = build(
        prompt,
        session_id,
        pinned_model,
        None,
        first_policy,
        max_budget_usd,
    );

    let seat_state = state.clone();
    let timeout = state.run_timeout;
    crate::execution::runs::spawn_registered(state, run_id, async move {
        // Moved in so the node's config file is removed when the task ends — normally, by timeout,
        // or aborted by a cancel (which drops the future and with it the guard).
        let _mcp_guard = mcp_guard;
        // Asked inside the task, so a slow adviser delays this seat and never the caller handing
        // out a round. `off` asks nothing and leaves the request as built above.
        // Its outcome is reported by `ingest_landed_items`, against the `route_decision_id` this
        // writes on the run's row, once the item lands.
        let mut request = request;
        let seat = match pick_seat(&seat_state, run_id, pin_kind, &runners, &mut request).await {
            Ok(seat) => seat,
            Err(why) => {
                fail_run(&pool, run_id, &why).await;
                return;
            }
        };
        let runner = seat.runner;
        // `codex exec` refuses `ToolPolicy::None`, which it cannot express; `McpOnly` is the
        // strictest it can, and a team node has no working directory for anything wider.
        if seat.kind == Some(crate::models::route_advice::RunnerKind::Codex) {
            request.tool_policy = crate::execution::runner::ToolPolicy::McpOnly;
        }
        // A ceiling the chosen runner would refuse is dropped, as the unrouted path never sets it.
        if !runner.honours_max_budget() {
            request.max_budget_usd = None;
        }
        // What a retry must repeat: the same engine and model, and the same everything else.
        let (model, effort) = (request.model.clone(), request.effort.clone());
        let (policy, budget) = (request.tool_policy, request.max_budget_usd);
        let original_prompt = request.prompt.clone();
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        // How much window this agent is holding, mirrored into the row while it runs.
        //
        // This path is a launcher of its own -- it does not go through `runs::spawn_run` -- and
        // for a long time that meant a team item, the ONE kind of run the pressure reading is
        // about, recorded none of the four things it reads. `run_prompt_with_context_fill` and the
        // mirror are what `runs.rs` does, and the reason is the same: a cancel aborts this task
        // and writes only a status, so an in-memory number would be dropped with the run most
        // worth inspecting.
        let context_fill = std::sync::Arc::new(std::sync::Mutex::new(None));
        let _live_context_fill = crate::execution::runs::mirror_context_fill(
            &pool,
            run_id,
            std::sync::Arc::clone(&context_fill),
        );
        let run_once = |request: crate::execution::runner::RunRequest| {
            tokio::time::timeout(
                timeout,
                runner.run_prompt_with_context_fill(
                    request,
                    session_tx.clone(),
                    transcript.clone(),
                    std::sync::Arc::clone(&context_fill),
                ),
            )
        };
        let mut outcome = run_once(request).await;
        // Spec 2026-10-20 section 7: a package agent's answer that does not fit its output shape is
        // asked for ONCE more, on the same engine and model, with the validator's report appended.
        let mut unshaped = None;
        if let Some(shape) = &shape
            && let Ok(Ok(first)) = &outcome
            && first.exit_code == 0
            && let Err(report) = crate::agents::output::validate(shape, &first.stdout)
        {
            // A fresh session id: the first one is spent, and a CLI refuses to reopen it.
            let retry = build(
                crate::agents::output::retry_prompt(&original_prompt, &report),
                crate::auth::generate_uuid_v4(),
                model,
                effort,
                policy,
                budget,
            );
            outcome = run_once(retry).await;
            if let Ok(Ok(second)) = &outcome
                && second.exit_code == 0
                && let Err(report) = crate::agents::output::validate(shape, &second.stdout)
            {
                unshaped = Some(report);
            }
        }
        if let Some(report) = unshaped {
            fail_run(
                &pool,
                run_id,
                &format!("the answer does not follow the required output format: {report}"),
            )
            .await;
            // Reported only when the seat was routed: the router learns its choice did not work.
            if let (Some(router), Some(decision)) = (seat.router, seat.decision) {
                crate::models::route_advice::report_detached(
                    router,
                    decision,
                    crate::models::route_advice::Outcome::Fail,
                );
            }
            return;
        }
        let completed_at = chrono::Utc::now().to_rfc3339();
        match outcome {
            Err(_) => {
                let partial = transcript.lock().unwrap().clone();
                // No outcome on this branch, so `compacted` cannot be known -- but the partial
                // transcript is the whole record of a run that went too far, which is the case the
                // reading exists to see.
                crate::storage::settle_write("team agent run (timed out)", run_id, || {
                    sqlx::query(
                        "UPDATE runs SET status = 'timed_out', stdout = ?, context_fill = ?,
                            context_peak = ?, tools_used = ?, completed_at = ?
                     WHERE id = ? AND status = 'running'",
                    )
                    .bind(&partial)
                    .bind(crate::execution::runs::observed_context_fill(
                        &context_fill,
                        &partial,
                    ))
                    .bind(crate::execution::runs::peak_of(&partial))
                    .bind(crate::execution::runs::tools_of(&partial))
                    .bind(&completed_at)
                    .bind(run_id)
                    .execute(&pool)
                })
                .await;
            }
            Ok(Ok(outcome)) if outcome.exit_code == 0 => {
                crate::storage::settle_write("team agent run (completed)", run_id, || {
                    sqlx::query(
                        "UPDATE runs SET status = 'completed', exit_code = 0, stdout = ?, cost_usd = ?,
                            input_tokens = ?, output_tokens = ?, cache_read_tokens = ?,
                            num_turns = ?, context_fill = ?, context_peak = ?, compacted = ?,
                            tools_used = ?, completed_at = ?
                     WHERE id = ? AND status = 'running'",
                    )
                    .bind(&outcome.stdout)
                    .bind(outcome.cost_usd)
                    .bind(outcome.input_tokens)
                    .bind(outcome.output_tokens)
                    .bind(outcome.cache_read_tokens)
                    .bind(outcome.num_turns)
                    .bind(crate::execution::runs::observed_context_fill(
                        &context_fill,
                        &outcome.stdout,
                    ))
                    .bind(crate::execution::runs::peak_of(&outcome.stdout))
                    .bind(outcome.compacted)
                    .bind(crate::execution::runs::tools_of(&outcome.stdout))
                    .bind(&completed_at)
                    .bind(run_id)
                    .execute(&pool)
                })
                .await;
            }
            Ok(Ok(outcome)) => {
                // An agent that started, spoke, and exited non-zero has a stream like any other,
                // and `num_turns` is written here too: the denominator was missing on every
                // non-`completed` path, which is exactly where the fullest runs are.
                crate::storage::settle_write("team agent run (failed)", run_id, || {
                    sqlx::query(
                        "UPDATE runs SET status = 'failed', exit_code = ?, stdout = ?, stderr = ?,
                            cost_usd = ?, num_turns = ?, context_fill = ?, context_peak = ?,
                            compacted = ?, tools_used = ?, completed_at = ?
                     WHERE id = ? AND status = 'running'",
                    )
                    .bind(outcome.exit_code)
                    .bind(&outcome.stdout)
                    .bind(&outcome.stderr)
                    .bind(outcome.cost_usd)
                    .bind(outcome.num_turns)
                    .bind(crate::execution::runs::observed_context_fill(
                        &context_fill,
                        &outcome.stdout,
                    ))
                    .bind(crate::execution::runs::peak_of(&outcome.stdout))
                    .bind(outcome.compacted)
                    .bind(crate::execution::runs::tools_of(&outcome.stdout))
                    .bind(&completed_at)
                    .bind(run_id)
                    .execute(&pool)
                })
                .await;
            }
            Ok(Err(error)) => fail_run(&pool, run_id, &error.to_string()).await,
        }
    });
    stop_if_the_run_ended(state, &run.id, run_id).await;
}

/// The runner kind a CLI engine launches on; the local engine has none.
fn runner_kind_of(
    engine: crate::agents::capabilities::Engine,
) -> Option<crate::models::route_advice::RunnerKind> {
    use crate::agents::capabilities::Engine;
    use crate::models::route_advice::RunnerKind;
    match engine {
        Engine::Claude => Some(RunnerKind::Claude),
        Engine::Codex => Some(RunnerKind::Codex),
        Engine::Local => None,
    }
}

/// Where one cloud team node runs: the runner, its kind when known, and the router decision (with
/// the router, to report the outcome to) when the seat was routed.
struct Seat {
    runner: std::sync::Arc<dyn crate::execution::runner::CommandRunner>,
    kind: Option<crate::models::route_advice::RunnerKind>,
    decision: Option<String>,
    router: Option<std::sync::Arc<crate::models::route_advice::Router>>,
}

/// Spec 2026-10-20 section 8: the effective engine launches, where a `codex` member used to run on
/// the daemon's own runner. A pinned engine is used as pinned; with none, the router chooses among
/// `runners`. Without a router there is nothing to choose with and the daemon's runner launches, as
/// it always did. May rewrite `request.model` and `request.effort` (see the two seat routers).
async fn pick_seat(
    state: &AppState,
    run_id: i64,
    pin: Option<crate::models::route_advice::RunnerKind>,
    runners: &[crate::models::route_advice::RunnerKind],
    request: &mut crate::execution::runner::RunRequest,
) -> Result<Seat, String> {
    use crate::models::seat_advice::{route_agent_seat, route_team_seat};
    let router = state.runner.router();
    let (kind, decision) = match (&router, pin) {
        (None, pin) => (pin, None),
        (Some(router), Some(kind)) => {
            // A seat pinned to the primary's CLI is routed within it, as a seat always was; a
            // pin to another CLI is not narrowed, and is not advised.
            let decision = if kind == router.primary.kind {
                route_team_seat(&state.pool, Some(router.clone()), run_id, request).await
            } else {
                None
            };
            (Some(kind), decision)
        }
        (Some(router), None) => {
            let (kind, decision) = route_agent_seat(&state.pool, router, run_id, request, runners)
                .await
                .map_err(|error| error.to_string())?;
            (Some(kind), decision)
        }
    };
    let runner = match (&router, kind) {
        (Some(router), Some(kind)) if kind != router.primary.kind => router
            .available(kind)
            .map(|available| available.runner.clone())
            .ok_or_else(|| format!("the {} engine is not available to this daemon", kind.as_str()))?,
        // No router: the only runner the daemon can name for Codex is the chat one. Anything else
        // runs on the daemon's own, which is what launched every member before this.
        (None, Some(crate::models::route_advice::RunnerKind::Codex)) => state
            .assistants
            .cli_runner("codex", request.model.as_deref())
            .unwrap_or_else(|| {
                tracing::warn!(run_id, "a codex team member and no codex runner configured; launching on the daemon's own");
                state.runner.clone()
            }),
        _ => state.runner.clone(),
    };
    Ok(Seat {
        runner,
        kind,
        decision,
        router,
    })
}

/// Stops a node whose team run ended while the tick that launched it was on its way here.
///
/// The tick advances a SNAPSHOT of the live runs, and nothing between that read and the spawn
/// looks at the row again: a cancel that lands in between terminates what it can find (the nodes
/// already holding an abort handle), marks the run cancelled, and finishes. The tick then opens a
/// node for a run that has ended and spawns it, and nobody is left to kill it: the department
/// reads cancelled while one of its agents goes on spending.
///
/// Asked AFTER `spawn_registered`, which is what closes the window rather than narrowing it. A
/// cancel that marked the run before this read is seen here, and the handle exists, so the kill
/// lands; a cancel that marks it after this read finds the node `running` with a handle in its
/// second pass (`lifecycle::cancel`). An unreadable or missing row stops nothing: this is the
/// guard against a race, not the authority over whether a node may run.
async fn stop_if_the_run_ended(state: &AppState, team_run_id: &str, run_id: i64) {
    let team_state = sqlx::query_scalar::<_, String>("SELECT state FROM team_runs WHERE id = ?")
        .bind(team_run_id)
        .fetch_optional(&state.pool)
        .await;
    match team_state {
        Ok(Some(team_state)) if !is_live(&team_state) => {}
        Ok(_) => return,
        Err(error) => {
            tracing::warn!(team_run = %team_run_id, run_id, %error, "could not re-read the team run after a launch");
            return;
        }
    }
    tracing::info!(team_run = %team_run_id, run_id, "the team run ended while this node was launched; stopping it");
    crate::execution::runs::finalize_termination(state, run_id, "cancelled").await;
    // The launch stamped these after the cancel cleared them, so a node that never got to work
    // would otherwise read as still in flight on a run that has ended.
    crate::storage::settle_write("team item of a stopped run", run_id, || {
        sqlx::query(
            "UPDATE team_items SET state = 'failed'
         WHERE team_run_id = ? AND run_id = ? AND state = 'running'",
        )
        .bind(team_run_id)
        .bind(run_id)
        .execute(&state.pool)
    })
    .await;
    crate::storage::settle_write("team run director of a stopped run", run_id, || {
        sqlx::query(
            "UPDATE team_runs SET director_node = 'none' WHERE id = ? AND director_run_id = ?",
        )
        .bind(team_run_id)
        .bind(run_id)
        .execute(&state.pool)
    })
    .await;
}

/// The terminal write of a local agent's turn.
///
/// `cost_usd = 0` and not NULL: NULL is what `budget.rs` time-approximates a cost for, so leaving
/// it unset would charge the window for electricity.
///
/// `num_turns` carries the turn's TOOL CALLS, and it is the one measurement this path can make.
/// There is no `stream-json` here — no usage lines, so no `context_fill` and no peak, and nothing
/// that could say whether the model summarised itself — but a count of calls is exactly the
/// denominator `pressure::steps_of` wants, and it is a finer one than a turn count: `num_turns` is
/// the fallback precisely because a turn with twelve tools and a turn with one count the same, and
/// this number does not have that problem.
///
/// `tools_used` stays NULL on purpose. `Turn` carries a count and not the names, and a synthetic
/// list of the right length would be read as a record of which tools ran. NULL means nobody asked;
/// an invented list would be an answer, and a false one.
///
/// Separate from the branch that calls it because what it does with a turn is the part that can
/// be wrong, independently of who answered. This used to say the surrounding task could not be
/// exercised at all without a live Ollama, and that was true for exactly as long as the local
/// path built its own `OllamaChat` against `OLLAMA_BASE_URL`: it asks the factory now, so a test
/// can hand it a chat pointed anywhere and `a_local_team_member_is_served_by_the_configured_engine`
/// does.
pub(super) async fn record_local_turn(
    pool: &sqlx::SqlitePool,
    run_id: i64,
    turn: &crate::models::local_agent::Turn,
    completed_at: &str,
) {
    crate::storage::settle_write("team local run (completed)", run_id, || {
        sqlx::query(
            "UPDATE runs SET status = 'completed', exit_code = 0, stdout = ?, cost_usd = 0,
                num_turns = ?, completed_at = ?
         WHERE id = ? AND status = 'running'",
        )
        .bind(&turn.answer)
        .bind(turn.tool_calls as i64)
        .bind(completed_at)
        .bind(run_id)
        .execute(pool)
    })
    .await;
}

async fn fail_run(pool: &sqlx::SqlitePool, run_id: i64, why: &str) {
    crate::storage::settle_write("team local run (failed)", run_id, || {
        sqlx::query(
            "UPDATE runs SET status = 'failed', stderr = ?, cost_usd = 0, completed_at = ?
         WHERE id = ? AND status = 'running'",
        )
        .bind(why)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(run_id)
        .execute(pool)
    })
    .await;
}

async fn team_and_director(
    state: &AppState,
    run: &TeamRun,
) -> Result<Option<(Team, crate::agent::Agent)>, sqlx::Error> {
    let team: Option<Team> = sqlx::query_as(
        "SELECT id, name, mission, director_agent_id, max_rounds, max_parallel, budget_usd,
                max_open_actions, max_live_runs, created_at, updated_at
         FROM teams WHERE id = ?",
    )
    .bind(&run.team_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(team) = team else {
        return Ok(None);
    };
    let director = crate::agent::get(&state.pool, &team.director_agent_id)
        .await
        .ok()
        .flatten();
    Ok(director.map(|director| (team, director)))
}
