//! What a run is allowed: its environment, job-node MCP token, untrusted-context marks, tool policy and timeouts.

/// Autonomous runs (mode shadow/worktree) retry a launch failure up to this many TOTAL attempts.
/// Only the pre-execution launch-failure path retries (the CLI never ran, so no work is double-applied);
/// a run that executed and failed, and a timeout, are never retried.
pub(super) const MAX_AUTONOMOUS_ATTEMPTS: u32 = 2;

/// The environment every autopilot/assistant CLI run needs: the daemon URL + token so the
/// PreToolUse hook can call back, and NUCLEOS_RUN_ID (== runs.id, spec §3.3) so the hook echoes it
/// back and the core can validate — and, for a `pending_approval`, terminate — the right run.
///
/// `token` is the caller's decision and not read from `state` on purpose. It used to be
/// `state.token` for every run, which handed a `shadow` run — a mode that has Bash and whose
/// classifier calls `echo $NUCLEOS_DAEMON_TOKEN` a `read-local` action — the key that approves
/// proposals and disengages the kill switch. Autonomous runs now get their own scoped key
/// (`auth::mint_run_token`); only orchestrator turns still carry the control token, and only
/// because `ToolPolicy::McpOnly` leaves them nothing to read it with.
///
/// `artifacts` is the directory a job's nodes hand work to each other through, and is `Some` only
/// for a node of a job. An ordinary run has no successor to write to, and handing it the variable
/// anyway would advertise a protocol nothing in its prompt describes.
///
/// `capacity` is the speed/ceiling contract every session is handed (`speed::Capacity::env`), set
/// here once rather than at each launcher so no launcher can forget it. A lone run passes
/// `Capacity::solo()`; a department's session passes what its round was granted.
pub fn run_env(
    token: &str,
    id: i64,
    artifacts: Option<&std::path::Path>,
    capacity: crate::speed::Capacity,
) -> Vec<(String, String)> {
    let mut env = vec![
        // Derived, never written out again. This is the address every tool a run calls will use to
        // come back, and a literal here is what lets a daemon bind one port and send its own tools
        // to another — which presents as every tool answering 404, from a route that exists.
        (
            "NUCLEOS_DAEMON_URL".to_string(),
            crate::daemon_client::daemon_url(),
        ),
        ("NUCLEOS_DAEMON_TOKEN".to_string(), token.to_string()),
        ("NUCLEOS_RUN_ID".to_string(), id.to_string()),
    ];
    if let Some(path) = artifacts {
        env.push((
            "NUCLEOS_JOB_ARTIFACTS".to_string(),
            path.to_string_lossy().into_owned(),
        ));
    }
    env.extend(capacity.env());
    env
}

pub struct JobNodeMcp {
    pub(super) path: std::path::PathBuf,
    pub(super) job_id: i64,
    /// The tool names the CLI is allowed to call on the box's server. Today's fixed list until a
    /// loadout narrows or widens it.
    pub(super) allowed: Vec<String>,
    /// The directories the CLI is widened to with `--add-dir`; empty unless a loadout names some.
    pub(super) add_dirs: Vec<std::path::PathBuf>,
}

impl JobNodeMcp {
    /// Launches with the tools and directories the node's resolved loadout names, instead of the
    /// fixed list.
    pub fn with_loadout(mut self, run: &crate::loadout::RunTools) -> Self {
        // Only NucleOS tools: the engine built-ins a loadout can list (`WebFetch`, `WebSearch`) are
        // the hook's business, and `--allowedTools` would otherwise read them as `mcp__nucleos__WebFetch`.
        self.allowed = run
            .tools
            .iter()
            .filter(|tool| !crate::loadout::is_engine_builtin(tool))
            .cloned()
            .collect();
        self.add_dirs = run.add_dirs.clone();
        self
    }

