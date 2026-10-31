//! The engine description table (contract A1): one row per engine, plain data plus lookups.
//!
//! RED stage: only the tests exist. The table, `EngineDescription`, `Transport` and the `*_in`
//! lookups (each takes the table as an argument, contract E3) are still to be written.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ModelsConfig, SeatKind};
    use crate::execution::runner::CommandRunner;
    use crate::models::model_catalog::Discovered;
    use std::sync::Arc;

    fn always_installed() -> bool {
        true
    }

    fn no_utility_default(_known: &[Discovered]) -> Option<String> {
        None
    }

    fn no_runner(_models: &ModelsConfig) -> Option<Arc<dyn CommandRunner>> {
        None
    }

    /// The fourth engine of the "one description plus tests" proof. It lives only in this module
    /// and reaches the lookups as part of a table argument; no production registration path.
    const EXTRA: EngineDescription = EngineDescription {
        id: "extra",
        display_name: "Extra",
        seat_kind: SeatKind::Cloud,
        transport: Transport::Http,
        installed: always_installed,
        utility_default: no_utility_default,
        runner: no_runner,
    };

    fn with_extra() -> Vec<EngineDescription> {
        let mut rows: Vec<EngineDescription> = TABLE.to_vec();
        rows.push(EXTRA);
        rows
    }

    #[test]
    fn table_lists_claude_codex_local_with_expected_fields() {
        let ids: Vec<&str> = TABLE.iter().map(|row| row.id).collect();
        assert_eq!(ids, ["claude", "codex", "local"]);

        let row = |id: &str| find_in(TABLE, id).expect("row present");
        assert_eq!(row("claude").display_name, "Claude");
        assert_eq!(row("codex").display_name, "Codex");
        assert_eq!(row("local").display_name, "Local");

        assert_eq!(row("claude").seat_kind, SeatKind::Cloud);
        assert_eq!(row("codex").seat_kind, SeatKind::Cloud);
        assert_eq!(row("local").seat_kind, SeatKind::Local);

        assert_eq!(row("claude").transport, Transport::Cli);
        assert_eq!(row("codex").transport, Transport::Cli);
        assert_eq!(row("local").transport, Transport::Http);

        // Utility defaults keep their legacy values: Claude's alias, no cheap tier for Local.
        assert_eq!((row("claude").utility_default)(&[]), Some("haiku".to_owned()));
        assert_eq!((row("local").utility_default)(&[]), None);

        // Runner constructors: the two CLIs build one, the local model has no CLI runner.
        let models = ModelsConfig::default();
        assert!((row("claude").runner)(&models).is_some());
        assert!((row("codex").runner)(&models).is_some());
        assert!((row("local").runner)(&models).is_none());
    }

    #[test]
    fn derived_lists_follow_the_table() {
        // Seat kinds: byte-identical to the legacy `ENGINE_SEAT_KINDS`.
        assert_eq!(
            seat_kinds_in(TABLE),
            vec![
                ("claude", SeatKind::Cloud),
                ("codex", SeatKind::Cloud),
                ("local", SeatKind::Local),
            ]
        );
        assert_eq!(
            seat_kinds_in(TABLE),
            crate::config::ENGINE_SEAT_KINDS.to_vec()
        );

        // CLI runners (what `translatable_runners` iterates) are the CLI-transport rows.
        assert_eq!(ids_with_transport_in(TABLE, Transport::Cli), ["claude", "codex"]);

        // Installed engines: only rows whose probe says so, in table order.
        let installed = installed_in(TABLE);
        assert!(installed.iter().all(|id| ["claude", "codex", "local"].contains(id)));

        // Chat engines a conversation can name.
        assert_eq!(chat_ids_in(TABLE), ["claude", "codex", "local"]);
    }

    #[test]
    fn string_lookups_match_legacy_results() {
        // `ModelsConfig::runner_of` / `cli_runner`: an unknown runner string collapses to claude.
        assert_eq!(runner_id_in(TABLE, "codex"), "codex");
        assert_eq!(runner_id_in(TABLE, "claude"), "claude");
        assert_eq!(runner_id_in(TABLE, "gemini"), "claude");
        assert_eq!(runner_id_in(TABLE, ""), "claude");
        // The local model is not a CLI runner: it never wins a runner lookup.
        assert_eq!(runner_id_in(TABLE, "local"), "claude");

        // `active_runner`: codex or claude, nothing else.
        let mut models = ModelsConfig::default();
        assert_eq!(models.active_runner(), "claude");
        models.primary_runner = Some("codex".to_owned());
        assert_eq!(models.active_runner(), "codex");
        models.primary_runner = Some("nonsense".to_owned());
        assert_eq!(models.active_runner(), "claude");

        // `runner_by_id`: ids that say nothing about a CLI stay `None`.
        use crate::models::model_catalog::runner_by_id;
        assert_eq!(runner_by_id("claude-opus-4"), Some("claude"));
        assert_eq!(runner_by_id("haiku"), Some("claude"));
        assert_eq!(runner_by_id("gpt-5"), Some("codex"));
        assert_eq!(runner_by_id("llama3"), None);
    }

    #[test]
    fn extra_description_appears_everywhere() {
        let rows = with_extra();
        let rows = rows.as_slice();

        assert!(find_in(rows, "extra").is_some());
        assert!(find_in(TABLE, "extra").is_none());
        assert_eq!(
            seat_kinds_in(rows).last(),
            Some(&("extra", SeatKind::Cloud))
        );
        assert!(installed_in(rows).contains(&"extra"));
        assert!(chat_ids_in(rows).contains(&"extra"));
        // HTTP transport: it is not a CLI runner, and no runner string resolves to it.
        assert!(!ids_with_transport_in(rows, Transport::Cli).contains(&"extra"));
        assert!(ids_with_transport_in(rows, Transport::Http).contains(&"extra"));
        assert_eq!(runner_id_in(rows, "extra"), "claude");
        // The production table is untouched by the test row.
        assert_eq!(TABLE.len(), 3);
    }
}
