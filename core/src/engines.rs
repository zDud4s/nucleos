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

pub mod agy;
mod agy_setting;

/// Engine ids, named once so the enums that mirror the table can match on them.
pub const CLAUDE: &str = "claude";
pub const CODEX: &str = "codex";
pub const LOCAL: &str = "local";
pub const AGY: &str = "agy";
pub const GEMINI_API: &str = "gemini-api";

/// The credential-store name of the Gemini API key. Named here, not in the daemon's `main.rs`,
/// because this crate (the row's gate and the catalogue) and the daemon (the key writer and loader)
/// must agree on one fact.
pub const GEMINI_KEY: &str = "gemini-api-key";

/// The refusal for the Gemini API route at every launch site of agents, teams and council, and for
/// its quota, which NucleOS does not meter.
pub const GEMINI_API_REFUSAL: &str =
    "the gemini-api engine is not available for agents, teams, council or quota";

/// The fact "a Gemini API key is stored", published once by the daemon at startup.
static GEMINI_KEY_PUBLISHED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Publishes whether a Gemini API key was found when the daemon started. A key stored later is
/// not seen until the daemon restarts.
pub fn publish_gemini_key(present: bool) {
    GEMINI_KEY_PUBLISHED.store(present, std::sync::atomic::Ordering::Relaxed);
}

/// Whether a credential-store read yielded a usable key: blank, absent and a read error are all
/// "no key".
pub fn gemini_key_present<E>(read: &Result<Option<String>, E>) -> bool {
    matches!(read, Ok(Some(key)) if !key.trim().is_empty())
}

/// Whether a Gemini API key is stored: the one fact the row's gate and the catalogue read.
///
/// Never touches the credential store: it returns the fact the daemon published at startup (see
/// `publish_gemini_key`), `false` until then. Under `cfg(test)` / `testkit` a thread-local
/// override set by `gemini_test_support::gemini_gate` wins when present.
pub fn gemini_key_stored() -> bool {
    #[cfg(any(test, feature = "testkit"))]
    {
        if let Some(stored) = gemini_test_support::OVERRIDE.with(|cell| cell.get()) {
            return stored;
        }
    }
    GEMINI_KEY_PUBLISHED.load(std::sync::atomic::Ordering::Relaxed)
}

/// The refusal for agy at every launch site of agents, teams and council.
pub const AGY_REFUSAL: &str = "the agy engine is not available for agents, teams or council";

/// Work that names no engine runs on the daemon's runner; under `primary_runner: agy` that is agy,
/// which agents, teams and council never use. Shared by their launch sites to refuse that work.
pub fn refuse_agy_primary(models: &crate::config::ModelsConfig) -> Result<(), &'static str> {
    if models.active_runner() == AGY {
        Err(AGY_REFUSAL)
    } else {
        Ok(())
    }
}

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
    /// Whether the row is switched on; a row that is off is absent from every derived list.
    pub gate: fn() -> bool,
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
pub fn claude_runner(models: &ModelsConfig) -> Arc<dyn CommandRunner> {
    Arc::new(crate::execution::runner::ClaudeCliRunner {
        model: models.claude_model.clone(),
        plan_model: models.plan_model.clone(),
        review_model: models.review_model.clone(),
        resolve_model: models.resolve_model.clone(),
        resolve_effort: models.resolve_effort.clone(),
    })
}

