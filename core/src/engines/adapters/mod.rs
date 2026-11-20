//! Engine-specific code, one directory per engine, and the open registry that lists them (spec
//! `2026-11-18-motor-por-contrato-design.md` §6.5): one entry per adapter, so adding a CLI is an
//! adapter plus one registration. Callers ask a capability, not a name.

pub mod capabilities;
pub mod claude;
pub mod codex;

use crate::accounts::provider::ProviderAdapter;
use crate::engines::{EngineDescription, EngineId, TABLE};

pub use capabilities::{EngineCapabilities, HookWiring, Support, WireRefusal};

/// What differs per engine in how a unit is run: capabilities, hook wiring, where its config lives.
pub trait ExecutionAdapter: Sync {
    fn capabilities(&self) -> EngineCapabilities;
    /// How the hook command that classifies a tool call BEFORE it runs reaches this CLI.
    fn wire_pretool(&self, command: &str) -> Result<HookWiring, WireRefusal>;
    /// The same for the hook that sees the outcome of a tool call.
    fn wire_posttool(&self, command: &str) -> Result<HookWiring, WireRefusal>;
    /// The name of the directory, under a unit's config root, that holds this CLI's config.
    fn unit_config_dir_name(&self) -> &'static str;
}

/// One registered engine.
pub struct RegistryEntry {
    pub id: EngineId,
    /// The row of `engines::TABLE` that describes the engine.
    pub descriptor: fn() -> Option<&'static EngineDescription>,
    pub execution: &'static dyn ExecutionAdapter,
    pub accounts: &'static dyn ProviderAdapter,
    /// The `AgentSource::name` of the engine's Fleet source (the sources compile in the app crate).
    pub fleet_source: Option<&'static str>,
}

fn claude_row() -> Option<&'static EngineDescription> {
    crate::engines::find_in(TABLE, crate::engines::CLAUDE)
}

fn codex_row() -> Option<&'static EngineDescription> {
    crate::engines::find_in(TABLE, crate::engines::CODEX)
}

static CLAUDE_EXECUTION: claude::engine::ClaudeEngine = claude::engine::ClaudeEngine;
static CLAUDE_ACCOUNTS: claude::account::ClaudeAdapter = claude::account::ClaudeAdapter;
static CODEX_EXECUTION: codex::engine::CodexEngine = codex::engine::CodexEngine;
static CODEX_ACCOUNTS: codex::account::CodexAdapter = codex::account::CodexAdapter;

/// The registry, in the order every derived list shows. Register a new CLI here.
pub static REGISTRY: &[RegistryEntry] = &[
    RegistryEntry {
        id: EngineId::CLAUDE,
        descriptor: claude_row,
        execution: &CLAUDE_EXECUTION,
        accounts: &CLAUDE_ACCOUNTS,
        fleet_source: Some("claude_code"),
    },
    RegistryEntry {
        id: EngineId::CODEX,
        descriptor: codex_row,
        execution: &CODEX_EXECUTION,
        accounts: &CODEX_ACCOUNTS,
        fleet_source: Some("codex_cli"),
    },
];

/// The entry of the engine with this id; `None` for engines outside the registry (local, agy,
/// gemini-api).
pub fn entry_of(engine: &str) -> Option<&'static RegistryEntry> {
    REGISTRY.iter().find(|entry| entry.id.as_str() == engine)
}

/// The capabilities the engine's adapter declares; `None` for an unregistered engine.
pub fn capabilities_of(engine: &str) -> Option<EngineCapabilities> {
    entry_of(engine).map(|entry| entry.execution.capabilities())
}

/// The execution adapter of the engine.
pub fn execution_of(engine: &str) -> Option<&'static dyn ExecutionAdapter> {
    entry_of(engine).map(|entry| entry.execution)
}

