//! B2 of the agent packages (spec agentes-agnosticos sections 6, 7 and 13): the launcher that gives
//! a node with `agent:` its catalogue agent.
//!
//! The prompt of such a node has the agent's own layers (body, knowledge, context, output shape)
//! first, in the cache-friendly order of the agent spec, then the engine's protocol and the node's
//! `body` as the task. The engine pin and the model follow section 6's order (node pin, agent pin,
//! router restricted to the runners the agent can be translated for). This module only translates
//! `resolve_agent_launch`'s answer into the dispatcher's vocabulary; it decides nothing of its own,
//! so a team member and a graph node cannot disagree about how an agent launches.
//!
//! A node WITHOUT `agent:` goes to [`ProtocolAndBody`], untouched.
use std::sync::Arc;

use crate::agents::capabilities::{CodexSandbox, Engine};
use crate::agents::launch::{self, AgentLaunch, LaunchError, Pins};
use crate::agents::output::{self, OutputShape};
use crate::models::route_advice::RunnerKind;

use super::dispatch::{
    AgentNodeLauncher, EnginePin, LaunchRefused, NodeCtx, NodeLaunch, OutputCheck, ProtocolAndBody,
    read_body,
};

/// The launcher `main` installs: catalogue agents for nodes that name one, today's behaviour for
/// the rest.
pub struct CatalogAgentLauncher;

/// The deterministic check of an agent's `output.md` (spec section 7).
struct ShapeCheck(OutputShape);

impl OutputCheck for ShapeCheck {
    fn check(&self, text: &str) -> Result<(), String> {
        output::validate(&self.0, text).map_err(|report| report.to_string())
    }

    fn facts(&self, reply: &str) -> serde_json::Map<String, serde_json::Value> {
        output::facts(&self.0, reply)
            .into_iter()
            .map(|(key, value)| (key, serde_json::Value::String(value)))
            .collect()
    }
}

/// The caller-side pin of a node: its `tool` and `model`. An empty string pins nothing.
fn pins_of(node: &crate::workflows::graph::Node) -> Pins {
    let non_empty = |value: &Option<String>| value.clone().filter(|text| !text.is_empty());
    Pins {
        engine: non_empty(&node.tool),
        model: non_empty(&node.model),
    }
}

fn runner_of(engine: Engine) -> Option<RunnerKind> {
    match engine {
        Engine::Claude => Some(RunnerKind::Claude),
        Engine::Codex => Some(RunnerKind::Codex),
        Engine::Local => None,
    }
}

/// The dispatcher's pin for the launch's `(engine, model)`. The local engine is not a runner a
/// graph node can run on.
fn pin_of(
    pin: Option<(Engine, String)>,
    effort: Option<&str>,
    label: &str,
    sandbox: Option<CodexSandbox>,
    web_search: bool,
) -> Result<Option<EnginePin>, LaunchRefused> {
    let Some((engine, model)) = pin else {
        return Ok(None);
    };
    let Some(runner) = runner_of(engine) else {
        return Err(LaunchRefused::Unavailable(format!(
            "{label} is pinned to the local engine, which a workflow node cannot run on"
        )));
    };
    Ok(Some(EnginePin {
        runner,
        model: Some(model).filter(|model| !model.is_empty()),
        effort: effort
            .filter(|effort| !effort.is_empty())
            .map(str::to_owned),
        // Only Codex has a sandbox to pin; an agent that asks for no fs/shell/web gets read-only,
        // the same default the generated Codex prefix states.
        sandbox: (runner == RunnerKind::Codex)
            .then(|| sandbox.unwrap_or(CodexSandbox::ReadOnly).as_str()),
        // The agent's `web` capability turns Codex's own web search on (`"live"`, the counterpart of
        // the `"disabled"` an `McpOnly` run is given). Claude has no such switch: its web tools are
        // granted through the loadout.
        web_search: (runner == RunnerKind::Codex && web_search).then_some("live"),
    }))
}

fn refused(error: LaunchError) -> LaunchRefused {
    match error {
        LaunchError::PackageMissing(why) => LaunchRefused::Invalid(why),
        other => LaunchRefused::Unavailable(other.to_string()),
    }
}

