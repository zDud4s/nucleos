//! The Claude execution adapter: its capabilities (spec `2026-11-18-motor-por-contrato-design.md`
//! §6.2) and how a hook command reaches the CLI.

use crate::engines::adapters::{
    EngineCapabilities, ExecutionAdapter, HookWiring, Support, WireRefusal,
};
use crate::execution::runner::Permission;

pub struct ClaudeEngine;

const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

const CAPABILITIES: EngineCapabilities = EngineCapabilities {
    pretool_hook: Support::Yes,
    posttool_hook: Support::Yes,
    permission_modes: &[
        Permission::Default,
        Permission::AcceptEdits,
        Permission::Plan,
        Permission::Bypass,
    ],
    strict_mcp: true,
    tool_policy_none: true,
    denied_tools: true,
    fs_write_without_shell: true,
    budget_cap: true,
    system_prompt: true,
    subagents: true,
    resume: true,
    fork: true,
    steer: true,
    fallback_model: true,
    per_stage_model: true,
    efforts: EFFORTS,
    session_name: true,
    schema_diet: true,
    context_fill: true,
    init_tools: true,
};

/// A settings-file hook entry running `command` on every tool.
fn entry(command: &str) -> HookWiring {
    HookWiring::SettingsEntry(serde_json::json!({
        "matcher": "*",
        "hooks": [{ "type": "command", "command": command }]
    }))
}

impl ExecutionAdapter for ClaudeEngine {
    fn capabilities(&self) -> EngineCapabilities {
        CAPABILITIES
    }

    fn wire_pretool(&self, command: &str) -> Result<HookWiring, WireRefusal> {
        Ok(entry(command))
    }

    fn wire_posttool(&self, command: &str) -> Result<HookWiring, WireRefusal> {
        Ok(entry(command))
    }

    /// Named `.claude` because it is the unit's `CLAUDE_CONFIG_DIR`.
    fn unit_config_dir_name(&self) -> &'static str {
        ".claude"
    }
}
