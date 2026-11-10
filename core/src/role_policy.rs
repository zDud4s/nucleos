//! The role policy table (spec `2026-11-09-arvores-autonomia-design`, D11-D12): one engine, one
//! explicit table, one answer to "what may this caller do" for the root, an orchestrator, a leaf and
//! a reviewer.
//!
//! The table starts as a map of what already exists, not as new restrictions:
//! - `conductor_fence`: the root's rule that it delegates instead of working source by hand. It is
//!   enforced locally by `conductor_verdict` in `core/hooks/ask_daemon.py` (no daemon round trip on
//!   every call), which is why the hook only needs to know "is this call the root's".
//! - `node_fence`: an edit outside the paths of the caller's work node is refused (`POST /nodes/scope`).
//! - `git_queue`: a session's `merge`/`rebase` goes through the queue (`session_git.rs`).
//! - `open_children`: who may open child nodes (`tree_job/branch_start.rs`: "a leaf does not open
//!   branches").
//! A governed run keeps its own rules in `pretool.rs`; its role comes from the node its `run_id` is
//! bound to.
//!
//! **The role comes from the node row, never from what the caller says.** `agent_type` is chosen by
//! whoever calls `Agent`, so it is only a hint when there is no node, and a hint can only select a
//! role no more permissive than a leaf. A call that carries subagent markers but no usable
//! `agent_id` (D12) is not the root: it falls to the most restricted role.

use sqlx::SqlitePool;

use crate::work_nodes::{KIND_LEAF, KIND_ORCHESTRATOR, KIND_ROOT};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Root,
    Orchestrator,
    Leaf,
    /// A leaf that owns no paths: it reads and reports.
    Reviewer,
}

/// Every role, from the most to the least permissive.
pub const ROLES: [Role; 4] = [Role::Root, Role::Orchestrator, Role::Leaf, Role::Reviewer];

/// What a role is held to. `true` on a restriction means it applies; `true` on a permission means it
/// is granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RolePolicy {
    /// Restriction: hand work limited to the conductor's allowlist (local, in the hook).
    pub conductor_fence: bool,
    /// Restriction: edits fenced to the paths of the caller's node.
    pub node_fence: bool,
    /// Restriction: git history operations go through the queue.
    pub git_queue: bool,
    /// Permission: may open child nodes.
    pub open_children: bool,
}

/// The table.
pub const fn policy(role: Role) -> RolePolicy {
    match role {
        Role::Root => RolePolicy {
            conductor_fence: true,
            node_fence: true,
            git_queue: true,
            open_children: true,
        },
        Role::Orchestrator => RolePolicy {
            conductor_fence: false,
            node_fence: true,
            git_queue: true,
            open_children: true,
        },
        Role::Leaf => RolePolicy {
            conductor_fence: false,
            node_fence: true,
            git_queue: true,
            open_children: false,
        },
        Role::Reviewer => RolePolicy {
            conductor_fence: false,
            node_fence: true,
            git_queue: true,
            open_children: false,
        },
    }
}

/// The role to give a caller that cannot be placed: the last of [`ROLES`], whose permissions are a
/// subset of every other role's (checked by a test).
pub const fn most_restricted() -> Role {
    Role::Reviewer
}

/// The role of a node row. A node whose kind is not one the table knows is the most restricted role,
/// not a guess upwards.
pub fn role_of_node(kind: &str, owned_paths: &[String]) -> Role {
    match kind {
        KIND_ROOT => Role::Root,
        KIND_ORCHESTRATOR => Role::Orchestrator,
        KIND_LEAF if owned_paths.is_empty() => Role::Reviewer,
        KIND_LEAF => Role::Leaf,
        _ => most_restricted(),
    }
}

/// What the hook knows about the caller. `subagent` is the hook's own reading of the markers a
/// subagent's call carries when `agent_id` is missing (`subagent_signals` in `ask_daemon.py`).
#[derive(Debug, Default, Clone)]
pub struct Caller {
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    pub subagent: bool,
}

