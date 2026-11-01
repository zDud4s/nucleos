//! What each engine can do for a conversation (spec section 6), in one table.
//!
//! `agents::capabilities::Engine` has no OpenRouter and belongs to launching agents, so the slash
//! layer keeps its own `ChatEngine`. A built-in that edits a field some engine ignores or refuses
//! asks `require` first, so the rule lives here and nowhere else.

use super::SlashError;
use crate::chats::Brain;
use crate::config::ModelsConfig;

/// The engine that answers a conversation.
///
/// A table-backed id: the named consts are the rows the production code names (plus OpenRouter,
/// which is a hosted brain and not a table row); any other row is a `ChatEngine` of its own, never
/// a stand-in for Claude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatEngine(crate::engines::EngineId);

#[allow(non_upper_case_globals)]
impl ChatEngine {
    pub const Claude: ChatEngine = ChatEngine(crate::engines::EngineId::CLAUDE);
    pub const Codex: ChatEngine = ChatEngine(crate::engines::EngineId::CODEX);
    pub const Local: ChatEngine = ChatEngine(crate::engines::EngineId::LOCAL);
    pub const Agy: ChatEngine = ChatEngine(crate::engines::EngineId::AGY);
    /// The Google Gemini API: a hosted chat whose pinned model carries the `gemini-api/` prefix.
    pub const GeminiApi: ChatEngine = ChatEngine(crate::engines::EngineId::GEMINI_API);
    /// Not an engine row: OpenRouter is a hosted brain, not a launchable engine.
    pub const OpenRouter: ChatEngine = ChatEngine(crate::engines::EngineId::OPENROUTER);

    /// The name the refusal text and the cards use.
    pub fn as_str(self) -> &'static str {
        self.0.as_str()
    }

    /// The chat engine of a CLI runner string; unknown strings collapse to claude.
    fn of_cli(cli: &str) -> Self {
        Self::of_cli_in(crate::engines::TABLE, cli)
    }

    /// `of_cli` over an explicit table: a CLI row of it maps to itself, anything else to claude.
    pub fn of_cli_in(table: &[crate::engines::EngineDescription], cli: &str) -> Self {
        match crate::engines::cli_row_in(table, cli) {
            Some(row) => Self(crate::engines::EngineId::of_row(row)),
            None => Self::Claude,
        }
    }

    /// The chat engine a stored engine id names, among the rows that are switched on. An id the
    /// table does not list (agy with its setting off included) is `None`: the caller refuses it,
    /// it never becomes Claude.
    pub fn from_stored(id: &str) -> Option<Self> {
        Self::from_stored_in(crate::engines::TABLE, id)
    }

    /// `from_stored` over an explicit table.
    pub fn from_stored_in(table: &[crate::engines::EngineDescription], id: &str) -> Option<Self> {
        let enabled: Vec<crate::engines::EngineDescription> = crate::engines::enabled_in(table)
            .into_iter()
            .cloned()
            .collect();
        let id = crate::engines::chat_ids_in(&enabled)
            .into_iter()
            .find(|row| *row == id)?;
        let row = enabled.iter().find(|row| row.id == id)?;
        Some(Self(crate::engines::EngineId::of_row(row)))
    }
}

/// A thing a conversation can be configured to do, one row of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Effort,
    ExtraDirs,
    DeniedTools,
    /// The per-turn spending ceiling.
    TurnBudget,
    Agents,
    /// The MCP servers of the registry.
    RegistryMcp,
}

/// How an engine treats a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// It works.
    Yes,
    /// The value is stored and the engine ignores it: a warning, not a refusal.
    Warn,
    /// The engine cannot do it, or refuses the run: 422.
    No,
}

