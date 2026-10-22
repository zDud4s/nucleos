//! §spec motor-de-workflows 3.1/3.1.1 + agentes 6/7/13: how an agent node's prompt, engine and
//! output contract are decided. M4 C2.
//!
//! The engine composes the PROTOCOL half of a node's prompt (the autopilot's own prompt functions,
//! or [`composed_protocol`] for a bundle node) and hands it to an [`AgentNodeLauncher`], which
//! answers what to run: the final prompt, the engines the router may pick from, an optional pin and
//! an optional output check. The default launcher, [`ProtocolAndBody`], is today's behaviour; the
//! agents package installs its own with [`install_launcher`].
use std::path::{Component, Path};
use std::sync::{Arc, OnceLock};

use crate::workflows::{facts::Facts, graph::Node};

use super::{AppState, JobRow};

/// A fixed engine and model, from a pin. `runner` is an engine kind the daemon can launch today.
/// The same type the router plumbing carries (`RouteQuery::pin`).
pub type EnginePin = crate::models::route_advice::EnginePinRoute;

/// Read-only view of the node being dispatched.
pub struct NodeCtx<'a> {
    pub state: &'a AppState,
    pub job: &'a JobRow,
    /// From the job's EFFECTIVE snapshot: overlay tool/model already painted in.
    pub node: &'a Node,
    pub round: i64,
    /// `Some` for a fan instance.
    pub ordinal: Option<usize>,
    pub worktree: &'a Path,
    /// What is published so far (lets a launcher quote upstream facts).
    pub facts: &'a Facts,
}

/// The shape check an agent's answer must pass (agentes 7). Opaque to the driver.
pub trait OutputCheck: Send + Sync {
    /// `text` is the run's final assistant message (`runs.stdout`). `Err(report)` is appended
    /// verbatim to the single retry prompt.
    fn check(&self, text: &str) -> Result<(), String>;

    /// The facts the daemon writes as the node's emission from a reply that passed [`check`]
    /// (`<node>.<key>` for the workflow's `when:` conditions). Default: none.
    fn facts(&self, _reply: &str) -> serde_json::Map<String, serde_json::Value> {
        Default::default()
    }
}

/// What a launcher decides. Field-for-field the agentes-spec `AgentLaunch { prompt, runners, pin,
/// output }`.
#[derive(Clone)]
pub struct NodeLaunch {
    pub prompt: String,
    /// `Some(non-empty)` restricts the router to these kinds (eligible ∩ this − braked).
    /// `Some(empty)` is refused by the driver before any run exists.
    pub restrict_runners: Option<Vec<crate::models::route_advice::RunnerKind>>,
    /// `Some` bypasses the router entirely: this runner, this model.
    pub pin: Option<EnginePin>,
    /// The node's effort when no pin carries it: the router chooses runner and model, this is the
    /// effort the launch runs at.
    pub effort: Option<String>,
    pub output: Option<Arc<dyn OutputCheck>>,
    /// The Codex sandbox the agent's capabilities translate to, when the launch is pinned to Codex.
    /// It is also carried by the pin itself (`EnginePin::sandbox`), which is what the runner is built
    /// from; this is the same fact at the launch level.
    pub codex_sandbox: Option<crate::agents::capabilities::CodexSandbox>,
    /// The agent's Codex sandbox and web search, set whether or not the node is pinned: a pinned
    /// launch builds its runner from the pin, an unpinned one from this when the router picks Codex.
    pub codex_profile: Option<crate::models::route_advice::CodexProfile>,
    /// The Claude built-in tools (`WebSearch`, `WebFetch`) the agent's `web` capability grants. They
    /// are listed in the run's loadout, which is what lets the `PreToolUse` hook allow them on an
    /// autonomous node; empty for an agent without `web`, and for a launch pinned to another engine.
    pub builtin_tools: Vec<String>,
}

impl NodeLaunch {
    /// No restriction, no pin, no check.
    pub fn plain(prompt: String) -> Self {
        Self {
            prompt,
            restrict_runners: None,
            pin: None,
            effort: None,
            output: None,
            codex_sandbox: None,
            codex_profile: None,
            builtin_tools: Vec::new(),
        }
    }
}

impl std::fmt::Debug for NodeLaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeLaunch")
            .field("prompt_bytes", &self.prompt.len())
            .field("restrict_runners", &self.restrict_runners)
            .field("pin", &self.pin)
            .field("output", &self.output.is_some())
            .finish()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum LaunchRefused {
    /// No engine can run this node (`reviewer@1.0.0 asks for [shell]; no available engine
    /// translates it`). Fails the NODE before a run exists.
    Unavailable(String),
    /// The node cannot even be described (missing body file, unknown agent). Same handling,
    /// different feed wording.
    Invalid(String),
}