    /// None when the run belongs to no job, or the file could not be written (warn; the run then
    /// launches with no MCP server, as a team run does).
    pub fn for_run(run_id: i64, job_id: Option<i64>) -> Option<Self> {
        let job_id = job_id?;
        let path =
            std::env::temp_dir().join(format!("nucleos-job-{}-{run_id}.json", std::process::id()));
        let written = (|| -> std::io::Result<()> {
            let exe = std::env::current_exe()?;
            let config =
                crate::assistant_types::build_job_node_mcp_config(&exe.to_string_lossy(), job_id);
            let bytes = serde_json::to_vec(&config).map_err(std::io::Error::other)?;
            crate::storage::write_atomic(&path, &bytes)
        })();
        if let Err(error) = written {
            tracing::warn!(run_id, job_id, %error, "could not write job-node MCP config");
            return None;
        }
        Some(Self {
            path,
            job_id,
            allowed: crate::mcp_tools::JOB_NODE_TOOLS
                .iter()
                .map(|tool| (*tool).to_owned())
                .collect(),
            add_dirs: Vec::new(),
        })
    }
}

impl Drop for JobNodeMcp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Mints a run's own daemon key and stores its secret, returning what goes in the environment.
///
/// Called before the CLI is spawned, never after: the hook fires on the run's first tool call, and
/// a secret that lands in the row a moment later would 401 that call for reasons no log explains.
/// A failure to store is not fatal — the run proceeds with a key that authenticates nothing, so its
/// tool calls are refused rather than ungoverned, which is the right direction to fail in.
pub async fn mint_run_token(pool: &sqlx::SqlitePool, id: i64) -> String {
    let (token, secret) = crate::auth::mint_run_token(id);
    if let Err(error) = sqlx::query("UPDATE runs SET token = ? WHERE id = ?")
        .bind(&secret)
        .bind(id)
        .execute(pool)
        .await
    {
        tracing::warn!(
            run_id = id,
            %error,
            "could not store the run's token — its tool calls will be refused"
        );
    }
    token
}

/// Records that a run's context now holds text a third party wrote.
///
/// One direction only: a turn that has read a mail body cannot un-read it, and every tool call that
/// follows in that turn is downstream of it.
///
/// On the row rather than in memory because the two readers are not the same task — `hooks.rs`
/// decides the next tool call, `assistant.rs` decides whether the turn may leave a resumable
/// session behind — and because a daemon restart in between must not lose it. A resumed session
/// carries the same words whether or not the process that read them is still alive.
///
/// Only the FIRST mark of a turn notifies: the UPDATE matches a row that is not yet marked, so a
/// second call changes nothing and posts nothing.
pub async fn mark_untrusted_context(pool: &sqlx::SqlitePool, id: i64) -> sqlx::Result<()> {
    let newly: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "UPDATE runs SET read_untrusted = 1 WHERE id = ? AND read_untrusted = 0
         RETURNING chat_id, mode",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    if let Some((Some(chat_id), Some(mode))) = newly
        && mode == "assistant"
    {
        notify_untrusted(pool, id, &chat_id).await;
    }
    Ok(())
}

/// Tells a ROOTED conversation (`chats.cwd` set, the test `hooks.rs::rooted_turn` uses) that its
/// turn is now read-only. Fail-closed stays with the UPDATE: an error here is only logged.
pub(super) async fn notify_untrusted(pool: &sqlx::SqlitePool, run_id: i64, chat_id: &str) {
    match crate::chats::cwd_of(pool, chat_id).await {
        Ok(Some(_)) => {
            if let Err(error) = crate::chats::notices::post_system(
                pool,
                chat_id,
                crate::chats::notices::KIND_UNTRUSTED,
                run_id,
                crate::chats::notices::UNTRUSTED_NOTICE,
            )
            .await
            {
                tracing::warn!(
                    run_id,
                    %error,
                    "could not announce the read-only mark to its conversation"
                );
            }
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(run_id, %error, "could not tell whether the conversation is rooted");
        }
    }
}

