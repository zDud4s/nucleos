//! Context-fill limits per node kind and the verdict they give on a run's fill.
//!
//! Pure: nothing here reads a file or the database. `limits` holds the defaults and
//! `limits_from_speed_yaml` overlays the `nodes:` mapping of a `speed.yaml` the caller has
//! already read. The caller enforces the verdict.

/// How full a node's context is, from least to most severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    Ok,
    Warn,
    Wrap,
    Block,
}

/// Context-fill thresholds in tokens; each one is reached at exactly its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextLimits {
    pub warn: i64,
    pub wrap: i64,
    pub block: i64,
}

/// The default of `nodes.ceiling` in `speed.yaml`.
const DEFAULT_BLOCK: i64 = 200_000;

/// Whether `kind` gets the orchestrator numbers. Everything else gets the leaf numbers, so an
/// unknown kind takes the stricter ones.
fn is_orchestrator(kind: &str) -> bool {
    matches!(kind, "orchestrator" | "root")
}

/// The default limits for a node kind.
pub fn limits(kind: &str) -> ContextLimits {
    let (warn, wrap) = if is_orchestrator(kind) {
        (100_000, 125_000)
    } else {
        (80_000, 100_000)
    };
    ContextLimits {
        warn,
        wrap,
        block: DEFAULT_BLOCK,
    }
}

/// The verdict for `fill` against the default limits of `kind`.
pub fn verdict(kind: &str, fill: Option<i64>) -> Fill {
    verdict_against(&limits(kind), fill)
}

/// The verdict for `fill` against explicit limits. An unknown fill is `Ok`.
pub fn verdict_against(limits: &ContextLimits, fill: Option<i64>) -> Fill {
    let Some(fill) = fill else {
        return Fill::Ok;
    };
    if fill >= limits.block {
        Fill::Block
    } else if fill >= limits.wrap {
        Fill::Wrap
    } else if fill >= limits.warn {
        Fill::Warn
    } else {
        Fill::Ok
    }
}

/// The limits of `kind` with the `nodes:` mapping of a `speed.yaml` over the defaults. A missing
/// or non-integer key keeps its default; empty, malformed or `nodes`-less text gives
/// `limits(kind)`.
pub fn limits_from_speed_yaml(text: &str, kind: &str) -> ContextLimits {
    let defaults = limits(kind);
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(text) else {
        return defaults;
    };
    let Some(nodes) = doc.get("nodes") else {
        return defaults;
    };
    let (warn_key, wrap_key) = if is_orchestrator(kind) {
        ("orchestrator_warn", "orchestrator_wrap")
    } else {
        ("leaf_warn", "leaf_wrap")
    };
    let read = |key: &str, default: i64| {
        nodes
            .get(key)
            .and_then(serde_yaml::Value::as_i64)
            .unwrap_or(default)
    };
    ContextLimits {
        warn: read(warn_key, defaults.warn),
        wrap: read(wrap_key, defaults.wrap),
        block: read("ceiling", defaults.block),
    }
}