/// The Codex runner, built from the models file as `main.rs` builds the primary.
pub fn codex_runner(models: &ModelsConfig) -> Arc<dyn CommandRunner> {
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

/// The gate of the engines that are always on.
fn always() -> bool {
    true
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
        gate: always,
        utility_default: crate::models::model_catalog::claude_utility_default,
        runner: claude_row_runner,
    },
    EngineDescription {
        id: CODEX,
        display_name: "Codex",
        seat_kind: SeatKind::Cloud,
        transport: Transport::Cli,
        installed: codex_installed,
        gate: always,
        utility_default: crate::models::model_catalog::codex_utility_default,
        runner: codex_row_runner,
    },
    EngineDescription {
        id: LOCAL,
        display_name: "Local",
        seat_kind: SeatKind::Local,
        transport: Transport::Http,
        installed: local_installed,
        gate: always,
        utility_default: local_utility_default,
        runner: local_runner,
    },
    EngineDescription {
        id: AGY,
        display_name: "Antigravity",
        seat_kind: SeatKind::Cloud,
        transport: Transport::Cli,
        installed: agy::installed,
        gate: agy::enabled,
        utility_default: local_utility_default,
        runner: agy::runner,
    },
    // Hosted over HTTP, no CLI runner, and on only while a key is stored.
    EngineDescription {
        id: GEMINI_API,
        display_name: "Gemini API",
        seat_kind: SeatKind::Cloud,
        transport: Transport::Http,
        installed: local_installed,
        gate: gemini_key_stored,
        utility_default: local_utility_default,
        runner: local_runner,
    },
];

/// The rows whose gate is open, in table order.
pub fn enabled_in(table: &[EngineDescription]) -> Vec<&EngineDescription> {
    table.iter().filter(|row| (row.gate)()).collect()
}

/// The production rows that are switched on: what every derived list reads.
pub fn enabled_table() -> Vec<&'static EngineDescription> {
    enabled_in(TABLE)
}

/// The row with this id.
pub fn find_in<'a>(table: &'a [EngineDescription], id: &str) -> Option<&'a EngineDescription> {
    table.iter().find(|row| row.id == id)
}

/// The row with this id when it is a CLI runner.
pub fn cli_row_in<'a>(table: &'a [EngineDescription], id: &str) -> Option<&'a EngineDescription> {
    find_in(table, id).filter(|row| row.transport == Transport::Cli)
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

/// A runner that fails every run with a fixed message. Stands in for an engine the owner asked for
/// by name but has not enabled, so the daemon never answers with a different engine instead.
struct RefusingRunner {
    message: String,
}

#[async_trait::async_trait]
impl CommandRunner for RefusingRunner {
    async fn run_prompt(
        &self,
        _request: crate::execution::runner::RunRequest,
        _session_tx: tokio::sync::mpsc::UnboundedSender<String>,
        _transcript: Arc<std::sync::Mutex<String>>,
    ) -> std::io::Result<crate::execution::runner::RunOutcome> {
        Err(std::io::Error::other(self.message.clone()))
    }
}

