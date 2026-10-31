//! The engine description table (contract A1): one row per engine, plain data plus lookups.
//!
//! `TABLE` lists Claude, Codex and Local. The `*_in` lookups take the table as an argument
//! (contract E3), so a test can append a fourth row without any production registration path;
//! production code passes `TABLE`.
//!
//! The vocabularies that used to repeat these facts (`capabilities::Engine`, `RunnerKind`,
//! `ChatEngine`, `ENGINE_SEAT_KINDS`, the runner and utility-default lookups) read from here.
//! What stays engine-specific (capability walls in `translate`, Codex flags) lives beside its
//! engine, not in a row.

use std::sync::Arc;

use crate::config::{ModelsConfig, SeatKind};
use crate::execution::runner::CommandRunner;
use crate::models::model_catalog::Discovered;

/// Engine ids, named once so the enums that mirror the table can match on them.
pub const CLAUDE: &str = "claude";
pub const CODEX: &str = "codex";
pub const LOCAL: &str = "local";

/// How an engine is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// A CLI process the daemon launches.
    Cli,
    /// An HTTP endpoint.
    Http,
}

/// One engine: plain data plus the three hooks that differ per engine.
#[derive(Clone)]
pub struct EngineDescription {
    pub id: &'static str,
    pub display_name: &'static str,
    pub seat_kind: SeatKind,
    pub transport: Transport,
    /// Whether the engine is usable on this machine (nothing is run).
    pub installed: fn() -> bool,
    /// The cheap model a background one-shot asks this engine for, among the models known now.
    pub utility_default: fn(&[Discovered]) -> Option<String>,
    /// The runner this engine launches, built from the models file; `None` when it has no CLI runner.
    pub runner: fn(&ModelsConfig) -> Option<Arc<dyn CommandRunner>>,
}

/// Whether the Claude CLI resolves on this machine.
fn claude_installed() -> bool {
    let configured = std::env::var_os("NUCLEOS_CLAUDE_BIN").unwrap_or_else(|| "claude".into());
    crate::health::resolve_program(&configured).is_some()
}

/// Whether the Codex CLI resolves on this machine.
fn codex_installed() -> bool {
    crate::execution::runner::codex_program().is_some()
}

/// The local model needs no binary of its own.
fn local_installed() -> bool {
    true
}

/// The Claude runner, built from the models file as `main.rs` builds the primary.
pub(crate) fn claude_runner(models: &ModelsConfig) -> Arc<dyn CommandRunner> {
    Arc::new(crate::execution::runner::ClaudeCliRunner {
        model: models.claude_model.clone(),
        plan_model: models.plan_model.clone(),
        review_model: models.review_model.clone(),
        resolve_model: models.resolve_model.clone(),
        resolve_effort: models.resolve_effort.clone(),
    })
}

/// The Codex runner, built from the models file as `main.rs` builds the primary.
pub(crate) fn codex_runner(models: &ModelsConfig) -> Arc<dyn CommandRunner> {
    Arc::new(crate::execution::runner::CodexCliRunner {
        model: models.codex_model.clone(),
        sandbox_mode: None,
        web_search: None,
    })
}

fn claude_row_runner(models: &ModelsConfig) -> Option<Arc<dyn CommandRunner>> {
    Some(claude_runner(models))
}

fn codex_row_runner(models: &ModelsConfig) -> Option<Arc<dyn CommandRunner>> {
    Some(codex_runner(models))
}

fn local_runner(_models: &ModelsConfig) -> Option<Arc<dyn CommandRunner>> {
    None
}

fn local_utility_default(_known: &[Discovered]) -> Option<String> {
    None
}

/// The production table, in the order every derived list shows.
pub const TABLE: &[EngineDescription] = &[
    EngineDescription {
        id: CLAUDE,
        display_name: "Claude",
        seat_kind: SeatKind::Cloud,
        transport: Transport::Cli,
        installed: claude_installed,
        utility_default: crate::models::model_catalog::claude_utility_default,
        runner: claude_row_runner,
    },
    EngineDescription {
        id: CODEX,
        display_name: "Codex",
        seat_kind: SeatKind::Cloud,
        transport: Transport::Cli,
        installed: codex_installed,
        utility_default: crate::models::model_catalog::codex_utility_default,
        runner: codex_row_runner,
    },
    EngineDescription {
        id: LOCAL,
        display_name: "Local",
        seat_kind: SeatKind::Local,
        transport: Transport::Http,
        installed: local_installed,
        utility_default: local_utility_default,
        runner: local_runner,
    },
];

