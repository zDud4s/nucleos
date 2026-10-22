//! Deciding how a catalogue agent launches: which engines may run it, whether the caller or the
//! agent pinned one, the prompt, and the shape its answer must have (spec 2026-10-20 section 6).
//!
//! This module never touches the network. It narrows the candidates and hands them back in
//! [`AgentLaunch::runners`]; whoever launches asks the router with exactly those
//! (`Router::build_request(&query, &launch.runners)`). When the router does not answer, the caller
//! uses [`fallback_engine`], which only lets the default engine launch if it is among `runners`.

use nucleos_base::agent::{Agent, Capability};

use super::capabilities::{self, Engine, EngineTools};
use super::output::OutputShape;
use super::package::{self, PackageRef};
use super::prompt::{self, ProjectContext};
use crate::execution::runner::{Permission, ToolPolicy};
use crate::models::route_advice::{RunShape, RunnerKind};
use crate::state::AppState;

/// What the caller pins: a workflow node's `tool`/`model`. Teams pin nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pins {
    pub engine: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug)]
pub struct AgentLaunch {
    pub prompt: String,
    /// The engines the router may choose from (a single one when pinned to a CLI; empty when the
    /// pin is the local engine, which no router reaches).
    pub runners: Vec<RunnerKind>,
    /// The pinned engine and its model (`""` when the pin names none).
    pub pin: Option<(Engine, String)>,
    pub output: OutputShape,
    pub tool_policy: ToolPolicy,
    /// What the pinned engine needs; `None` when nothing is pinned (translate for the chosen
    /// engine with `capabilities::translate` once the router has answered).
    pub tools: Option<EngineTools>,
    /// The Claude built-in tools the agent's capabilities grant (`WebSearch`, `WebFetch` for `web`).
    /// Derived from the capabilities even when nothing is pinned, because the router may still land
    /// the launch on Claude and the grant must be there when it does; empty when the pin names
    /// another engine, which has no such tools.
    pub builtin_tools: Vec<String>,
}