/// The runner the daemon launches runs with, from `primary_runner` in the models file.
///
/// `codex` builds the Codex runner; `agy` builds the agy runner while its opt-in is on and a
/// refusing runner while it is off, never Claude; an absent or unknown name keeps the Claude CLI
/// (an unknown one warns, so a typo does not take the daemon down).
pub fn runner_for_primary(name: Option<&str>, models: &ModelsConfig) -> Arc<dyn CommandRunner> {
    match name {
        Some(CODEX) => {
            tracing::info!(model = %models.codex_model, "codex CLI selected as the run runner");
            codex_runner(models)
        }
        Some(AGY) => match agy::runner(models) {
            Some(runner) => {
                tracing::info!("agy CLI selected as the run runner");
                runner
            }
            None => {
                tracing::warn!("primary_runner is agy, but the agy engine is disabled in settings");
                Arc::new(RefusingRunner {
                    message: "primary_runner is agy, but the agy engine is disabled in settings"
                        .to_string(),
                })
            }
        },
        Some(other) if other != CLAUDE => {
            tracing::warn!(%other, "unknown primary_runner — keeping the Claude CLI");
            claude_runner(models)
        }
        _ => claude_runner(models),
    }
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

    fn gate_open() -> bool {
        true
    }

    fn gate_shut() -> bool {
        false
    }

    /// The extra engine of the "one description plus tests" proof. It lives only in this module
    /// and reaches the lookups as part of a table argument; no production registration path.
    const EXTRA: EngineDescription = EngineDescription {
        id: "extra",
        display_name: "Extra",
        seat_kind: SeatKind::Cloud,
        transport: Transport::Http,
        installed: always_installed,
        gate: gate_open,
        utility_default: no_utility_default,
        runner: no_runner,
    };

    fn with_extra() -> Vec<EngineDescription> {
        let mut rows: Vec<EngineDescription> = TABLE.to_vec();
        rows.push(EXTRA);
        rows
    }

    #[test]
    fn table_lists_claude_codex_local_agy_with_expected_fields() {
        assert_eq!(TABLE.len(), 5);
        let ids: Vec<&str> = TABLE.iter().map(|row| row.id).collect();
        assert_eq!(ids, ["claude", "codex", "local", "agy", "gemini-api"]);
        assert_eq!(AGY, "agy");

        let row = |id: &str| find_in(TABLE, id).expect("row present");
        assert_eq!(row("claude").display_name, "Claude");
        assert_eq!(row("codex").display_name, "Codex");
        assert_eq!(row("local").display_name, "Local");
        assert!(!row("agy").display_name.is_empty());

        assert_eq!(row("claude").seat_kind, SeatKind::Cloud);
        assert_eq!(row("codex").seat_kind, SeatKind::Cloud);
        assert_eq!(row("local").seat_kind, SeatKind::Local);
        // The config guard demands a seat kind for every row; agy is a cloud engine.
        assert_eq!(row("agy").seat_kind, SeatKind::Cloud);

        assert_eq!(row("claude").transport, Transport::Cli);
        assert_eq!(row("codex").transport, Transport::Cli);
        assert_eq!(row("local").transport, Transport::Http);
        assert_eq!(row("agy").transport, Transport::Cli);

        // The three original engines are always on; agy answers to its opt-in setting, which is
        // off by default.
        assert!((row("claude").gate)());
        assert!((row("codex").gate)());
        assert!((row("local").gate)());
        assert_eq!((row("agy").gate)(), agy::enabled());
        assert!(!(row("agy").gate)());

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

    /// RED (plan Node 3, test 1; contract E6/E22). The Gemini API is ONE Http row with no CLI runner,
    /// whose gate is "a key is stored". The gate is flipped through
    /// `gemini_test_support::gemini_gate`, which GREEN adds beside `agy_test_support` and which
    /// replaces the credential-store read for the guard's lifetime; no real credential is touched.
    #[test]
    fn gemini_api_row_is_http_without_runner_and_off_cli_lists() {
        assert_eq!(GEMINI_API, "gemini-api");
        let rows: Vec<_> = TABLE.iter().filter(|row| row.id == GEMINI_API).collect();
        assert_eq!(rows.len(), 1, "exactly one gemini-api row");
        let row = rows[0];
        assert!(!row.display_name.is_empty());
        assert_eq!(row.transport, Transport::Http);
        assert!((row.runner)(&ModelsConfig::default()).is_none());
        assert_eq!(
            (row.utility_default)(&[]),
            None,
            "no cheap tier is invented for it"
        );

        // Gate: true only with a key stored.
        {
            let _gate = gemini_test_support::gemini_gate(false);
            assert!(!(row.gate)());
            assert!(!enabled_in(TABLE).iter().any(|r| r.id == GEMINI_API));
        }
        {
            let _gate = gemini_test_support::gemini_gate(true);
            assert!((row.gate)());
            assert!(enabled_in(TABLE).iter().any(|r| r.id == GEMINI_API));
        }

        // Off the CLI lists: not a CLI row, so no id lookup launches it and an unknown-looking
        // runner string still collapses to claude.
        assert!(cli_row_in(TABLE, GEMINI_API).is_none());
        assert!(!ids_with_transport_in(TABLE, Transport::Cli).contains(&GEMINI_API));
        assert_eq!(runner_id_in(TABLE, GEMINI_API), CLAUDE);
        let _gate = gemini_test_support::gemini_gate(true);
        assert!(
            !crate::agents::capabilities::translatable_runners(&[])
                .iter()
                .any(|kind| kind.as_str() == GEMINI_API)
        );
        assert!(
            !crate::models::route_advice::installed_engines()
                .iter()
                .any(|kind| kind.as_str() == GEMINI_API)
        );

        // Seat-kind based lists include it.
        assert!(
            crate::config::ENGINE_SEAT_KINDS
                .iter()
                .any(|(id, _)| *id == GEMINI_API)
        );
    }

    #[test]
    fn derived_lists_follow_the_table() {
        // Seat kinds: `ENGINE_SEAT_KINDS` is the derived list and the only consumer.
        assert_eq!(
            crate::config::ENGINE_SEAT_KINDS.to_vec(),
            vec![
                ("claude", SeatKind::Cloud),
                ("codex", SeatKind::Cloud),
                ("local", SeatKind::Local),
                ("agy", SeatKind::Cloud),
                ("gemini-api", SeatKind::Cloud),
            ]
        );

        // CLI runners (what `translatable_runners` iterates) are the CLI-transport rows.
        assert_eq!(
            ids_with_transport_in(TABLE, Transport::Cli),
            ["claude", "codex", "agy"]
        );

        // Installed engines: only rows whose probe says so, in table order.
        let installed = installed_in(TABLE);
        assert!(
            installed
                .iter()
                .all(|id| ["claude", "codex", "local", "agy", "gemini-api"].contains(id))
        );

        // Chat engines a conversation can name.
        assert_eq!(
            chat_ids_in(TABLE),
            ["claude", "codex", "local", "agy", "gemini-api"]
        );
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
            find_in(rows, "extra").map(|row| row.seat_kind),
            Some(SeatKind::Cloud)
        );
        assert!(installed_in(rows).contains(&"extra"));
        assert!(chat_ids_in(rows).contains(&"extra"));
        // HTTP transport: it is not a CLI runner, and no runner string resolves to it.
        assert!(!ids_with_transport_in(rows, Transport::Cli).contains(&"extra"));
        assert!(ids_with_transport_in(rows, Transport::Http).contains(&"extra"));
        assert_eq!(runner_id_in(rows, "extra"), "claude");
        // The production table is untouched by the test row.
        assert_eq!(TABLE.len(), 5);
    }

    #[test]
    fn gate_false_row_is_absent_from_every_derived_list() {
        let mut rows: Vec<EngineDescription> = TABLE.to_vec();
        rows.push(EngineDescription {
            id: "gated",
            display_name: "Gated",
            seat_kind: SeatKind::Cloud,
            transport: Transport::Cli,
            installed: always_installed,
            gate: gate_shut,
            utility_default: no_utility_default,
            runner: no_runner,
        });
        // The lookups still take the table (E3); the gate decides which rows reach them.
        let enabled: Vec<EngineDescription> = enabled_in(&rows).into_iter().cloned().collect();
        let enabled = enabled.as_slice();
        assert!(find_in(enabled, "claude").is_some());
        assert!(find_in(enabled, "gated").is_none());
        assert!(!installed_in(enabled).contains(&"gated"));
        assert!(!chat_ids_in(enabled).contains(&"gated"));
        assert!(!ids_with_transport_in(enabled, Transport::Cli).contains(&"gated"));
        assert_eq!(runner_id_in(enabled, "gated"), "claude");
        // Without the gate the same row would have shown up everywhere.
        assert!(installed_in(&rows).contains(&"gated"));

        // The production table with agy's setting off (the default) loses agy and nothing else.
        let ids: Vec<&str> = enabled_table().iter().map(|row| row.id).collect();
        assert_eq!(ids, ["claude", "codex", "local"]);
    }

    #[test]
    fn runner_id_in_keeps_agy_when_asked_even_if_disabled() {
        // The single exception to "unknown collapses to claude": a stored `agy` is named, so the
        // caller can refuse it, even while the gate is shut and agy is absent from the lists.
        assert!(!agy::enabled());
        assert_eq!(runner_id_in(TABLE, "agy"), "agy");
        assert_eq!(runner_id_in(TABLE, "gemini"), "claude");
        assert_eq!(runner_id_in(TABLE, "local"), "claude");
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
            gate: probe_true,
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
            ["claude", "codex", "agy", "extra-cli"]
        );
        // The production table does not know the row: it collapses to claude.
        assert_eq!(runner_id_in(TABLE, "extra-cli"), "claude");
    }

    /// E25: the override is thread-local, so a gate held here is invisible to any other thread.
    #[test]
    fn the_gemini_gate_is_invisible_to_other_threads() {
        let _gate = gemini_test_support::gemini_gate(true);
        assert!(gemini_key_stored(), "the holding thread sees the override");
        let seen_elsewhere = std::thread::spawn(gemini_key_stored)
            .join()
            .expect("the probe thread ran");
        assert!(!seen_elsewhere, "another thread must not see this test's gate");
    }

    /// E25: a blank, missing or unreadable key is not a key.
    #[test]
    fn a_blank_missing_or_unreadable_key_is_not_present() {
        let ok = |value: Option<&str>| -> Result<Option<String>, String> {
            Ok(value.map(str::to_string))
        };
        assert!(gemini_key_present(&ok(Some("k"))));
        assert!(!gemini_key_present(&ok(Some("  "))));
        assert!(!gemini_key_present(&ok(None)));
        assert!(!gemini_key_present(&Err("store unreadable".to_string())));
    }
}

