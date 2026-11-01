//! Capability translation: which concrete tools a launch hands each engine for a capability list.
//!
//! One function, `translate`, turns the closed capability vocabulary (spec section 5) into what a
//! runner needs: the `ToolPolicy` (derived through `policy_str_for`, the single source of the
//! section 4 table), the exact Claude tool names, the Codex sandbox, and the local `ToolBox` set.
//! It feeds `RunShape.tool_policy`; it is not a second eligibility rule.
//!
//! Codex limitation (declared): its wall is coarse. `read-only` blocks writes but not read
//! commands in the shell, and `workspace-write` grants writing and the shell together, so `fs.write`
//! without `shell` does not exist there. For Codex the capabilities are therefore a WRITE CEILING,
//! not an exact list, and `Unsupported` never fires on Codex for `fs.*`, `shell` or `web`.
//!
//! The local engine (reachable by pinning only) supports just `memory.read` and `mcp.nucleos`;
//! `fs.*`, `shell` and `web` are refused in v1 (`web_search`/`web_read` are `ReadsUntrusted` and
//! sit outside the team boxes by decision).

use std::fmt;

use nucleos_base::agent::{Capability, policy_str_for};

use crate::execution::runner::ToolPolicy;
use crate::models::route_advice::RunnerKind;

/// The engine a launch targets: the CLI runners plus the local model. A table-backed id; the named
/// consts are the rows the production code names, and any other CLI row is an `Engine` of its own,
/// never a stand-in for Claude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Engine(crate::engines::EngineId);

#[allow(non_upper_case_globals)]
impl Engine {
    pub const Claude: Engine = Engine(crate::engines::EngineId::from_row(crate::engines::CLAUDE));
    pub const Codex: Engine = Engine(crate::engines::EngineId::from_row(crate::engines::CODEX));
    pub const Local: Engine = Engine(crate::engines::EngineId::from_row(crate::engines::LOCAL));
    pub const Agy: Engine = Engine(crate::engines::EngineId::from_row(crate::engines::AGY));

    pub fn as_str(self) -> &'static str {
        self.0.as_str()
    }

    /// The engine of a CLI row of `table`, or the local model; `None` for any other id.
    pub fn parse_in(table: &[crate::engines::EngineDescription], value: &str) -> Option<Self> {
        if value == crate::engines::LOCAL {
            return Some(Self::Local);
        }
        let row = crate::engines::cli_row_in(table, value)?;
        Some(Self(crate::engines::EngineId::from_row(row.id)))
    }

    /// An engine outside the production table, to exercise the fail-closed arms.
    #[cfg(any(test, feature = "testkit"))]
    pub fn for_test(id: &'static str) -> Self {
        Self(crate::engines::EngineId::for_test(id))
    }
}

impl From<RunnerKind> for Engine {
    fn from(kind: RunnerKind) -> Self {
        Self(kind.0)
    }
}

/// The Codex sandbox a launch pins (`sandbox_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexSandbox {
    ReadOnly,
    WorkspaceWrite,
}

impl CodexSandbox {
    /// The value `sandbox_mode` takes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
        }
    }
}

/// What one engine needs to run under a capability list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineTools {
    /// The policy the runners and `eligible` already use, derived from the capabilities.
    pub tool_policy: ToolPolicy,
    /// Claude: exact built-in tool names granted, plus `mcp__nucleos__*` when `mcp` is set.
    pub claude_allowed_tools: Vec<String>,
    /// Codex: the sandbox to pin; `None` leaves the runner's own resolution (no fs/shell/web asked).
    pub codex_sandbox: Option<CodexSandbox>,
    /// Codex: whether `web_search` stays enabled.
    pub codex_web_search: bool,
    /// Local: the `ToolBox` sets to build (`"recall"` for memory.read, `"nucleos"` for mcp.nucleos).
    pub local_tools: Vec<&'static str>,
    /// Whether the NucleOS MCP server is exposed (memory.read is served through it).
    pub mcp: bool,
}

/// A capability the engine cannot honour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported {
    pub engine: Engine,
    pub capability: Capability,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the {} engine does not support the capability `{}`",
            self.engine.as_str(),
            self.capability.as_str()
        )
    }
}

impl std::error::Error for Unsupported {}

/// The `ToolPolicy` a capability list derives (spec section 4), via `policy_str_for`.
pub fn tool_policy_of(caps: &[Capability]) -> ToolPolicy {
    match policy_str_for(caps) {
        "unrestricted" => ToolPolicy::Unrestricted,
        "mcp_only" => ToolPolicy::McpOnly,
        _ => ToolPolicy::None,
    }
}