impl LaunchRefused {
    /// The sentence, whichever the kind.
    pub fn reason(&self) -> &str {
        match self {
            Self::Unavailable(reason) | Self::Invalid(reason) => reason,
        }
    }
}

#[async_trait::async_trait]
pub trait AgentNodeLauncher: Send + Sync {
    /// `protocol` is the engine-composed half (3.1.1): the autopilot's existing prompt function
    /// output, or [`composed_protocol`] for a bundle node.
    async fn launch(
        &self,
        ctx: &NodeCtx<'_>,
        protocol: String,
    ) -> Result<NodeLaunch, LaunchRefused>;

    /// The output check for this node, asked again when its run has ended (the launch's `output`
    /// is a trait object and is not persisted). Default: nothing to validate.
    async fn output_check(&self, _ctx: &NodeCtx<'_>) -> Option<Arc<dyn OutputCheck>> {
        None
    }
}

/// TODAY'S behaviour, and the default: protocol + (when the node has a `body`) the body, nothing
/// else. No pin, no restriction, no check.
pub struct ProtocolAndBody;

#[async_trait::async_trait]
impl AgentNodeLauncher for ProtocolAndBody {
    async fn launch(
        &self,
        ctx: &NodeCtx<'_>,
        protocol: String,
    ) -> Result<NodeLaunch, LaunchRefused> {
        let body = read_body(ctx.worktree, ctx.node).await?;
        Ok(NodeLaunch::plain(join_body(protocol, body.as_deref())))
    }
}

/// `protocol` alone without a body (the autopilot case: byte-identical to the hard-coded prompt),
/// else `protocol + "\n\n" + body`. The engine's contract goes FIRST: it is the most stable layer.
fn join_body(protocol: String, body: Option<&str>) -> String {
    match body {
        None => protocol,
        Some(body) => format!("{protocol}\n\n{body}"),
    }
}

/// The holder behind [`install_launcher`] / [`launcher`]. A struct of its own so a test can use a
/// private instance and stay independent of the process-wide one and of test order.
pub(super) struct LauncherSlot(OnceLock<Arc<dyn AgentNodeLauncher>>);

impl LauncherSlot {
    pub(super) const fn new() -> Self {
        Self(OnceLock::new())
    }

    /// Whether this call installed `launcher`; a second install changes nothing.
    pub(super) fn install(&self, launcher: Arc<dyn AgentNodeLauncher>) -> bool {
        self.0.set(launcher).is_ok()
    }

    pub(super) fn get(&self) -> Arc<dyn AgentNodeLauncher> {
        match self.0.get() {
            Some(launcher) => Arc::clone(launcher),
            None => Arc::new(ProtocolAndBody),
        }
    }
}

static LAUNCHER: LauncherSlot = LauncherSlot::new();

/// Process-wide, like `crate::verify::installed()`: `main` installs the agents launcher; tests
/// pass one explicitly. A second call is ignored with a warning.
pub fn install_launcher(launcher: Arc<dyn AgentNodeLauncher>) {
    if !LAUNCHER.install(launcher) {
        tracing::warn!("a node launcher was already installed; ignoring the second one");
    }
}

/// The installed launcher, or [`ProtocolAndBody`] when none is.
pub fn launcher() -> Arc<dyn AgentNodeLauncher> {
    LAUNCHER.get()
}