/// Fixtures for the tests that need the Gemini API key "stored" or "absent". The override is
/// thread-local: it replaces the published fact on the calling thread only until the guard drops,
/// so tests running in parallel never see each other's gate and need no lock.
#[cfg(any(test, feature = "testkit"))]
pub mod gemini_test_support {
    use std::cell::Cell;

    thread_local! {
        pub(super) static OVERRIDE: Cell<Option<bool>> = const { Cell::new(None) };
    }

    /// Holds the override on this thread; dropping it clears it.
    pub struct GeminiGate {
        // The override lives in this thread's storage, so the guard must drop on this thread.
        _not_send: std::marker::PhantomData<*const ()>,
    }

    impl Drop for GeminiGate {
        fn drop(&mut self) {
            OVERRIDE.with(|cell| cell.set(None));
        }
    }

    /// `true` makes a key read as stored, `false` as absent, until the guard drops.
    pub fn gemini_gate(stored: bool) -> GeminiGate {
        OVERRIDE.with(|cell| cell.set(Some(stored)));
        GeminiGate {
            _not_send: std::marker::PhantomData,
        }
    }
}

/// Shared fixtures for the tests that need agy's opt-in on or off, or a stand-in `agy` binary.
///
/// RED (plan Node 2, leaf D). The gate is turned on by publishing the real `AgyDoc` through the same
/// settings snapshot the document is read from; there is no test-only registration path (E3).
/// Lock order, for every test that uses both: the stand-in binary first (it takes
/// `worktree::test_env_lock`), then the gate (it takes the settings serial lock).
#[cfg(any(test, feature = "testkit"))]
pub mod agy_test_support {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::agy_setting::AgyDoc;
    use crate::execution::runner::{CommandRunner, Permission, RunOutcome, RunRequest, ToolPolicy};
    use crate::settings::testing::{self as settings_testing, Installed};
    use crate::settings::{Scope, Settings, Snapshot};

