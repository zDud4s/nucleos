//! The llm-router as an adviser: which model and effort should run the agent the daemon is about to
//! launch. Plan: `.ai/plans/2026-09-30-router-como-conselheiro.md`.
//!
//! **The daemon decides; the router suggests.** The router never runs anything for us — it answers
//! one `POST /v1/route` (`router_client.rs`) and the daemon keeps the final word. Its rules travel
//! IN the request rather than as a filter applied to the answer: the models this agent may use go in
//! `models`, the runners this run can take (eligibility minus whatever the quota brake is holding)
//! go in `runners`, and the attempts that already failed the item go in `failed`. What stays here is
//! small and is not a decision: the fallback (router down, slow, 400 or 422 launches exactly what
//! would have launched without it), a check that the answer stayed inside what was sent, and the
//! speed profile's effort ceiling.
//!
//! **Off is structural, not a branch.** `main.rs` calls [`front`], which hands back the very `Arc`
//! it was given unless `.ai/router.yaml` turns a surface on. Only then is the primary runner wrapped
//! in a [`RoutedRunner`], whose one difference from what it wraps is answering
//! `CommandRunner::router()` with `Some` — a capability on the trait, not an `AppState` field.
//!
//! Three modes, in `.ai/router.yaml`: `off` (the default, and what an absent, malformed or unknown
//! value means), `shadow` (ask, record the advice beside what ran, launch what would have launched)
//! and `apply` (launch the advice when it passes the checks). One switch per surface — `runs`,
//! `team`, `council`, `recruit` — so `apply` can be turned on a surface at a time.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use sqlx::SqlitePool;
use tokio::sync::mpsc::UnboundedSender;

use crate::router_client::{RouteAdvice, RouteRequest, RouterClient};
use crate::runner::{CommandRunner, Permission, RunOutcome, RunRequest, ToolPolicy, TurnEvent};
use crate::speed::Speed;

/// Where the router listens unless `.ai/router.yaml` says otherwise.
pub const DEFAULT_URL: &str = "http://127.0.0.1:18733";
/// The ceiling on one route call. A run waits at most this long before launching without advice.
pub const DEFAULT_TIMEOUT_MS: u64 = 2500;
const MIN_TIMEOUT_MS: u64 = 100;
const MAX_TIMEOUT_MS: u64 = 10_000;
/// How much of the prompt leaves the daemon as `task`: the head, where the ask is.
pub const TASK_CHARS: usize = 4000;
/// How much of the gate's output goes as `gate_output`: the tail, where the failure is. The router
/// keeps the same tail (`route_api.GATE_OUTPUT_CHARS`), so sending more is only bytes.
pub const GATE_TAIL_CHARS: usize = 1500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    Shadow,
    Apply,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::Apply => "apply",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "shadow" => Some(Self::Shadow),
            "apply" => Some(Self::Apply),
            _ => None,
        }
    }

    /// A YAML value as a mode. `false` is accepted as `off` because a YAML 1.1 reader — and a
    /// person who learnt YAML from one — writes `off` and means the boolean.
    fn from_yaml(value: &serde_yaml::Value) -> Option<Self> {
        match value {
            serde_yaml::Value::String(text) => Self::parse(text.trim()),
            serde_yaml::Value::Bool(false) => Some(Self::Off),
            _ => None,
        }
    }
}

/// The places the daemon already chooses an agent's model, each with its own switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Runs,
    Team,
    Council,
    Recruit,
}

impl Surface {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Runs => "runs",
            Self::Team => "team",
            Self::Council => "council",
            Self::Recruit => "recruit",
        }
    }

    const ALL: [Self; 4] = [Self::Runs, Self::Team, Self::Council, Self::Recruit];
}

/// A CLI the daemon can launch, named as the router names it (`route_api.RUNNERS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RunnerKind {
    Claude,
    Codex,
}

impl RunnerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

/// `.ai/router.yaml`, read once at startup.
#[derive(Debug, Clone, PartialEq)]
pub struct RouterConfig {
    /// The mode of every surface that names none of its own.
    pub mode: Mode,
    pub url: String,
    pub timeout_ms: u64,
    /// Runners besides the primary that a routed run may be moved to. Empty: the primary only.
    pub runners: Vec<String>,
    /// Per runner, the globs the router may choose from. Absent for a runner: the daemon's own
    /// catalogue for it (see [`allowed_models`]).
    pub models: BTreeMap<String, Vec<String>>,
    /// A surface's own mode, overriding `mode`.
    pub surfaces: BTreeMap<&'static str, Mode>,
}

impl RouterConfig {
    pub fn off() -> Self {
        Self {
            mode: Mode::Off,
            url: DEFAULT_URL.to_owned(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            runners: Vec::new(),
            models: BTreeMap::new(),
            surfaces: BTreeMap::new(),
        }
    }

    /// The mode a surface runs under: its own when the file names one, else the global one.
    pub fn mode_for(&self, surface: Surface) -> Mode {
        self.surfaces
            .get(surface.as_str())
            .copied()
            .unwrap_or(self.mode)
    }

    /// Every surface off: nothing to front, and [`front`] hands the runner back untouched.
    pub fn is_off(&self) -> bool {
        Surface::ALL
            .iter()
            .all(|surface| self.mode_for(*surface) == Mode::Off)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    mode: Option<serde_yaml::Value>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    runners: Vec<String>,
    #[serde(default)]
    models: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    surfaces: BTreeMap<String, serde_yaml::Value>,
}

/// Reads `.ai/router.yaml`. Anything short of a well-formed, loopback configuration is `off`.
///
/// An absent file is the ordinary case and is silent. A malformed one, an unknown mode or a URL off
/// this machine is `off` plus a warning: the prompt's head travels in the request, and task text
/// must not leave the machine because somebody mistyped a host.
pub fn load_config(path: &Path) -> RouterConfig {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(path = %path.display(), "no router config; routing is off");
            return RouterConfig::off();
        }
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "router config unreadable; routing is off");
            return RouterConfig::off();
        }
    };
    match parse_config(&text) {
        Ok(config) => config,
        Err(why) => {
            tracing::warn!(path = %path.display(), %why, "router config refused; routing is off");
            RouterConfig::off()
        }
    }
}

/// The pure half of [`load_config`]: the reason a file is refused, or the configuration it names.
pub fn parse_config(text: &str) -> Result<RouterConfig, String> {
    let raw: RawConfig = serde_yaml::from_str(text).map_err(|error| error.to_string())?;
    let mode = match raw.mode.as_ref() {
        None => Mode::Off,
        Some(value) => {
            Mode::from_yaml(value).ok_or_else(|| format!("unknown router mode {value:?}"))?
        }
    };
    let url = raw.url.unwrap_or_else(|| DEFAULT_URL.to_owned());
    if !is_loopback(&url) {
        return Err(format!(
            "router url {url} is not on this machine; only loopback is allowed"
        ));
    }
    let mut surfaces = BTreeMap::new();
    for (name, value) in &raw.surfaces {
        let surface = Surface::ALL
            .into_iter()
            .find(|surface| surface.as_str() == name)
            .ok_or_else(|| format!("unknown router surface {name}"))?;
        let mode = Mode::from_yaml(value)
            .ok_or_else(|| format!("unknown router mode {value:?} for surface {name}"))?;
        surfaces.insert(surface.as_str(), mode);
    }
    Ok(RouterConfig {
        mode,
        url,
        timeout_ms: raw
            .timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS),
        runners: raw.runners,
        models: raw.models,
        surfaces,
    })
}

/// Only `http(s)://` to 127.0.0.1, localhost or ::1.
fn is_loopback(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    matches!(parsed.scheme(), "http" | "https")
        && parsed.username().is_empty()
        && matches!(
            parsed.host_str(),
            Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
        )
}

/// One runner the daemon can launch a routed run on.
pub struct Available {
    pub kind: RunnerKind,
    pub runner: Arc<dyn CommandRunner>,
    /// What this runner launches when the request names no model. Recorded as the effective model,
    /// so the next retry can name it in `failed`.
    pub default_model: String,
    /// The globs over tier names and model ids the router may choose from for this runner.
    pub models: Vec<String>,
}

/// The router, as the daemon holds it: its configuration, the client, and the runners it may pick.
pub struct Router {
    pub config: RouterConfig,
    pub client: RouterClient,
    pub primary: Available,
    pub alternates: Vec<Available>,
}

impl std::fmt::Debug for Router {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Router")
            .field("config", &self.config)
            .field("primary", &self.primary.kind)
            .finish()
    }
}