#[derive(Debug)]
pub enum LaunchError {
    /// A pin names an engine that cannot honour the agent's capabilities. Carries the agent ref.
    Unsupported(String, capabilities::Unsupported),
    /// No engine is left to run the agent, or a pin names an engine that does not exist.
    NoRunner(String),
    /// The package the catalogue row points at is not installed or is unreadable.
    PackageMissing(String),
    /// The router gave no advice and the default engine is not allowed for this agent.
    Router(String),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(agent, why) => write!(f, "{agent} cannot launch: {why}"),
            Self::NoRunner(why) | Self::PackageMissing(why) | Self::Router(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for LaunchError {}

/// `runners = eligible ∩ translatable − braked`, in `eligible`'s order.
pub fn narrow_runners(
    eligible: Vec<RunnerKind>,
    translatable: &[RunnerKind],
    braked: &[RunnerKind],
) -> Vec<RunnerKind> {
    eligible
        .into_iter()
        .filter(|kind| translatable.contains(kind) && !braked.contains(kind))
        .collect()
}

/// What to launch on when the router gave no advice: the default engine, but only if it is one of
/// the agent's `runners`.
pub fn fallback_engine(
    default: RunnerKind,
    runners: &[RunnerKind],
) -> Result<RunnerKind, LaunchError> {
    if runners.contains(&default) {
        Ok(default)
    } else {
        Err(LaunchError::Router(format!(
            "the llm-router gave no advice and the default engine {} is not one this agent may run on",
            default.as_str()
        )))
    }
}

/// The decision, over plain values: no state, no router handle, no I/O.
#[derive(Debug)]
struct Plan {
    runners: Vec<RunnerKind>,
    pin: Option<(Engine, String)>,
    tools: Option<EngineTools>,
}

/// The engine and model a pin names: the caller's first, then the agent row's. A model alone
/// pins nothing (the router picks models).
fn effective_pin(pins: &Pins, agent: &Agent) -> Option<(String, String)> {
    if let Some(engine) = pins.engine.as_deref().filter(|e| !e.is_empty()) {
        return Some((engine.to_owned(), pins.model.clone().unwrap_or_default()));
    }
    let engine = agent.engine.as_deref().filter(|e| !e.is_empty())?;
    let model = pins.model.clone().or_else(|| agent.model.clone());
    Some((engine.to_owned(), model.unwrap_or_default()))
}

fn caps_list(caps: &[Capability]) -> String {
    caps.iter()
        .map(|cap| cap.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `eligible` is `None` when routing is off: nothing narrows beyond translation, and the daemon's
/// own runner launches. `braked` is ignored then.
fn plan_launch(
    label: &str,
    caps: &[Capability],
    pin: Option<(String, String)>,
    eligible: Option<Vec<RunnerKind>>,
    braked: &[RunnerKind],
) -> Result<Plan, LaunchError> {
    if let Some((name, model)) = pin {
        let engine = match name.as_str() {
            "local" => Engine::Local,
            other => RunnerKind::parse(other).map(Engine::from).ok_or_else(|| {
                LaunchError::NoRunner(format!("{label} is pinned to unknown engine `{other}`"))
            })?,
        };
        let tools = capabilities::translate(engine, caps)
            .map_err(|why| LaunchError::Unsupported(label.to_owned(), why))?;
        let runners = match engine {
            Engine::Claude => vec![RunnerKind::Claude],
            Engine::Codex => vec![RunnerKind::Codex],
            Engine::Local => Vec::new(),
        };
        return Ok(Plan {
            runners,
            pin: Some((engine, model)),
            tools: Some(tools),
        });
    }
    let translatable = capabilities::translatable_runners(caps);
    let runners = match eligible {
        Some(eligible) => narrow_runners(eligible, &translatable, braked),
        None => translatable,
    };
    if runners.is_empty() {
        return Err(LaunchError::NoRunner(format!(
            "{label} asks for [{}]; no available engine can run it",
            caps_list(caps)
        )));
    }
    Ok(Plan {
        runners,
        pin: None,
        tools: None,
    })
}

/// The Claude built-ins `caps` grant that a loadout can list, or none when `pin` names another
/// engine.
fn claude_builtins(caps: &[Capability], pin: Option<&(Engine, String)>) -> Vec<String> {
    if pin.is_some_and(|(engine, _)| *engine != Engine::Claude) {
        return Vec::new();
    }
    capabilities::translate(Engine::Claude, caps)
        .map(|tools| tools.claude_allowed_tools)
        .unwrap_or_default()
        .into_iter()
        .filter(|tool| crate::loadout::is_engine_builtin(tool))
        .collect()
}

/// The launch of `agent` for `task`, with no project context (see [`resolve_agent_launch_with`]).
pub async fn resolve_agent_launch(
    state: &AppState,
    agent: &Agent,
    pins: Pins,
    task: &str,
    loadout_block: &str,
    contract: Option<&str>,
) -> Result<AgentLaunch, LaunchError> {
    resolve_agent_launch_with(
        state,
        agent,
        pins,
        task,
        loadout_block,
        contract,
        &ProjectContext::default(),
    )
    .await
}

/// The launch of `agent`: engine candidates, pin, prompt and output shape.
///
/// A `contract` replaces `output.md` as the shape told to the model; it is never validated here
/// (a workflow packet is validated by the workflow). A user agent has no package: its prompt is
/// `task` followed by `loadout_block`, which is what the team launchers already send, and it has
/// no output shape to validate unless a contract tells the model one.
pub async fn resolve_agent_launch_with(
    state: &AppState,
    agent: &Agent,
    pins: Pins,
    task: &str,
    loadout_block: &str,
    contract: Option<&str>,
    project: &ProjectContext,
) -> Result<AgentLaunch, LaunchError> {
    let caps = agent.capability_set();
    let tool_policy = capabilities::tool_policy_of(&caps);
    let label = agent
        .package_ref
        .clone()
        .unwrap_or_else(|| agent.name.clone());

    let router = state.runner.router();
    let eligible = router.as_ref().map(|router| {
        router.eligible(RunShape {
            permission: Permission::Default,
            classifier_governs_tools: false,
            tool_policy,
            resume: false,
        })
    });
    // Asked only when there is a router and no pin: a pinned launch is not narrowed.
    let pinned = effective_pin(&pins, agent);
    let braked = if eligible.is_some() && pinned.is_none() {
        crate::models::route_advice::braked_runners(&state.pool).await
    } else {
        Vec::new()
    };
    let plan = plan_launch(&label, &caps, pinned, eligible, &braked)?;

    let (prompt, output) = if agent.is_package() {
        let reference = agent
            .package_ref
            .as_deref()
            .ok_or_else(|| LaunchError::PackageMissing(format!("{label} has no package ref")))
            .and_then(|text| {
                PackageRef::parse(text)
                    .map_err(|error| LaunchError::PackageMissing(format!("{label}: {error}")))
            })?;
        let home = state
            .machine_config_root
            .as_ref()
            .ok_or_else(|| LaunchError::PackageMissing(format!("{label}: no agents directory")))?
            .join("agents");
        let pkg = package::load_installed(&home, &reference)
            .map_err(|error| LaunchError::PackageMissing(format!("{label}: {error}")))?;
        let output = match contract {
            Some(text) => OutputShape::free(text),
            None => super::output::parse_output_md(&pkg.output_md).map_err(|error| {
                LaunchError::PackageMissing(format!("{label}: output.md: {error}"))
            })?,
        };
        let prompt = prompt::build_prompt(&pkg, project, loadout_block, &output.instruction, task);
        (prompt, output)
    } else {
        let output = OutputShape::free(contract.unwrap_or(""));
        let mut prompt = format!("{task}{loadout_block}");
        if let Some(text) = contract {
            prompt = format!("{prompt}\n\n# Required output format\n{text}");
        }
        (prompt, output)
    };

    let builtin_tools = claude_builtins(&caps, plan.pin.as_ref());
    Ok(AgentLaunch {
        prompt,
        runners: plan.runners,
        pin: plan.pin,
        output,
        tool_policy,
        tools: plan.tools,
        builtin_tools,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use Capability::*;
    use RunnerKind::{Claude, Codex};

    #[test]
    fn narrowing_is_eligible_and_translatable_minus_braked() {
        assert_eq!(
            narrow_runners(vec![Claude, Codex], &[Claude, Codex], &[]),
            vec![Claude, Codex]
        );
        assert_eq!(
            narrow_runners(vec![Claude, Codex], &[Claude, Codex], &[Claude]),
            vec![Codex]
        );
        assert_eq!(
            narrow_runners(vec![Claude, Codex], &[Claude], &[]),
            vec![Claude]
        );
        assert_eq!(
            narrow_runners(vec![Claude], &[Codex], &[]),
            Vec::<RunnerKind>::new()
        );
        assert!(narrow_runners(vec![Claude, Codex], &[Claude, Codex], &[Claude, Codex]).is_empty());
    }

    #[test]
    fn web_grants_the_claude_builtins_even_unpinned_and_nothing_else_does() {
        let names = |caps: &[Capability], pin: Option<(Engine, String)>| {
            let mut names = claude_builtins(caps, pin.as_ref());
            names.sort();
            names
        };
        assert_eq!(names(&[FsRead, Web], None), vec!["WebFetch", "WebSearch"]);
        assert_eq!(
            names(&[Web], Some((Engine::Claude, String::new()))),
            vec!["WebFetch", "WebSearch"]
        );
        assert!(names(&[FsRead, FsWrite, Shell], None).is_empty());
        assert!(names(&[Web], Some((Engine::Codex, "m".to_owned()))).is_empty());
    }

    #[test]
    fn nothing_left_fails_before_any_request() {
        let error = plan_launch(
            "reviewer@1.0.0",
            &[Shell],
            None,
            Some(vec![Claude]),
            &[Claude],
        )
        .unwrap_err();
        assert!(matches!(error, LaunchError::NoRunner(_)));
        assert_eq!(
            error.to_string(),
            "reviewer@1.0.0 asks for [shell]; no available engine can run it"
        );
        // Nothing eligible at all (e.g. a resume-shaped run) fails the same way.
        assert!(plan_launch("a", &[FsRead], None, Some(Vec::new()), &[]).is_err());
    }

    #[test]
    fn a_pin_on_an_engine_that_cannot_translate_fails() {
        let pin = Some(("local".to_owned(), "qwen".to_owned()));
        let error = plan_launch("coder@1.0.0", &[FsRead, Shell], pin, None, &[]).unwrap_err();
        assert!(matches!(error, LaunchError::Unsupported(ref agent, _) if agent == "coder@1.0.0"));
        assert!(error.to_string().starts_with("coder@1.0.0 cannot launch"));
    }

    #[test]
    fn a_pin_is_kept_and_not_narrowed_by_the_router_facts() {
        let pin = Some(("codex".to_owned(), "gpt-x".to_owned()));
        let plan = plan_launch("a", &[FsRead], pin, Some(vec![Claude]), &[Codex]).unwrap();
        assert_eq!(plan.runners, vec![Codex]);
        assert_eq!(plan.pin, Some((Engine::Codex, "gpt-x".to_owned())));
        assert!(plan.tools.is_some());
        let local = plan_launch(
            "a",
            &[McpNucleos],
            Some(("local".into(), "m".into())),
            None,
            &[],
        )
        .unwrap();
        assert!(local.runners.is_empty());
        assert_eq!(local.pin, Some((Engine::Local, "m".to_owned())));
        assert!(matches!(
            plan_launch("a", &[], Some(("gemini".into(), String::new())), None, &[]),
            Err(LaunchError::NoRunner(_))
        ));
    }

    #[test]
    fn without_a_router_only_translation_narrows() {
        let plan = plan_launch("a", &[FsRead], None, None, &[Claude]).unwrap();
        assert_eq!(plan.runners, vec![Claude, Codex]);
        assert!(plan.pin.is_none());
    }

    #[test]
    fn a_router_that_is_down_launches_the_default_only_if_it_is_allowed() {
        assert_eq!(fallback_engine(Claude, &[Claude, Codex]).unwrap(), Claude);
        assert!(matches!(
            fallback_engine(Claude, &[Codex]),
            Err(LaunchError::Router(_))
        ));
    }

    #[test]
    fn the_caller_pin_wins_over_the_agent_row_and_a_model_alone_pins_nothing() {
        let agent = |engine: Option<&str>, model: Option<&str>| Agent {
            id: "i".into(),
            name: "n".into(),
            speciality: "s".into(),
            prompt: "p".into(),
            engine: engine.map(str::to_owned),
            model: model.map(str::to_owned),
            capabilities: Vec::new(),
            source: "user".into(),
            package_ref: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let pins = |engine: Option<&str>, model: Option<&str>| Pins {
            engine: engine.map(str::to_owned),
            model: model.map(str::to_owned),
        };
        assert_eq!(
            effective_pin(
                &pins(Some("codex"), Some("m1")),
                &agent(Some("claude"), Some("m2"))
            ),
            Some(("codex".into(), "m1".into()))
        );
        assert_eq!(
            effective_pin(&pins(None, None), &agent(Some("claude"), Some("m2"))),
            Some(("claude".into(), "m2".into()))
        );
        assert_eq!(
            effective_pin(&pins(None, Some("m")), &agent(None, Some("m2"))),
            None
        );
    }
}