/// The row with this id.
pub fn find_in<'a>(table: &'a [EngineDescription], id: &str) -> Option<&'a EngineDescription> {
    table.iter().find(|row| row.id == id)
}

/// The row with this id when it is a CLI runner.
pub fn cli_row_in<'a>(table: &'a [EngineDescription], id: &str) -> Option<&'a EngineDescription> {
    find_in(table, id).filter(|row| row.transport == Transport::Cli)
}

/// `(id, seat kind)` per row, in table order.
pub fn seat_kinds_in(table: &[EngineDescription]) -> Vec<(&'static str, SeatKind)> {
    table.iter().map(|row| (row.id, row.seat_kind)).collect()
}

/// The ids of the rows reached through `transport`.
pub fn ids_with_transport_in(
    table: &[EngineDescription],
    transport: Transport,
) -> Vec<&'static str> {
    table
        .iter()
        .filter(|row| row.transport == transport)
        .map(|row| row.id)
        .collect()
}

/// The ids of the rows whose probe says they are usable here.
pub fn installed_in(table: &[EngineDescription]) -> Vec<&'static str> {
    table
        .iter()
        .filter(|row| (row.installed)())
        .map(|row| row.id)
        .collect()
}

/// The ids a conversation can name as its engine.
pub fn chat_ids_in(table: &[EngineDescription]) -> Vec<&'static str> {
    table.iter().map(|row| row.id).collect()
}

/// The CLI runner a string names. Unknown, empty or non-CLI ids collapse to claude.
pub fn runner_id_in(table: &[EngineDescription], id: &str) -> &'static str {
    cli_row_in(table, id).map_or(CLAUDE, |row| row.id)
}

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
        assert_eq!(
            (row("claude").utility_default)(&[]),
            Some("haiku".to_owned())
        );
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
        assert_eq!(
            ids_with_transport_in(TABLE, Transport::Cli),
            ["claude", "codex"]
        );

        // Installed engines: only rows whose probe says so, in table order.
        let installed = installed_in(TABLE);
        assert!(
            installed
                .iter()
                .all(|id| ["claude", "codex", "local"].contains(id))
        );

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

#[cfg(test)]
mod row_tests {
    use super::*;
    use crate::config::{ModelsConfig, SeatKind};
    use crate::execution::runner::CommandRunner;
    use crate::models::model_catalog::Discovered;
    use std::sync::Arc;

    fn probe_true() -> bool {
        true
    }

    fn probe_false() -> bool {
        false
    }

    fn no_utility_default(_known: &[Discovered]) -> Option<String> {
        None
    }

    fn no_runner(_models: &ModelsConfig) -> Option<Arc<dyn CommandRunner>> {
        None
    }

    fn row(id: &'static str, transport: Transport, installed: fn() -> bool) -> EngineDescription {
        EngineDescription {
            id,
            display_name: "Row",
            seat_kind: SeatKind::Cloud,
            transport,
            installed,
            utility_default: no_utility_default,
            runner: no_runner,
        }
    }

    #[test]
    fn installed_in_keeps_table_order_and_drops_a_false_probe() {
        let rows = vec![
            row("first", Transport::Cli, probe_true),
            row("second", Transport::Cli, probe_false),
            row("third", Transport::Cli, probe_true),
        ];
        assert_eq!(installed_in(&rows), ["first", "third"]);
        // The row is dropped by its probe alone: the transport lookup still lists it.
        assert_eq!(
            ids_with_transport_in(&rows, Transport::Cli),
            ["first", "second", "third"]
        );
    }

    #[test]
    fn a_fourth_cli_row_resolves_through_the_runner_lookups() {
        let mut rows: Vec<EngineDescription> = TABLE.to_vec();
        rows.push(row("extra-cli", Transport::Cli, probe_true));
        let rows = rows.as_slice();
        assert_eq!(runner_id_in(rows, "extra-cli"), "extra-cli");
        assert!(cli_row_in(rows, "extra-cli").is_some());
        assert_eq!(
            ids_with_transport_in(rows, Transport::Cli),
            ["claude", "codex", "extra-cli"]
        );
        // The production table does not know the row: it collapses to claude.
        assert_eq!(runner_id_in(TABLE, "extra-cli"), "claude");
    }
}
