//! The one resolver every spawn site with an agent goes through (spec 2026-10-08 section 5).
//!
//! A spawn site hands over WHO is working (agent, team, project, job), WHAT the work is (task
//! text, node kind, files) and gets back one [`Loadout`]: the knowledge `Brief` and the rendered
//! block to append to the prompt. Resolving here, and not once per caller, is what keeps the
//! isolation rule in one place: an agent reads its own memory and its team's, and no other
//! agent's.
//!
//! Phase 1b is memory only. Tools and context references arrive in later phases, as further
//! fields of [`Loadout`]. A read failure never stops a run: the loadout answers no brief and an
//! empty block, and the run starts without one.

use crate::knowledge::{Brief, Context, NodeKind, Scope};

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
}

/// What a spawn site gets back: the briefing and the block that renders it.
pub struct Loadout {
    /// The briefing with its trace, or `None` when nothing could be read.
    pub brief: Option<Brief>,
    /// The text to append to the prompt; empty when there is nothing to say.
    pub block: String,
}

/// Resolves the loadout for one spawn.
pub async fn resolve(pool: &sqlx::SqlitePool, input: &LoadoutInput<'_>) -> Loadout {
    match crate::brief::of(pool, &context_for(input), input.task_text).await {
        Ok(brief) => Loadout {
            block: brief.block.clone().unwrap_or_default(),
            brief: Some(brief),
        },
        Err(error) => {
            tracing::warn!(
                agent = ?input.agent,
                team = ?input.team,
                job = ?input.job,
                %error,
                "could not read what is known; the run starts without it"
            );
            Loadout {
                brief: None,
                block: String::new(),
            }
        }
    }
}

/// The selection context an input stands for: machine -> project -> team -> agent -> job, with
/// every absent link left out.
fn context_for(input: &LoadoutInput<'_>) -> Context {
    let mut context = Context::for_project(input.project);
    if let Some(id) = input.job {
        context.chain.push(Scope::Job {
            id,
            project: input.project.map(str::to_owned),
        });
    }
    context.agent = input.agent.map(str::to_owned);
    context.team = input.team.map(str::to_owned);
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
    use super::{Loadout, LoadoutInput, node_kind, resolve};
    use crate::knowledge::NodeKind;
    use sqlx::SqlitePool;

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

    async fn seed_all(pool: &SqlitePool) -> Ids {
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
