//! What each engine can do for a conversation (spec section 6), in one table.
//!
//! `agents::capabilities::Engine` has no OpenRouter and belongs to launching agents, so the slash
//! layer keeps its own `ChatEngine`. A built-in that edits a field some engine ignores or refuses
//! asks `require` first, so the rule lives here and nowhere else.

use super::SlashError;
use crate::chats::Brain;
use crate::config::ModelsConfig;

/// The engine that answers a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatEngine {
    Claude,
    Codex,
    Local,
    OpenRouter,
    Agy,
}

impl ChatEngine {
    /// The name the refusal text and the cards use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => crate::engines::CLAUDE,
            Self::Codex => crate::engines::CODEX,
            Self::Local => crate::engines::LOCAL,
            Self::Agy => crate::engines::AGY,
            // Not an engine row: OpenRouter is a hosted brain, not a launchable engine.
            Self::OpenRouter => "openrouter",
        }
    }

    /// The chat engine of a CLI runner string; unknown strings collapse to claude.
    fn of_cli(cli: &str) -> Self {
        match crate::engines::runner_id_in(crate::engines::TABLE, cli) {
            crate::engines::CODEX => Self::Codex,
            crate::engines::AGY => Self::Agy,
            _ => Self::Claude,
        }
    }

    /// The chat engine a stored engine id names, among the rows that are switched on. An id the
    /// table does not list (agy with its setting off included) is `None`: the caller refuses it,
    /// it never becomes Claude.
    pub fn from_stored(id: &str) -> Option<Self> {
        let enabled: Vec<crate::engines::EngineDescription> = crate::engines::enabled_table()
            .into_iter()
            .cloned()
            .collect();
        let id = crate::engines::chat_ids_in(&enabled)
            .into_iter()
            .find(|row| *row == id)?;
        match id {
            crate::engines::CLAUDE => Some(Self::Claude),
            crate::engines::CODEX => Some(Self::Codex),
            crate::engines::LOCAL => Some(Self::Local),
            crate::engines::AGY => Some(Self::Agy),
            _ => None,
        }
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
        Brain::OpenRouter => ChatEngine::OpenRouter,
        Brain::Cloud => ChatEngine::of_cli(crate::assistant::answering_cli(config, model)),
    }
}

/// The table of spec section 6.
pub fn support(engine: ChatEngine, capability: Capability) -> Support {
    use Capability::*;
    use ChatEngine::*;
    use Support::{No, Warn, Yes};
    match (capability, engine) {
        (Effort | ExtraDirs | RegistryMcp, Claude | Codex) => Yes,
        (DeniedTools | Agents, Claude) => Yes,
        (TurnBudget, Claude) => Yes,
        (TurnBudget, Local | OpenRouter) => Warn,
        // Agy takes `--effort` and `--add-dir` and refuses every other control.
        (Effort | ExtraDirs, Agy) => Yes,
        (DeniedTools | TurnBudget | Agents | RegistryMcp, Agy) => No,
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
        assert_eq!(ChatEngine::from_stored("claude"), Some(ChatEngine::Claude));
        assert_eq!(ChatEngine::from_stored("codex"), Some(ChatEngine::Codex));
        assert_eq!(ChatEngine::from_stored("local"), Some(ChatEngine::Local));
        assert_eq!(ChatEngine::from_stored("gemini"), None);
        assert_eq!(ChatEngine::from_stored(""), None);
        // agy's setting is off by default, so a stored agy is not an engine a chat can name: the
        // caller refuses it, it does not become Claude.
        assert_eq!(ChatEngine::from_stored("agy"), None);
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