impl Router {
    pub fn new(config: RouterConfig, primary: Available, alternates: Vec<Available>) -> Self {
        let client = RouterClient::new(&config.url, Duration::from_millis(config.timeout_ms));
        Self {
            config,
            client,
            primary,
            alternates,
        }
    }

    /// The mode `surface` runs under.
    pub fn mode_for(&self, surface: Surface) -> Mode {
        self.config.mode_for(surface)
    }

    /// The runs surface's mode: what `resolve` asks under and records.
    pub fn runs_mode(&self) -> Mode {
        self.mode_for(Surface::Runs)
    }

    pub fn available(&self, kind: RunnerKind) -> Option<&Available> {
        std::iter::once(&self.primary)
            .chain(self.alternates.iter())
            .find(|available| available.kind == kind)
    }

    /// Which runners a run of this shape may be moved to. A safety property, not a preference.
    ///
    /// The primary is always eligible: it is what launches without a router. Nothing else is for a
    /// resume, which continues a session only the CLI that started it can read. Codex needs more
    /// again: `CodexCliRunner` refuses every permission rung above `Default`, a restrictive tool
    /// policy and a classifier-governed run (it has no `PreToolUse` hook), so offering it such a run
    /// would either fail the launch or ungovern it.
    pub fn eligible(&self, shape: RunShape) -> Vec<RunnerKind> {
        let mut kinds = vec![self.primary.kind];
        if shape.resume {
            return kinds;
        }
        for alternate in &self.alternates {
            let fits = match alternate.kind {
                RunnerKind::Claude => true,
                RunnerKind::Codex => {
                    shape.permission == Permission::Default
                        && !shape.classifier_governs_tools
                        && shape.tool_policy == ToolPolicy::Unrestricted
                }
            };
            if fits && !kinds.contains(&alternate.kind) {
                kinds.push(alternate.kind);
            }
        }
        kinds
    }

    /// The request, with the daemon's rules in it: `runners` as given (already eligible and clear of
    /// the quota brake), `models` as the union of those runners' globs, and the item's history.
    pub fn build_request(&self, query: &RouteQuery, runners: &[RunnerKind]) -> RouteRequest {
        let mut models: Vec<String> = Vec::new();
        for kind in runners {
            if let Some(available) = self.available(*kind) {
                for glob in &available.models {
                    if !models.contains(glob) {
                        models.push(glob.clone());
                    }
                }
            }
        }
        let item = query.item.clone().unwrap_or_default();
        RouteRequest {
            task: head(&query.task, TASK_CHARS).to_owned(),
            stage: query.stage.clone(),
            files: item.files,
            attempt: item.attempt,
            gate_output: item
                .gate_output
                .as_deref()
                .map(|output| tail(output, GATE_TAIL_CHARS).to_owned())
                .filter(|output| !output.trim().is_empty()),
            failed: item.failed,
            runners: runners
                .iter()
                .map(|kind| kind.as_str().to_owned())
                .collect(),
            packet: None,
            models,
            exclude: Vec::new(),
        }
    }

    /// Whether an answer stayed inside what was sent. An answer outside it is a router failure.
    ///
    /// The router filters before it chooses, so this should never refuse anything — which is exactly
    /// why it is checked: a router that ignored a filter would otherwise move a run onto a runner
    /// the daemon ruled out, and nothing would say so.
    pub fn check_answer(
        &self,
        advice: &RouteAdvice,
        sent: &RouteRequest,
    ) -> Result<RunnerKind, String> {
        if !sent.runners.iter().any(|runner| runner == &advice.runner) {
            return Err(format!(
                "runner {} is not among those sent {:?}",
                advice.runner, sent.runners
            ));
        }
        let kind = RunnerKind::parse(&advice.runner)
            .ok_or_else(|| format!("runner {} cannot be launched here", advice.runner))?;
        if advice.model.trim().is_empty() {
            return Err("the advice names no model".to_owned());
        }
        let names: Vec<&str> = std::iter::once(advice.model.as_str())
            .chain(advice.tier.as_deref())
            .collect();
        let matches = |globs: &[String]| {
            globs
                .iter()
                .any(|glob| names.iter().any(|name| glob_match(glob, name)))
        };
        if !sent.models.is_empty() && !matches(&sent.models) {
            return Err(format!(
                "model {} matches none of the globs sent {:?}",
                advice.model, sent.models
            ));
        }
        let own = self
            .available(kind)
            .map(|available| available.models.as_slice())
            .unwrap_or_default();
        if !own.is_empty() && !matches(own) {
            return Err(format!(
                "model {} is not one {} may run {:?}",
                advice.model,
                kind.as_str(),
                own
            ));
        }
        Ok(kind)
    }

    /// Warns, once at startup, about every allowed glob the router serves nothing for. Such a glob
    /// is not an error — but a catalogue that names models by ids the router does not know turns
    /// every routed run into a 422 and a fallback, and this is the only place that would be said.
    pub async fn check_targets(&self) {
        let targets = match self.client.targets().await {
            Ok(targets) => targets,
            Err(error) => {
                tracing::warn!(%error, "could not list the llm-router's targets");
                return;
            }
        };
        for available in std::iter::once(&self.primary).chain(self.alternates.iter()) {
            for glob in &available.models {
                let served = targets.iter().any(|target| {
                    target.runner == available.kind.as_str()
                        && (glob_match(glob, &target.model) || glob_match(glob, &target.tier))
                });
                if !served {
                    tracing::warn!(
                        runner = available.kind.as_str(),
                        %glob,
                        "the llm-router serves no tier this allowed model matches"
                    );
                }
            }
        }
    }
}

/// `fnmatch.fnmatchcase` over lowercased text, as the router matches `models`: `*` any run, `?` one
/// character. Character classes are not supported and match literally; nothing here writes one.
pub fn glob_match(glob: &str, text: &str) -> bool {
    fn go(pattern: &[char], text: &[char]) -> bool {
        match pattern.split_first() {
            None => text.is_empty(),
            Some(('*', rest)) => (0..=text.len()).any(|skip| go(rest, &text[skip..])),
            Some(('?', rest)) => !text.is_empty() && go(rest, &text[1..]),
            Some((c, rest)) => text.first() == Some(c) && go(rest, &text[1..]),
        }
    }
    let pattern: Vec<char> = glob.to_lowercase().chars().collect();
    let text: Vec<char> = text.to_lowercase().chars().collect();
    go(&pattern, &text)
}

/// The shape of a run, as far as runner eligibility cares.
#[derive(Debug, Clone, Copy)]
pub struct RunShape {
    pub permission: Permission,
    pub classifier_governs_tools: bool,
    pub tool_policy: ToolPolicy,
    pub resume: bool,
}

/// What the daemon would launch, or what it will.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub kind: RunnerKind,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// Effort levels, weakest first: the order the ceiling is compared in.
const EFFORT_ORDER: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The effort ceiling a speed allows the router to advise.
///
/// Owned here rather than read from the workflow's speed table, which the daemon deliberately does
/// not read (`speed.rs`): this is the router's clamp, and the values are the spec's §2 row for
/// `reasoning_effort` (`normal` medium, `fast` low, `thorough` high).
pub fn effort_ceiling(speed: Speed) -> &'static str {
    match speed {
        Speed::Normal => "medium",
        Speed::Fast => "low",
        Speed::Thorough => "high",
    }
}

/// `effort` held at or under `ceiling`. An effort this daemon cannot rank is taken down to the
/// ceiling rather than trusted, since it cannot be shown to sit under it.
pub fn clamp_effort(effort: &str, ceiling: &str) -> String {
    let rank = |value: &str| EFFORT_ORDER.iter().position(|level| *level == value);
    match (rank(effort), rank(ceiling)) {
        (Some(asked), Some(limit)) if asked <= limit => effort.to_owned(),
        _ => ceiling.to_owned(),
    }
}

/// The speed a launch is told, read from the environment it is handed (`speed::Capacity::env`).
/// Absent is `normal`.
pub fn speed_of(env: &[(String, String)]) -> Speed {
    Speed::from_column(
        env.iter()
            .find(|(name, _)| name == crate::speed::SPEED_VAR)
            .map(|(_, value)| value.as_str()),
    )
}