/// Writes down WHICH stranger, once [`mark_untrusted_context`] has recorded that there was one.
///
/// Separate from the marking above, and the separation is the security argument rather than tidiness.
/// The mark fails closed: `hooks.rs` refuses the read when it cannot be written, because a turn
/// holding a stranger's words with no record of it is the state every later refusal depends on not
/// existing. This one must NOT fail closed — a provenance able to refuse a read would be a
/// convenience holding a veto over the pillar's main verb — so it is called after the mark, its
/// error is logged by the caller, and the read proceeds either way.
///
/// The cost is stated where it is paid: a run can carry the mark and no rows here, and a reader has
/// to say "not recorded" instead of "read nothing". Those are different facts and only one of them
/// is ever true of a turn the barrier refused.
///
/// `arguments` is the call's arguments verbatim. Not prettied into a source string: `browser_open`
/// and `web_read` both carry the url that decides the question, and a per-tool extractor would be a
/// second per-tool table beside `TOOL_EFFECTS` for someone to keep in step by hand. `None` is for
/// the entries that come from no call at all, where `tool` names the source in words rather than
/// borrowing a tool name for a call that never happened.
pub async fn record_untrusted_read(
    pool: &sqlx::SqlitePool,
    id: i64,
    tool: &str,
    arguments: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO run_untrusted_reads (run_id, tool, arguments, at) VALUES (?, ?, ?, ?)")
        .bind(id)
        .bind(tool)
        .bind(arguments)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .map(|_| ())
}

/// What this run has read, oldest first, as the JSON a proposal carries away.
///
/// `None` when nothing was recorded, which a caller must not render as "read nothing" — see
/// [`record_untrusted_read`] for why the two are different and why the empty case is real.
///
/// Read once, at the moment a refusal is written, and copied onto the proposal: `runs` rows are
/// pruned on their own schedule, and a record answering "where did this idea come from" with a
/// dangling id answers nothing.
pub async fn untrusted_reads_json(
    pool: &sqlx::SqlitePool,
    id: i64,
) -> sqlx::Result<Option<String>> {
    let rows = sqlx::query_as::<_, (String, Option<String>, String)>(
        "SELECT tool, arguments, at FROM run_untrusted_reads WHERE run_id = ? ORDER BY rowid",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    // Silence and an empty answer are not the same fact, so they do not share a representation. A
    // turn refused by the barrier read something by definition; an empty list here means the
    // recording failed, and handing that back as `Some("[]")` would let a reader print "read
    // nothing" over a turn that read plenty.
    if rows.is_empty() {
        return Ok(None);
    }
    let listed: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(tool, arguments, at)| {
            serde_json::json!({ "tool": tool, "arguments": arguments, "at": at })
        })
        .collect();
    Ok(Some(serde_json::Value::Array(listed).to_string()))
}

/// Whether this run has read third-party text.
///
/// A row that is not there answers `true`. The callers use this to decide whether to REFUSE
/// something, so the absent-row case has to fail in the direction that refuses: an id naming no run
/// is not evidence that a turn is clean.
pub async fn read_untrusted_context(pool: &sqlx::SqlitePool, id: i64) -> sqlx::Result<bool> {
    sqlx::query_scalar::<_, i64>("SELECT read_untrusted FROM runs WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map(|flag| flag.unwrap_or(1) != 0)
}

/// Whether this run, or any LATER run of the same chat, has read third-party text. A task that
/// outlives its turn runs in a process that later turns also fed, so it is never cleaner than them.
/// A row that is not there answers `true`, as in `read_untrusted_context`.
pub async fn read_untrusted_since(pool: &sqlx::SqlitePool, id: i64) -> sqlx::Result<bool> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(MAX(read_untrusted), 1) FROM runs
          WHERE id = ? OR (id > ? AND chat_id = (SELECT chat_id FROM runs WHERE id = ?))",
    )
    .bind(id)
    .bind(id)
    .bind(id)
    .fetch_one(pool)
    .await
    .map(|flag| flag != 0)
}