/// `None` for a shape that validates nothing (a free-text contract, a user agent).
fn check_of(shape: OutputShape) -> Option<Arc<dyn OutputCheck>> {
    if shape.sections.is_empty() && shape.alt.is_none() {
        return None;
    }
    Some(Arc::new(ShapeCheck(shape)))
}

fn node_launch(
    launch: AgentLaunch,
    effort: Option<&str>,
    label: &str,
) -> Result<NodeLaunch, LaunchRefused> {
    let codex_sandbox = launch.tools.as_ref().and_then(|tools| tools.codex_sandbox);
    let web_search = launch
        .tools
        .as_ref()
        .is_some_and(|tools| tools.codex_web_search);
    let pin = pin_of(launch.pin, effort, label, codex_sandbox, web_search)?;
    Ok(NodeLaunch {
        codex_sandbox,
        builtin_tools: launch.builtin_tools,
        prompt: launch.prompt,
        // A pin bypasses the router, so there is nothing to restrict then.
        restrict_runners: pin.is_none().then_some(launch.runners),
        // A pin carries the effort itself; unpinned, the router picks engine and model and the
        // node's effort still has to reach the launch.
        effort: effort
            .filter(|effort| !effort.is_empty() && pin.is_none())
            .map(str::to_owned),
        pin,
        output: check_of(launch.output),
    })
}

/// Looks the package up in the catalogue and asks the shared resolver. `protocol` and the node's
/// `body` are the task: the agent's layers come before them in the prompt.
async fn resolve(
    ctx: &NodeCtx<'_>,
    reference: &str,
    protocol: String,
) -> Result<AgentLaunch, LaunchRefused> {
    let agents = crate::agent::list(&ctx.state.pool).await.map_err(|error| {
        LaunchRefused::Invalid(format!("the agent catalogue cannot be read: {error}"))
    })?;
    let agent = agents
        .into_iter()
        .find(|agent| agent.package_ref.as_deref() == Some(reference))
        .ok_or_else(|| {
            LaunchRefused::Invalid(format!(
                "node `{}`: agent `{reference}` is not in the catalogue",
                ctx.node.id
            ))
        })?;
    let task = match read_body(ctx.worktree, ctx.node).await? {
        Some(body) => format!("{protocol}\n\n{body}"),
        None => protocol,
    };
    launch::resolve_agent_launch(
        ctx.state,
        &agent,
        pins_of(ctx.node),
        &task,
        "",
        ctx.node.contract.as_deref(),
    )
    .await
    .map_err(refused)
}

#[async_trait::async_trait]
impl AgentNodeLauncher for CatalogAgentLauncher {
    async fn launch(
        &self,
        ctx: &NodeCtx<'_>,
        protocol: String,
    ) -> Result<NodeLaunch, LaunchRefused> {
        let Some(reference) = ctx.node.agent.as_deref() else {
            return ProtocolAndBody.launch(ctx, protocol).await;
        };
        let launch = resolve(ctx, reference, protocol).await?;
        node_launch(launch, ctx.node.effort.as_deref(), reference)
    }