/// Translate `caps` for `engine`, or name the first capability it cannot honour.
pub fn translate(engine: Engine, caps: &[Capability]) -> Result<EngineTools, Unsupported> {
    let has = |c: Capability| caps.contains(&c);
    let mcp = has(Capability::MemoryRead) || has(Capability::McpNucleos);
    let mut tools = EngineTools {
        tool_policy: tool_policy_of(caps),
        claude_allowed_tools: Vec::new(),
        codex_sandbox: None,
        codex_web_search: false,
        local_tools: Vec::new(),
        mcp,
    };
    match engine {
        Engine::Claude => {
            let table: [(Capability, &[&str]); 4] = [
                (Capability::FsRead, &["Read", "Glob", "Grep"]),
                (Capability::FsWrite, &["Edit", "Write"]),
                (Capability::Shell, &["Bash"]),
                (Capability::Web, &["WebSearch", "WebFetch"]),
            ];
            for (cap, names) in table {
                if has(cap) {
                    tools
                        .claude_allowed_tools
                        .extend(names.iter().map(|n| n.to_string()));
                }
            }
            if mcp {
                tools
                    .claude_allowed_tools
                    .push("mcp__nucleos__*".to_string());
            }
        }
        Engine::Codex => {
            tools.codex_sandbox = if has(Capability::FsWrite) || has(Capability::Shell) {
                Some(CodexSandbox::WorkspaceWrite)
            } else if has(Capability::FsRead) || has(Capability::Web) {
                Some(CodexSandbox::ReadOnly)
            } else {
                None
            };
            tools.codex_web_search = has(Capability::Web);
        }
        Engine::Local => {
            for &capability in caps {
                match capability {
                    Capability::MemoryRead => tools.local_tools.push("recall"),
                    Capability::McpNucleos => tools.local_tools.push("nucleos"),
                    _ => return Err(Unsupported { engine, capability }),
                }
            }
        }
        // Agents cannot run agy in this delivery: every capability is refused by name.
        Engine::Agy => {
            if let Some(&capability) = caps.first() {
                return Err(Unsupported { engine, capability });
            }
        }
        // An engine with no arm above refuses the first capability, as agy does: it never
        // falls through to Claude's tool set.
        _ => {
            if let Some(&capability) = caps.first() {
                return Err(Unsupported { engine, capability });
            }
        }
    }
    Ok(tools)
}

