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
}