    async fn output_check(&self, ctx: &NodeCtx<'_>) -> Option<Arc<dyn OutputCheck>> {
        let reference = ctx.node.agent.as_deref()?;
        match resolve(ctx, reference, String::new()).await {
            Ok(launch) => check_of(launch.output),
            Err(error) => {
                tracing::warn!(
                    node = %ctx.node.id,
                    reason = error.reason(),
                    "the output check of an agent node cannot be rebuilt"
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(yaml: &str) -> crate::workflows::graph::Node {
        serde_yaml::from_str(yaml).expect("a node")
    }

    #[test]
    fn a_node_pin_is_its_tool_and_model_and_empty_strings_pin_nothing() {
        let pins = pins_of(&node("id: a\ntype: agent\ntool: codex\nmodel: gpt-5\n"));
        assert_eq!(pins.engine.as_deref(), Some("codex"));
        assert_eq!(pins.model.as_deref(), Some("gpt-5"));
        let none = pins_of(&node("id: a\ntype: agent\ntool: \"\"\n"));
        assert_eq!(none, Pins::default());
    }

    #[test]
    fn a_cli_pin_maps_to_its_runner_with_the_nodes_effort() {
        let pin = pin_of(
            Some((Engine::Codex, "gpt-5".to_owned())),
            Some("high"),
            "r@1.0.0",
            None,
            false,
        )
        .unwrap()
        .expect("a pin");
        assert_eq!(pin.runner, RunnerKind::Codex);
        assert_eq!(pin.model.as_deref(), Some("gpt-5"));
        assert_eq!(pin.effort.as_deref(), Some("high"));
        // A Codex pin always names a sandbox: the agent's, else read-only.
        assert_eq!(pin.sandbox, Some("read-only"));
        let bare = pin_of(
            Some((Engine::Claude, String::new())),
            None,
            "r@1.0.0",
            Some(CodexSandbox::WorkspaceWrite),
            false,
        )
        .unwrap()
        .expect("a pin");
        assert_eq!(bare.runner, RunnerKind::Claude);
        assert_eq!((bare.model, bare.effort), (None, None));
        assert_eq!(bare.sandbox, None, "a Claude pin has no sandbox to pin");
        assert!(
            pin_of(None, Some("high"), "r@1.0.0", None, false)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_codex_pinned_launch_carries_the_sandbox_its_capabilities_translate_to() {
        let launch = |sandbox: Option<CodexSandbox>| AgentLaunch {
            prompt: "p".to_owned(),
            runners: vec![RunnerKind::Codex],
            pin: Some((Engine::Codex, "gpt-5".to_owned())),
            output: OutputShape::free("anything"),
            tool_policy: crate::execution::runner::ToolPolicy::Unrestricted,
            tools: Some(crate::agents::capabilities::EngineTools {
                tool_policy: crate::execution::runner::ToolPolicy::Unrestricted,
                claude_allowed_tools: Vec::new(),
                codex_sandbox: sandbox,
                codex_web_search: false,
                local_tools: Vec::new(),
                mcp: false,
            }),
            builtin_tools: Vec::new(),
        };
        let writing =
            node_launch(launch(Some(CodexSandbox::WorkspaceWrite)), None, "r@1.0.0").unwrap();
        assert_eq!(writing.codex_sandbox, Some(CodexSandbox::WorkspaceWrite));
        assert_eq!(
            writing.pin.and_then(|pin| pin.sandbox),
            Some("workspace-write")
        );
        let reading = node_launch(launch(Some(CodexSandbox::ReadOnly)), None, "r@1.0.0").unwrap();
        assert_eq!(reading.pin.and_then(|pin| pin.sandbox), Some("read-only"));
        // Nothing asked of the filesystem: the pin still runs read-only.
        let bare = node_launch(launch(None), None, "r@1.0.0").unwrap();
        assert_eq!(bare.codex_sandbox, None);
        assert_eq!(bare.pin.and_then(|pin| pin.sandbox), Some("read-only"));
    }

    #[test]
    fn an_unpinned_node_keeps_its_effort_for_the_router_and_a_pinned_one_carries_it_in_the_pin() {
        let launch = |pin: Option<(Engine, String)>| AgentLaunch {
            prompt: "p".to_owned(),
            runners: vec![RunnerKind::Claude],
            pin,
            output: OutputShape::free("anything"),
            tool_policy: crate::execution::runner::ToolPolicy::McpOnly,
            tools: None,
            builtin_tools: Vec::new(),
        };
        let unpinned = node_launch(launch(None), Some("high"), "r@1.0.0").unwrap();
        assert!(unpinned.pin.is_none());
        assert_eq!(unpinned.effort.as_deref(), Some("high"));
        let empty = node_launch(launch(None), Some(""), "r@1.0.0").unwrap();
        assert_eq!(empty.effort, None);
        let pinned = node_launch(
            launch(Some((Engine::Claude, "m".to_owned()))),
            Some("high"),
            "r@1.0.0",
        )
        .unwrap();
        assert_eq!(
            pinned.pin.and_then(|pin| pin.effort).as_deref(),
            Some("high")
        );
        assert_eq!(pinned.effort, None);
    }

    #[test]
    fn a_codex_pin_turns_web_search_live_only_when_the_agent_declares_web() {
        let pin = |runner: Engine, web: bool| {
            pin_of(Some((runner, String::new())), None, "r@1.0.0", None, web)
                .unwrap()
                .expect("a pin")
        };
        assert_eq!(pin(Engine::Codex, true).web_search, Some("live"));
        assert_eq!(pin(Engine::Codex, false).web_search, None);
        assert_eq!(
            pin(Engine::Claude, true).web_search,
            None,
            "Claude's web tools come through the loadout, not a pin"
        );
    }

    #[test]
    fn a_launch_carries_the_builtin_tools_its_agent_was_granted_and_codex_gets_web_search() {
        let launch =
            |builtin_tools: Vec<String>, pin: Option<(Engine, String)>, web: bool| AgentLaunch {
                prompt: "p".to_owned(),
                runners: vec![RunnerKind::Claude, RunnerKind::Codex],
                pin,
                output: OutputShape::free("anything"),
                tool_policy: crate::execution::runner::ToolPolicy::Unrestricted,
                tools: Some(crate::agents::capabilities::EngineTools {
                    tool_policy: crate::execution::runner::ToolPolicy::Unrestricted,
                    claude_allowed_tools: Vec::new(),
                    codex_sandbox: Some(CodexSandbox::ReadOnly),
                    codex_web_search: web,
                    local_tools: Vec::new(),
                    mcp: false,
                }),
                builtin_tools,
            };
        let web = || vec!["WebSearch".to_owned(), "WebFetch".to_owned()];
        let claude = node_launch(
            launch(web(), Some((Engine::Claude, "m".to_owned())), true),
            None,
            "r@1.0.0",
        )
        .unwrap();
        assert_eq!(claude.builtin_tools, web());
        assert_eq!(claude.pin.and_then(|pin| pin.web_search), None);
        let without = node_launch(
            launch(Vec::new(), Some((Engine::Claude, "m".to_owned())), false),
            None,
            "r@1.0.0",
        )
        .unwrap();
        assert!(without.builtin_tools.is_empty());
        let codex = node_launch(
            launch(Vec::new(), Some((Engine::Codex, "gpt-5".to_owned())), true),
            None,
            "r@1.0.0",
        )
        .unwrap();
        assert_eq!(codex.pin.and_then(|pin| pin.web_search), Some("live"));
        let codex_without = node_launch(
            launch(Vec::new(), Some((Engine::Codex, "gpt-5".to_owned())), false),
            None,
            "r@1.0.0",
        )
        .unwrap();
        assert_eq!(codex_without.pin.and_then(|pin| pin.web_search), None);
    }

    #[test]
    fn a_local_pin_is_unavailable_for_a_node() {
        let Err(LaunchRefused::Unavailable(why)) = pin_of(
            Some((Engine::Local, "m".to_owned())),
            None,
            "r@1.0.0",
            None,
            false,
        ) else {
            panic!("a local pin is Unavailable");
        };
        assert!(why.contains("r@1.0.0") && why.contains("local"), "{why}");
    }

    #[test]
    fn a_missing_package_is_invalid_and_every_other_refusal_is_unavailable() {
        assert_eq!(
            refused(LaunchError::PackageMissing("gone".to_owned())),
            LaunchRefused::Invalid("gone".to_owned())
        );
        assert_eq!(
            refused(LaunchError::NoRunner("none".to_owned())),
            LaunchRefused::Unavailable("none".to_owned())
        );
        assert_eq!(
            refused(LaunchError::Router("down".to_owned())),
            LaunchRefused::Unavailable("down".to_owned())
        );
    }

    #[test]
    fn a_free_shape_has_no_check_and_a_declared_one_does() {
        assert!(check_of(OutputShape::free("anything")).is_none());
        let shape =
            output::parse_output_md("Say a verdict.\n\n## Verdict\none-of: approved | changes\n")
                .expect("a shape");
        let check = check_of(shape).expect("sections declared, so a check");
        assert!(check.check("## Verdict\napproved\n").is_ok());
        let report = check
            .check("## Verdict\nmaybe\n")
            .expect_err("not in the set");
        assert!(report.contains("Verdict"), "{report}");
        assert!(check.check("no headings at all").is_err());
    }
}