/// The engine a chat runs on: its brain and, for the cloud, the CLI that answers its pinned model.
pub fn engine_of(config: &ModelsConfig, brain: Brain, model: Option<&str>) -> ChatEngine {
    match brain {
        Brain::Local => ChatEngine::Local,
        // The provider of a hosted chat is read from the id prefix, as `assistants.rs` does.
        Brain::OpenRouter => {
            if model.is_some_and(|id| id.starts_with("gemini-api/")) {
                ChatEngine::GeminiApi
            } else {
                ChatEngine::OpenRouter
            }
        }
        // A refusal means the chat is bound to agy while agy is off.
        Brain::Cloud => crate::assistant::answering_cli(config, model)
            .map_or(ChatEngine::Agy, ChatEngine::of_cli),
    }
}

/// The table of spec section 6.
pub fn support(engine: ChatEngine, capability: Capability) -> Support {
    use Capability::*;
    use Support::{No, Warn, Yes};
    match (capability, engine) {
        (Effort | ExtraDirs | RegistryMcp, ChatEngine::Claude | ChatEngine::Codex) => Yes,
        (DeniedTools | Agents, ChatEngine::Claude) => Yes,
        (TurnBudget, ChatEngine::Claude) => Yes,
        (TurnBudget, ChatEngine::Local | ChatEngine::OpenRouter) => Warn,
        // Agy takes `--effort` and `--add-dir` and refuses every other control.
        (Effort | ExtraDirs, ChatEngine::Agy) => Yes,
        (DeniedTools | TurnBudget | Agents | RegistryMcp, ChatEngine::Agy) => No,
        // The Gemini API route ignores a per-turn budget (a warning, like OpenRouter) and refuses
        // every other control; the `_` arm answers `No` for them.
        (TurnBudget, ChatEngine::GeminiApi) => Warn,
        _ => No,
    }
}