/// Whether a tool may still run once a node is at `Wrap` or `Block`. Exact match on the name only:
/// the handoff-path restriction on `Write` and the orchestrator-only restriction on `node_open`
/// are added by D-PRETOOL, which has the arguments.
pub fn allowed_when_wrapping(tool: &str) -> bool {
    matches!(
        tool,
        "mcp__nucleos__node_update"
            | "mcp__nucleos__node_close"
            | "mcp__nucleos__node_open"
            | "Write"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [&str; 2] = ["leaf", "orchestrator"];

    #[test]
    fn leaf_warns_at_80k() {
        assert_eq!(limits("leaf").warn, 80_000);
        assert_eq!(verdict("leaf", Some(79_999)), Fill::Ok);
        assert_eq!(verdict("leaf", Some(80_000)), Fill::Warn);
    }

    #[test]
    fn leaf_wraps_at_100k() {
        assert_eq!(limits("leaf").wrap, 100_000);
        assert_eq!(verdict("leaf", Some(99_999)), Fill::Warn);
        assert_eq!(verdict("leaf", Some(100_000)), Fill::Wrap);
    }

    #[test]
    fn orchestrator_warns_at_100k_wraps_at_125k() {
        let limits = limits("orchestrator");
        assert_eq!(limits.warn, 100_000);
        assert_eq!(limits.wrap, 125_000);
        assert_eq!(verdict("orchestrator", Some(99_999)), Fill::Ok);
        assert_eq!(verdict("orchestrator", Some(100_000)), Fill::Warn);
        assert_eq!(verdict("orchestrator", Some(124_999)), Fill::Warn);
        assert_eq!(verdict("orchestrator", Some(125_000)), Fill::Wrap);
    }

    #[test]
    fn none_fill_is_ok() {
        for kind in KINDS {
            assert_eq!(verdict(kind, None), Fill::Ok, "kind {kind}");
        }
    }

    #[test]
    fn limits_read_from_speed_yaml_nodes_mapping() {
        let yaml = "\
nodes:
  leaf_warn: 70000
  leaf_wrap: 90000
  orchestrator_warn: 110000
  orchestrator_wrap: 130000
  ceiling: 190000
";
        assert_eq!(
            limits_from_speed_yaml(yaml, "leaf"),
            ContextLimits {
                warn: 70_000,
                wrap: 90_000,
                block: 190_000,
            }
        );
        assert_eq!(
            limits_from_speed_yaml(yaml, "orchestrator"),
            ContextLimits {
                warn: 110_000,
                wrap: 130_000,
                block: 190_000,
            }
        );
    }

    #[test]
    fn allowed_when_wrapping_only_node_update_close_open_and_handoff_write() {
        // The handoff-path restriction on `Write` and the orchestrator-only restriction on
        // `node_open` are not expressible in this signature; they belong to D-PRETOOL.
        for tool in [
            "mcp__nucleos__node_update",
            "mcp__nucleos__node_close",
            "mcp__nucleos__node_open",
            "Write",
        ] {
            assert!(allowed_when_wrapping(tool), "{tool} must be allowed");
        }
        for tool in [
            "Edit",
            "Bash",
            "Read",
            "Grep",
            "Agent",
            "mcp__nucleos__verify",
            "mcp__nucleos__node_tree",
            "node_close",
            "mcp__nucleos__node_close_all",
            "",
        ] {
            assert!(!allowed_when_wrapping(tool), "{tool:?} must be denied");
        }
    }

    #[test]
    fn block_is_stricter_than_wrap() {
        for kind in KINDS {
            let limits = limits(kind);
            assert!(limits.block > limits.wrap, "kind {kind}");
            assert_eq!(verdict(kind, Some(limits.block - 1)), Fill::Wrap, "kind {kind}");
            assert_eq!(verdict(kind, Some(limits.block)), Fill::Block, "kind {kind}");
        }
    }

    #[test]
    fn missing_speed_yaml_keys_keep_defaults() {
        let only_leaf_warn = "nodes:\n  leaf_warn: 70000\n";
        assert_eq!(
            limits_from_speed_yaml(only_leaf_warn, "leaf"),
            ContextLimits {
                warn: 70_000,
                ..limits("leaf")
            }
        );
        assert_eq!(
            limits_from_speed_yaml(only_leaf_warn, "orchestrator"),
            limits("orchestrator")
        );

        let only_ceiling = "nodes:\n  ceiling: 190000\n";
        for kind in KINDS {
            assert_eq!(
                limits_from_speed_yaml(only_ceiling, kind),
                ContextLimits {
                    block: 190_000,
                    ..limits(kind)
                },
                "kind {kind}"
            );
        }

        for yaml in ["", "nodes: {}\n", "other: 1\n"] {
            for kind in KINDS {
                assert_eq!(
                    limits_from_speed_yaml(yaml, kind),
                    limits(kind),
                    "kind {kind}, yaml {yaml:?}"
                );
            }
        }
    }

    #[test]
    fn malformed_speed_yaml_gives_defaults() {
        for yaml in ["nodes: {leaf_warn: 70000", "nodes:\n  leaf_warn: [unclosed\n"] {
            for kind in KINDS {
                assert_eq!(
                    limits_from_speed_yaml(yaml, kind),
                    limits(kind),
                    "kind {kind}, yaml {yaml:?}"
                );
            }
        }
    }

    // -- `verdict_for_run` --------------------------------------------------------------------

    /// A run with `fill`, as the hook finds it.
    async fn seed_run(pool: &sqlx::SqlitePool, fill: Option<i64>) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, project_id, created_at, context_fill)
             VALUES ('x', 'running', 'shadow', 'p', '2026-10-30T00:00:00Z', ?)",
        )
        .bind(fill)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// A node bound to `run_id`, stored the way `node_run::launch` stores it: decimal TEXT.
    async fn seed_node(pool: &sqlx::SqlitePool, node_id: &str, kind: &str, run_id: i64) {
        sqlx::query(
            "INSERT INTO work_nodes
               (id, project_id, root_id, depth, kind, title, goal, status, run_id, created_at,
                updated_at)
             VALUES (?, 'p', 'the-root', 1, ?, 't', 'g', 'running', ?, '2026-10-30T00:00:00Z',
                '2026-10-30T00:00:00Z')",
        )
        .bind(node_id)
        .bind(kind)
        .bind(run_id.to_string())
        .execute(pool)
        .await
        .unwrap();
    }

    /// The fill of a verdict that must exist.
    async fn fill_of(
        pool: &sqlx::SqlitePool,
        run_id: i64,
        speed_yaml: Option<&str>,
        cap: Option<i64>,
    ) -> Fill {
        verdict_for_run(pool, run_id, speed_yaml, cap)
            .await
            .unwrap()
            .expect("the run has a node")
            .fill
    }

    #[tokio::test]
    async fn verdict_for_run_reads_fill_and_node_kind() {
        let pool = crate::testdb::fresh_pool().await;
        let run_id = seed_run(&pool, Some(100_000)).await;
        seed_node(&pool, "the-node", "leaf", run_id).await;

        let verdict = verdict_for_run(&pool, run_id, None, None).await.unwrap();

        assert_eq!(
            verdict,
            Some(NodeRunVerdict {
                fill: Fill::Wrap,
                node_id: "the-node".to_owned(),
                root_id: "the-root".to_owned(),
                kind: "leaf".to_owned(),
                project_id: Some("p".to_owned()),
            })
        );
    }

    #[tokio::test]
    async fn verdict_for_run_is_none_for_a_run_without_a_node() {
        let pool = crate::testdb::fresh_pool().await;
        let run_id = seed_run(&pool, Some(500_000)).await;
        assert_eq!(
            verdict_for_run(&pool, run_id, None, None).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn verdict_for_run_is_none_for_a_missing_run() {
        let pool = crate::testdb::fresh_pool().await;
        assert_eq!(
            verdict_for_run(&pool, 999_999, None, None).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn verdict_for_run_with_unknown_fill_is_ok() {
        let pool = crate::testdb::fresh_pool().await;
        let run_id = seed_run(&pool, None).await;
        seed_node(&pool, "the-node", "leaf", run_id).await;
        assert_eq!(fill_of(&pool, run_id, None, None).await, Fill::Ok);
    }

    #[tokio::test]
    async fn verdict_for_run_uses_the_speed_yaml_limits() {
        let pool = crate::testdb::fresh_pool().await;
        let run_id = seed_run(&pool, Some(25)).await;
        seed_node(&pool, "the-node", "leaf", run_id).await;
        let yaml = "nodes:\n  leaf_warn: 10\n  leaf_wrap: 20\n  ceiling: 30\n";

        assert_eq!(fill_of(&pool, run_id, Some(yaml), None).await, Fill::Wrap);
        assert_eq!(fill_of(&pool, run_id, None, None).await, Fill::Ok);
    }

    #[tokio::test]
    async fn verdict_for_run_uses_orchestrator_limits_for_an_orchestrator_node() {
        let pool = crate::testdb::fresh_pool().await;
        // Past a leaf's wrap and short of an orchestrator's.
        for (node_id, kind, expected) in [
            ("a-leaf", "leaf", Fill::Wrap),
            ("an-orchestrator", "orchestrator", Fill::Warn),
            ("a-root", "root", Fill::Warn),
        ] {
            let run_id = seed_run(&pool, Some(110_000)).await;
            seed_node(&pool, node_id, kind, run_id).await;
            assert_eq!(fill_of(&pool, run_id, None, None).await, expected, "{kind}");
        }
    }

    #[tokio::test]
    async fn verdict_for_run_applies_block_cap() {
        let pool = crate::testdb::fresh_pool().await;
        let run_id = seed_run(&pool, Some(150_000)).await;
        seed_node(&pool, "the-node", "leaf", run_id).await;
        assert_eq!(fill_of(&pool, run_id, None, None).await, Fill::Wrap);
        assert_eq!(
            fill_of(&pool, run_id, None, Some(150_000)).await,
            Fill::Block,
            "a smaller cap lowers block, reached at exactly its value"
        );
        assert_eq!(
            fill_of(&pool, run_id, None, Some(150_001)).await,
            Fill::Wrap
        );

        // A larger cap never raises it.
        let at_block = seed_run(&pool, Some(limits("leaf").block)).await;
        seed_node(&pool, "at-block", "leaf", at_block).await;
        assert_eq!(
            fill_of(&pool, at_block, None, Some(900_000)).await,
            Fill::Block
        );

        // The cap also lowers a `ceiling` the yaml set.
        let yaml = "nodes:\n  ceiling: 190000\n";
        assert_eq!(
            fill_of(&pool, run_id, Some(yaml), Some(150_000)).await,
            Fill::Block
        );
    }

    #[tokio::test]
    async fn verdict_for_run_does_not_match_another_runs_node() {
        let pool = crate::testdb::fresh_pool().await;
        let with_node = seed_run(&pool, Some(1_000)).await;
        let without = seed_run(&pool, Some(500_000)).await;
        seed_node(&pool, "the-node", "leaf", with_node).await;

        assert_eq!(
            verdict_for_run(&pool, without, None, None).await.unwrap(),
            None
        );
        assert_eq!(fill_of(&pool, with_node, None, None).await, Fill::Ok);
    }

    // -- `block_cap` --------------------------------------------------------------------------

    #[test]
    fn block_cap_subtracts_the_margin_and_ignores_unknown_limits() {
        let margin = MODEL_BLOCK_MARGIN;
        assert!(margin > 0, "got {margin}");
        assert_eq!(block_cap(Some(margin + 100_000)), Some(100_000));
        assert_eq!(block_cap(None), None);
        assert_eq!(block_cap(Some(0)), None);
        assert_eq!(block_cap(Some(-1)), None);
    }

    // -- `allowed_for_node` -------------------------------------------------------------------

    const CLOSE: &str = "mcp__nucleos__node_close";
    const UPDATE: &str = "mcp__nucleos__node_update";
    const OPEN: &str = "mcp__nucleos__node_open";

    fn state_dir() -> &'static std::path::Path {
        std::path::Path::new("state-dir")
    }

    fn write_to(path: &std::path::Path) -> serde_json::Value {
        serde_json::json!({"file_path": path.to_str().unwrap(), "content": "x"})
    }

    #[test]
    fn handoff_path_is_the_one_close_node_writes() {
        assert_eq!(
            handoff_path(state_dir(), "r", "n"),
            state_dir().join("nodes").join("r").join("n.md")
        );
    }

    #[test]
    fn handoff_write_allowed_only_on_the_nodes_own_path() {
        let own = handoff_path(state_dir(), "r", "n");
        let handoff = Some(own.as_path());
        assert!(allowed_for_node("Write", &write_to(&own), "leaf", handoff));

        for other in [
            handoff_path(state_dir(), "r", "another"),
            handoff_path(state_dir(), "another", "n"),
            own.with_extension("md.bak"),
            own.parent().unwrap().to_path_buf(),
            std::path::PathBuf::from("n.md"),
        ] {
            assert!(
                !allowed_for_node("Write", &write_to(&other), "leaf", handoff),
                "{}",
                other.display()
            );
        }
        for input in [
            serde_json::json!({}),
            serde_json::json!({"file_path": 7}),
            serde_json::json!({"path": own.to_str().unwrap()}),
            serde_json::Value::Null,
        ] {
            assert!(!allowed_for_node("Write", &input, "leaf", handoff), "{input}");
        }
    }

    #[test]
    fn handoff_write_with_parent_dir_segments_is_denied() {
        // Each one resolves onto the node's own file; only the exact path passes.
        let own = handoff_path(state_dir(), "r", "n");
        let nodes = state_dir().join("nodes");
        for dressed in [
            nodes.join("r").join("..").join("r").join("n.md"),
            nodes.join("other").join("..").join("r").join("n.md"),
        ] {
            assert!(
                !allowed_for_node("Write", &write_to(&dressed), "leaf", Some(own.as_path())),
                "{}",
                dressed.display()
            );
        }
    }

    #[test]
    fn write_without_a_handoff_path_is_denied() {
        let path = handoff_path(state_dir(), "r", "n");
        assert!(!allowed_for_node("Write", &write_to(&path), "leaf", None));
        assert!(!allowed_for_node("Write", &serde_json::json!({}), "leaf", None));
    }

    #[test]
    fn node_open_allowed_only_for_orchestrator_kinds() {
        let input = serde_json::json!({"title": "split"});
        for kind in ["orchestrator", "root"] {
            assert!(allowed_for_node(OPEN, &input, kind, None), "{kind}");
        }
        for kind in ["leaf", "", "something-else"] {
            assert!(!allowed_for_node(OPEN, &input, kind, None), "{kind:?}");
        }
    }

    #[test]
    fn node_update_and_close_allowed_for_every_kind() {
        let input = serde_json::json!({});
        let own = handoff_path(state_dir(), "r", "n");
        for tool in [UPDATE, CLOSE] {
            for kind in ["leaf", "orchestrator", "root", "something-else"] {
                assert!(allowed_for_node(tool, &input, kind, None), "{tool} {kind}");
                assert!(
                    allowed_for_node(tool, &input, kind, Some(own.as_path())),
                    "{tool} {kind}"
                );
            }
        }
    }

    #[test]
    fn other_tools_denied_whatever_the_input() {
        // The input names the very file a `Write` may touch, and the kind is the most trusted.
        let own = handoff_path(state_dir(), "r", "n");
        let input = write_to(&own);
        for tool in [
            "Edit",
            "NotebookEdit",
            "Bash",
            "Read",
            "Agent",
            "write",
            "mcp__nucleos__verify",
            "mcp__nucleos__node_tree",
            "node_close",
            "",
        ] {
            assert!(
                !allowed_for_node(tool, &input, "orchestrator", Some(own.as_path())),
                "{tool:?}"
            );
        }
    }
}