    /// Publishes a machine-scope `AgyDoc` and keeps it installed until the guard drops. `true` writes
    /// the document the endpoint would store after an acknowledged enable; `false` is a stored `off`.
    pub fn agy_gate(enabled: bool) -> Installed {
        let document = AgyDoc {
            enabled,
            acknowledged_warning_version: enabled.then_some(super::agy::WARNING_VERSION),
        };
        let snapshot = Snapshot::default().with_document(&Scope::Machine, document);
        settings_testing::install(Settings::from_snapshot(snapshot))
    }

    /// A stand-in `agy`, found through `NUCLEOS_AGY_BIN`: records its argv to `args.txt`, prints the
    /// one-shot success JSON for `reply`. The real `agy` is never called.
    pub struct FakeAgyBin {
        dir: tempfile::TempDir,
        previous: Option<OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl FakeAgyBin {
        pub fn success(reply: &str) -> Self {
            let lock = crate::execution::worktree::test_env_lock();
            let dir = tempfile::tempdir().unwrap();
            let body = serde_json::json!({
                "conversation_id": "conv-fake",
                "status": "SUCCESS",
                "response": reply,
                "duration_seconds": 0.5,
                "num_turns": 1,
                "usage": {
                    "input_tokens": 5,
                    "output_tokens": 7,
                    "thinking_tokens": 0,
                    "cache_read_tokens": 0,
                    "total_tokens": 12
                }
            })
            .to_string();
            std::fs::write(dir.path().join("out.json"), body).unwrap();
            let program = write_script(dir.path());
            let previous = std::env::var_os("NUCLEOS_AGY_BIN");
            unsafe { std::env::set_var("NUCLEOS_AGY_BIN", &program) };
            Self {
                dir,
                previous,
                _lock: lock,
            }
        }

        /// Whether the binary was launched at all (it writes `args.txt` first thing).
        pub fn was_launched(&self) -> bool {
            self.dir.path().join("args.txt").exists()
        }

        /// The argv the stand-in recorded, whitespace-normalised to single spaces. The Windows stand-in
        /// writes one line and the Unix one writes one argument per line; this reads both the same way.
        pub fn args(&self) -> String {
            std::fs::read_to_string(self.dir.path().join("args.txt"))
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        }
    }