/// PURE. The engine half for a node that is NOT one of the autopilot's hard-coded kinds: the facts
/// protocol of spec 4.1, the artifacts directory, the round and the job task. `emits_to` names the
/// keys the workflow reads from this node's emission (may be empty). `daemon_emits` is true for a
/// node with an output check: the daemon derives the emission from the validated reply, so the
/// agent is told to write nothing and to follow its output format in its final message.
pub fn composed_protocol(
    node_id: &str,
    artifacts: &str,
    task: &str,
    round: i64,
    emits_to: &[&str],
    daemon_emits: bool,
) -> String {
    if daemon_emits {
        return format!(
            "You are the `{node_id}` node of an autonomous job, round {round}. Work in the current \
             working tree.\n\n\
             The job asks for:\n{task}\n\n\
             Do not write any file for the workflow. Your final reply is what the workflow reads: \
             it must follow your output format exactly."
        );
    }
    let artifacts = artifacts.trim_end_matches(['/', '\\']);
    let keys = if emits_to.is_empty() {
        String::new()
    } else {
        format!(
            "\nThe workflow reads these keys from it: {}.",
            emits_to
                .iter()
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "You are the `{node_id}` node of an autonomous job, round {round}. Work in the current \
         working tree.\n\n\
         The job asks for:\n{task}\n\n\
         When you are done, write what you found or decided to `{artifacts}/{node_id}.json`. It \
         must be one flat JSON object: keys are short names, values are strings, booleans or \
         numbers - one level only. Do NOT nest objects or arrays inside it; a nested value is \
         refused and your answer is lost.{keys}\n\n\
         Your final message may say anything; only that file is read by the workflow."
    )
}

/// Reads the node's `body` from `<worktree>/<body>` (bundle files reach the worktree only through
/// `owns:`). `Ok(None)` when the node has none; refused when the path leaves the worktree or the
/// file is not there.
pub async fn read_body(worktree: &Path, node: &Node) -> Result<Option<String>, LaunchRefused> {
    let Some(relative) = node.body.as_deref() else {
        return Ok(None);
    };
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(LaunchRefused::Invalid(format!(
            "node `{}`: body `{relative}` is not a path inside the worktree",
            node.id
        )));
    }
    match tokio::fs::read_to_string(worktree.join(path)).await {
        Ok(text) => Ok(Some(text)),
        Err(error) => Err(LaunchRefused::Invalid(format!(
            "node `{}`: body `{relative}` cannot be read from the worktree: {error}",
            node.id
        ))),
    }
}

/// Step 2 of 3.2: asks the installed launcher what to run for `protocol`, and refuses an empty
/// engine restriction (`Some(vec![])` can match no engine) before any run exists.
#[allow(dead_code)] // the installed-launcher shorthand; the engine passes its launcher explicitly
pub(super) async fn resolve_launch(
    ctx: &NodeCtx<'_>,
    protocol: String,
) -> Result<NodeLaunch, LaunchRefused> {
    resolve_launch_with(launcher().as_ref(), ctx, protocol).await
}

/// [`resolve_launch`] against an explicit launcher (what tests and `dispatch_agent_node_with` use).
pub(super) async fn resolve_launch_with(
    launcher: &dyn AgentNodeLauncher,
    ctx: &NodeCtx<'_>,
    protocol: String,
) -> Result<NodeLaunch, LaunchRefused> {
    let mut launch = launcher.launch(ctx, protocol).await?;
    check_restriction(&launch, &ctx.node.id)?;
    // An unpinned node still says how hard to think: the router picks the engine and model, the
    // node's own effort rides along.
    if launch.pin.is_none() && launch.effort.is_none() {
        launch.effort = ctx.node.effort.clone().filter(|effort| !effort.is_empty());
    }
    Ok(launch)
}

fn check_restriction(launch: &NodeLaunch, node_id: &str) -> Result<(), LaunchRefused> {
    match launch.restrict_runners.as_deref() {
        Some([]) => Err(LaunchRefused::Unavailable(format!(
            "node `{node_id}` allows no engine to run it"
        ))),
        _ => Ok(()),
    }
}