/// What launches. `shadow` and `off` launch the configured choice whatever the advice. `apply`
/// launches a checked advice, its effort held under the speed's ceiling, and falls back to the
/// configured choice when there is none.
pub fn choose(
    mode: Mode,
    configured: Choice,
    advice: Option<(&RouteAdvice, RunnerKind)>,
    ceiling: &str,
) -> Choice {
    match (mode, advice) {
        (Mode::Apply, Some((advice, kind))) if !advice.model.trim().is_empty() => Choice {
            kind,
            model: Some(advice.model.clone()),
            effort: advice
                .effort
                .as_deref()
                .map(|effort| clamp_effort(effort, ceiling)),
        },
        _ => configured,
    }
}

/// The item a run works on, as the route request needs it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ItemContext {
    pub files: Vec<String>,
    /// `gate_attempts + 1`: which attempt at the item this run is.
    pub attempt: Option<u32>,
    pub gate_output: Option<String>,
    /// `model[@effort]` of every attempt that already failed the item.
    pub failed: Vec<String>,
}

/// How a run names its item: a team item by its id, a sequential job's by its place in the queue.
#[derive(Debug, Clone, Copy)]
pub enum ItemRef {
    Id(i64),
    Ordinal { job_id: i64, ordinal: i64 },
}

/// What a run tells the router about itself.
#[derive(Debug, Clone)]
pub struct RouteQuery {
    pub task: String,
    pub stage: Option<String>,
    /// Read when the run is created, not when it is routed: `job.rs` points the item at the new run
    /// right after creating it, and after that the previous attempt — the one that failed — can no
    /// longer be found from the item.
    pub item: Option<ItemContext>,
    pub resume: bool,
}

/// `model[@effort]`, as the router's `failed` matches it.
pub fn failed_label(model: &str, effort: Option<&str>) -> String {
    match effort {
        Some(effort) if !effort.is_empty() => format!("{model}@{effort}"),
        _ => model.to_owned(),
    }
}

/// The item's files, attempt, last gate output, and every model that already failed it.
///
/// `failed` is a chain: the previous run recorded, in `route_failed`, what had failed before it,
/// so the whole history is that list plus the previous run's own model when its gate failed. A
/// previous run launched with routing off recorded no model and adds nothing — nothing is guessed.
/// Every read is best-effort: a missing row is an item with no history, never a failed run.
pub async fn item_context(pool: &SqlitePool, item: ItemRef, current_run: i64) -> ItemContext {
    type Row = (
        Option<String>,
        i64,
        Option<String>,
        Option<String>,
        Option<i64>,
    );
    let row: Option<Row> = match item {
        ItemRef::Id(id) => sqlx::query_as(
            "SELECT files, gate_attempts, gate_output, gate_status, run_id FROM job_items WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await,
        ItemRef::Ordinal { job_id, ordinal } => sqlx::query_as(
            "SELECT files, gate_attempts, gate_output, gate_status, run_id
               FROM job_items WHERE job_id = ? AND ordinal = ?",
        )
        .bind(job_id)
        .bind(ordinal)
        .fetch_optional(pool)
        .await,
    }
    .unwrap_or_else(|error| {
        tracing::warn!(%error, "could not read a routed run's item");
        None
    });
    let Some((files, gate_attempts, gate_output, gate_status, previous)) = row else {
        return ItemContext::default();
    };
    let mut failed: Vec<String> = Vec::new();
    if gate_status.as_deref() == Some("failed")
        && let Some(previous) = previous.filter(|previous| *previous != current_run)
    {
        let prior: Option<(Option<String>, Option<String>, Option<String>)> =
            sqlx::query_as("SELECT model, effort, route_failed FROM runs WHERE id = ?")
                .bind(previous)
                .fetch_optional(pool)
                .await
                .unwrap_or_default();
        if let Some((model, effort, route_failed)) = prior {
            for label in route_failed
                .as_deref()
                .and_then(|text| serde_json::from_str::<Vec<String>>(text).ok())
                .unwrap_or_default()
            {
                if !failed.contains(&label) {
                    failed.push(label);
                }
            }
            if let Some(model) = model.filter(|model| !model.is_empty()) {
                let label = failed_label(&model, effort.as_deref());
                if !failed.contains(&label) {
                    failed.push(label);
                }
            }
        }
    }
    ItemContext {
        files: files
            .as_deref()
            .and_then(|text| serde_json::from_str::<Vec<String>>(text).ok())
            .unwrap_or_default(),
        attempt: u32::try_from(gate_attempts + 1).ok(),
        gate_output,
        failed,
    }
}

/// The runners the quota brake is holding right now, by the stored readings only.
///
/// Side-effect free on purpose: `quota::quota_permits_new_run` announces a blind brake and clears
/// markers, which is the scheduler's job, and a live sidecar call could cost the run 25 seconds.
/// Fails open like the brake itself — an unreadable policy or reading holds nothing.
async fn braked_runners(pool: &SqlitePool) -> Vec<RunnerKind> {
    let Ok(policy) = crate::quota::load_brake_policy(pool).await else {
        return Vec::new();
    };
    if !policy.enabled {
        return Vec::new();
    }
    let now = chrono::Utc::now();
    let Ok(providers) = crate::quota::stored(pool, now).await else {
        return Vec::new();
    };
    [RunnerKind::Claude, RunnerKind::Codex]
        .into_iter()
        .filter(|kind| {
            matches!(
                crate::quota::judge(&policy, kind.as_str(), &providers, now),
                crate::quota::Verdict::Over { .. }
            )
        })
        .collect()
}

/// What `spawn_run` launches, and what it keeps for reporting the outcome.
pub struct Resolved {
    pub runner: Arc<dyn CommandRunner>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub decision_id: Option<String>,
    pub router: Option<Arc<Router>>,
}

/// Asks the router about one run, records the choice beside the advice, and says what to launch.
///
/// With no router (routing off), no query (a triage run, a handoff) or the runs surface off, it
/// returns its inputs untouched and writes nothing: off is today's launch, byte for byte. Otherwise
/// every path — advice, 400, 422, timeout, an answer outside the set — writes ONE update, so a row
/// never holds half a decision, and a failed write only warns.
#[allow(clippy::too_many_arguments)]
pub async fn resolve(
    pool: &SqlitePool,
    run_id: i64,
    runner: Arc<dyn CommandRunner>,
    model: Option<String>,
    effort: Option<String>,
    route: Option<RouteQuery>,
    shape: RunShape,
    speed: Speed,
) -> Resolved {
    let (Some(router), Some(query)) = (runner.router(), route) else {
        return Resolved {
            runner,
            model,
            effort,
            decision_id: None,
            router: None,
        };
    };
    let mode = router.runs_mode();
    if mode == Mode::Off {
        return Resolved {
            runner,
            model,
            effort,
            decision_id: None,
            router: None,
        };
    }

    let shape = RunShape {
        resume: shape.resume || query.resume,
        ..shape
    };
    let braked = braked_runners(pool).await;
    let runners: Vec<RunnerKind> = router
        .eligible(shape)
        .into_iter()
        .filter(|kind| !braked.contains(kind))
        .collect();
    let request = router.build_request(&query, &runners);
    // Never asked with no runners: an empty list is "any runner" to the router.
    let advice = if runners.is_empty() {
        tracing::info!(
            run_id,
            "every eligible runner is held by the quota brake; not routing"
        );
        None
    } else {
        match router.client.route(&request).await {
            Ok(advice) => match router.check_answer(&advice, &request) {
                Ok(kind) => Some((advice, kind)),
                Err(why) => {
                    tracing::warn!(run_id, %why, "llm-router answered outside what was sent; ignoring it");
                    None
                }
            },
            Err(error) => {
                tracing::warn!(run_id, %error, "no advice from the llm-router; launching as configured");
                None
            }
        }
    };

    let configured = Choice {
        kind: router.primary.kind,
        model: model.clone(),
        effort: effort.clone(),
    };
    let choice = choose(
        mode,
        configured,
        advice.as_ref().map(|(advice, kind)| (advice, *kind)),
        effort_ceiling(speed),
    );
    let launched = router.available(choice.kind).unwrap_or(&router.primary);
    let effective_model = choice
        .model
        .clone()
        .unwrap_or_else(|| launched.default_model.clone());
    let route_failed = (!request.failed.is_empty())
        .then(|| serde_json::to_string(&request.failed).unwrap_or_default());
    let advised = advice.as_ref().map(|(advice, _)| advice);
    if let Err(error) = sqlx::query(
        "UPDATE runs SET model = ?, effort = ?, runner = ?, route_mode = ?, route_decision_id = ?,
                         advised_runner = ?, advised_model = ?, advised_effort = ?, route_failed = ?
          WHERE id = ?",
    )
    .bind(&effective_model)
    .bind(&choice.effort)
    .bind(choice.kind.as_str())
    .bind(mode.as_str())
    .bind(advised.map(|advice| advice.decision_id.as_str()))
    .bind(advised.map(|advice| advice.runner.as_str()))
    .bind(advised.map(|advice| advice.model.as_str()))
    .bind(advised.and_then(|advice| advice.effort.as_deref()))
    .bind(&route_failed)
    .bind(run_id)
    .execute(pool)
    .await
    {
        tracing::warn!(run_id, %error, "could not record a run's route decision");
    }

    // The runner the run already held whenever the primary launches: the wrapper, not what it
    // wraps, so nothing downstream can tell a routed-to-primary launch from an unrouted one.
    let chosen_runner = if choice.kind == router.primary.kind {
        runner
    } else {
        Arc::clone(&launched.runner)
    };
    Resolved {
        runner: chosen_runner,
        model: choice.model,
        effort: choice.effort,
        decision_id: advised.map(|advice| advice.decision_id.clone()),
        router: Some(router),
    }
}

/// The primary runner, fronted by the router — or the very same `Arc` when every surface is off.
///
/// The only call `main.rs` makes. It builds the alternates `config.runners` names from the models
/// file, exactly as `main.rs` builds the primary, and never the primary's own kind twice.
pub fn front(
    primary: Arc<dyn CommandRunner>,
    models: &crate::config::ModelsConfig,
    config: RouterConfig,
) -> Arc<dyn CommandRunner> {
    if config.is_off() {
        return primary;
    }
    let primary_kind = RunnerKind::parse(models.active_runner()).unwrap_or(RunnerKind::Claude);
    let mut alternates: Vec<Available> = Vec::new();
    for name in &config.runners {
        let Some(kind) = RunnerKind::parse(name) else {
            tracing::warn!(%name, "router config names a runner this daemon cannot launch; ignoring it");
            continue;
        };
        if kind == primary_kind || alternates.iter().any(|available| available.kind == kind) {
            continue;
        }
        let runner: Arc<dyn CommandRunner> = match kind {
            RunnerKind::Claude => Arc::new(crate::runner::ClaudeCliRunner {
                model: models.claude_model.clone(),
                plan_model: models.plan_model.clone(),
                review_model: models.review_model.clone(),
            }),
            RunnerKind::Codex => Arc::new(crate::runner::CodexCliRunner {
                model: models.codex_model.clone(),
                sandbox_mode: None,
            }),
        };
        alternates.push(Available {
            kind,
            runner,
            default_model: default_model(models, kind),
            models: allowed_models(models, &config, kind),
        });
    }
    let primary_available = Available {
        kind: primary_kind,
        runner: Arc::clone(&primary),
        default_model: default_model(models, primary_kind),
        models: allowed_models(models, &config, primary_kind),
    };
    tracing::info!(
        mode = config.mode.as_str(),
        url = %config.url,
        "llm-router advice is on"
    );
    let router = Arc::new(Router::new(config, primary_available, alternates));
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let checking = Arc::clone(&router);
        handle.spawn(async move { checking.check_targets().await });
    }
    Arc::new(RoutedRunner {
        inner: primary,
        router,
    })
}