    impl Drop for FakeAgyBin {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var("NUCLEOS_AGY_BIN", value),
                    None => std::env::remove_var("NUCLEOS_AGY_BIN"),
                }
            }
        }
    }

    #[cfg(windows)]
    fn write_script(dir: &Path) -> PathBuf {
        let path = dir.join("agy.cmd");
        let script =
            "@echo off\r\necho %*> \"%~dp0args.txt\"\r\ntype \"%~dp0out.json\"\r\nexit /b 0\r\n";
        std::fs::write(&path, script).unwrap();
        path
    }

    #[cfg(unix)]
    fn write_script(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("agy.sh");
        let script = "#!/bin/sh\nd=$(dirname \"$0\")\nprintf '%s\\n' \"$@\" > \"$d/args.txt\"\ncat \"$d/out.json\"\nexit 0\n";
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A request with every control at its neutral value, as a chat turn would send it.
    pub fn chat_request(prompt: &str) -> RunRequest {
        RunRequest {
            prompt: prompt.to_string(),
            env: Vec::new(),
            cwd: None,
            permission: Permission::Default,
            resume_session_id: None,
            mcp_config: None,
            mcp_job: None,
            mcp_team_run: None,
            tool_policy: ToolPolicy::McpOnly,
            progress_timeout: None,
            max_turns: None,
            session_id: None,
            fork_session: false,
            include_partial_messages: false,
            images: Vec::new(),
            steerable: false,
            classifier_governs_tools: false,
            mcp_servers: Vec::new(),
            model: None,
            effort: None,
            fallback_model: Vec::new(),
            add_dirs: Vec::new(),
            max_budget_usd: None,
            agents: Vec::new(),
            append_system_prompt: None,
            denied_tools: Vec::new(),
            session_name: None,
            context_window: None,
            messages: None,
            request_tx: None,
            allowed_mcp_tools: None,
            background_tasks: false,
        }
    }

    /// One run of `prompt` as a neutral chat request; see [`run_request`].
    pub async fn run_once(runner: &dyn CommandRunner, prompt: &str) -> std::io::Result<RunOutcome> {
        run_request(runner, chat_request(prompt)).await
    }

    /// One run of `request` through `runner`, with a minute's ceiling so a wedged stand-in fails the test.
    pub async fn run_request(
        runner: &dyn CommandRunner,
        request: RunRequest,
    ) -> std::io::Result<RunOutcome> {
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        tokio::time::timeout(
            std::time::Duration::from_secs(60),
            runner.run_prompt(request, session_tx, transcript),
        )
        .await
        .expect("the run finished within a minute")
    }
}

