//! The engines a node's pin may name and the runner that fronts the primary: `Engines`,
//! `RoutedRunner`, the pin resolution and the construction of both (`front`). Re-exported from the
//! parent, so the public paths stay `route_advice::{Engines, RoutedRunner, front, front_with, ...}`.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::SqlitePool;
use tokio::sync::mpsc::UnboundedSender;

use super::{
    Available, EnginePinRoute, Resolved, Router, RouterConfig, RunShape, RunnerKind, codex_refusal,
    default_model,
};
use crate::execution::runner::{CommandRunner, RunOutcome, RunRequest, TurnEvent};

/// The runner a launch gets when its agent's Codex settings must hold: a fresh `CodexCliRunner`
/// pinning them, never the shared instance. Both the pin path and the routed path build it here.
pub(super) fn codex_runner(
    model: String,
    sandbox: &'static str,
    web_search: Option<&'static str>,
) -> Arc<dyn CommandRunner> {
    Arc::new(crate::execution::runner::CodexCliRunner {
        model,
        sandbox_mode: Some(sandbox),
        web_search,
    })
}

/// A pinned launch: the runner the pin names (the wrapper `runner` when it is the primary), its
/// model and effort, written in one UPDATE. An engine the daemon does not hold is refused, and so is
/// a Codex pin on a run of a shape Codex cannot take (the sentence names why, instead of leaving it
/// to the runner's generic error).
pub(super) async fn resolve_pin(
    pool: &SqlitePool,
    run_id: i64,
    runner: Arc<dyn CommandRunner>,
    router: Option<Arc<Router>>,
    engines: &Engines,
    pin: EnginePinRoute,
    shape: RunShape,
) -> Resolved {
    let Some(available) = engines.available(pin.runner) else {
        return Resolved {
            runner,
            model: pin.model,
            effort: pin.effort,
            decision_id: None,
            router: None,
            refused: Some(format!(
                "this node is pinned to the {} engine, which this daemon cannot launch",
                pin.runner.as_str()
            )),
        };
    };
    if pin.runner == RunnerKind::Codex
        && let Some(why) = codex_refusal(shape)
    {
        return Resolved {
            runner,
            model: pin.model,
            effort: pin.effort,
            decision_id: None,
            router: None,
            refused: Some(format!(
                "this node is pinned to the codex engine, but {why}"
            )),
        };
    }
    let effective_model = pin
        .model
        .clone()
        .unwrap_or_else(|| available.default_model.clone());
    if let Err(error) =
        sqlx::query("UPDATE runs SET model = ?, effort = ?, runner = ? WHERE id = ?")
            .bind(&effective_model)
            .bind(&pin.effort)
            .bind(pin.runner.as_str())
            .bind(run_id)
            .execute(pool)
            .await
    {
        tracing::warn!(run_id, %error, "could not record a pinned run's engine");
    }
    let chosen_runner: Arc<dyn CommandRunner> = match (pin.runner, pin.sandbox) {
        // The agent's sandbox is the barrier on Codex, so the launch gets a runner that pins it
        // rather than the shared instance, which pins nothing.
        (RunnerKind::Codex, Some(sandbox)) => {
            codex_runner(available.default_model.clone(), sandbox, pin.web_search)
        }
        _ if pin.runner == engines.primary.kind => runner,
        _ => Arc::clone(&available.runner),
    };
    Resolved {
        runner: chosen_runner,
        model: pin.model,
        effort: pin.effort,
        decision_id: None,
        router,
        refused: None,
    }
}

/// The primary runner, fronted by the router — or the very same `Arc` when every surface is off and
/// no other engine is known.
///
/// The only call `main.rs` makes. It builds the alternates `config.runners` names from the models
/// file, exactly as `main.rs` builds the primary, and never the primary's own kind twice. The engines
/// a node may be pinned to are wider than that: every kind `config.runners` names plus every CLI
/// installed on this machine, because a pin is an explicit choice and `runners:` is only the
/// router's consent to move a routed run.
pub fn front(
    primary: Arc<dyn CommandRunner>,
    models: &crate::config::ModelsConfig,
    config: RouterConfig,
) -> Arc<dyn CommandRunner> {
    front_with(primary, models, config, &installed_engines())
}

/// Which engines' CLIs are installed on this machine, by resolving their programs (nothing is run).
pub fn installed_engines() -> Vec<RunnerKind> {
    [RunnerKind::Claude, RunnerKind::Codex]
        .into_iter()
        .filter(|kind| match kind {
            RunnerKind::Claude => {
                let configured =
                    std::env::var_os("NUCLEOS_CLAUDE_BIN").unwrap_or_else(|| "claude".into());
                crate::health::resolve_program(&configured).is_some()
            }
            RunnerKind::Codex => crate::execution::runner::codex_program().is_some(),
        })
        .collect()
}

/// The runner a kind launches, built from the models file as `main.rs` builds the primary.
fn engine_runner(models: &crate::config::ModelsConfig, kind: RunnerKind) -> Arc<dyn CommandRunner> {
    match kind {
        RunnerKind::Claude => Arc::new(crate::execution::runner::ClaudeCliRunner {
            model: models.claude_model.clone(),
            plan_model: models.plan_model.clone(),
            review_model: models.review_model.clone(),
            resolve_model: models.resolve_model.clone(),
            resolve_effort: models.resolve_effort.clone(),
        }),
        RunnerKind::Codex => Arc::new(crate::execution::runner::CodexCliRunner {
            model: models.codex_model.clone(),
            sandbox_mode: None,
            web_search: None,
        }),
    }
}

