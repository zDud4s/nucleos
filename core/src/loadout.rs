//! The one resolver every spawn site with an agent goes through (spec 2026-10-08 section 5).
//!
//! A spawn site hands over WHO is working (agent, team, project, job), WHAT the work is (task
//! text, node kind, files) and gets back one [`Loadout`]: the knowledge `Brief` and the rendered
//! block to append to the prompt. Resolving here, and not once per caller, is what keeps the
//! isolation rule in one place: an agent reads its own memory and its team's, and no other
//! agent's.
//!
//! Memory, tools and context all arrive here. A caller that wants the run's tools and context as
//! well hands over an [`Equipment`] (its box's lists, its tool policy, whether it has a working
//! directory): the resolver then answers the full effective tool set of the run, the directories
//! to widen the sandbox with and the context index to append to the prompt. A caller that hands
//! none gets the memory half only. A read failure never stops a run: the loadout answers no brief
//! and an empty block, and the run starts without one.

use crate::knowledge::{Brief, Context, NodeKind, Scope};
use std::path::{Path, PathBuf};

/// What the box a run launches in, and the agent's policy, say about its tools.
pub struct Equipment<'a> {
    /// The agent's `tool_policy`: `"none"` launches the run with no tools at all.
    pub tool_policy: &'a str,
    /// The tools the box serves to every run.
    pub base: &'static [&'static str],
    /// The tools the box serves only to a run whose owner was approved for them.
    pub extras: &'static [&'static str],
    /// The managed files root context references are re-resolved against; `None` offers none.
    pub managed_root: Option<&'a Path>,
    /// Whether the run has a working directory, the only place `--add-dir` can widen.
    pub has_cwd: bool,
}

/// Who is working and on what, as far as the selection is allowed to know it.
pub struct LoadoutInput<'a> {
    /// The agent doing the work, when there is one: its own memory joins the chain.
    pub agent: Option<&'a str>,
    /// The team that agent works for, when there is one: the team's memory joins the chain.
    pub team: Option<&'a str>,
    /// The project the work belongs to, when there is one.
    pub project: Option<&'a str>,
    /// The job the work belongs to, when there is one.
    pub job: Option<i64>,
    /// The words of the task, used to query the knowledge.
    pub task_text: &'a str,
    /// The kind of job node being briefed, when the work is one.
    pub node: Option<NodeKind>,
    /// The files the work touches or declares.
    pub files: &'a [String],
    /// The tools and context half of the loadout; `None` resolves the memory half only.
    pub equipment: Option<Equipment<'a>>,
}

/// The tools and directories one run was resolved to, and whom they were resolved for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunTools {
    /// The agent whose approvals were read, when it has a row.
    pub agent: Option<String>,
    /// The team whose approvals were read, when it has a row.
    pub team: Option<String>,
    /// The full effective tool set, base included, sorted.
    pub tools: Vec<String>,
    /// The directories to pass to the CLI with `--add-dir`.
    pub add_dirs: Vec<PathBuf>,
}

/// What a spawn site gets back: the briefing and the block that renders it.
pub struct Loadout {
    /// The briefing with its trace, or `None` when nothing could be read.
    pub brief: Option<Brief>,
    /// The text to append to the prompt; empty when there is nothing to say.
    pub block: String,
    /// The run's tools and directories; `None` when the input carried no equipment.
    pub run: Option<RunTools>,
}

/// The effective tool set of a run: nothing for `tool_policy` `"none"`, else the box's base plus
/// the approved tools that are among the box's extras. Sorted, no duplicates. Anything approved
/// that is not an extra of the box (a built-in, another box's tool) never enters.
pub fn effective_tools(
    base: &[&str],
    extras: &[&str],
    approved: &[String],
    tool_policy: &str,
) -> Vec<String> {
    if tool_policy == "none" {
        return Vec::new();
    }
    let mut tools: Vec<String> = base.iter().map(|tool| (*tool).to_owned()).collect();
    tools.extend(
        approved
            .iter()
            .filter(|tool| extras.contains(&tool.as_str()))
            .cloned(),
    );
    tools.sort();
    tools.dedup();
    tools
}

/// The tools an agent or a team was approved for and not since revoked.
async fn approved_tools(
    pool: &sqlx::SqlitePool,
    agent: Option<&str>,
    team: Option<&str>,
) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar::<_, String>(
        "SELECT tool FROM loadout_tools
         WHERE status = 'active'
           AND ((owner_kind = 'agent' AND owner_id = ?)
             OR (owner_kind = 'team' AND owner_id = ?))",
    )
    .bind(agent)
    .bind(team)
    .fetch_all(pool)
    .await
}