/// The CLI runners that translate `caps` without error.
pub fn translatable_runners(caps: &[Capability]) -> Vec<RunnerKind> {
    let enabled: Vec<crate::engines::EngineDescription> = crate::engines::enabled_table()
        .into_iter()
        .cloned()
        .collect();
    crate::engines::ids_with_transport_in(&enabled, crate::engines::Transport::Cli)
        .into_iter()
        .filter_map(RunnerKind::parse)
        // An empty capability list translates for agy, but agents never run on it: allow-list the
        // engines that do, so a new engine is excluded until it is wired.
        .filter(|&kind| kind == RunnerKind::Claude || kind == RunnerKind::Codex)
        .filter(|&kind| translate(kind.into(), caps).is_ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use Capability::*;

    fn claude(caps: &[Capability]) -> Vec<String> {
        translate(Engine::Claude, caps)
            .unwrap()
            .claude_allowed_tools
    }

    /// An engine with no arm refuses by name; it never falls through to Claude's tool set.
    #[test]
    fn translate_refuses_unwired_engine() {
        let zed = Engine::for_test("zed");
        let error = translate(zed, &[FsRead]).expect_err("an unwired engine refuses");
        assert_eq!(error.capability, FsRead);
        assert!(error.to_string().contains("zed"), "{error}");
    }

    /// An unwired engine is never a translatable runner, whatever the capability list.
    #[test]
    fn translatable_runners_excludes_unwired() {
        let zed = RunnerKind::for_test("zed");
        for caps in [&[][..], &[FsRead][..], &[Shell, Web][..]] {
            let runners = translatable_runners(caps);
            assert!(!runners.contains(&zed), "{runners:?}");
            assert!(
                runners
                    .iter()
                    .all(|kind| *kind == RunnerKind::Claude || *kind == RunnerKind::Codex),
                "{runners:?}"
            );
        }
    }

    #[test]
    fn claude_table_per_capability() {
        assert_eq!(claude(&[FsRead]), ["Read", "Glob", "Grep"]);
        assert_eq!(claude(&[FsWrite]), ["Edit", "Write"]);
        assert_eq!(claude(&[Shell]), ["Bash"]);
        assert_eq!(claude(&[Web]), ["WebSearch", "WebFetch"]);
        assert_eq!(claude(&[MemoryRead]), ["mcp__nucleos__*"]);
        assert_eq!(claude(&[McpNucleos]), ["mcp__nucleos__*"]);
        assert!(claude(&[]).is_empty());
        assert_eq!(
            claude(&[McpNucleos, MemoryRead, Shell]),
            ["Bash", "mcp__nucleos__*"]
        );
    }

    #[test]
    fn codex_table_per_capability() {
        let t = |caps: &[Capability]| translate(Engine::Codex, caps).unwrap();
        assert_eq!(t(&[FsRead]).codex_sandbox, Some(CodexSandbox::ReadOnly));
        assert_eq!(
            t(&[FsWrite]).codex_sandbox,
            Some(CodexSandbox::WorkspaceWrite)
        );
        assert_eq!(
            t(&[Shell]).codex_sandbox,
            Some(CodexSandbox::WorkspaceWrite)
        );
        assert_eq!(
            t(&[FsRead, FsWrite]).codex_sandbox,
            Some(CodexSandbox::WorkspaceWrite)
        );
        let web = t(&[Web]);
        assert!(web.codex_web_search);
        assert_eq!(web.codex_sandbox, Some(CodexSandbox::ReadOnly));
        assert!(!t(&[FsRead]).codex_web_search);
        assert_eq!(t(&[MemoryRead]).codex_sandbox, None);
        assert!(t(&[MemoryRead]).mcp);
        assert!(t(&[McpNucleos]).mcp);
        assert!(!t(&[]).mcp);
    }

    #[test]
    fn codex_never_refuses_a_capability() {
        for &c in Capability::ALL {
            assert!(translate(Engine::Codex, &[c]).is_ok(), "{c:?}");
        }
    }

    #[test]
    fn local_supports_only_memory_and_mcp() {
        let ok = translate(Engine::Local, &[MemoryRead, McpNucleos]).unwrap();
        assert_eq!(ok.local_tools, ["recall", "nucleos"]);
        assert!(ok.mcp);
        assert!(
            translate(Engine::Local, &[])
                .unwrap()
                .local_tools
                .is_empty()
        );
        for c in [FsRead, FsWrite, Shell, Web] {
            let err = translate(Engine::Local, &[McpNucleos, c]).unwrap_err();
            assert_eq!(
                err,
                Unsupported {
                    engine: Engine::Local,
                    capability: c
                }
            );
        }
    }

    #[test]
    fn tool_policy_follows_the_section_4_table() {
        assert_eq!(tool_policy_of(&[]), ToolPolicy::None);
        assert_eq!(tool_policy_of(&[McpNucleos]), ToolPolicy::McpOnly);
        assert_eq!(
            tool_policy_of(&[MemoryRead, McpNucleos]),
            ToolPolicy::McpOnly
        );
        for c in [FsRead, FsWrite, Shell, Web] {
            assert_eq!(tool_policy_of(&[McpNucleos, c]), ToolPolicy::Unrestricted);
        }
        for engine in [Engine::Claude, Engine::Codex] {
            assert_eq!(
                translate(engine, &[Shell]).unwrap().tool_policy,
                ToolPolicy::Unrestricted
            );
        }
    }

    #[test]
    fn unsupported_message_names_engine_and_capability() {
        let err = Unsupported {
            engine: Engine::Local,
            capability: FsWrite,
        };
        assert_eq!(
            err.to_string(),
            "the local engine does not support the capability `fs.write`"
        );
    }

    #[test]
    fn translatable_runners_lists_the_clis_that_translate() {
        assert_eq!(
            translatable_runners(&[FsRead, Web]),
            vec![RunnerKind::Claude, RunnerKind::Codex]
        );
        assert_eq!(
            translatable_runners(&[]),
            vec![RunnerKind::Claude, RunnerKind::Codex]
        );
    }

    #[test]
    fn engine_from_runner_kind() {
        assert_eq!(Engine::from(RunnerKind::Claude), Engine::Claude);
        assert_eq!(Engine::from(RunnerKind::Codex), Engine::Codex);
        assert_eq!(Engine::from(RunnerKind::Agy), Engine::Agy);
    }

    #[test]
    fn engine_and_runner_kind_strings_agree_with_the_table() {
        assert_eq!(Engine::Agy.as_str(), crate::engines::AGY);
        assert_eq!(RunnerKind::Agy.as_str(), crate::engines::AGY);
        assert_eq!(RunnerKind::parse("agy"), Some(RunnerKind::Agy));
        // Every CLI row of the table round-trips through both vocabularies.
        for id in crate::engines::ids_with_transport_in(
            crate::engines::TABLE,
            crate::engines::Transport::Cli,
        ) {
            let kind = RunnerKind::parse(id).unwrap_or_else(|| panic!("no RunnerKind for `{id}`"));
            assert_eq!(kind.as_str(), id);
            assert_eq!(Engine::from(kind).as_str(), id);
        }
        // The local model is not a CLI runner.
        assert_eq!(RunnerKind::parse("local"), None);
    }

    #[test]
    fn translate_agy_refuses_unsupported_capabilities_by_name() {
        // Agents cannot run agy in this delivery: every capability is refused, by engine and name.
        for &capability in Capability::ALL {
            let err = translate(Engine::Agy, &[capability]).unwrap_err();
            assert_eq!(
                err,
                Unsupported {
                    engine: Engine::Agy,
                    capability
                }
            );
            assert_eq!(
                err.to_string(),
                format!(
                    "the agy engine does not support the capability `{}`",
                    capability.as_str()
                )
            );
        }
    }

    #[test]
    fn translatable_runners_leave_agy_out_while_its_gate_is_off() {
        assert!(!translatable_runners(&[]).contains(&RunnerKind::Agy));
        assert!(!translatable_runners(&[McpNucleos]).contains(&RunnerKind::Agy));
    }
}
