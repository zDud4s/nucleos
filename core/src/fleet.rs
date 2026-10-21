//! The live fleet tree: every running agent session on this machine, normalised, pushed to the
//! shell over one NDJSON stream. Design: `.ai/specs/2026-10-20-fleet-arvore-viva-design.md`.

pub mod daemon_runs;
pub mod event;
pub mod fuse;
pub mod limits;
pub mod model;
pub mod source;
pub mod tail;
#[cfg(test)]
pub(crate) mod testkit;

use std::path::PathBuf;
use std::sync::Arc;

use source::{AgentSource, EdgeSource};

/// What an adapter may need from the daemon. Built once by the hub; tests pass a tempdir `home`.
#[derive(Clone)]
pub struct FleetEnv {
    pub pool: sqlx::SqlitePool,
    /// The user's home (`~`). Adapters derive `~/.claude` and `~/.codex` from it.
    pub home: PathBuf,
    /// The live buffers of the runs this daemon is executing (`AppState.run_tails`). Only
    /// `daemon_runs` reads it; `crate::execution::runs::read_tail` is the reader.
    pub run_tails: crate::state::RunTails,
}

/// The registered agent and edge sources, as `sources` returns them.
pub type Sources = (Vec<Arc<dyn AgentSource>>, Vec<Arc<dyn EdgeSource>>);

/// Every source, in precedence order. Each track adds ONE `push` line here; nothing else in this
/// file changes. Edge sources are applied in the order they are listed: native edges first (they
/// come from the `AgentSource`s themselves), then `work_nodes`, then `run_launch`.
pub fn sources(env: &FleetEnv) -> Sources {
    let _ = env;
    #[allow(unused_mut, clippy::vec_init_then_push)]
    let mut agents: Vec<Arc<dyn AgentSource>> = Vec::new();
    agents.push(Arc::new(daemon_runs::DaemonRuns::new(env)));
    // TODO-B: agents.push(Arc::new(claude_code::ClaudeCode::new(env)));
    // TODO-C: agents.push(Arc::new(codex_cli::CodexCli::new(env)));
    #[allow(unused_mut)]
    let mut edges: Vec<Arc<dyn EdgeSource>> = Vec::new();
    // TODO-C: edges.push(Arc::new(work_nodes_edges::WorkNodesEdges::new(env)));
    // TODO-C: edges.push(Arc::new(run_launch::RunLaunch::new(env)));
    (agents, edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_registry_builds_over_an_empty_home() {
        let pool = crate::testdb::fresh_pool().await;
        let env = FleetEnv {
            pool,
            home: std::env::temp_dir(),
            run_tails: Default::default(),
        };
        let (agents, edges) = sources(&env);
        let names: Vec<_> = agents.iter().map(|a| a.name()).collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            names.len(),
            unique.len(),
            "two sources share a name: {names:?}"
        );
        assert!(edges.iter().all(|e| !e.name().is_empty()));
    }
}