/// Resolves the loadout for one spawn. An agent or team that has no row is skipped, and the skip
/// is logged: its link leaves the chain, the others stay.
pub async fn resolve(pool: &sqlx::SqlitePool, input: &LoadoutInput<'_>) -> Loadout {
    let (agent, team) = match owners(pool, input).await {
        Ok(owners) => owners,
        Err(error) => {
            tracing::warn!(
                agent = ?input.agent,
                team = ?input.team,
                job = ?input.job,
                %error,
                "could not read what is known; the run starts without it"
            );
            // The owners could not be checked, so no approval can be read: the box's base.
            return Loadout {
                brief: None,
                block: String::new(),
                run: input.equipment.as_ref().map(|equipment| RunTools {
                    tools: effective_tools(
                        equipment.base,
                        equipment.extras,
                        &[],
                        equipment.tool_policy,
                    ),
                    ..RunTools::default()
                }),
            };
        }
    };
    let context = context_for(input, agent, team);
    let brief = match crate::brief::of(pool, &context, input.task_text).await {
        Ok(brief) => Some(brief),
        Err(error) => {
            tracing::warn!(
                agent = ?input.agent,
                team = ?input.team,
                job = ?input.job,
                %error,
                "could not read what is known; the run starts without it"
            );
            None
        }
    };
    let mut block = brief
        .as_ref()
        .and_then(|brief| brief.block.clone())
        .unwrap_or_default();
    let run = match &input.equipment {
        Some(equipment) => Some(equip(pool, equipment, agent, team, &mut block).await),
        None => None,
    };
    Loadout { brief, block, run }
}

/// The tools and context of one run: reads the approvals and the references of the checked
/// `agent` and `team`, and appends the context index to `block` when the run can open its files.
async fn equip(
    pool: &sqlx::SqlitePool,
    equipment: &Equipment<'_>,
    agent: Option<&str>,
    team: Option<&str>,
    block: &mut String,
) -> RunTools {
    let approved = match approved_tools(pool, agent, team).await {
        Ok(approved) => approved,
        Err(error) => {
            tracing::warn!(
                ?agent,
                ?team,
                %error,
                "could not read the approved tools; the run gets its base"
            );
            Vec::new()
        }
    };
    let tools = effective_tools(
        equipment.base,
        equipment.extras,
        &approved,
        equipment.tool_policy,
    );
    let mut add_dirs = Vec::new();
    // The index belongs to the tool that opens its files: without `read_context` there is
    // nothing to offer and no directory to widen.
    if let Some(root) = equipment.managed_root
        && tools.iter().any(|tool| tool == "read_context")
    {
        let refs = match crate::context_refs::spawn_refs(pool, root, agent, team).await {
            Ok(refs) => refs,
            Err(error) => {
                tracing::warn!(
                    ?agent,
                    ?team,
                    %error,
                    "could not read the context references; the run gets none"
                );
                Vec::new()
            }
        };
        let index = crate::context_refs::render_index(&refs);
        if !index.is_empty() {
            if !block.is_empty() {
                block.push_str("\n\n");
            }
            block.push_str("Context files you were given (open one with the read_context tool):\n");
            block.push_str(&index);
        }
        if equipment.has_cwd {
            add_dirs = crate::context_refs::add_dirs(&refs);
        }
    }
    RunTools {
        agent: agent.map(str::to_owned),
        team: team.map(str::to_owned),
        tools,
        add_dirs,
    }
}

/// Writes the tools a run was started with, frozen at launch; the routes and the hook read it.
/// Recording a run again replaces its row.
pub async fn record(pool: &sqlx::SqlitePool, run_id: i64, run: &RunTools) -> sqlx::Result<()> {
    let tools = serde_json::to_string(&run.tools).unwrap_or_else(|_| "[]".to_owned());
    sqlx::query(
        "INSERT INTO run_loadout (run_id, agent_id, team_id, tools, resolved_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(run_id) DO UPDATE SET
           agent_id = excluded.agent_id,
           team_id = excluded.team_id,
           tools = excluded.tools,
           resolved_at = excluded.resolved_at",
    )
    .bind(run_id)
    .bind(run.agent.as_deref())
    .bind(run.team.as_deref())
    .bind(tools)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

/// Copies one run's frozen loadout to the run that continues it (a handoff successor or an
/// approval resume): the same work, so the same tools, including an extra approved mid-run. A
/// predecessor with no row gives the successor none, so it stays on its box's base tools.
pub async fn carry(pool: &sqlx::SqlitePool, from_run: i64, to_run: i64) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO run_loadout (run_id, agent_id, team_id, tools, resolved_at)
         SELECT ?, agent_id, team_id, tools, resolved_at FROM run_loadout WHERE run_id = ?
         ON CONFLICT(run_id) DO UPDATE SET
           agent_id = excluded.agent_id,
           team_id = excluded.team_id,
           tools = excluded.tools,
           resolved_at = excluded.resolved_at",
    )
    .bind(to_run)
    .bind(from_run)
    .execute(pool)
    .await?;
    Ok(())
}

/// The agent and team of an input, each only when it has a row (the skip is logged).
async fn owners<'a>(
    pool: &sqlx::SqlitePool,
    input: &LoadoutInput<'a>,
) -> sqlx::Result<(Option<&'a str>, Option<&'a str>)> {
    let agent = agent_or_skip(pool, input.agent).await?;
    let team = team_or_skip(pool, input.team).await?;
    Ok((agent, team))
}

/// The agent id when it has a row; `None` for no agent, and for one without a row (logged).
async fn agent_or_skip<'a>(
    pool: &sqlx::SqlitePool,
    id: Option<&'a str>,
) -> sqlx::Result<Option<&'a str>> {
    let Some(id) = id else { return Ok(None) };
    let row = sqlx::query_scalar::<_, i64>("SELECT 1 FROM agents WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    if row.is_none() {
        tracing::warn!(agent = id, "agent has no row; its link is skipped");
    }
    Ok(row.map(|_| id))
}