impl Caller {
    fn agent_id(&self) -> Option<&str> {
        self.agent_id.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }

    fn agent_type(&self) -> Option<&str> {
        self.agent_type.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }

    /// Whether anything says this call comes from a subagent.
    pub fn is_subagent(&self) -> bool {
        self.subagent || self.agent_id().is_some() || self.agent_type().is_some()
    }
}

/// The role for a caller with no node: the root only when nothing says it is a subagent (D12);
/// otherwise `agent_type` as a hint that can lower the role but never raise it past a leaf. A type
/// that is not recognisably a leaf gets the most restricted role.
pub fn role_without_node(caller: &Caller) -> Role {
    if !caller.is_subagent() {
        return Role::Root;
    }
    match caller.agent_type() {
        Some(kind) if kind.to_ascii_lowercase().contains("leaf") => Role::Leaf,
        _ => most_restricted(),
    }
}

type NodeRow = (String, String);

async fn role_from_row(pool: &SqlitePool, column: &'static str, value: &str) -> Option<Role> {
    // `AssertSqlSafe`: `column` is one of two literal column names, and the value is bound.
    let sql = sqlx::AssertSqlSafe(format!(
        "SELECT kind, owned_paths FROM work_nodes WHERE {column} = ? \
         ORDER BY (status = 'running') DESC, created_at DESC, id DESC LIMIT 1"
    ));
    let found: Result<Option<NodeRow>, _> =
        sqlx::query_as(sql).bind(value).fetch_optional(pool).await;
    match found {
        Ok(Some((kind, owned))) => {
            let owned: Vec<String> = serde_json::from_str(&owned).unwrap_or_default();
            Some(role_of_node(&kind, &owned))
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(%error, "role policy: could not read the caller's node");
            None
        }
    }
}