/// PURE: barrier 1 of spec §5.5 — the tools a run in `mode` launches with.
///
/// One function rather than the same `if` at each reader, because the two readers ask opposite
/// questions about the same fact: `create_run_inner` asks what to launch, and
/// `http::post_run_message` asks whether a live run may be given a second author. A toolless run
/// exists to read words nobody vouches for, so it is the last run that may be spoken to — and the day
/// a second such mode appears, both answers have to change together or the barrier narrows to one
/// mode without anyone deciding to narrow it.
/// `'team'` answers `None` and that is a deliberate under-statement rather than the truth about
/// every team run.
///
/// A specialist may well hold `McpOnly` — its agent's `tool_policy` decides, and `team.rs` builds
/// the `RunRequest` itself, so this function never launches one. What it is actually asked, by both
/// its other readers, is whether a run of this mode may gain a SECOND AUTHOR: `http::post_run_message`
/// refuses `None`, and `create_run_inner` refuses to create such a run `steerable`. For a
/// department the answer is no in every case, so the most restrictive value is the honest one to
/// return — and the alternative, `Unrestricted`, is worse than merely wrong: it is what this
/// function returns for everything it does not recognise, and it would have handed a team run Bash,
/// Edit and Write on any path that ever did read it to launch.
pub fn tool_policy_for_mode(mode: &str) -> crate::execution::runner::ToolPolicy {
    if mode == crate::integrations::email::TRIAGE_MODE || mode == crate::team::TEAM_MODE {
        crate::execution::runner::ToolPolicy::None
    } else {
        crate::execution::runner::ToolPolicy::Unrestricted
    }
}

/// PURE: whether a run in `mode` proceeds with nobody there to answer it.
///
/// Three policies read this, for the same reason each time — there is no human in the loop: how
/// many attempts a failure gets, how long the wall clock runs, and whether the classifier replaces
/// the CLI's permission surface. `email_triage` is unattended too and is deliberately NOT in this
/// set: it is one toolless classification against a local model, so it has no tools to permit, no
/// build to outlast, and nothing a retry would fix.
///
/// One definition rather than the same `if` at each reader, because the day a fourth unattended
/// mode appears, three policies have to learn about it together or two of them quietly won't.
///
/// That day arrived with `'team'`, and it is here for the reason the sentence above predicted:
/// inside a team run there is nobody to answer the CLI. The third policy does not fire on it all
/// the same — `classifier_governs_tools` also demands `Unrestricted`, and a department never is
/// (`tool_policy_for_mode` above) — which is the AND doing its job rather than an exception.
pub fn runs_unattended(mode: &str) -> bool {
    mode == "shadow" || mode == "worktree" || mode == crate::team::TEAM_MODE
}

/// Whether this run's actions are governed by the classifier rather than by the CLI's allow-list.
///
/// Reads the disk, so not pure: the answer depends on what is wired up in `dir`, which for a
/// worktree run is the worktree the CLI will start in and not the project root.
///
/// Every condition is a refusal, and the AND between them is the point:
///
///   * unattended only — an interactive run has somebody who can approve, and taking that decision
///     away from them is not this function's business;
///   * `Unrestricted` only — a run with no tools has no permissions worth changing;
///   * classifier verified present — the load-bearing one. Standing the CLI's barrier down is only
///     safe because another barrier takes over. With no hook there is no second barrier, and the
///     run would be governed by nothing at all.
///
/// The third condition means a project that has not wired the hook keeps today's behaviour, which
/// is also today's failure: its unattended runs still cannot execute what the interactive list
/// omits. That is the right trade — the answer for such a project is to wire the classifier, not
/// to stand the barrier down without one.
pub(super) fn classifier_governs_tools(
    mode: &str,
    tool_policy: crate::execution::runner::ToolPolicy,
    dir: Option<&std::path::Path>,
) -> bool {
    if !runs_unattended(mode) || tool_policy != crate::execution::runner::ToolPolicy::Unrestricted {
        return false;
    }
    dir.is_some_and(crate::governance::autopilot::classifier_hook_is_wired)
}