/// `front` with the installed engines given, so a test does not depend on this machine's PATH.
pub fn front_with(
    primary: Arc<dyn CommandRunner>,
    models: &crate::config::ModelsConfig,
    config: RouterConfig,
    installed: &[RunnerKind],
) -> Arc<dyn CommandRunner> {
    let primary_kind = RunnerKind::parse(models.active_runner()).unwrap_or(RunnerKind::Claude);
    let make = |kind: RunnerKind, runner: Arc<dyn CommandRunner>| Available {
        kind,
        runner,
        default_model: default_model(models, kind),
        models: allowed_models(models, &config, kind),
    };
    // What the router may route to: only what `config.runners` names, never widened.
    let mut alternates: Vec<Available> = Vec::new();
    for name in &config.runners {
        let Some(kind) = RunnerKind::parse(name) else {
            tracing::warn!(%name, "router config names a runner this daemon cannot launch; ignoring it");
            continue;
        };
        if kind == primary_kind || alternates.iter().any(|available| available.kind == kind) {
            continue;
        }
        alternates.push(make(kind, engine_runner(models, kind)));
    }
    // What a pin may name: the router's alternates plus every installed CLI.
    let mut others: Vec<Available> = alternates
        .iter()
        .map(|alternate| make(alternate.kind, Arc::clone(&alternate.runner)))
        .collect();
    for kind in installed {
        if *kind != primary_kind && !others.iter().any(|available| available.kind == *kind) {
            others.push(make(*kind, engine_runner(models, *kind)));
        }
    }
    let engines = Arc::new(Engines {
        primary: make(primary_kind, Arc::clone(&primary)),
        others,
    });
    if config.is_off() {
        if engines.others.is_empty() {
            return primary;
        }
        return Arc::new(RoutedRunner {
            inner: primary,
            router: None,
            engines: Some(engines),
        });
    }
    let primary_available = make(primary_kind, Arc::clone(&primary));
    tracing::info!(
        mode = config.mode.as_str(),
        url = %config.url,
        "llm-router advice is on"
    );
    let model_efforts = models
        .assistant_choices
        .iter()
        .filter(|choice| choice.brain == "cloud")
        .map(|choice| (choice.id.clone(), choice.efforts.clone()))
        .collect();
    let router = Arc::new(
        Router::new(config, primary_available, alternates).with_model_efforts(model_efforts),
    );
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let checking = Arc::clone(&router);
        handle.spawn(async move { checking.check_targets().await });
    }
    Arc::new(RoutedRunner {
        inner: primary,
        router: Some(router),
        engines: Some(engines),
    })
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
    /// `None` when routing is off but the daemon still holds engines a node may be pinned to.
    pub router: Option<Arc<Router>>,
    pub engines: Option<Arc<Engines>>,
}

impl RoutedRunner {
    /// A runner fronted by `router`, whose engines are exactly the router's own runners.
    pub fn routed(inner: Arc<dyn CommandRunner>, router: Arc<Router>) -> Self {
        let engines = Arc::new(Engines::from_router(&router));
        Self {
            inner,
            router: Some(router),
            engines: Some(engines),
        }
    }
}

/// The engines a node's pin may name, independent of whether the router is on. A pin is an explicit
/// choice, so it needs no `runners:` consent; the router's own alternates stay what `config.runners`
/// names.
pub struct Engines {
    /// The runner the daemon launches by default.
    pub primary: Available,
    /// Every other engine: named in `config.runners` or installed on this machine.
    pub others: Vec<Available>,
}

impl Engines {
    /// The engine of `kind`, the primary included.
    pub fn available(&self, kind: RunnerKind) -> Option<&Available> {
        std::iter::once(&self.primary)
            .chain(self.others.iter())
            .find(|available| available.kind == kind)
    }

    fn from_router(router: &Router) -> Self {
        let copy = |available: &Available| Available {
            kind: available.kind,
            runner: Arc::clone(&available.runner),
            default_model: available.default_model.clone(),
            models: available.models.clone(),
        };
        Self {
            primary: copy(&router.primary),
            others: router.alternates.iter().map(copy).collect(),
        }
    }
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

    fn effort_for_stage(&self, stage: Option<&str>) -> Option<String> {
        self.inner.effort_for_stage(stage)
    }

    fn authored_prompt(
        &self,
        request: &RunRequest,
    ) -> Option<crate::prompt_budget::AuthoredPrompt> {
        self.inner.authored_prompt(request)
    }

    fn honours_max_budget(&self) -> bool {
        self.inner.honours_max_budget()
    }

    fn reports_model_request(&self) -> bool {
        self.inner.reports_model_request()
    }

    fn router(&self) -> Option<Arc<Router>> {
        self.router.clone()
    }

    fn engines(&self) -> Option<Arc<Engines>> {
        self.engines.clone()
    }
}