/// Resolves the caller's role: the node bound to its `agent_id`, then the node bound to its
/// `run_id`, then [`role_without_node`]. A storage error is treated as "no node" and so never
/// resolves upwards of what the markers allow.
pub async fn resolve(pool: &SqlitePool, caller: &Caller, run_id: Option<i64>) -> Role {
    if let Some(agent_id) = caller.agent_id()
        && let Some(role) = role_from_row(pool, "agent_id", agent_id).await
    {
        return role;
    }
    if let Some(run_id) = run_id
        && let Some(role) = role_from_row(pool, "run_id", &run_id.to_string()).await
    {
        return role;
    }
    role_without_node(caller)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller(id: Option<&str>, kind: Option<&str>, subagent: bool) -> Caller {
        Caller {
            agent_id: id.map(str::to_owned),
            agent_type: kind.map(str::to_owned),
            subagent,
        }
    }

    #[test]
    fn a_leaf_never_has_more_permission_than_the_root() {
        let root = policy(Role::Root);
        for role in ROLES {
            let held = policy(role);
            // A restriction the root carries, every other role carries too (the conductor fence is
            // the root's division-of-labour rule, and the node fence bounds everyone else's hand work).
            assert!(held.node_fence >= root.node_fence, "{role:?} node fence");
            assert!(held.git_queue >= root.git_queue, "{role:?} git queue");
            // A permission the role holds, the root holds too.
            assert!(root.open_children >= held.open_children, "{role:?} open children");
        }
        assert!(!policy(Role::Leaf).conductor_fence);
        assert!(!policy(Role::Leaf).open_children);
    }

    #[test]
    fn the_table_starts_as_the_behaviour_that_exists_today() {
        assert!(policy(Role::Root).conductor_fence);
        for role in [Role::Orchestrator, Role::Leaf, Role::Reviewer] {
            assert!(!policy(role).conductor_fence, "{role:?}");
        }
        for role in ROLES {
            assert!(policy(role).node_fence && policy(role).git_queue, "{role:?}");
        }
        assert!(policy(Role::Orchestrator).open_children);
        assert!(!policy(Role::Leaf).open_children && !policy(Role::Reviewer).open_children);
    }

    #[test]
    fn the_most_restricted_role_holds_a_subset_of_every_other_roles_permissions() {
        let low = policy(most_restricted());
        for role in ROLES {
            let held = policy(role);
            assert!(held.open_children >= low.open_children, "{role:?}");
            assert!(low.node_fence >= held.node_fence && low.git_queue >= held.git_queue);
        }
        assert_eq!(ROLES[ROLES.len() - 1], most_restricted());
    }

    #[test]
    fn a_node_row_gives_the_role_and_an_unknown_kind_the_lowest() {
        let owns = vec!["core/src/a/**".to_owned()];
        assert_eq!(role_of_node("root", &[]), Role::Root);
        assert_eq!(role_of_node("orchestrator", &owns), Role::Orchestrator);
        assert_eq!(role_of_node("leaf", &owns), Role::Leaf);
        assert_eq!(role_of_node("leaf", &[]), Role::Reviewer);
        assert_eq!(role_of_node("planner", &owns), most_restricted());
    }

    #[test]
    fn without_a_node_only_a_call_with_no_subagent_marker_is_the_root() {
        assert_eq!(role_without_node(&caller(None, None, false)), Role::Root);
        // Blank fields are not markers.
        assert_eq!(role_without_node(&caller(Some(" "), Some(""), false)), Role::Root);
        // D12: a marker without a usable agent_id is not the root.
        assert_eq!(role_without_node(&caller(None, None, true)), most_restricted());
        assert_eq!(role_without_node(&caller(None, Some("general-purpose"), false)), most_restricted());
        assert_eq!(role_without_node(&caller(Some("a1"), None, false)), most_restricted());
    }

    #[test]
    fn agent_type_is_a_hint_that_never_raises_a_caller_past_a_leaf() {
        assert_eq!(role_without_node(&caller(Some("a1"), Some("wf-leaf"), false)), Role::Leaf);
        for chosen in ["root", "wf-orchestrator", "orchestrator", "wf-reviewer"] {
            let role = role_without_node(&caller(Some("a1"), Some(chosen), false));
            assert_eq!(role, most_restricted(), "{chosen}");
        }
    }

    async fn pool() -> SqlitePool {
        let (state, _) = crate::execution::runs::testkit::test_state_with_runner(
            None,
            crate::state::DEFAULT_RUN_TIMEOUT,
        )
        .await;
        state.pool.clone()
    }

    async fn node(pool: &SqlitePool, id: &str, kind: &str, owned: &str, agent: Option<&str>, run: Option<&str>) {
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, owned_paths, \
             agent_id, run_id, created_at, updated_at) \
             VALUES (?, ?, 0, ?, 't', 'g', 'running', ?, ?, ?, '2026-11-10T00:00:00Z', '2026-11-10T00:00:00Z')",
        )
        .bind(id)
        .bind(id)
        .bind(kind)
        .bind(owned)
        .bind(agent)
        .bind(run)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn the_role_comes_from_the_node_row_and_not_from_the_type_the_caller_chose() {
        let pool = pool().await;
        node(&pool, "n-leaf", "leaf", r#"["core/src/a/**"]"#, Some("a-leaf"), None).await;
        node(&pool, "n-orch", "orchestrator", "[]", Some("a-orch"), None).await;
        node(&pool, "n-rev", "leaf", "[]", None, Some("42")).await;
        // A leaf that names itself an orchestrator stays a leaf.
        let liar = caller(Some("a-leaf"), Some("wf-orchestrator"), false);
        assert_eq!(resolve(&pool, &liar, None).await, Role::Leaf);
        assert_eq!(resolve(&pool, &caller(Some("a-orch"), None, false), None).await, Role::Orchestrator);
        // The run's node, when there is no agent id.
        assert_eq!(resolve(&pool, &Caller::default(), Some(42)).await, Role::Reviewer);
        // An agent id with no node falls to the hint, never to the root.
        assert_eq!(resolve(&pool, &caller(Some("unknown"), None, false), None).await, most_restricted());
        // Nothing at all: the root.
        assert_eq!(resolve(&pool, &Caller::default(), None).await, Role::Root);
    }
}