/// `runner_for_primary` and `RefusingRunner` (plan Node 2, test 32). They live here, not in
/// `main.rs`, so they are unit-testable in the lib: the plan's `--bin nucleos-core` filter does not
/// apply; the command is `cargo test -p nucleos-core --lib engines::primary_tests`.
#[cfg(test)]
mod primary_tests {
    use super::agy_test_support::{FakeAgyBin, agy_gate, run_once};
    use super::*;
    use crate::execution::runner::extract_reply;

    #[tokio::test]
    async fn primary_runner_agy_gate_on_builds_agy_runner() {
        let fake = FakeAgyBin::success("answered by the fake agy");
        let _gate = agy_gate(true);

        let runner = runner_for_primary(Some("agy"), &ModelsConfig::default());

        // Not Claude and not Codex: neither of their markers is set.
        assert!(!runner.honours_max_budget());
        assert!(runner.codex_pins().is_none());
        let outcome = run_once(runner.as_ref(), "hello")
            .await
            .expect("the agy runner spawned the stand-in");
        assert_eq!(outcome.exit_code, 0, "{}", outcome.stderr);
        assert_eq!(
            extract_reply(&outcome.stdout).as_deref(),
            Some("answered by the fake agy")
        );
        assert!(fake.was_launched());
    }

    #[tokio::test]
    async fn primary_runner_agy_gate_off_builds_refusing_runner_naming_agy() {
        // The binary is present and would work: the setting alone decides.
        let fake = FakeAgyBin::success("must never be asked");
        let _gate = agy_gate(false);

        let runner = runner_for_primary(Some("agy"), &ModelsConfig::default());

        // Never Claude: the Claude CLI's marker is absent.
        assert!(!runner.honours_max_budget());
        let error = run_once(runner.as_ref(), "hello")
            .await
            .expect_err("a refusing runner fails every run");
        assert!(
            error
                .to_string()
                .contains("primary_runner is agy, but the agy engine is disabled in settings"),
            "{error}"
        );
        assert!(!fake.was_launched(), "the refusal never reaches the binary");
    }

    /// E16 F3: the helper agents, teams and council share to refuse unpinned work under an agy primary.
    #[test]
    fn refuse_agy_primary_names_agy_only_when_the_primary_is_agy() {
        let with = |name: Option<&str>| ModelsConfig {
            primary_runner: name.map(str::to_string),
            ..ModelsConfig::default()
        };
        assert_eq!(refuse_agy_primary(&with(Some("agy"))), Err(AGY_REFUSAL));
        assert_eq!(refuse_agy_primary(&with(None)), Ok(()));
        assert_eq!(refuse_agy_primary(&with(Some("codex"))), Ok(()));
        assert_eq!(refuse_agy_primary(&with(Some("claude"))), Ok(()));
    }

    #[test]
    fn primary_runner_unknown_still_warns_and_uses_claude() {
        let _gate = agy_gate(false);
        let models = ModelsConfig::default();

        // An unknown name keeps today's warn + Claude; so do an absent name and `claude` itself.
        for name in [Some("gemini"), Some("claude"), None] {
            let runner = runner_for_primary(name, &models);
            assert!(runner.honours_max_budget(), "{name:?} is the Claude CLI");
            assert!(runner.codex_pins().is_none(), "{name:?}");
        }
        // Codex is still Codex.
        let codex = runner_for_primary(Some("codex"), &models);
        assert!(codex.codex_pins().is_some());
        assert!(!codex.honours_max_budget());
    }
}