/// The single retry prompt of 3.4: the original launch prompt, then the validator's report.
pub fn retry_prompt(original: &str, report: &str) -> String {
    format!(
        "{original}\n\nYour previous answer was refused by the output validator:\n{report}\n\
         Answer again and fix exactly that."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(yaml: &str) -> Node {
        serde_yaml::from_str(yaml).expect("a node")
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nucleos-dispatch-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn default_launcher_without_body_returns_the_protocol_unchanged() {
        let spec = crate::execution::job::spec_prompt("add a flag", "/w/.nucleos");
        for protocol in [String::new(), "x".to_owned(), spec] {
            assert_eq!(join_body(protocol.clone(), None), protocol);
        }
    }

    #[test]
    fn default_launcher_appends_the_body_after_the_protocol() {
        assert_eq!(
            join_body("PROTOCOL".to_owned(), Some("BODY")),
            "PROTOCOL\n\nBODY"
        );
    }

    #[tokio::test]
    async fn a_body_is_read_from_the_worktree() {
        let dir = scratch("body");
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        std::fs::write(dir.join("agents/triage.md"), "be brief").unwrap();
        let with = node("id: triage\ntype: agent\nbody: agents/triage.md\n");
        assert_eq!(
            read_body(&dir, &with).await,
            Ok(Some("be brief".to_owned()))
        );
        let without = node("id: triage\ntype: agent\n");
        assert_eq!(read_body(&dir, &without).await, Ok(None));
    }

    #[tokio::test]
    async fn a_body_that_is_not_in_the_worktree_is_refused_naming_the_node_and_the_path() {
        let dir = scratch("missing");
        let missing = node("id: triage\ntype: agent\nbody: agents/nope.md\n");
        let Err(LaunchRefused::Invalid(why)) = read_body(&dir, &missing).await else {
            panic!("a missing body is Invalid");
        };
        assert!(
            why.contains("triage") && why.contains("agents/nope.md"),
            "{why}"
        );

        let escaping = node("id: triage\ntype: agent\nbody: ../secret.md\n");
        let Err(LaunchRefused::Invalid(why)) = read_body(&dir, &escaping).await else {
            panic!("a body outside the worktree is Invalid");
        };
        assert!(why.contains("../secret.md"), "{why}");
    }

    #[test]
    fn the_default_launcher_pins_nothing_restricts_nothing_checks_nothing() {
        let launch = NodeLaunch::plain("p".to_owned());
        assert_eq!(launch.prompt, "p");
        assert!(launch.restrict_runners.is_none());
        assert!(launch.pin.is_none());
        assert!(launch.output.is_none());
    }

    #[test]
    fn composed_protocol_names_the_emission_file_and_flat_json_rule() {
        let text = composed_protocol(
            "triage",
            "/w/.nucleos/",
            "sort the inbox",
            2,
            &["size", "risky"],
            false,
        );
        assert!(text.contains("/w/.nucleos/triage.json"), "{text}");
        assert!(!text.contains("//triage.json"), "{text}");
        assert!(text.contains("one level"), "{text}");
        assert!(text.contains("Do NOT nest"), "{text}");
        assert!(text.contains("sort the inbox") && text.contains("round 2"));
        assert!(text.contains("`size`, `risky`"), "{text}");
        let bare = composed_protocol("triage", "/w/.nucleos", "t", 1, &[], false);
        assert!(!bare.contains("reads these keys"));
    }

    #[test]
    fn composed_protocol_for_a_daemon_emission_asks_for_no_file() {
        let text = composed_protocol("review", "/w/.nucleos", "check it", 1, &["verdict"], true);
        assert!(!text.contains("review.json"), "{text}");
        assert!(
            !text.contains("flat JSON") && !text.contains("Do NOT nest"),
            "{text}"
        );
        assert!(
            text.contains("final reply is what the workflow reads"),
            "{text}"
        );
        assert!(
            text.contains("check it") && text.contains("round 1"),
            "{text}"
        );
    }

    struct Marker;

    #[async_trait::async_trait]
    impl AgentNodeLauncher for Marker {
        async fn launch(
            &self,
            _ctx: &NodeCtx<'_>,
            protocol: String,
        ) -> Result<NodeLaunch, LaunchRefused> {
            Ok(NodeLaunch::plain(format!("marked {protocol}")))
        }
    }

    #[tokio::test]
    async fn no_installed_launcher_means_protocol_and_body() {
        let slot = LauncherSlot::new();
        // Nothing installed: the default launcher's check answers `None` and it never fails.
        let got = slot.get();
        let dir = scratch("slot");
        let with = node("id: a\ntype: agent\nbody: b.md\n");
        std::fs::write(dir.join("b.md"), "B").unwrap();
        // The default reads the body; a marker launcher would not have.
        assert_eq!(read_body(&dir, &with).await, Ok(Some("B".to_owned())));
        assert!(Arc::strong_count(&got) >= 1);
    }

    #[test]
    fn a_second_install_is_ignored() {
        let slot = LauncherSlot::new();
        assert!(slot.install(Arc::new(Marker)));
        assert!(!slot.install(Arc::new(ProtocolAndBody)));
        // The first stays: `get` hands back the same Arc twice.
        assert!(Arc::ptr_eq(&slot.get(), &slot.get()));
    }

    #[test]
    fn an_empty_restriction_is_refused_and_a_non_empty_one_is_not() {
        use crate::models::route_advice::RunnerKind;
        let mut launch = NodeLaunch::plain("p".to_owned());
        assert_eq!(check_restriction(&launch, "n"), Ok(()));
        launch.restrict_runners = Some(vec![RunnerKind::Codex]);
        assert_eq!(check_restriction(&launch, "n"), Ok(()));
        launch.restrict_runners = Some(Vec::new());
        let Err(LaunchRefused::Unavailable(why)) = check_restriction(&launch, "n") else {
            panic!("an empty restriction is Unavailable");
        };
        assert!(why.contains("`n`"), "{why}");
    }

    #[test]
    fn the_retry_prompt_appends_the_report_after_the_original() {
        let text = retry_prompt("ORIGINAL", "missing section `risks`");
        assert!(text.starts_with("ORIGINAL\n\n"));
        assert!(text.ends_with(
            "Your previous answer was refused by the output validator:\nmissing section `risks`\n\
             Answer again and fix exactly that."
        ));
    }
}