/// The ids of every registered engine, in registry order.
pub fn registered_ids() -> Vec<&'static str> {
    REGISTRY.iter().map(|entry| entry.id.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::runner::Permission;

    #[test]
    fn the_registry_holds_claude_then_codex_each_with_its_descriptor() {
        assert_eq!(registered_ids(), vec!["claude", "codex"]);
        for entry in REGISTRY {
            let row = (entry.descriptor)().expect("every entry names a TABLE row");
            assert_eq!(row.id, entry.id.as_str());
            assert_eq!(entry.accounts.engine(), entry.id.as_str());
        }
        assert!(entry_of("local").is_none());
        assert!(capabilities_of("agy").is_none());
    }

    #[test]
    fn fleet_sources_are_named_per_entry() {
        assert_eq!(
            entry_of("claude").unwrap().fleet_source,
            Some("claude_code")
        );
        assert_eq!(entry_of("codex").unwrap().fleet_source, Some("codex_cli"));
    }

    #[test]
    fn claude_declares_every_control_the_cli_has() {
        let caps = capabilities_of("claude").unwrap();
        assert_eq!(caps.pretool_hook, Support::Yes);
        assert_eq!(caps.posttool_hook, Support::Yes);
        assert_eq!(caps.permission_modes.len(), 4);
        assert!(caps.permission_modes.contains(&Permission::Plan));
        for (name, on) in [
            ("strict_mcp", caps.strict_mcp),
            ("tool_policy_none", caps.tool_policy_none),
            ("denied_tools", caps.denied_tools),
            ("fs_write_without_shell", caps.fs_write_without_shell),
            ("budget_cap", caps.budget_cap),
            ("system_prompt", caps.system_prompt),
            ("subagents", caps.subagents),
            ("resume", caps.resume),
            ("fork", caps.fork),
            ("steer", caps.steer),
            ("fallback_model", caps.fallback_model),
            ("per_stage_model", caps.per_stage_model),
            ("session_name", caps.session_name),
            ("schema_diet", caps.schema_diet),
            ("context_fill", caps.context_fill),
            ("init_tools", caps.init_tools),
        ] {
            assert!(on, "claude should declare {name}");
        }
        assert_eq!(caps.efforts, &["low", "medium", "high", "xhigh", "max"]);
    }

    #[test]
    fn codex_declares_only_what_it_has() {
        let caps = capabilities_of("codex").unwrap();
        assert_eq!(caps.pretool_hook.min_version(), Some("0.166.0"));
        assert!(caps.pretool_hook.is_native());
        assert_eq!(caps.posttool_hook, Support::No);
        assert_eq!(caps.permission_modes, &[Permission::Default]);
        for (name, on) in [
            ("strict_mcp", caps.strict_mcp),
            ("tool_policy_none", caps.tool_policy_none),
            ("denied_tools", caps.denied_tools),
            ("fs_write_without_shell", caps.fs_write_without_shell),
            ("budget_cap", caps.budget_cap),
            ("system_prompt", caps.system_prompt),
            ("subagents", caps.subagents),
            ("fork", caps.fork),
            ("steer", caps.steer),
            ("fallback_model", caps.fallback_model),
            ("per_stage_model", caps.per_stage_model),
            ("session_name", caps.session_name),
            ("schema_diet", caps.schema_diet),
            ("init_tools", caps.init_tools),
        ] {
            assert!(!on, "codex should not declare {name}");
        }
        assert!(caps.resume && caps.context_fill);
        assert!(caps.efforts.contains(&"xhigh"));
    }

    #[test]
    fn claude_wires_both_hooks_as_settings_entries() {
        let adapter = execution_of("claude").unwrap();
        for wiring in [
            adapter.wire_pretool("python hook.py").unwrap(),
            adapter.wire_posttool("python hook.py").unwrap(),
        ] {
            let HookWiring::SettingsEntry(entry) = wiring else {
                panic!("claude wires through the settings file");
            };
            assert_eq!(entry["hooks"][0]["command"], "python hook.py");
        }
        assert_eq!(adapter.unit_config_dir_name(), ".claude");
    }

    #[test]
    fn codex_refuses_to_wire_and_says_why() {
        let adapter = execution_of("codex").unwrap();
        let pre = adapter.wire_pretool("x").unwrap_err();
        assert!(pre.to_string().contains("0.166.0"), "{pre}");
        let post = adapter.wire_posttool("x").unwrap_err();
        assert!(post.to_string().contains("posttool_hook"), "{post}");
        assert_ne!(
            adapter.unit_config_dir_name(),
            execution_of("claude").unwrap().unit_config_dir_name()
        );
    }
}