fn default_model(models: &crate::config::ModelsConfig, kind: RunnerKind) -> String {
    match kind {
        RunnerKind::Claude => models.claude_model.clone(),
        RunnerKind::Codex => models.codex_model.clone(),
    }
}

/// The globs a runner's routed runs may use: the router file's own list for it when it names one,
/// else every cloud model the daemon's catalogue offers that runner, plus the model it launches by
/// default.
pub fn allowed_models(
    models: &crate::config::ModelsConfig,
    config: &RouterConfig,
    kind: RunnerKind,
) -> Vec<String> {
    if let Some(globs) = config
        .models
        .get(kind.as_str())
        .filter(|globs| !globs.is_empty())
    {
        return globs.clone();
    }
    let mut allowed: Vec<String> = models
        .assistant_choices
        .iter()
        .filter(|choice| choice.brain == "cloud")
        .filter(|choice| choice.runner.as_deref().unwrap_or("claude") == kind.as_str())
        .map(|choice| choice.id.clone())
        .collect();
    let own = default_model(models, kind);
    if !allowed.contains(&own) {
        allowed.push(own);
    }
    allowed
}

/// The primary runner, answering `router()`. Every other method is the wrapped runner's own: a
/// method left to the trait's default here would silently change what the daemon launches, which
/// is what `the_fronted_runner_answers_exactly_as_the_runner_it_fronts` holds shut.
pub struct RoutedRunner {
    pub inner: Arc<dyn CommandRunner>,
    pub router: Arc<Router>,
}

#[async_trait]
impl CommandRunner for RoutedRunner {
    async fn run_prompt(
        &self,
        request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: Arc<std::sync::Mutex<String>>,
    ) -> std::io::Result<RunOutcome> {
        self.inner.run_prompt(request, session_tx, transcript).await
    }

    async fn run_prompt_with_context_fill(
        &self,
        request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: Arc<std::sync::Mutex<String>>,
        context_fill: Arc<std::sync::Mutex<Option<i64>>>,
    ) -> std::io::Result<RunOutcome> {
        self.inner
            .run_prompt_with_context_fill(request, session_tx, transcript, context_fill)
            .await
    }

    async fn run_prompt_with_turns(
        &self,
        request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: Arc<std::sync::Mutex<String>>,
        context_fill: Arc<std::sync::Mutex<Option<i64>>>,
        turns: Option<UnboundedSender<TurnEvent>>,
    ) -> std::io::Result<RunOutcome> {
        self.inner
            .run_prompt_with_turns(request, session_tx, transcript, context_fill, turns)
            .await
    }

    fn model_for_stage(&self, stage: Option<&str>) -> Option<String> {
        self.inner.model_for_stage(stage)
    }

    fn authored_prompt(
        &self,
        request: &RunRequest,
    ) -> Option<crate::prompt_budget::AuthoredPrompt> {
        self.inner.authored_prompt(request)
    }

    fn router(&self) -> Option<Arc<Router>> {
        Some(Arc::clone(&self.router))
    }
}