/// The team id when it has a row; `None` for no team, and for one without a row (logged).
async fn team_or_skip<'a>(
    pool: &sqlx::SqlitePool,
    id: Option<&'a str>,
) -> sqlx::Result<Option<&'a str>> {
    let Some(id) = id else { return Ok(None) };
    let row = sqlx::query_scalar::<_, i64>("SELECT 1 FROM teams WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    if row.is_none() {
        tracing::warn!(team = id, "team has no row; its link is skipped");
    }
    Ok(row.map(|_| id))
}

/// The selection context an input stands for: machine -> project -> team -> agent -> job, with
/// every absent link left out. `agent` and `team` are the checked values, not the input's.
fn context_for(input: &LoadoutInput<'_>, agent: Option<&str>, team: Option<&str>) -> Context {
    let mut context = Context::for_project(input.project);
    if let Some(id) = input.job {
        context.chain.push(Scope::Job {
            id,
            project: input.project.map(str::to_owned),
        });
    }
    context.agent = agent.map(str::to_owned);
    context.team = team.map(str::to_owned);
    context.node = input.node;
    context.files = input.files.to_vec();
    context
}

/// The node kind a job stage name stands for, or `None` for a stage that is not a briefed node.
pub fn node_kind(stage: &str) -> Option<NodeKind> {
    match stage {
        "plan" | "spec" => Some(NodeKind::Plan),
        "implement" => Some(NodeKind::Implement),
        "review" => Some(NodeKind::Review),
        "replan" => Some(NodeKind::Replan),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Equipment, Loadout, LoadoutInput, RunTools, carry, effective_tools, node_kind, record,
        resolve,
    };
    use crate::knowledge::NodeKind;
    use crate::mcp_tools::{JOB_NODE_BASE, JOB_NODE_EXTRAS, TEAM_BASE, TEAM_EXTRAS};
    use sqlx::SqlitePool;
    use std::path::Path;

    async fn seed(
        pool: &SqlitePool,
        scope_kind: &str,
        scope_id: Option<&str>,
        title: &str,
    ) -> sqlx::Result<i64> {
        let result = sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('semantic', ?, ?, 'owner', 'memory', ?, 'body', 'active',
                     '2026-08-19T00:00:00+00:00')",
        )
        .bind(scope_kind)
        .bind(scope_id)
        .bind(title)
        .execute(pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    /// The ids of the seven rows `seed_all` writes, one per scope link.
    struct Ids {
        machine: i64,
        project: i64,
        team_x: i64,
        team_y: i64,
        agent_a: i64,
        agent_b: i64,
        job: i64,
    }

    /// The owners `seed_all` writes knowledge for: agents "a" and "b", teams "x" and "y". The
    /// resolver only follows a link whose owner has a row, so a test that wants a link to count
    /// has to seed the owner as well as the knowledge.
    async fn seed_owners(pool: &SqlitePool) {
        let now = "2026-08-19T00:00:00+00:00";
        for id in ["a", "b"] {
            sqlx::query(
                "INSERT INTO agents (id, name, speciality, prompt, engine, tool_policy,
                                     created_at, updated_at)
                 VALUES (?, ?, 'tests', 'work', 'claude', 'inherit', ?, ?)",
            )
            .bind(id)
            .bind(format!("Agent {id}"))
            .bind(now)
            .bind(now)
            .execute(pool)
            .await
            .unwrap();
        }
        for id in ["x", "y"] {
            sqlx::query(
                "INSERT INTO teams (id, name, mission, director_agent_id, max_rounds,
                                    max_parallel, created_at, updated_at)
                 VALUES (?, ?, 'ship it', 'a', 3, 2, ?, ?)",
            )
            .bind(id)
            .bind(format!("Team {id}"))
            .bind(now)
            .bind(now)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    async fn seed_all(pool: &SqlitePool) -> Ids {
        seed_owners(pool).await;
        Ids {
            machine: seed(pool, "machine", None, "zanzibar machine-note")
                .await
                .unwrap(),
            project: seed(pool, "project", Some("p"), "zanzibar project-note")
                .await
                .unwrap(),
            team_x: seed(pool, "team", Some("x"), "zanzibar team-x-note")
                .await
                .unwrap(),
            team_y: seed(pool, "team", Some("y"), "zanzibar team-y-note")
                .await
                .unwrap(),
            agent_a: seed(pool, "agent", Some("a"), "zanzibar agent-a-note")
                .await
                .unwrap(),
            agent_b: seed(pool, "agent", Some("b"), "zanzibar agent-b-note")
                .await
                .unwrap(),
            job: seed(pool, "job", Some("7"), "zanzibar job-note")
                .await
                .unwrap(),
        }
    }

    fn traced_ids(loadout: &Loadout) -> Vec<i64> {
        loadout
            .brief
            .as_ref()
            .expect("the loadout carries a brief")
            .trace
            .iter()
            .map(|scored| scored.knowledge_id)
            .collect()
    }

    fn s_scope_of(loadout: &Loadout, id: i64) -> f64 {
        loadout
            .brief
            .as_ref()
            .expect("the loadout carries a brief")
            .trace
            .iter()
            .find(|scored| scored.knowledge_id == id)
            .unwrap_or_else(|| panic!("row {id} is not in the trace"))
            .s_scope
    }

    fn input<'a>(
        agent: Option<&'a str>,
        team: Option<&'a str>,
        project: Option<&'a str>,
        job: Option<i64>,
        files: &'a [String],
    ) -> LoadoutInput<'a> {
        LoadoutInput {
            agent,
            team,
            project,
            job,
            task_text: "zanzibar",
            node: None,
            files,
            equipment: None,
        }
    }

    #[tokio::test]
    async fn resolve_gives_an_agent_its_own_and_its_teams_memory_and_no_one_elses() {
        let pool = crate::testdb::fresh_pool().await;
        let ids = seed_all(&pool).await;
        let files: Vec<String> = Vec::new();

        let loadout = resolve(
            &pool,
            &input(Some("a"), Some("x"), Some("p"), Some(7), &files),
        )
        .await;

        assert!(!loadout.block.is_empty(), "the linked rows render a block");
        for note in [
            "machine-note",
            "project-note",
            "team-x-note",
            "agent-a-note",
            "job-note",
        ] {
            assert!(
                loadout.block.contains(note),
                "{note} missing: {}",
                loadout.block
            );
        }
        assert!(
            !loadout.block.contains("team-y-note"),
            "another team's row was rendered: {}",
            loadout.block
        );
        assert!(
            !loadout.block.contains("agent-b-note"),
            "another agent's row was rendered: {}",
            loadout.block
        );
        let traced = traced_ids(&loadout);
        for mine in [ids.machine, ids.project, ids.team_x, ids.agent_a, ids.job] {
            assert!(traced.contains(&mine), "row {mine} missing from the trace");
        }
        assert!(
            !traced.contains(&ids.team_y) && !traced.contains(&ids.agent_b),
            "a row outside the chain reached the trace: {traced:?}"
        );
    }

    #[tokio::test]
    async fn resolve_ranks_the_more_specific_scope_higher() {
        let pool = crate::testdb::fresh_pool().await;
        let ids = seed_all(&pool).await;
        let files: Vec<String> = Vec::new();

        let loadout = resolve(
            &pool,
            &input(Some("a"), Some("x"), Some("p"), Some(7), &files),
        )
        .await;

        let machine = s_scope_of(&loadout, ids.machine);
        let project = s_scope_of(&loadout, ids.project);
        let team = s_scope_of(&loadout, ids.team_x);
        let agent = s_scope_of(&loadout, ids.agent_a);
        let job = s_scope_of(&loadout, ids.job);
        assert!(
            machine < project && project < team && team < agent && agent < job,
            "s_scope must rise with specificity: machine {machine}, project {project}, \
             team {team}, agent {agent}, job {job}"
        );
    }

    #[tokio::test]
    async fn resolve_without_an_agent_or_team_skips_those_links() {
        let pool = crate::testdb::fresh_pool().await;
        let ids = seed_all(&pool).await;
        let files: Vec<String> = Vec::new();

        let loadout = resolve(&pool, &input(None, None, Some("p"), None, &files)).await;

        assert!(loadout.block.contains("machine-note"), "{}", loadout.block);
        assert!(loadout.block.contains("project-note"), "{}", loadout.block);
        for absent in [
            "team-x-note",
            "team-y-note",
            "agent-a-note",
            "agent-b-note",
            "job-note",
        ] {
            assert!(
                !loadout.block.contains(absent),
                "{absent} rendered without its link: {}",
                loadout.block
            );
        }
        let traced = traced_ids(&loadout);
        for outside in [ids.team_x, ids.team_y, ids.agent_a, ids.agent_b, ids.job] {
            assert!(
                !traced.contains(&outside),
                "row {outside} reached the trace without its link: {traced:?}"
            );
        }
    }

    /// Spec section 5.1: an agent or team that is named but has no row is a dangling link, and a
    /// dangling link is skipped rather than followed. The rest of the chain still applies.
    #[tokio::test]
    async fn resolve_skips_an_agent_or_team_that_has_no_row() {
        let pool = crate::testdb::fresh_pool().await;
        let ghost = seed(&pool, "agent", Some("ghost"), "zanzibar ghost-note")
            .await
            .unwrap();
        let phantom = seed(&pool, "team", Some("phantom"), "zanzibar phantom-note")
            .await
            .unwrap();
        let machine = seed(&pool, "machine", None, "zanzibar machine-note")
            .await
            .unwrap();
        let files: Vec<String> = Vec::new();

        let loadout = resolve(
            &pool,
            &LoadoutInput {
                agent: Some("ghost"),
                team: Some("phantom"),
                project: None,
                job: None,
                task_text: "zanzibar ghost phantom",
                node: None,
                files: &files,
                equipment: None,
            },
        )
        .await;

        assert!(
            !loadout.block.contains("ghost-note"),
            "a row-less agent's memory was rendered: {}",
            loadout.block
        );
        assert!(
            !loadout.block.contains("phantom-note"),
            "a row-less team's memory was rendered: {}",
            loadout.block
        );
        let traced = traced_ids(&loadout);
        assert!(
            !traced.contains(&ghost) && !traced.contains(&phantom),
            "a link without an owner reached the trace: {traced:?}"
        );
        assert!(
            traced.contains(&machine),
            "the machine link still applies: {traced:?}"
        );
    }

    /// Spec section 8: the textual threshold leaves out what the task does not mention, and a
    /// briefing may come out empty. An empty block is still a briefing, not a read failure.
    #[tokio::test]
    async fn resolve_leaves_out_what_does_not_match_the_task_and_can_brief_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        seed_owners(&pool).await;
        seed(&pool, "agent", Some("a"), "zanzibar agent-a-note")
            .await
            .unwrap();
        seed(&pool, "machine", None, "quokka unrelated")
            .await
            .unwrap();
        let files: Vec<String> = Vec::new();
        let asking = |task_text: &'static str| LoadoutInput {
            agent: Some("a"),
            team: None,
            project: None,
            job: None,
            task_text,
            node: None,
            files: &files,
            equipment: None,
        };

        let matching = resolve(&pool, &asking("zanzibar")).await;
        assert!(
            matching.block.contains("agent-a-note"),
            "{}",
            matching.block
        );
        assert!(
            !matching.block.contains("quokka unrelated"),
            "a row the task does not mention was rendered: {}",
            matching.block
        );

        let nothing = resolve(&pool, &asking("nothing here matches")).await;
        assert!(
            nothing.brief.is_some(),
            "an empty briefing is still a briefing"
        );
        assert!(
            nothing.block.is_empty(),
            "nothing matched, yet a block was rendered: {}",
            nothing.block
        );
    }

    #[tokio::test]
    async fn resolve_on_a_read_failure_answers_no_brief_and_an_empty_block() {
        let pool = crate::testdb::fresh_pool().await;
        seed(&pool, "machine", None, "zanzibar machine-note")
            .await
            .unwrap();
        pool.close().await;
        let files: Vec<String> = Vec::new();

        let loadout = resolve(&pool, &input(None, None, None, None, &files)).await;

        assert!(loadout.brief.is_none(), "a failed read left a brief");
        assert!(loadout.block.is_empty(), "a failed read left a block");
    }

    // -----------------------------------------------------------------------------------------
    // Tools and context (wave B1): the equipment half of the resolver
    // -----------------------------------------------------------------------------------------

    /// The names of a constant list as the owned, sorted vector a `RunTools` carries.
    fn sorted(names: &[&str]) -> Vec<String> {
        let mut owned: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
        owned.sort();
        owned
    }

    /// The equipment of a job node: its box's lists, `mcp_only`, and whatever root and cwd the
    /// test wants.
    fn job_node_equipment(managed_root: Option<&Path>, has_cwd: bool) -> Equipment<'_> {
        Equipment {
            tool_policy: "mcp_only",
            base: JOB_NODE_BASE,
            extras: JOB_NODE_EXTRAS,
            managed_root,
            has_cwd,
        }
    }

    /// The equipment of a team agent: its box's lists, `mcp_only`, no root and no cwd.
    fn team_equipment() -> Equipment<'static> {
        Equipment {
            tool_policy: "mcp_only",
            base: TEAM_BASE,
            extras: TEAM_EXTRAS,
            managed_root: None,
            has_cwd: false,
        }
    }

    /// The input of `agent` and `team` working on `zanzibar`, carrying the given equipment.
    fn equipped<'a>(
        agent: Option<&'a str>,
        team: Option<&'a str>,
        files: &'a [String],
        equipment: Equipment<'a>,
    ) -> LoadoutInput<'a> {
        let mut asked = input(agent, team, None, None, files);
        asked.equipment = Some(equipment);
        asked
    }

    /// The tools agent `a` of team `x` gets with the given equipment.
    async fn tools_with(pool: &SqlitePool, equipment: Equipment<'_>) -> Vec<String> {
        let files: Vec<String> = Vec::new();
        resolve(pool, &equipped(Some("a"), Some("x"), &files, equipment))
            .await
            .run
            .expect("an equipped input records a run")
            .tools
    }

    /// An owner-granted `loadout_tools` row with the given status.
    async fn grant(pool: &SqlitePool, owner_kind: &str, owner_id: &str, tool: &str, status: &str) {
        sqlx::query(
            "INSERT INTO loadout_tools
               (owner_kind, owner_id, tool, status, source, created_at)
             VALUES (?, ?, ?, ?, 'owner', '2026-10-09T00:00:00+00:00')",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .bind(tool)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A `context_refs` row, written straight into the table.
    async fn add_ref(pool: &SqlitePool, owner_kind: &str, owner_id: &str, path: &Path, kind: &str) {
        sqlx::query(
            "INSERT INTO context_refs (owner_kind, owner_id, path, kind, note, created_at)
             VALUES (?, ?, ?, ?, NULL, '2026-10-09T00:00:00+00:00')",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .bind(path.to_str().unwrap())
        .bind(kind)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A canonical temp root with a `docs` directory and a `notes.md` file inside it.
    fn managed_root_with_files() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir(root.join("docs")).unwrap();
        std::fs::write(root.join("notes.md"), "hello").unwrap();
        (tmp, root)
    }

    /// The start of the heading the context index is rendered under.
    const INDEX_HEADER: &str = "Context files you were given";

    #[tokio::test]
    async fn loadout_no_approved_tools_gives_exactly_the_base() {
        let pool = crate::testdb::fresh_pool().await;
        seed_owners(&pool).await;
        let files: Vec<String> = Vec::new();

        let job_node = resolve(
            &pool,
            &equipped(
                Some("a"),
                Some("x"),
                &files,
                job_node_equipment(None, false),
            ),
        )
        .await;
        let run = job_node.run.expect("an equipped input records a run");
        assert_eq!(run.tools, sorted(JOB_NODE_BASE));
        assert_eq!(run.agent.as_deref(), Some("a"));
        assert_eq!(run.team.as_deref(), Some("x"));
        assert!(run.add_dirs.is_empty(), "no refs, so no directories");

        assert_eq!(tools_with(&pool, team_equipment()).await, sorted(TEAM_BASE));

        // The pure half: nothing approved is the base, sorted, with no duplicate.
        assert_eq!(
            effective_tools(&["zeta", "alpha", "alpha"], &["web"], &[], "mcp_only"),
            vec!["alpha".to_owned(), "zeta".to_owned()]
        );
    }

    #[tokio::test]
    async fn loadout_only_an_active_extra_of_the_box_enters() {
        let pool = crate::testdb::fresh_pool().await;
        seed_owners(&pool).await;
        // Active, but a built-in, a tool of another box and a tool of another box's base are no
        // extra of the job-node box.
        grant(&pool, "agent", "a", "web_read", "active").await;
        grant(&pool, "agent", "a", "Bash", "active").await;
        grant(&pool, "agent", "a", "create_run", "active").await;
        grant(&pool, "team", "x", "read_team_file", "active").await;
        let mut with_web_read = sorted(JOB_NODE_BASE);
        with_web_read.push("web_read".to_owned());
        with_web_read.sort();

        assert_eq!(
            tools_with(&pool, job_node_equipment(None, false)).await,
            with_web_read,
            "exactly the base and the one active extra of the job-node box"
        );

        // The same grant means nothing in a box that does not list it as an extra.
        assert_eq!(tools_with(&pool, team_equipment()).await, sorted(TEAM_BASE));

        // Proposed, rejected and revoked rows are no approval.
        for status in ["proposed", "rejected", "revoked"] {
            sqlx::query(
                "UPDATE loadout_tools SET status = ?
                 WHERE owner_kind = 'agent' AND owner_id = 'a' AND tool = 'web_read'",
            )
            .bind(status)
            .execute(&pool)
            .await
            .unwrap();
            assert_eq!(
                tools_with(&pool, job_node_equipment(None, false)).await,
                sorted(JOB_NODE_BASE),
                "a {status} row let an extra in"
            );
        }

        // A team's approval reaches the team's agents too.
        grant(&pool, "team", "x", "web_read", "active").await;
        assert_eq!(
            tools_with(&pool, job_node_equipment(None, false)).await,
            with_web_read,
            "the team's own approval was not honoured"
        );

        // The pure half: only the intersection of approved and extras joins the base.
        assert_eq!(
            effective_tools(
                &["b", "a"],
                &["c", "d"],
                &["c".to_owned(), "z".to_owned(), "a".to_owned()],
                "mcp_only"
            ),
            vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
        );
    }

    #[tokio::test]
    async fn loadout_tool_policy_none_gives_no_tools() {
        let pool = crate::testdb::fresh_pool().await;
        seed_owners(&pool).await;
        grant(&pool, "agent", "a", "web_read", "active").await;
        let (_tmp, root) = managed_root_with_files();
        add_ref(&pool, "agent", "a", &root.join("notes.md"), "file").await;
        add_ref(&pool, "agent", "a", &root.join("docs"), "dir").await;
        let files: Vec<String> = Vec::new();

        let loadout = resolve(
            &pool,
            &equipped(
                Some("a"),
                None,
                &files,
                Equipment {
                    tool_policy: "none",
                    base: JOB_NODE_BASE,
                    extras: JOB_NODE_EXTRAS,
                    managed_root: Some(&root),
                    has_cwd: true,
                },
            ),
        )
        .await;

        let run = loadout.run.expect("a toolless agent still records a run");
        assert!(
            run.tools.is_empty(),
            "tool_policy none left {:?}",
            run.tools
        );
        assert!(run.add_dirs.is_empty(), "no tools, so no directories");
        assert!(
            !loadout.block.contains(INDEX_HEADER),
            "an index for a tool the run does not hold: {}",
            loadout.block
        );
        assert!(
            effective_tools(
                JOB_NODE_BASE,
                JOB_NODE_EXTRAS,
                &["web_read".to_owned()],
                "none"
            )
            .is_empty()
        );
    }

    #[tokio::test]
    async fn loadout_an_unreadable_tool_table_falls_back_to_the_base() {
        let pool = crate::testdb::fresh_pool().await;
        seed_owners(&pool).await;
        grant(&pool, "agent", "a", "web_read", "active").await;
        sqlx::query("DROP TABLE loadout_tools")
            .execute(&pool)
            .await
            .unwrap();
        let files: Vec<String> = Vec::new();

        let loadout = resolve(
            &pool,
            &equipped(
                Some("a"),
                Some("x"),
                &files,
                job_node_equipment(None, false),
            ),
        )
        .await;

        let run = loadout
            .run
            .expect("a failed tool read still launches with the base");
        assert_eq!(run.tools, sorted(JOB_NODE_BASE));
        assert!(
            loadout.brief.is_some(),
            "the memory half does not depend on the tool table"
        );
    }

    #[tokio::test]
    async fn loadout_agent_a_never_receives_agent_bs_tools_or_refs() {
        let pool = crate::testdb::fresh_pool().await;
        seed_owners(&pool).await;
        let (_tmp, root) = managed_root_with_files();
        std::fs::write(root.join("a-only.md"), "a").unwrap();
        std::fs::write(root.join("b-only.md"), "b").unwrap();
        std::fs::create_dir(root.join("b-docs")).unwrap();
        grant(&pool, "agent", "b", "web_read", "active").await;
        add_ref(&pool, "agent", "a", &root.join("a-only.md"), "file").await;
        add_ref(&pool, "agent", "b", &root.join("b-only.md"), "file").await;
        add_ref(&pool, "agent", "b", &root.join("b-docs"), "dir").await;
        let files: Vec<String> = Vec::new();
        let b_file = root.join("b-only.md").to_str().unwrap().to_owned();
        let b_dir = std::fs::canonicalize(root.join("b-docs")).unwrap();

        let for_a = resolve(
            &pool,
            &equipped(
                Some("a"),
                None,
                &files,
                job_node_equipment(Some(&root), true),
            ),
        )
        .await;
        let run_a = for_a.run.expect("agent a records a run");
        assert_eq!(run_a.agent.as_deref(), Some("a"));
        assert_eq!(
            run_a.tools,
            sorted(JOB_NODE_BASE),
            "b's web_read reached a: {:?}",
            run_a.tools
        );
        assert!(
            for_a.block.contains("a-only.md"),
            "a's own ref is missing: {}",
            for_a.block
        );
        assert!(
            !for_a.block.contains(&b_file) && !for_a.block.contains("b-docs"),
            "b's refs reached a's prompt: {}",
            for_a.block
        );
        assert!(
            !run_a.add_dirs.contains(&b_dir),
            "b's directory reached a's launch: {:?}",
            run_a.add_dirs
        );

        // The control: b does receive its own, so the absence above is the isolation.
        let for_b = resolve(
            &pool,
            &equipped(
                Some("b"),
                None,
                &files,
                job_node_equipment(Some(&root), true),
            ),
        )
        .await;
        let run_b = for_b.run.expect("agent b records a run");
        assert!(
            run_b.tools.contains(&"web_read".to_owned()),
            "{:?}",
            run_b.tools
        );
        assert!(for_b.block.contains(&b_file), "{}", for_b.block);
        assert!(!for_b.block.contains("a-only.md"), "{}", for_b.block);
        assert_eq!(run_b.add_dirs, vec![b_dir]);
    }

    #[tokio::test]
    async fn loadout_context_refs_render_an_index_and_add_dirs_only_with_a_cwd() {
        let pool = crate::testdb::fresh_pool().await;
        seed_owners(&pool).await;
        let (_tmp, root) = managed_root_with_files();
        add_ref(&pool, "agent", "a", &root.join("notes.md"), "file").await;
        add_ref(&pool, "agent", "a", &root.join("docs"), "dir").await;
        let files: Vec<String> = Vec::new();
        let docs = std::fs::canonicalize(root.join("docs")).unwrap();
        let notes_text = root.join("notes.md").to_str().unwrap().to_owned();
        let docs_text = root.join("docs").to_str().unwrap().to_owned();

        let with_cwd = resolve(
            &pool,
            &equipped(
                Some("a"),
                None,
                &files,
                job_node_equipment(Some(&root), true),
            ),
        )
        .await;
        assert!(with_cwd.block.contains(INDEX_HEADER), "{}", with_cwd.block);
        assert!(with_cwd.block.contains(&notes_text), "{}", with_cwd.block);
        assert!(with_cwd.block.contains(&docs_text), "{}", with_cwd.block);
        assert_eq!(with_cwd.run.unwrap().add_dirs, vec![docs]);

        // No cwd: there is nothing to widen. The index stays.
        let no_cwd = resolve(
            &pool,
            &equipped(
                Some("a"),
                None,
                &files,
                job_node_equipment(Some(&root), false),
            ),
        )
        .await;
        assert!(no_cwd.block.contains(&notes_text), "{}", no_cwd.block);
        assert!(
            no_cwd.run.unwrap().add_dirs.is_empty(),
            "--add-dir without a cwd"
        );

        // No managed root: refs cannot be resolved, so none are offered.
        let no_root = resolve(
            &pool,
            &equipped(Some("a"), None, &files, job_node_equipment(None, true)),
        )
        .await;
        assert!(!no_root.block.contains(INDEX_HEADER), "{}", no_root.block);
        assert!(no_root.run.unwrap().add_dirs.is_empty());

        // The index belongs to the tool that opens its files: a box without `read_context` gets
        // neither the index nor the directories.
        let no_tool = resolve(
            &pool,
            &equipped(
                Some("a"),
                None,
                &files,
                Equipment {
                    tool_policy: "mcp_only",
                    base: &["request_tool"],
                    extras: &[],
                    managed_root: Some(&root),
                    has_cwd: true,
                },
            ),
        )
        .await;
        assert!(!no_tool.block.contains(INDEX_HEADER), "{}", no_tool.block);
        assert!(no_tool.run.unwrap().add_dirs.is_empty());
    }

    #[tokio::test]
    async fn loadout_record_writes_the_full_set_and_its_owners() {
        let pool = crate::testdb::fresh_pool().await;
        let run_id: i64 = sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('t', 'running', 'real', '2026-10-09T00:00:00Z') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let tools = sorted(&["note_finding", "request_tool", "verify", "web_read"]);

        record(
            &pool,
            run_id,
            &RunTools {
                agent: Some("a".to_owned()),
                team: Some("x".to_owned()),
                tools: tools.clone(),
                add_dirs: Vec::new(),
            },
        )
        .await
        .unwrap();

        let raw: String = sqlx::query_scalar("SELECT tools FROM run_loadout WHERE run_id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let stored: Vec<String> = serde_json::from_str(&raw).unwrap();
        assert_eq!(stored, tools, "the whole effective set, base included");
        for (tool, listed) in [
            ("request_tool", true),
            ("web_read", true),
            ("create_run", false),
        ] {
            assert_eq!(
                crate::tool_loadout::run_lists_tool(&pool, run_id, tool)
                    .await
                    .unwrap(),
                listed,
                "{tool}"
            );
        }
        assert_eq!(
            crate::context_refs::loadout_owner(&pool, run_id)
                .await
                .unwrap(),
            (Some("a".to_owned()), Some("x".to_owned()))
        );

        // Recording again replaces the row, and a toolless run records the empty set.
        record(
            &pool,
            run_id,
            &RunTools {
                agent: Some("a".to_owned()),
                team: None,
                tools: Vec::new(),
                add_dirs: Vec::new(),
            },
        )
        .await
        .unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM run_loadout WHERE run_id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1, "one row per run");
        let raw: String = sqlx::query_scalar("SELECT tools FROM run_loadout WHERE run_id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(raw, "[]");
        assert_eq!(
            crate::context_refs::loadout_owner(&pool, run_id)
                .await
                .unwrap(),
            (Some("a".to_owned()), None)
        );
    }

    /// A run continuing another (handoff successor, approval resume) gets the predecessor's row as
    /// it stands, and a predecessor with no row leaves the successor with none (fail closed).
    #[tokio::test]
    async fn loadout_carry_copies_the_row_and_writes_none_without_one() {
        let pool = crate::testdb::fresh_pool().await;
        let mut ids = Vec::new();
        for _ in 0..4 {
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO runs (prompt, status, mode, created_at)
                 VALUES ('t', 'running', 'real', '2026-10-10T00:00:00Z') RETURNING id",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            ids.push(id);
        }
        let (from, to, bare_from, bare_to) = (ids[0], ids[1], ids[2], ids[3]);
        let tools = sorted(&["read_context", "request_tool", "web_read"]);
        record(
            &pool,
            from,
            &RunTools {
                agent: Some("a".to_owned()),
                team: Some("x".to_owned()),
                tools: tools.clone(),
                add_dirs: Vec::new(),
            },
        )
        .await
        .unwrap();

        carry(&pool, from, to).await.unwrap();

        let row: (Option<String>, Option<String>, String) =
            sqlx::query_as("SELECT agent_id, team_id, tools FROM run_loadout WHERE run_id = ?")
                .bind(to)
                .fetch_one(&pool)
                .await
                .expect("the successor has a run_loadout row");
        assert_eq!(row.0.as_deref(), Some("a"));
        assert_eq!(row.1.as_deref(), Some("x"));
        let stored: Vec<String> = serde_json::from_str(&row.2).unwrap();
        assert_eq!(
            stored, tools,
            "the successor keeps every tool the predecessor had"
        );
        assert!(
            crate::tool_loadout::run_lists_tool(&pool, to, "web_read")
                .await
                .unwrap()
        );

        carry(&pool, bare_from, bare_to).await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM run_loadout WHERE run_id = ?")
            .bind(bare_to)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 0, "no predecessor row, no successor row");
    }

    #[test]
    fn node_kind_maps_every_stage_and_nothing_else() {
        assert_eq!(node_kind("plan"), Some(NodeKind::Plan));
        assert_eq!(node_kind("spec"), Some(NodeKind::Plan));
        assert_eq!(node_kind("implement"), Some(NodeKind::Implement));
        assert_eq!(node_kind("review"), Some(NodeKind::Review));
        assert_eq!(node_kind("replan"), Some(NodeKind::Replan));
        for other in ["gate", "", "PLAN", "plan "] {
            assert_eq!(node_kind(other), None, "{other:?} is not a briefed node");
        }
    }
}
