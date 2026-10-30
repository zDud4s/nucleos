//! stub: context-fill limits and verdicts

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
