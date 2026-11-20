//! The Codex execution adapter: its capabilities (spec `2026-11-18-motor-por-contrato-design.md`
//! §6.2) and the hooks it cannot be wired with yet.

use crate::engines::adapters::{
    EngineCapabilities, ExecutionAdapter, HookWiring, Support, WireRefusal,
};
use crate::execution::runner::Permission;

pub struct CodexEngine;

/// The codex-cli version the `PreToolUse` hook override was spiked on (2026-11-03 spec §4).
pub const PRETOOL_FROM_VERSION: &str = "0.166.0";

/// The union over the models; each model narrows it in the models file.
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultra"];

const CAPABILITIES: EngineCapabilities = EngineCapabilities {
    pretool_hook: Support::FromVersion(PRETOOL_FROM_VERSION),
    // To be verified (2026-11-03 §6.8): not claimed until it is.
    posttool_hook: Support::No,
    permission_modes: &[Permission::Default],
    strict_mcp: false,
    tool_policy_none: false,
    denied_tools: false,
    fs_write_without_shell: false,
    budget_cap: false,
    system_prompt: false,
    subagents: false,
    resume: true,
    fork: false,
    steer: false,
    fallback_model: false,
    per_stage_model: false,
    efforts: EFFORTS,
    session_name: false,
    // The Codex adapter has no tool-list diet of its own yet: the run skips it and records.
    schema_diet: false,
    context_fill: true,
    init_tools: false,
};

impl ExecutionAdapter for CodexEngine {
    fn capabilities(&self) -> EngineCapabilities {
        CAPABILITIES
    }

    /// The CLI has the hook from `PRETOOL_FROM_VERSION`; the daemon wires none into `codex exec`
    /// today (the runner refuses `classifier_governs_tools`), so this refuses until the gate of the
    /// 2026-11-03 spec lands on this method.
    fn wire_pretool(&self, _command: &str) -> Result<HookWiring, WireRefusal> {
        Err(WireRefusal::NotWired {
            capability: "pretool_hook",
            since: Some(PRETOOL_FROM_VERSION),
        })
    }

    fn wire_posttool(&self, _command: &str) -> Result<HookWiring, WireRefusal> {
        Err(WireRefusal::Unsupported {
            capability: "posttool_hook",
        })
    }

    fn unit_config_dir_name(&self) -> &'static str {
        ".codex"
    }
}