/// The first `limit` characters, cut on a character boundary.
fn head(text: &str, limit: usize) -> &str {
    match text.char_indices().nth(limit) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

/// The last `limit` characters, cut on a character boundary.
fn tail(text: &str, limit: usize) -> &str {
    let count = text.chars().count();
    if count <= limit {
        return text;
    }
    match text.char_indices().nth(count - limit) {
        Some((index, _)) => &text[index..],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router_client::test_support::{dead_address, slow_router, stub_router};

    async fn pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    async fn seed_run(pool: &SqlitePool) -> i64 {
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES ('p', 'x', 'running', 'worktree', '2026-09-30T00:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    fn outcome(stdout: &str) -> RunOutcome {
        RunOutcome {
            exit_code: 0,
            stdout: stdout.into(),
            stderr: String::new(),
            session_id: None,
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            num_turns: None,
            compacted: false,
        }
    }

    /// A runner whose every method answers something no trait default would.
    struct Probe;

    #[async_trait]
    impl CommandRunner for Probe {
        async fn run_prompt(
            &self,
            request: RunRequest,
            _session_tx: UnboundedSender<String>,
            _transcript: Arc<std::sync::Mutex<String>>,
        ) -> std::io::Result<RunOutcome> {
            Ok(outcome(&format!(
                "plain {}",
                request.model.unwrap_or_default()
            )))
        }

        async fn run_prompt_with_context_fill(
            &self,
            _request: RunRequest,
            _session_tx: UnboundedSender<String>,
            _transcript: Arc<std::sync::Mutex<String>>,
            context_fill: Arc<std::sync::Mutex<Option<i64>>>,
        ) -> std::io::Result<RunOutcome> {
            *context_fill.lock().unwrap() = Some(7);
            Ok(outcome("fill"))
        }

        async fn run_prompt_with_turns(
            &self,
            _request: RunRequest,
            _session_tx: UnboundedSender<String>,
            _transcript: Arc<std::sync::Mutex<String>>,
            _context_fill: Arc<std::sync::Mutex<Option<i64>>>,
            turns: Option<UnboundedSender<TurnEvent>>,
        ) -> std::io::Result<RunOutcome> {
            if let Some(turns) = turns {
                let _ = turns.send(TurnEvent::Line("a turn".into()));
            }
            Ok(outcome("turns"))
        }

        fn model_for_stage(&self, stage: Option<&str>) -> Option<String> {
            stage.map(|stage| format!("{stage}-model"))
        }

        fn authored_prompt(
            &self,
            request: &RunRequest,
        ) -> Option<crate::prompt_budget::AuthoredPrompt> {
            Some(crate::runner::authored_prompt(request))
        }
    }

    fn request() -> RunRequest {
        RunRequest {
            prompt: "a prompt".into(),
            env: Vec::new(),
            cwd: None,
            permission: Permission::Default,
            resume_session_id: None,
            mcp_config: None,
            mcp_box: None,
            tool_policy: ToolPolicy::Unrestricted,
            progress_timeout: None,
            max_turns: None,
            session_id: None,
            fork_session: false,
            include_partial_messages: false,
            images: Vec::new(),
            steerable: false,
            classifier_governs_tools: false,
            ambient_mcp: false,
            model: Some("m".into()),
            effort: None,
            fallback_model: Vec::new(),
            add_dirs: Vec::new(),
            max_budget_usd: None,
            agents: Vec::new(),
            append_system_prompt: Some("sys".into()),
            denied_tools: Vec::new(),
            session_name: None,
            context_window: None,
            messages: None,
            allowed_mcp_tools: None,
        }
    }

    fn config(mode: Mode, url: &str) -> RouterConfig {
        RouterConfig {
            mode,
            url: url.to_owned(),
            timeout_ms: 1000,
            ..RouterConfig::off()
        }
    }

    fn available(kind: RunnerKind, runner: Arc<dyn CommandRunner>, globs: &[&str]) -> Available {
        Available {
            kind,
            runner,
            default_model: match kind {
                RunnerKind::Claude => "claude-sonnet-5".into(),
                RunnerKind::Codex => "gpt-5.6-terra".into(),
            },
            models: globs.iter().map(|glob| (*glob).to_owned()).collect(),
        }
    }

    /// A routed runner over `inner`, with Codex as an alternate.
    fn routed(
        config: RouterConfig,
        inner: Arc<dyn CommandRunner>,
    ) -> (Arc<dyn CommandRunner>, Arc<dyn CommandRunner>) {
        let codex: Arc<dyn CommandRunner> = Arc::new(crate::runner::FakeCommandRunner::default());
        let router = Arc::new(Router::new(
            config,
            available(RunnerKind::Claude, Arc::clone(&inner), &["claude-*"]),
            vec![available(RunnerKind::Codex, Arc::clone(&codex), &["gpt-*"])],
        ));
        (Arc::new(RoutedRunner { inner, router }), codex)
    }

    fn shape() -> RunShape {
        RunShape {
            permission: Permission::Default,
            classifier_governs_tools: false,
            tool_policy: ToolPolicy::Unrestricted,
            resume: false,
        }
    }

    fn query() -> RouteQuery {
        RouteQuery {
            task: "implement the thing".into(),
            stage: Some("implement".into()),
            item: None,
            resume: false,
        }
    }

    fn advice(runner: &str, model: &str, effort: Option<&str>) -> RouteAdvice {
        RouteAdvice {
            decision_id: "rt_1".into(),
            runner: runner.into(),
            model: model.into(),
            effort: effort.map(str::to_owned),
            tier: None,
            estimated_cost_usd: None,
            rule: None,
        }
    }

    type RouteRow = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );

    async fn route_row(pool: &SqlitePool, run_id: i64) -> RouteRow {
        sqlx::query_as(
            "SELECT model, effort, runner, route_mode, route_decision_id,
                    advised_runner, advised_model, advised_effort
               FROM runs WHERE id = ?",
        )
        .bind(run_id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    // --- configuration (AC1, AC9) ---

    #[test]
    fn no_config_file_means_off_and_fronts_nothing() {
        let config = load_config(Path::new("definitely/not/here/router.yaml"));
        assert_eq!(config.mode, Mode::Off);
        assert!(config.is_off());

        let primary: Arc<dyn CommandRunner> = Arc::new(Probe);
        let fronted = front(
            Arc::clone(&primary),
            &crate::config::ModelsConfig::default(),
            config,
        );
        assert!(Arc::ptr_eq(&primary, &fronted), "off must be the same Arc");
        assert!(fronted.router().is_none());

        let explicit = parse_config("mode: off\n").unwrap();
        assert!(explicit.is_off());
        let boolean = parse_config("mode: false\n").unwrap();
        assert!(boolean.is_off());
    }

    #[test]
    fn a_malformed_or_unknown_config_is_refused() {
        assert!(parse_config("mode: sometimes\n").is_err());
        assert!(parse_config("mode: [shadow\n").is_err());
        assert!(parse_config("mode: shadow\nsurprise: 1\n").is_err());
        assert!(parse_config("mode: shadow\nsurfaces:\n  kitchen: apply\n").is_err());
    }

    #[test]
    fn a_router_off_this_machine_is_refused() {
        for url in [
            "http://10.0.0.5:18733",
            "http://router.example.com",
            "ftp://127.0.0.1:1",
            "http://user@127.0.0.1:1",
            "not a url",
        ] {
            assert!(
                parse_config(&format!("mode: shadow\nurl: {url}\n")).is_err(),
                "{url} must be refused"
            );
        }
        for url in [
            "http://127.0.0.1:18733",
            "http://localhost:9",
            "http://[::1]:9",
        ] {
            assert!(
                parse_config(&format!("mode: shadow\nurl: {url}\n")).is_ok(),
                "{url} must be accepted"
            );
        }
    }

    #[test]
    fn the_timeout_is_clamped_and_surfaces_override_the_global_mode() {
        let config =
            parse_config("mode: shadow\ntimeout_ms: 5\nsurfaces:\n  runs: apply\n  council: off\n")
                .unwrap();
        assert_eq!(config.timeout_ms, MIN_TIMEOUT_MS);
        assert_eq!(config.url, DEFAULT_URL);
        assert_eq!(config.mode_for(Surface::Runs), Mode::Apply);
        assert_eq!(config.mode_for(Surface::Team), Mode::Shadow);
        assert_eq!(config.mode_for(Surface::Council), Mode::Off);
        assert_eq!(
            parse_config("mode: shadow\ntimeout_ms: 999999\n")
                .unwrap()
                .timeout_ms,
            MAX_TIMEOUT_MS
        );
        assert_eq!(
            parse_config("mode: shadow\n").unwrap().timeout_ms,
            DEFAULT_TIMEOUT_MS
        );
    }

    /// `apply` configured for runs is `apply` live: nothing between the file and the launch
    /// quietly degrades it to `shadow`.
    #[test]
    fn apply_on_runs_is_live() {
        let (runner, _) = routed(config(Mode::Apply, DEFAULT_URL), Arc::new(Probe));
        let router = runner.router().unwrap();
        assert_eq!(router.mode_for(Surface::Runs), Mode::Apply);
        assert_eq!(router.runs_mode(), Mode::Apply);
    }

    // --- the wrapper (the faithfulness risk) ---

    #[tokio::test]
    async fn the_fronted_runner_answers_exactly_as_the_runner_it_fronts() {
        let inner: Arc<dyn CommandRunner> = Arc::new(Probe);
        let (fronted, _) = routed(config(Mode::Shadow, DEFAULT_URL), Arc::clone(&inner));

        assert!(fronted.router().is_some());
        assert!(inner.router().is_none());
        for stage in [None, Some("plan"), Some("review")] {
            assert_eq!(fronted.model_for_stage(stage), inner.model_for_stage(stage));
        }
        assert_eq!(
            fronted.authored_prompt(&request()),
            inner.authored_prompt(&request())
        );

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let transcript = Arc::new(std::sync::Mutex::new(String::new()));
        let plain = fronted
            .run_prompt(request(), tx.clone(), Arc::clone(&transcript))
            .await
            .unwrap();
        assert_eq!(plain.stdout, "plain m");

        let fill = Arc::new(std::sync::Mutex::new(None));
        let filled = fronted
            .run_prompt_with_context_fill(
                request(),
                tx.clone(),
                Arc::clone(&transcript),
                Arc::clone(&fill),
            )
            .await
            .unwrap();
        assert_eq!(filled.stdout, "fill");
        assert_eq!(*fill.lock().unwrap(), Some(7));

        let (turns_tx, mut turns_rx) = tokio::sync::mpsc::unbounded_channel();
        let turned = fronted
            .run_prompt_with_turns(request(), tx, transcript, fill, Some(turns_tx))
            .await
            .unwrap();
        assert_eq!(turned.stdout, "turns");
        assert!(matches!(turns_rx.recv().await, Some(TurnEvent::Line(line)) if line == "a turn"));
    }

    // --- eligibility (AC6) ---

    #[test]
    fn a_classifier_governed_run_is_never_offered_codex() {
        let (runner, _) = routed(config(Mode::Shadow, DEFAULT_URL), Arc::new(Probe));
        let router = runner.router().unwrap();

        assert_eq!(
            router.eligible(shape()),
            vec![RunnerKind::Claude, RunnerKind::Codex]
        );
        let governed = RunShape {
            classifier_governs_tools: true,
            permission: Permission::Bypass,
            ..shape()
        };
        let planning = RunShape {
            permission: Permission::Plan,
            ..shape()
        };
        let resumed = RunShape {
            resume: true,
            ..shape()
        };
        let restricted = RunShape {
            tool_policy: ToolPolicy::McpOnly,
            ..shape()
        };
        for ruled_out in [governed, planning, resumed, restricted] {
            assert_eq!(
                router.eligible(ruled_out),
                vec![RunnerKind::Claude],
                "{ruled_out:?}"
            );
            let sent = router.build_request(&query(), &router.eligible(ruled_out));
            assert_eq!(sent.runners, vec!["claude".to_owned()]);
            assert_eq!(sent.models, vec!["claude-*".to_owned()]);
        }
    }

    // --- the request (AC3, AC7) ---

    #[test]
    fn the_request_carries_the_daemons_rules_and_nothing_else() {
        let (runner, _) = routed(config(Mode::Shadow, DEFAULT_URL), Arc::new(Probe));
        let router = runner.router().unwrap();
        let long = "é".repeat(TASK_CHARS + 50);
        let gate = format!("{}END", "x".repeat(3000));
        let sent = router.build_request(
            &RouteQuery {
                task: long,
                stage: Some("implement".into()),
                item: Some(ItemContext {
                    files: vec!["core/src/a.rs".into()],
                    attempt: Some(2),
                    gate_output: Some(gate),
                    failed: vec!["claude-opus-5@high".into()],
                }),
                resume: false,
            },
            &router.eligible(shape()),
        );

        assert_eq!(sent.task.chars().count(), TASK_CHARS);
        let gate_output = sent.gate_output.clone().unwrap();
        assert_eq!(gate_output.chars().count(), GATE_TAIL_CHARS);
        assert!(gate_output.ends_with("END"));
        assert_eq!(sent.runners, vec!["claude", "codex"]);
        assert_eq!(sent.models, vec!["claude-*", "gpt-*"]);
        assert_eq!(sent.failed, vec!["claude-opus-5@high"]);
        assert_eq!(sent.attempt, Some(2));

        let json = serde_json::to_value(&sent).unwrap();
        let known = [
            "task",
            "stage",
            "files",
            "attempt",
            "gate_output",
            "failed",
            "runners",
            "packet",
            "models",
            "exclude",
        ];
        for key in json.as_object().unwrap().keys() {
            assert!(known.contains(&key.as_str()), "{key} is not in the schema");
        }
    }

    #[tokio::test]
    async fn a_retry_names_every_model_that_already_failed_the_item() {
        let pool = pool().await;
        let job_id = sqlx::query(
            "INSERT INTO jobs (project_id, project_root, status, max_items, created_at)
             VALUES ('p', 'C:/p', 'implementing', 3, '2026-09-30T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let first = seed_run(&pool).await;
        sqlx::query("UPDATE runs SET model = 'claude-opus-5', effort = 'high' WHERE id = ?")
            .bind(first)
            .execute(&pool)
            .await
            .unwrap();
        let second = seed_run(&pool).await;
        sqlx::query(
            "UPDATE runs SET model = 'claude-sonnet-5', effort = NULL,
                             route_failed = '[\"claude-opus-5@high\"]' WHERE id = ?",
        )
        .bind(second)
        .execute(&pool)
        .await
        .unwrap();
        let item_id = sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, files, gate_attempts,
                                    gate_output, gate_status, run_id)
             VALUES (?, 0, 'an item', 'gate_failed', '[\"a.rs\"]', 2, 'boom', 'failed', ?)",
        )
        .bind(job_id)
        .bind(second)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let third = seed_run(&pool).await;

        let by_ordinal = item_context(&pool, ItemRef::Ordinal { job_id, ordinal: 0 }, third).await;
        assert_eq!(by_ordinal.attempt, Some(3));
        assert_eq!(by_ordinal.files, vec!["a.rs"]);
        assert_eq!(by_ordinal.gate_output.as_deref(), Some("boom"));
        assert_eq!(
            by_ordinal.failed,
            vec!["claude-opus-5@high", "claude-sonnet-5"]
        );
        assert_eq!(
            item_context(&pool, ItemRef::Id(item_id), third).await,
            by_ordinal
        );

        // Once the item points at the run being routed, its predecessor is gone: nothing is guessed.
        let pointing_here = item_context(&pool, ItemRef::Id(item_id), second).await;
        assert!(pointing_here.failed.is_empty());
    }

    // --- the answer (AC5) ---

    #[test]
    fn an_answer_outside_what_was_sent_is_refused() {
        let (runner, _) = routed(config(Mode::Shadow, DEFAULT_URL), Arc::new(Probe));
        let router = runner.router().unwrap();
        let only_claude = router.build_request(&query(), &[RunnerKind::Claude]);

        assert_eq!(
            router.check_answer(&advice("claude", "claude-opus-5", None), &only_claude),
            Ok(RunnerKind::Claude)
        );
        assert!(
            router
                .check_answer(&advice("codex", "gpt-5.6-terra", None), &only_claude)
                .is_err()
        );
        assert!(
            router
                .check_answer(&advice("claude", "gpt-5.6-terra", None), &only_claude)
                .is_err()
        );
        assert!(
            router
                .check_answer(&advice("ollama", "qwen", None), &only_claude)
                .is_err()
        );
        assert!(
            router
                .check_answer(&advice("claude", "", None), &only_claude)
                .is_err()
        );

        // A glob may name the router's tier rather than the model id.
        let by_tier = RouteAdvice {
            tier: Some("claude-cheap".into()),
            ..advice("claude", "some-model-id", None)
        };
        assert_eq!(
            router.check_answer(&by_tier, &only_claude),
            Ok(RunnerKind::Claude)
        );
    }

    #[test]
    fn globs_match_as_the_router_matches_them() {
        assert!(glob_match("claude-*", "Claude-Sonnet-5"));
        assert!(glob_match("gpt-5.?-terra", "gpt-5.6-terra"));
        assert!(glob_match("*", ""));
        assert!(!glob_match("claude-*", "gpt-5"));
        assert!(!glob_match("sonnet", "claude-sonnet-5"));
    }

    #[test]
    fn the_effort_is_held_under_the_speed_ceiling() {
        assert_eq!(effort_ceiling(Speed::Normal), "medium");
        assert_eq!(effort_ceiling(Speed::Fast), "low");
        assert_eq!(effort_ceiling(Speed::Thorough), "high");
        assert_eq!(clamp_effort("low", "medium"), "low");
        assert_eq!(clamp_effort("medium", "medium"), "medium");
        assert_eq!(clamp_effort("max", "medium"), "medium");
        assert_eq!(clamp_effort("ultra", "high"), "high");

        let env = vec![(crate::speed::SPEED_VAR.to_owned(), "fast".to_owned())];
        assert_eq!(speed_of(&env), Speed::Fast);
        assert_eq!(speed_of(&[]), Speed::Normal);
    }

    #[test]
    fn apply_uses_the_advised_model_and_effort_and_shadow_never_does() {
        let configured = Choice {
            kind: RunnerKind::Claude,
            model: None,
            effort: None,
        };
        let advised = advice("codex", "gpt-5.6-terra", Some("xhigh"));

        let applied = choose(
            Mode::Apply,
            configured.clone(),
            Some((&advised, RunnerKind::Codex)),
            "high",
        );
        assert_eq!(
            applied,
            Choice {
                kind: RunnerKind::Codex,
                model: Some("gpt-5.6-terra".into()),
                effort: Some("high".into()),
            }
        );
        assert_eq!(
            choose(
                Mode::Shadow,
                configured.clone(),
                Some((&advised, RunnerKind::Codex)),
                "high"
            ),
            configured
        );
        assert_eq!(
            choose(Mode::Apply, configured.clone(), None, "high"),
            configured
        );
    }

    // --- resolve (AC1, AC2, AC4) ---

    #[tokio::test]
    async fn with_routing_off_nothing_is_asked_and_nothing_written() {
        let pool = pool().await;
        let run_id = seed_run(&pool).await;
        let (url, mut received) =
            stub_router(200, serde_json::to_value(serde_json::json!({})).unwrap()).await;
        let runner: Arc<dyn CommandRunner> = Arc::new(Probe);

        let resolved = resolve(
            &pool,
            run_id,
            Arc::clone(&runner),
            Some("plan-model".into()),
            None,
            Some(query()),
            shape(),
            Speed::Normal,
        )
        .await;

        assert!(Arc::ptr_eq(&resolved.runner, &runner));
        assert_eq!(resolved.model.as_deref(), Some("plan-model"));
        assert!(resolved.router.is_none());
        assert!(received.try_recv().is_err(), "nothing may be asked: {url}");
        assert_eq!(route_row(&pool, run_id).await, Default::default());
    }

    #[tokio::test]
    async fn shadow_records_the_advice_and_keeps_the_configured_model() {
        let pool = pool().await;
        let run_id = seed_run(&pool).await;
        let (url, mut received) = stub_router(
            200,
            serde_json::json!({"decision_id": "rt_9", "tier": "opus", "runner": "claude",
                               "model": "claude-opus-5", "effort": "high", "success": 0.9}),
        )
        .await;
        let (runner, _) = routed(config(Mode::Shadow, &url), Arc::new(Probe));

        let resolved = resolve(
            &pool,
            run_id,
            Arc::clone(&runner),
            None,
            None,
            Some(query()),
            shape(),
            Speed::Normal,
        )
        .await;

        assert!(Arc::ptr_eq(&resolved.runner, &runner));
        assert_eq!(
            resolved.model, None,
            "shadow launches exactly as configured"
        );
        assert_eq!(resolved.effort, None);
        assert_eq!(resolved.decision_id.as_deref(), Some("rt_9"));
        assert_eq!(
            route_row(&pool, run_id).await,
            (
                Some("claude-sonnet-5".into()),
                None,
                Some("claude".into()),
                Some("shadow".into()),
                Some("rt_9".into()),
                Some("claude".into()),
                Some("claude-opus-5".into()),
                Some("high".into()),
            )
        );
        let sent = received.recv().await.unwrap();
        assert_eq!(sent["runners"], serde_json::json!(["claude", "codex"]));
        assert_eq!(sent["models"], serde_json::json!(["claude-*", "gpt-*"]));
        assert_eq!(sent["task"], "implement the thing");
    }

    /// The advice launches on its own runner, its effort held under the speed's ceiling, and the
    /// row records what launched beside what was advised.
    #[tokio::test]
    async fn apply_launches_the_advice_on_its_runner_with_its_effort_clamped() {
        let pool = pool().await;
        let run_id = seed_run(&pool).await;
        let (url, _received) = stub_router(
            200,
            serde_json::json!({"decision_id": "rt_2", "runner": "codex",
                               "model": "gpt-5.6-terra", "effort": "max"}),
        )
        .await;
        let (runner, codex) = routed(config(Mode::Apply, &url), Arc::new(Probe));
        let router = runner.router().unwrap();

        // The pure half, exactly as `resolve` composes it once the surface is live.
        let sent = router.build_request(&query(), &router.eligible(shape()));
        let answer = router.client.route(&sent).await.unwrap();
        let kind = router.check_answer(&answer, &sent).unwrap();
        let choice = choose(
            Mode::Apply,
            Choice {
                kind: RunnerKind::Claude,
                model: None,
                effort: None,
            },
            Some((&answer, kind)),
            effort_ceiling(Speed::Normal),
        );
        assert_eq!(choice.kind, RunnerKind::Codex);
        assert_eq!(choice.model.as_deref(), Some("gpt-5.6-terra"));
        assert_eq!(choice.effort.as_deref(), Some("medium"));
        assert!(Arc::ptr_eq(
            &router.available(choice.kind).unwrap().runner,
            &codex
        ));

        // And through `resolve`, which records it under the mode that is live for runs.
        let resolved = resolve(
            &pool,
            run_id,
            Arc::clone(&runner),
            None,
            None,
            Some(query()),
            shape(),
            Speed::Normal,
        )
        .await;
        assert!(Arc::ptr_eq(&resolved.runner, &codex));
        assert_eq!(resolved.model.as_deref(), Some("gpt-5.6-terra"));
        assert_eq!(resolved.effort.as_deref(), Some("medium"));
        assert_eq!(resolved.decision_id.as_deref(), Some("rt_2"));
        assert_eq!(
            route_row(&pool, run_id).await,
            (
                Some("gpt-5.6-terra".into()),
                Some("medium".into()),
                Some("codex".into()),
                Some("apply".into()),
                Some("rt_2".into()),
                Some("codex".into()),
                Some("gpt-5.6-terra".into()),
                Some("max".into()),
            )
        );
    }

    /// AC6 through the whole path: a run the classifier governs is offered its primary only, and
    /// a router that names Codex anyway is ignored — in `apply` the run still launches on Claude.
    #[tokio::test]
    async fn apply_never_moves_a_governed_run_to_codex() {
        let pool = pool().await;
        let run_id = seed_run(&pool).await;
        let (url, mut received) = stub_router(
            200,
            serde_json::json!({"decision_id": "rt_3", "runner": "codex",
                               "model": "gpt-5.6-terra", "effort": "low"}),
        )
        .await;
        let (runner, _codex) = routed(config(Mode::Apply, &url), Arc::new(Probe));

        let resolved = resolve(
            &pool,
            run_id,
            Arc::clone(&runner),
            Some("claude-sonnet-5".into()),
            None,
            Some(query()),
            RunShape {
                classifier_governs_tools: true,
                ..shape()
            },
            Speed::Normal,
        )
        .await;

        assert!(Arc::ptr_eq(&resolved.runner, &runner));
        assert_eq!(resolved.model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(resolved.decision_id, None);
        let sent = received.recv().await.unwrap();
        assert_eq!(sent["runners"], serde_json::json!(["claude"]));
    }

    async fn assert_falls_back(url: &str, started_within: Duration) {
        for mode in [Mode::Shadow, Mode::Apply] {
            assert_falls_back_in(mode, url, started_within).await;
        }
    }

    async fn assert_falls_back_in(mode: Mode, url: &str, started_within: Duration) {
        let pool = pool().await;
        let run_id = seed_run(&pool).await;
        let mut config = config(mode, url);
        config.timeout_ms = 150;
        let (runner, _) = routed(config, Arc::new(Probe));
        let started = std::time::Instant::now();

        let resolved = resolve(
            &pool,
            run_id,
            Arc::clone(&runner),
            Some("claude-sonnet-5".into()),
            Some("low".into()),
            Some(query()),
            shape(),
            Speed::Normal,
        )
        .await;

        assert!(started.elapsed() < started_within, "{url}");
        assert!(Arc::ptr_eq(&resolved.runner, &runner));
        assert_eq!(resolved.model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(resolved.effort.as_deref(), Some("low"));
        assert_eq!(resolved.decision_id, None);
        let row = route_row(&pool, run_id).await;
        assert_eq!(row.0.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(row.3.as_deref(), Some(mode.as_str()));
        assert_eq!(row.4, None, "no decision to report against");
        assert_eq!(row.6, None);
    }

    #[tokio::test]
    async fn a_router_that_is_down_leaves_the_configured_model() {
        assert_falls_back(&dead_address().await, Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn a_router_slower_than_the_timeout_leaves_the_configured_model() {
        let url = slow_router(
            Duration::from_secs(5),
            serde_json::json!({"decision_id": "rt_late", "runner": "claude",
                               "model": "claude-opus-5"}),
        )
        .await;
        assert_falls_back(&url, Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn a_400_or_a_422_leaves_the_configured_model() {
        let (bad, _rx) = stub_router(400, serde_json::json!({"error": "unknown fields"})).await;
        assert_falls_back(&bad, Duration::from_secs(2)).await;
        let (none, _rx) = stub_router(422, serde_json::json!({"error": "no tier"})).await;
        assert_falls_back(&none, Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn an_answer_outside_the_set_leaves_the_configured_model() {
        let (url, _rx) = stub_router(
            200,
            serde_json::json!({"decision_id": "rt_x", "runner": "ollama", "model": "qwen"}),
        )
        .await;
        assert_falls_back(&url, Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn a_runner_held_by_the_quota_brake_is_not_offered() {
        let pool = pool().await;
        let run_id = seed_run(&pool).await;
        crate::quota::test_support::arm(&pool, true, 50, 50).await;
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO quota_readings (provider, window_name, used_fraction, fidelity, read_at)
             VALUES ('codex', '5h', 0.9, 'official', ?), ('claude', '5h', 0.1, 'official', ?)",
        )
        .bind(&now)
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();
        let (url, mut received) = stub_router(
            200,
            serde_json::json!({"decision_id": "rt_q", "runner": "claude", "model": "claude-opus-5"}),
        )
        .await;
        let (runner, _) = routed(config(Mode::Shadow, &url), Arc::new(Probe));

        resolve(
            &pool,
            run_id,
            runner,
            None,
            None,
            Some(query()),
            shape(),
            Speed::Normal,
        )
        .await;

        let sent = received.recv().await.unwrap();
        assert_eq!(sent["runners"], serde_json::json!(["claude"]));
        assert_eq!(sent["models"], serde_json::json!(["claude-*"]));
    }

    #[test]
    fn head_and_tail_cut_on_character_boundaries() {
        assert_eq!(head("ééé", 2), "éé");
        assert_eq!(head("ab", 5), "ab");
        assert_eq!(tail("ééé", 2), "éé");
        assert_eq!(tail("ab", 5), "ab");
        assert_eq!(failed_label("m", Some("high")), "m@high");
        assert_eq!(failed_label("m", None), "m");
    }
}

// ---- outcomes (P2) ----
//
// What became of a routed run, told back to the router so it can learn from it. Every report is
// fire-and-forget: it is spawned, never awaited, and a failure only warns — a router that is down
// must never cost a run, a gate or a job tick a single second. Only a run that holds a decision id
// reports; a run launched with routing off, or on a fallback, has nothing to report against.

pub use crate::router_client::Outcome;

/// The router's word for a gate's verdict. `Errored` is the gate not measuring, which says nothing
/// about the model, so it is `error` rather than `fail`.
pub fn outcome_of_gate(outcome: &crate::gate::GateOutcome) -> Outcome {
    match outcome {
        crate::gate::GateOutcome::Passed => Outcome::Pass,
        crate::gate::GateOutcome::Failed { .. } => Outcome::Fail,
        crate::gate::GateOutcome::Errored { .. } => Outcome::Error,
    }
}

/// What a run that ended `failed` without a gate verdict says about its infrastructure: a 429 is
/// `rate_limited`, another API error a retry could get past is `error`. Anything else ended on the
/// work itself and is left unreported — only a gate may say `fail`.
pub fn outcome_of_run_end(stdout: &str) -> Option<Outcome> {
    if crate::runner::failed_on_rate_limit(stdout) {
        Some(Outcome::RateLimited)
    } else if crate::runner::failed_on_a_transient_api_error(stdout) {
        Some(Outcome::Error)
    } else {
        None
    }
}

/// Reports `outcome` for `decision_id` on a task of its own and returns at once.
pub fn report_detached(router: Arc<Router>, decision_id: String, outcome: Outcome) {
    tokio::spawn(async move {
        if let Err(error) = router.client.report(&decision_id, outcome).await {
            tracing::warn!(
                %decision_id,
                outcome = outcome.as_str(),
                %error,
                "could not report a routed run's outcome to the llm-router"
            );
        }
    });
}

/// Reports a sequential job item's gate verdict against the decision of the run the item points
/// at. One DB read and a spawn: it cannot block on the network, and an unreadable row only warns.
pub async fn report_item_gate(
    pool: &SqlitePool,
    router: Arc<Router>,
    job_id: i64,
    ordinal: i64,
    outcome: &crate::gate::GateOutcome,
) {
    let decision: Option<Option<String>> = sqlx::query_scalar(
        "SELECT r.route_decision_id FROM job_items i JOIN runs r ON r.id = i.run_id
          WHERE i.job_id = ? AND i.ordinal = ?",
    )
    .bind(job_id)
    .bind(ordinal)
    .fetch_optional(pool)
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(job_id, ordinal, %error, "could not read an item's route decision");
        None
    });
    if let Some(decision_id) = decision.flatten().filter(|id| !id.is_empty()) {
        report_detached(router, decision_id, outcome_of_gate(outcome));
    }
}

#[cfg(test)]
mod outcome_tests {
    use super::*;
    use crate::gate::GateOutcome;
    use crate::router_client::test_support::{dead_address, outcome_router, slow_router};

    async fn pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    fn router_at(url: &str) -> Arc<Router> {
        let runner: Arc<dyn CommandRunner> = Arc::new(crate::runner::FakeCommandRunner::default());
        Arc::new(Router::new(
            RouterConfig {
                mode: Mode::Shadow,
                url: url.to_owned(),
                timeout_ms: 1000,
                ..RouterConfig::off()
            },
            Available {
                kind: RunnerKind::Claude,
                runner,
                default_model: "claude-sonnet-5".into(),
                models: vec!["claude-*".into()],
            },
            Vec::new(),
        ))
    }

    #[test]
    fn gate_verdicts_map_to_router_outcomes() {
        assert_eq!(outcome_of_gate(&GateOutcome::Passed), Outcome::Pass);
        assert_eq!(
            outcome_of_gate(&GateOutcome::Failed {
                exit_code: 1,
                output: "red".into()
            }),
            Outcome::Fail
        );
        assert_eq!(
            outcome_of_gate(&GateOutcome::Errored {
                reason: "no binary".into()
            }),
            Outcome::Error
        );
    }

    #[test]
    fn a_run_end_reports_only_what_the_infrastructure_did() {
        let ended_on = |status: &str| {
            format!(
                "{{\"type\":\"result\",\"is_error\":true,\"terminal_reason\":\"api_error\",\
                 \"api_error_status\":{status},\"result\":\"API Error\"}}"
            )
        };
        assert_eq!(
            outcome_of_run_end(&ended_on("429")),
            Some(Outcome::RateLimited)
        );
        assert_eq!(outcome_of_run_end(&ended_on("529")), Some(Outcome::Error));
        assert_eq!(outcome_of_run_end(&ended_on("null")), Some(Outcome::Error));
        assert_eq!(outcome_of_run_end(&ended_on("400")), None);
        assert_eq!(
            outcome_of_run_end(
                r#"{"type":"result","is_error":true,"terminal_reason":"max_turns"}"#
            ),
            None
        );
        assert_eq!(outcome_of_run_end(""), None);
    }

    /// Returns before the router has answered, and a router that is not there costs nothing.
    #[tokio::test]
    async fn a_detached_report_never_waits_on_the_router() {
        let slow = slow_router(Duration::from_secs(5), serde_json::json!({})).await;
        let dead = dead_address().await;
        let started = std::time::Instant::now();
        report_detached(router_at(&slow), "rt_1".into(), Outcome::Pass);
        report_detached(router_at(&dead), "rt_1".into(), Outcome::Error);
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    async fn seed_item(pool: &SqlitePool, decision: Option<&str>) -> i64 {
        let job_id = sqlx::query(
            "INSERT INTO jobs (project_id, project_root, status, max_items, created_at)
             VALUES ('p', 'C:/p', 'implementing', 3, '2026-09-30T00:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let run_id = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at, route_decision_id)
             VALUES ('p', 'x', 'completed', 'worktree', '2026-09-30T00:00:00Z', ?)",
        )
        .bind(decision)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, run_id)
             VALUES (?, 0, 'an item', 'gating', ?)",
        )
        .bind(job_id)
        .bind(run_id)
        .execute(pool)
        .await
        .unwrap();
        job_id
    }

    #[tokio::test]
    async fn a_settled_item_gate_reports_its_decision() {
        let pool = pool().await;
        let (url, mut received) = outcome_router(200).await;
        let job_id = seed_item(&pool, Some("rt_1")).await;

        report_item_gate(
            &pool,
            router_at(&url),
            job_id,
            0,
            &GateOutcome::Failed {
                exit_code: 2,
                output: "red".into(),
            },
        )
        .await;

        let (id, body) = tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .expect("reported within 2s")
            .unwrap();
        assert_eq!(id, "rt_1");
        assert_eq!(body, serde_json::json!({"status": "fail"}));
    }

    /// A run launched unrouted, or on a fallback, has no decision and reports nothing.
    #[tokio::test]
    async fn an_item_whose_run_holds_no_decision_reports_nothing() {
        let pool = pool().await;
        let (url, mut received) = outcome_router(200).await;
        let job_id = seed_item(&pool, None).await;

        report_item_gate(&pool, router_at(&url), job_id, 0, &GateOutcome::Passed).await;
        report_item_gate(&pool, router_at(&url), job_id + 99, 0, &GateOutcome::Passed).await;

        assert!(
            tokio::time::timeout(Duration::from_millis(300), received.recv())
                .await
                .is_err()
        );
    }
}