/// Gives a provisioned tree the classifier hook its project root wires.
///
/// 4711d292 stopped tracking `.claude/settings.json`, so a worktree checked out from the repository
/// no longer carries the hook, and `classifier_governs_tools` then (rightly) refused to stand the
/// CLI's barrier down for it. The opt-in stays the root's: a root that does not wire the hook
/// leaves its trees unwired, and a tree that already wires it is not touched. The script already
/// in the tree is kept, because a project may track its own copy.
pub(super) fn inherit_classifier_hook(project_root: &std::path::Path, worktree: &std::path::Path) {
    if !crate::governance::autopilot::classifier_hook_is_wired(project_root)
        || crate::governance::autopilot::classifier_hook_is_wired(worktree)
    {
        return;
    }
    if let Err(error) = crate::governance::autopilot::wire_classifier_hook_keeping_script(worktree)
    {
        tracing::warn!(
            worktree = %worktree.display(),
            %error,
            "could not give a provisioned worktree the classifier hook its project root wires"
        );
    }
}

/// PURE: the wall clock a run in `mode` gets, given the interactive default `base`.
///
/// `shadow` and `worktree` are the modes that check out a tree, edit it, build it and run a gate,
/// and they are the ones measured dying on the 600-second deadline with work still open (see
/// `state::AUTONOMOUS_RUN_TIMEOUT_MULTIPLIER`). `email_triage` is autonomous too and deliberately
/// stays on the short clock: it classifies one message against a local model, so a triage run still
/// going after ten minutes is stuck, not busy.
///
/// Derived from `base` rather than given a constant of its own, so a test that shortens the clock
/// still gets a short one, and an operator who tunes the deadline moves both together.
pub fn run_timeout_for_mode(base: std::time::Duration, mode: &str) -> std::time::Duration {
    if runs_unattended(mode) {
        base * crate::state::AUTONOMOUS_RUN_TIMEOUT_MULTIPLIER
    } else {
        base
    }
}

/// PURE: the silence a run in `mode` is allowed, given the interactive default `base`.
///
/// The same shape as `run_timeout_for_mode` and the same modes, because the two deadlines fail
/// unattended runs for the same reason: both were sized for work somebody is watching. A single
/// tool call that compiles this workspace streams nothing for minutes (see
/// `state::AUTONOMOUS_PROGRESS_TIMEOUT_MULTIPLIER`), so the deadline meant to catch a stuck run
/// was killing runs that were merely building.
///
/// `email_triage` stays on the short one for its own reason, unchanged: it classifies one message
/// against a local model, and a triage run silent for five minutes is stuck rather than busy.
pub fn progress_timeout_for_mode(base: std::time::Duration, mode: &str) -> std::time::Duration {
    if runs_unattended(mode) {
        base * crate::state::AUTONOMOUS_PROGRESS_TIMEOUT_MULTIPLIER
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loadout_with_engine_builtins_never_turns_them_into_nucleos_tool_names() {
        let mcp = JobNodeMcp {
            path: std::env::temp_dir().join("nucleos-policy-test-missing.json"),
            job_id: 1,
            allowed: Vec::new(),
            add_dirs: Vec::new(),
        }
        .with_loadout(&crate::loadout::RunTools {
            tools: vec![
                "WebFetch".to_owned(),
                "WebSearch".to_owned(),
                "verify".to_owned(),
            ],
            ..Default::default()
        });
        assert_eq!(mcp.allowed, vec!["verify".to_owned()]);
    }
}
