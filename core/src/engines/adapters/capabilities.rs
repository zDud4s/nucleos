//! What an engine's CLI can do on its own, declared by its adapter (spec
//! `2026-11-18-motor-por-contrato-design.md` §6.2). Plain data: a caller asks the capability, never
//! the engine's name, and a missing one is refused or replaced by the daemon (§6.3).

use crate::execution::runner::Permission;

/// Whether a capability is there, and from which CLI version when that matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    No,
    Yes,
    /// Present from this CLI version on (the version of the spike or of the matrix that proved it).
    FromVersion(&'static str),
}

impl Support {
    /// Whether the CLI has it at all, whatever the version.
    pub fn is_native(self) -> bool {
        !matches!(self, Support::No)
    }

    /// The first CLI version that has it, when the capability is version-gated.
    pub fn min_version(self) -> Option<&'static str> {
        match self {
            Support::FromVersion(version) => Some(version),
            _ => None,
        }
    }
}

/// The capabilities of one engine. Control capabilities fall to a daemon substitute or a refusal
/// when absent; `schema_diet` is a hint (skipped and recorded); `context_fill` is the minimum
/// contract (an engine without it is not eligible); `init_tools` is data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineCapabilities {
    pub pretool_hook: Support,
    pub posttool_hook: Support,
    /// The permission rungs the CLI honours; the rest are refused or covered by the gate.
    pub permission_modes: &'static [Permission],
    pub strict_mcp: bool,
    /// `ToolPolicy::None` and a list of denied tools enforced by the CLI itself.
    pub tool_policy_none: bool,
    pub denied_tools: bool,
    pub fs_write_without_shell: bool,
    pub budget_cap: bool,
    pub system_prompt: bool,
    pub subagents: bool,
    pub resume: bool,
    pub fork: bool,
    pub steer: bool,
    pub fallback_model: bool,
    pub per_stage_model: bool,
    /// The effort levels the adapter accepts; a level outside the list is refused.
    pub efforts: &'static [&'static str],
    pub session_name: bool,
    /// The leaf's tool-list diet (a hint: where it cannot be applied the run skips it and records).
    pub schema_diet: bool,
    pub context_fill: bool,
    pub init_tools: bool,
}

/// Why an adapter cannot wire a hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireRefusal {
    /// The CLI has no such hook.
    Unsupported { capability: &'static str },
    /// The CLI has it (from `since`, when known) but the adapter does not wire it yet.
    NotWired {
        capability: &'static str,
        since: Option<&'static str>,
    },
}

impl std::fmt::Display for WireRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireRefusal::Unsupported { capability } => {
                write!(formatter, "the engine has no {capability}")
            }
            WireRefusal::NotWired {
                capability,
                since: Some(version),
            } => write!(
                formatter,
                "the {capability} exists from CLI version {version} but is not wired by the daemon yet"
            ),
            WireRefusal::NotWired {
                capability,
                since: None,
            } => write!(formatter, "the {capability} is not wired by the daemon yet"),
        }
    }
}

/// How an adapter delivers a hook command to its CLI.
#[derive(Debug, Clone, PartialEq)]
pub enum HookWiring {
    /// An entry for the unit's settings file, under the hook event it was asked for.
    SettingsEntry(serde_json::Value),
    /// Extra arguments for the CLI's command line.
    CliArgs(Vec<String>),
}