/// `Ok` unless the engine cannot do `what` (a phrase such as "set an effort"), then `Unsupported`.
/// A `Warn` is not a refusal: the caller adds the warning to its card.
pub fn require(engine: ChatEngine, capability: Capability, what: &str) -> Result<(), SlashError> {
    match support(engine, capability) {
        Support::No => Err(SlashError::Unsupported {
            engine,
            what: what.to_string(),
        }),
        Support::Yes | Support::Warn => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::Capability::*;
    use super::Support::{No, Warn, Yes};
    use super::*;
    use crate::config::AssistantChoice;

    #[test]
    fn slash_b_capabilities_table_matches_spec() {
        // Columns: claude, codex, local, openrouter, agy. Spec section 6; agy has `--effort` and
        // `--add-dir` and refuses every other control by name.
        let table = [
            (Effort, [Yes, Yes, No, No, Yes]),
            (ExtraDirs, [Yes, Yes, No, No, Yes]),
            (DeniedTools, [Yes, No, No, No, No]),
            (TurnBudget, [Yes, No, Warn, Warn, No]),
            (Agents, [Yes, No, No, No, No]),
            (RegistryMcp, [Yes, Yes, No, No, No]),
        ];
        let engines = [
            ChatEngine::Claude,
            ChatEngine::Codex,
            ChatEngine::Local,
            ChatEngine::OpenRouter,
            ChatEngine::Agy,
        ];
        for (capability, expected) in table {
            for (engine, want) in engines.into_iter().zip(expected) {
                assert_eq!(
                    support(engine, capability),
                    want,
                    "{capability:?} on {engine:?}"
                );
            }
        }
    }

    #[test]
    fn slash_b_capabilities_require_refuses_only_where_the_table_says_no() {
        assert_eq!(require(ChatEngine::Claude, Effort, "set an effort"), Ok(()));
        // A warning is not a refusal.
        assert_eq!(
            require(ChatEngine::Local, TurnBudget, "set a budget"),
            Ok(())
        );
        assert_eq!(
            require(ChatEngine::Local, Effort, "set an effort"),
            Err(SlashError::Unsupported {
                engine: ChatEngine::Local,
                what: "set an effort".to_string(),
            })
        );
        assert!(require(ChatEngine::Codex, TurnBudget, "set a budget").is_err());
        assert!(require(ChatEngine::Codex, DeniedTools, "deny a tool").is_err());
    }

    #[test]
    fn chat_engine_of_cli_maps_agy_not_claude() {
        assert_eq!(ChatEngine::of_cli("agy"), ChatEngine::Agy);
        assert_eq!(ChatEngine::of_cli("codex"), ChatEngine::Codex);
        assert_eq!(ChatEngine::of_cli("claude"), ChatEngine::Claude);
        // An unknown string still collapses to claude.
        assert_eq!(ChatEngine::of_cli("gemini"), ChatEngine::Claude);
        assert_eq!(ChatEngine::Agy.as_str(), crate::engines::AGY);
    }

    #[test]
    fn chat_engine_from_stored_uses_chat_ids() {
        let _gate = crate::engines::agy_test_support::agy_gate(false);
        assert_eq!(ChatEngine::from_stored("claude"), Some(ChatEngine::Claude));
        assert_eq!(ChatEngine::from_stored("codex"), Some(ChatEngine::Codex));
        assert_eq!(ChatEngine::from_stored("local"), Some(ChatEngine::Local));
        assert_eq!(ChatEngine::from_stored("gemini"), None);
        assert_eq!(ChatEngine::from_stored(""), None);
        // agy's setting is off by default, so a stored agy is not an engine a chat can name: the
        // caller refuses it, it does not become Claude.
        assert_eq!(ChatEngine::from_stored("agy"), None);
    }

    fn zed_installed() -> bool {
        true
    }

    fn zed_utility(_known: &[crate::models::model_catalog::Discovered]) -> Option<String> {
        None
    }

    fn zed_runner(
        _models: &ModelsConfig,
    ) -> Option<std::sync::Arc<dyn crate::execution::runner::CommandRunner>> {
        None
    }

    /// AC3: a row only the table argument lists is itself, not Claude. The row is built from the
    /// public fields of `EngineDescription`, so nothing here needs a `cfg(test)` item of the core
    /// crate; the lookups take the table as an argument.
    #[test]
    fn of_cli_in_and_from_stored_in_keep_unknown_row() {
        use crate::engines::{EngineDescription, TABLE, Transport};

        let mut rows: Vec<EngineDescription> = TABLE.to_vec();
        rows.push(EngineDescription {
            id: "zed",
            display_name: "Zed",
            seat_kind: crate::config::SeatKind::Cloud,
            transport: Transport::Cli,
            installed: zed_installed,
            gate: zed_installed,
            utility_default: zed_utility,
            runner: zed_runner,
        });
        let rows = rows.as_slice();

        // A CLI row of the table passed in maps to itself.
        let zed = ChatEngine::of_cli_in(rows, "zed");
        assert_eq!(zed.as_str(), "zed");
        assert_ne!(zed, ChatEngine::Claude);
        assert_ne!(zed, ChatEngine::Agy);
        // Known rows are unchanged, and an unknown string still collapses to claude.
        assert_eq!(ChatEngine::of_cli_in(rows, "codex"), ChatEngine::Codex);
        assert_eq!(ChatEngine::of_cli_in(rows, "gemini"), ChatEngine::Claude);
        // The production table does not list it.
        assert_eq!(ChatEngine::of_cli_in(TABLE, "zed"), ChatEngine::Claude);

        // A stored id the table lists is a chat engine of that text; one it lacks is `None`.
        let stored = ChatEngine::from_stored_in(rows, "zed").expect("zed is listed");
        assert_eq!(stored.as_str(), "zed");
        assert_eq!(stored, zed);
        assert_eq!(
            ChatEngine::from_stored_in(rows, "claude"),
            Some(ChatEngine::Claude)
        );
        assert_eq!(ChatEngine::from_stored_in(rows, "gemini"), None);
        assert_eq!(ChatEngine::from_stored_in(TABLE, "zed"), None);
    }

    /// RED (plan Node 3, test 7; contract E22). The Gemini API route is a chat engine of its own
    /// (`ChatEngine::GeminiApi`, the provider read from the `gemini-api/` id prefix on the hosted
    /// brain) and every control that needs a CLI, agents, teams, council or quota is refused by
    /// name, as `AGY_REFUSAL` does for agy.
    #[test]
    fn gemini_api_refused_by_name_for_agents_teams_council_quota() {
        // The engine exists, answers to the table's id, and is not any of the others.
        assert_eq!(ChatEngine::GeminiApi.as_str(), crate::engines::GEMINI_API);
        assert_eq!(crate::engines::GEMINI_API, "gemini-api");
        for other in [
            ChatEngine::Claude,
            ChatEngine::Codex,
            ChatEngine::Local,
            ChatEngine::OpenRouter,
            ChatEngine::Agy,
        ] {
            assert_ne!(ChatEngine::GeminiApi, other);
        }

        // A hosted chat pinned to `gemini-api/<model>` is the Gemini engine; every other hosted
        // chat (a bare or `openrouter/` pick, or no pin) is still OpenRouter.
        let config = ModelsConfig::default();
        assert_eq!(
            engine_of(
                &config,
                Brain::OpenRouter,
                Some("gemini-api/gemini-2.5-flash")
            ),
            ChatEngine::GeminiApi
        );
        assert_eq!(
            engine_of(&config, Brain::OpenRouter, Some("google/gemini-2.5-flash")),
            ChatEngine::OpenRouter
        );
        assert_eq!(
            engine_of(&config, Brain::OpenRouter, None),
            ChatEngine::OpenRouter
        );

        // The controls: only a per-turn budget is a warning (the hosted route ignores it, like
        // OpenRouter); effort, extra dirs, denied tools, agents and registry MCP are refused.
        for capability in [Effort, ExtraDirs, DeniedTools, Agents, RegistryMcp] {
            assert_eq!(
                support(ChatEngine::GeminiApi, capability),
                No,
                "{capability:?} on gemini-api"
            );
            assert_eq!(
                require(ChatEngine::GeminiApi, capability, "do that"),
                Err(SlashError::Unsupported {
                    engine: ChatEngine::GeminiApi,
                    what: "do that".to_string(),
                })
            );
        }
        assert_eq!(support(ChatEngine::GeminiApi, TurnBudget), Warn);

        // The by-name refusal for the launch sites names the route and every refused surface.
        let refusal = crate::engines::GEMINI_API_REFUSAL;
        for needle in ["gemini-api", "agents", "teams", "council", "quota"] {
            assert!(
                refusal.contains(needle),
                "`{needle}` missing from: {refusal}"
            );
        }
    }

    fn codex_choice() -> AssistantChoice {
        AssistantChoice {
            id: "gpt-test".to_string(),
            label: "GPT test".to_string(),
            brain: "cloud".to_string(),
            efforts: Vec::new(),
            runner: Some("codex".to_string()),
            tools: None,
            installed: None,
        }
    }

    #[test]
    fn slash_b_engine_of_chat_maps_brain_and_cli() {
        let mut config = ModelsConfig::default();
        config.assistant_choices.push(codex_choice());

        // The brain decides everything but the cloud; a model name on a local chat changes nothing.
        assert_eq!(engine_of(&config, Brain::Local, None), ChatEngine::Local);
        assert_eq!(
            engine_of(&config, Brain::Local, Some("gpt-test")),
            ChatEngine::Local
        );
        assert_eq!(
            engine_of(&config, Brain::OpenRouter, Some("some/model")),
            ChatEngine::OpenRouter
        );
        // The cloud follows the CLI that answers the pinned model.
        assert_eq!(
            engine_of(&config, Brain::Cloud, Some("gpt-test")),
            ChatEngine::Codex
        );
        assert_eq!(
            engine_of(&config, Brain::Cloud, Some("sonnet")),
            ChatEngine::Claude
        );
        assert_eq!(engine_of(&config, Brain::Cloud, None), ChatEngine::Claude);
        assert_eq!(
            engine_of(&config, Brain::Cloud, Some("no-such-model")),
            ChatEngine::Claude
        );
    }
}
