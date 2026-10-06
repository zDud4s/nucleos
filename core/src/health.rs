//! §spec pilar-de-browser
//!
//! Bounded, credential-safe readiness readout for the protected HTTP API.
//!
//! Sidecar entries report liveness: the supervisor publishes whether it started each child and
//! whether it has since seen it exit, and these rows read that. One limit remains, and it is worth
//! stating precisely because the previous one was overstated in the other direction — a child that
//! is running but wedged reads `ok`, because the supervisor watches processes, not progress.
//!
//! `cli_binary` and `voice_transcriber` name no resident process, so for them "alive" can only mean
//! the program runs on this machine. They execute it, behind a cache: the shell polls this endpoint
//! every three seconds and the whole readout is budgeted at one second, so running a CLI on the
//! request path would flap `down` on an install that works. See [`exec_probe`].
//!
//! A disabled subsystem never drags the aggregate down. An unconfigured optional pillar is not a
//! fault, and treating it as one teaches readers to ignore the readout when it matters.

use serde::Serialize;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::sidecar::Liveness;
use crate::state::AppState;
use crate::worktree;

const PROBE_TIMEOUT: Duration = Duration::from_millis(250);
const SUBSYSTEM_TIMEOUT: Duration = Duration::from_millis(500);
const AGGREGATE_TIMEOUT: Duration = Duration::from_secs(1);
const LOW_DISK_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const DAEMON_TOKEN_KEY: &str = "daemon-token";
const TELEGRAM_TOKEN_KEY: &str = "telegram-token";
/// How long an exec verdict is trusted before a refresh is kicked off behind the readout.
const EXEC_CACHE_TTL: Duration = Duration::from_secs(60);
/// A generous ceiling for the background exec. Nobody waits on it, so it can afford to be patient
/// with a CLI that takes its time starting — which is exactly what the readout itself cannot do.
const EXEC_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// A small, stable verdict vocabulary shared by the aggregate and every subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Ok,
    Degraded,
    Down,
    Disabled,
}

/// A closed diagnostic vocabulary. It intentionally carries no error text: network and IMAP
/// errors can embed credential-bearing URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureCategory {
    Timeout,
    NotConfigured,
    Unreachable,
    PermissionDenied,
    Missing,
    /// Configured, and the process is not up. Distinct from `Missing`, which is about a file that
    /// is not there, and from `NotConfigured`, which is about a pillar nobody asked for.
    NotRunning,
    LowDiskSpace,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubsystemReadout {
    pub name: &'static str,
    pub status: HealthState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<FailureCategory>,
    /// Named tallies a row chooses to show beside its state; absent for the rows that have none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counts: Option<std::collections::BTreeMap<&'static str, i64>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthReadout {
    pub status: HealthState,
    pub subsystems: Vec<SubsystemReadout>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl HealthReadout {
    /// A degraded or down aggregate is the health signal's breach; disabled is not a breach.
    pub fn is_breach(&self) -> bool {
        matches!(self.status, HealthState::Degraded | HealthState::Down)
    }
}

/// The feed kind a breached health signal is recorded under.
pub const BREACH_INTENT_KIND: &str = "health_breach_intent";

/// Records one breached health signal without starting any autonomous work.
///
/// This is deliberately a seam rather than a call from [`readout`]: health polling has no project
/// rules, and wiring this recorder into a trigger is an owner-approved follow-up. When enabled by
/// the caller, one line is written to that project's feed, under [`BREACH_INTENT_KIND`].
///
/// **The feed, and no longer `.ai/local/ledgers/intents.jsonl`.** That ledger is the AI dev
/// workflow's, in the project's own `.ai/` folder, and the product has no business writing into it
/// — the same line `project_state.rs` draws for the rules file. The feed is the daemon's own record
/// of what happened to a project, it is where a person already looks, and it needed no new table.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn record_breach_intent(
    pool: &SqlitePool,
    project_id: &str,
    rules: &crate::config::AutopilotRules,
    readout: &HealthReadout,
) -> sqlx::Result<bool> {
    if !rules.health_breach_intent || !readout.is_breach() {
        return Ok(false);
    }

    let problem = format!(
        "Health signal breached: status={}; {}",
        health_state_name(readout.status),
        readout
            .subsystems
            .iter()
            .filter(|entry| matches!(entry.status, HealthState::Degraded | HealthState::Down))
            .map(|entry| {
                format!(
                    "{} ({})",
                    entry.name,
                    entry.reason.map(failure_category_name).unwrap_or("unknown")
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    );
    let summary = format!(
        "{problem}. Review it before taking action; this is a record only, and nothing was \
         queued, started or sent for approval."
    );
    crate::feed::append(
        pool,
        Some(project_id),
        BREACH_INTENT_KIND,
        &summary,
        None,
        None,
    )
    .await?;
    Ok(true)
}

fn health_state_name(state: HealthState) -> &'static str {
    match state {
        HealthState::Ok => "ok",
        HealthState::Degraded => "degraded",
        HealthState::Down => "down",
        HealthState::Disabled => "disabled",
    }
}

fn failure_category_name(category: FailureCategory) -> &'static str {
    match category {
        FailureCategory::Timeout => "timeout",
        FailureCategory::NotConfigured => "not-configured",
        FailureCategory::Unreachable => "unreachable",
        FailureCategory::PermissionDenied => "permission-denied",
        FailureCategory::Missing => "missing",
        FailureCategory::NotRunning => "not-running",
        FailureCategory::LowDiskSpace => "low-disk-space",
        FailureCategory::Unknown => "unknown",
    }
}

impl HealthReadout {
    fn aggregate_timeout() -> Self {
        Self {
            status: HealthState::Down,
            subsystems: vec![SubsystemReadout::down(
                "aggregate",
                FailureCategory::Timeout,
            )],
        }
    }
}

impl SubsystemReadout {
    fn ok(name: &'static str) -> Self {
        Self {
            name,
            status: HealthState::Ok,
            reason: None,
            counts: None,
        }
    }

    fn degraded(name: &'static str, reason: FailureCategory) -> Self {
        Self {
            name,
            status: HealthState::Degraded,
            reason: Some(reason),
            counts: None,
        }
    }

    fn down(name: &'static str, reason: FailureCategory) -> Self {
        Self {
            name,
            status: HealthState::Down,
            reason: Some(reason),
            counts: None,
        }
    }

    fn disabled(name: &'static str, reason: FailureCategory) -> Self {
        Self {
            name,
            status: HealthState::Disabled,
            reason: Some(reason),
            counts: None,
        }
    }
}

/// Collects independent probes under per-probe, per-subsystem, and aggregate budgets.
pub async fn readout(state: AppState) -> HealthReadout {
    match tokio::time::timeout(AGGREGATE_TIMEOUT, collect_readout(state)).await {
        Ok(readout) => readout,
        Err(_) => HealthReadout::aggregate_timeout(),
    }
}

async fn collect_readout(state: AppState) -> HealthReadout {
    let email_enabled = state.email.enabled;
    let web_enabled = state.web.enabled;
    let browser_enabled = state.browser.enabled;
    let github_asked_for = state.github.enabled && state.github.configured;
    let github_binary = state.github.binary.clone();
    let stt_command = state.voice.stt_command.clone();
    let voice_armed = probes_a_program(state.voice.armed, &stt_command);
    // `speaker.is_some()` and not `!tts_command.is_empty()`: `speaker_for` is the one place that
    // decides whether a command becomes a capability, and a probe that re-derives that condition is a
    // second opinion about it. The transcriber probe learned this the hard way with `split_command`.
    let voice_speaks = probes_a_program(state.voice.speaker.is_some(), &state.voice.tts_command);
    let tts_command = state.voice.tts_command.clone();
    let (
        pool,
        cli,
        credentials,
        disk,
        echo,
        telegram,
        email,
        web,
        browser,
        quota,
        voice,
        speaker,
        github,
        hook,
        router,
        devtime,
        distiller,
    ) = tokio::join!(
        run_subsystem("sqlite_pool", pool_probe(state.pool.clone())),
        run_subsystem("cli_binary", cli_probe()),
        run_subsystem("credential_manager", credential_manager_probe()),
        run_subsystem("worktree_disk", disk_probe()),
        run_subsystem(
            "echo_sidecar",
            sidecar_probe("echo_sidecar", crate::sidecar::ECHO, true),
        ),
        run_subsystem("telegram_sidecar", telegram_sidecar_probe()),
        run_subsystem(
            "email_sidecar",
            sidecar_probe("email_sidecar", crate::sidecar::EMAIL, email_enabled),
        ),
        run_subsystem(
            "web_sidecar",
            sidecar_probe("web_sidecar", crate::sidecar::WEB, web_enabled),
        ),
        // Spec §9.4 asks for three states rather than one — Chromium downloaded, sidecar running,
        // browser reachable — and this is the second of the three. The first belongs to the sidecar,
        // which is the only process that knows where the binary is, and it reports it by refusing to
        // start with a message naming the path. Collapsing them here would make a fresh installation
        // that has not downloaded 300MB yet look like a fault, which is the readout teaching people
        // to ignore it.
        run_subsystem(
            "browser_sidecar",
            sidecar_probe("browser_sidecar", crate::sidecar::BROWSER, browser_enabled),
        ),
        // Always `true`, unlike its three neighbours, because this sidecar has no pillar switch:
        // it is supervised beside `echo`. A provider with no credential on this machine is a
        // reading that says `unmeasured`, not a subsystem that is off.
        run_subsystem(
            "quota_sidecar",
            sidecar_probe("quota_sidecar", crate::sidecar::QUOTA, true),
        ),
        run_subsystem("voice_transcriber", voice_probe(voice_armed, stt_command)),
        run_subsystem("voice_speaker", speaker_probe(voice_speaks, tts_command)),
        run_subsystem("github", github_probe(github_asked_for, github_binary)),
        run_subsystem(
            "hook_interpreter",
            hook_interpreter_probe(state.pool.clone()),
        ),
        run_subsystem("llm_router", router_probe()),
        run_subsystem("devtime_ingest", devtime_probe(state.pool.clone())),
        run_subsystem("distiller", distiller_probe(state.pool.clone())),
    );
    let subsystems = vec![
        pool,
        cli,
        credentials,
        disk,
        echo,
        telegram,
        email,
        web,
        browser,
        quota,
        voice,
        speaker,
        github,
        hook,
        router,
        devtime,
        distiller,
    ];

    HealthReadout {
        status: aggregate_state(&subsystems),
        subsystems,
    }
}

async fn run_subsystem<F>(name: &'static str, probe: F) -> SubsystemReadout
where
    F: Future<Output = SubsystemReadout>,
{
    match tokio::time::timeout(SUBSYSTEM_TIMEOUT, probe).await {
        Ok(readout) => readout,
        Err(_) => SubsystemReadout::down(name, FailureCategory::Timeout),
    }
}

async fn run_probe<F>(name: &'static str, probe: F) -> SubsystemReadout
where
    F: Future<Output = Result<HealthState, FailureCategory>>,
{
    run_probe_with_timeout(name, PROBE_TIMEOUT, probe).await
}

async fn run_probe_with_timeout<F>(
    name: &'static str,
    timeout: Duration,
    probe: F,
) -> SubsystemReadout
where
    F: Future<Output = Result<HealthState, FailureCategory>>,
{
    match tokio::time::timeout(timeout, probe).await {
        Ok(Ok(HealthState::Ok)) => SubsystemReadout::ok(name),
        Ok(Ok(HealthState::Degraded)) => SubsystemReadout::degraded(name, FailureCategory::Unknown),
        Ok(Ok(HealthState::Disabled)) => {
            SubsystemReadout::disabled(name, FailureCategory::NotConfigured)
        }
        Ok(Ok(HealthState::Down)) => SubsystemReadout::down(name, FailureCategory::Unknown),
        Ok(Err(category)) => SubsystemReadout::down(name, category),
        Err(_) => SubsystemReadout::down(name, FailureCategory::Timeout),
    }
}

async fn pool_probe(pool: SqlitePool) -> SubsystemReadout {
    run_probe("sqlite_pool", async move {
        sqlx::query("SELECT 1")
            .execute(&pool)
            .await
            .map(|_| HealthState::Ok)
            .map_err(classify_error)
    })
    .await
}

async fn cli_probe() -> SubsystemReadout {
    run_probe("cli_binary", async {
        let resolved = tokio::task::spawn_blocking(resolve_cli_binary)
            .await
            .map_err(classify_error)?
            .ok_or(FailureCategory::Missing)?;
        // The resolved path, not the configured name: the lookup has already decided which file on
        // PATH this is, and re-deriving it in another process would be a second chance to disagree.
        exec_probe(resolved.to_string_lossy().into_owned(), "--version").await
    })
    .await
}

/// What `router.yaml` asks of the daemon, before anybody has asked the router anything.
#[derive(Debug, PartialEq)]
enum RouterPlan {
    /// No file, or every surface off: routing is not asked for, so there is nothing to measure.
    Off,
    /// A file that does not parse, or names a router off this machine. The daemon runs without
    /// routing, but the owner believes it is on, which is what a red row is for.
    Refused,
    /// Routing is on. The URL and the budget are the file's own.
    Ask { url: String, timeout: Duration },
}

/// PURE: `router.yaml`'s text (`None` for an absent file) as a plan. The grammar and the loopback
/// fence are `route_advice::parse_config`'s, not restated here, and the error text is dropped:
/// this readout carries a closed vocabulary and no text.
fn router_plan(text: Option<&str>) -> RouterPlan {
    let Some(text) = text else {
        return RouterPlan::Off;
    };
    match crate::route_advice::parse_config(text) {
        Err(_) => RouterPlan::Refused,
        Ok(config) if config.is_off() => RouterPlan::Off,
        Ok(config) => RouterPlan::Ask {
            url: config.url,
            // Inside the readout's own half-second budget, whatever the file allows a run.
            timeout: Duration::from_millis(config.timeout_ms).min(PROBE_TIMEOUT),
        },
    }
}

/// PURE: the devtime ingestion row, from the status the ingestion loop last wrote.
///
/// `None` means nothing has run yet, which is green: a daemon that has just started has not failed
/// at anything. Past the failure rate is `degraded` and never `down`, because ingestion only feeds
/// a reading and stops nothing else.
fn devtime_row(status: Option<crate::devtime_store::IngestStatus>) -> SubsystemReadout {
    let Some(status) = status else {
        return SubsystemReadout::ok("devtime_ingest");
    };
    let mut row = if !status.enabled {
        SubsystemReadout::disabled("devtime_ingest", FailureCategory::NotConfigured)
    } else if status.lines_read >= status.failure_min_lines
        && status.lines_read > 0
        && status.lines_failed as f64 / status.lines_read as f64 > status.failure_amber_rate
    {
        SubsystemReadout::degraded("devtime_ingest", FailureCategory::Unknown)
    } else {
        SubsystemReadout::ok("devtime_ingest")
    };
    row.counts = Some(std::collections::BTreeMap::from([
        ("lines_read", status.lines_read),
        ("lines_failed", status.lines_failed),
        ("unknown_records", status.unknown_records),
        ("files_failed", status.files_failed),
        ("unmapped", status.unmapped_sessions),
        ("skipped_daemon", status.skipped_daemon_sessions),
    ]));
    row
}

async fn devtime_probe(pool: sqlx::SqlitePool) -> SubsystemReadout {
    match crate::devtime_store::read_ingest_status(&pool).await {
        Ok(status) => devtime_row(status),
        Err(_) => SubsystemReadout::down("devtime_ingest", FailureCategory::Unknown),
    }
}

/// What the distiller's durable queue holds right now, as the readout reports it.
struct DistillTally {
    /// Rows still waiting: `pending` and `running` both.
    pending: i64,
    /// Rows that failed inside the last 24 hours.
    failed_24h: i64,
    /// The newest `done` row's finish instant, as unix seconds; `None` before anything finished.
    last_done_unix: Option<i64>,
}

/// One query, three scalar subselects over `distill_queue`.
///
/// `finished_at` is RFC 3339 from chrono on both sides, so the 24h cutoff is a plain string
/// comparison. `MAX(finished_at)` is parsed back to an instant; a value that does not parse is
/// read as "no instant" rather than failing the whole row.
async fn distiller_tally(
    pool: &SqlitePool,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<DistillTally> {
    let cutoff = (now - chrono::Duration::hours(24)).to_rfc3339();
    let (pending, failed_24h, last_done): (i64, i64, Option<String>) = sqlx::query_as(
        "SELECT \
           (SELECT COUNT(*) FROM distill_queue WHERE status IN (?, ?)), \
           (SELECT COUNT(*) FROM distill_queue WHERE status = ? AND finished_at >= ?), \
           (SELECT MAX(finished_at) FROM distill_queue WHERE status = ?)",
    )
    .bind(crate::distill::STATUS_PENDING)
    .bind(crate::distill::STATUS_RUNNING)
    .bind(crate::distill::STATUS_FAILED)
    .bind(cutoff)
    .bind(crate::distill::STATUS_DONE)
    .fetch_one(pool)
    .await?;
    Ok(DistillTally {
        pending,
        failed_24h,
        last_done_unix: last_done
            .and_then(|at| chrono::DateTime::parse_from_rfc3339(&at).ok())
            .map(|at| at.timestamp()),
    })
}

/// PURE: the distiller row, from its queue tally.
///
/// A failure inside the last 24 hours is `degraded` and never `down`: the distiller only turns
/// closed work into learnings and stops nothing else. `last_done_unix` is absent, not zero, when
/// nothing has ever been distilled.
fn distiller_row(tally: DistillTally) -> SubsystemReadout {
    let mut row = if tally.failed_24h > 0 {
        SubsystemReadout::degraded("distiller", FailureCategory::Unknown)
    } else {
        SubsystemReadout::ok("distiller")
    };
    let mut counts = std::collections::BTreeMap::from([
        ("pending", tally.pending),
        ("failed_24h", tally.failed_24h),
    ]);
    if let Some(at) = tally.last_done_unix {
        counts.insert("last_done_unix", at);
    }
    row.counts = Some(counts);
    row
}

async fn distiller_probe(pool: sqlx::SqlitePool) -> SubsystemReadout {
    match distiller_tally(&pool, chrono::Utc::now()).await {
        Ok(tally) => distiller_row(tally),
        Err(_) => SubsystemReadout::down("distiller", FailureCategory::Unknown),
    }
}

/// PURE: the row. `reachable` matters only for a plan that asks.
///
/// An unreachable router is `degraded` and not `down`: the daemon falls back to the choice it
/// made before routing existed, so nothing stops, and a `down` here would take the whole readout
/// down for an adviser.
fn router_row(plan: &RouterPlan, reachable: bool) -> SubsystemReadout {
    match (plan, reachable) {
        (RouterPlan::Off, _) => {
            SubsystemReadout::disabled("llm_router", FailureCategory::NotConfigured)
        }
        (RouterPlan::Refused, _) => {
            SubsystemReadout::down("llm_router", FailureCategory::NotConfigured)
        }
        (RouterPlan::Ask { .. }, true) => SubsystemReadout::ok("llm_router"),
        (RouterPlan::Ask { .. }, false) => {
            SubsystemReadout::degraded("llm_router", FailureCategory::Unreachable)
        }
    }
}

/// Reads `~/.nucleos/router.yaml` as the settings door does, then asks the router for its tiers.
/// Task text never travels: `targets` is a GET with no body.
async fn router_probe() -> SubsystemReadout {
    let text = tokio::task::spawn_blocking(|| {
        let path = crate::machine_config::root()?.join(crate::machine_config::ROUTER_FILE);
        std::fs::read_to_string(path).ok()
    })
    .await
    .ok()
    .flatten();
    router_answer(router_plan(text.as_deref())).await
}

/// The probe's one question, for a plan already made: does the router answer `targets`?
async fn router_answer(plan: RouterPlan) -> SubsystemReadout {
    let reachable = match &plan {
        RouterPlan::Ask { url, timeout } => crate::router_client::RouterClient::new(url, *timeout)
            .targets()
            .await
            .is_ok(),
        _ => false,
    };
    router_row(&plan, reachable)
}

/// PURE: the row, from whether the interpreter resolved and whether any rostered project wires
/// the hook. A resolvable interpreter is `ok` either way.
fn hook_interpreter_row(resolved: Option<PathBuf>, wired_anywhere: bool) -> SubsystemReadout {
    match (resolved, wired_anywhere) {
        (Some(_), _) => SubsystemReadout::ok("hook_interpreter"),
        (None, true) => SubsystemReadout::down("hook_interpreter", FailureCategory::Missing),
        (None, false) => {
            SubsystemReadout::disabled("hook_interpreter", FailureCategory::NotConfigured)
        }
    }
}

/// This row exists because a hook whose interpreter is missing is invisible by construction.
///
/// Claude Code runs the command, gets 127, and treats every exit but 2 as non-blocking, so the tool
/// call goes ahead unclassified.
///
/// **`disabled`, not `down`, when no project in the roster wires the hook** (owner decision,
/// 2026-09-14). Then no session runs the hook, so a missing interpreter harms nothing, and a `down`
/// here would turn the whole readout `down` and, with `health_breach_intent` on, write a breach
/// record for a hole nobody has. With the hook wired in at least one project the row is unchanged:
/// `ok` when the interpreter resolves, `down`/`missing` when it does not.
///
/// The roster is read only when the interpreter does not resolve, so the common case costs what it
/// always did. Both lookups run on the blocking pool, as `get_projects` does for the same roots.
async fn hook_interpreter_probe(pool: SqlitePool) -> SubsystemReadout {
    let resolved = match tokio::task::spawn_blocking(|| {
        resolve_program(std::ffi::OsStr::new(crate::autopilot::HOOK_INTERPRETER))
    })
    .await
    {
        Ok(resolved) => resolved,
        Err(_) => return SubsystemReadout::down("hook_interpreter", FailureCategory::Unknown),
    };
    let wired_anywhere = resolved.is_none() && hook_wired_in_any_project(&pool).await;
    hook_interpreter_row(resolved, wired_anywhere)
}

/// Whether any `autopilot_state` row with a root has THIS daemon's classifier hook wired, asked of
/// `autopilot::classifier_hook_is_wired` (the function activation and `runs.rs` ask) rather than
/// restated. Not filtered by mode. Today only shadow/active rows keep a root, because
/// `set_project_mode` clears it on `off`, so an `off` project that still carries the hook is not
/// seen here.
///
/// An unreadable roster, or a check that panicked, answers `true`. That keeps the row as it was
/// before it knew about projects, while `false` would turn a real hole into `disabled` exactly
/// when nobody can check. The database fault itself is `sqlite_pool`'s row to report.
async fn hook_wired_in_any_project(pool: &SqlitePool) -> bool {
    let Ok(roots) = sqlx::query_scalar::<_, String>(
        "SELECT project_root FROM autopilot_state WHERE project_root IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    else {
        return true;
    };
    tokio::task::spawn_blocking(move || {
        roots
            .iter()
            .any(|root| crate::autopilot::classifier_hook_is_wired(Path::new(root)))
    })
    .await
    .unwrap_or(true)
}

/// PURE: whether a configured engine is a program this daemon would spawn, and so a program to grade.
///
/// Both voice rows ask this, and they ask it together rather than each in its own words: the
/// transcriber row learned once already, with `split_command`, what it costs when a probe reasons
/// about its subject differently from the code that runs it.
///
/// **A resident engine answers `false`, and that is the whole point.** `stt_url` and `tts_url` arm
/// their halves of the pillar with no program anywhere, so grading them as programs would resolve an
/// empty string, fail, and paint a machine red for being configured the faster way. Probing them over
/// HTTP instead is the other wrong answer: this module grades programs this daemon spawns, and a row
/// that graded the operator's whisper-server while saying nothing about their Ollama would be
/// describing their setup rather than this daemon's. The failure surfaces where it can be acted on --
/// a refused connection becomes a 502 and the window says which engine failed.
fn probes_a_program(configured: bool, command: &str) -> bool {
    configured && !command.trim().is_empty()
}

/// Whether the configured transcriber is a program that runs here.
///
/// Splitting is load-bearing: `stt_command` is a whole command line, so handing the string to a path
/// lookup unsplit would look for a program whose name contains its own arguments and report every
/// configured transcriber as missing.
///
/// It splits through `transcribe::split_command`, the same function that spawns the child, and that
/// sharing is the point rather than a tidiness. This probe used its own `split_whitespace` until a
/// quoted program path became supported — at which point it started looking for a program whose name
/// began with a quote character, and reported a transcriber that worked perfectly as missing. A probe
/// that parses its subject differently from the code under test is worse than no probe: it raises the
/// alarm on exactly the configuration it was added to bless.
///
/// Absent config is `Ok`, not a failure. Voice being off is a state, not a fault.
async fn voice_probe(armed: bool, command: String) -> SubsystemReadout {
    voice_probe_within(armed, command, PROBE_TIMEOUT).await
}

async fn voice_probe_within(armed: bool, command: String, budget: Duration) -> SubsystemReadout {
    run_probe_with_timeout("voice_transcriber", budget, async move {
        if !armed {
            return Ok(HealthState::Ok);
        }
        let program = crate::transcribe::split_command(&command)
            .into_iter()
            .next()
            .unwrap_or_default();
        let resolved =
            tokio::task::spawn_blocking(move || resolve_program(std::ffi::OsStr::new(&program)))
                .await
                .map_err(classify_error)?
                .ok_or(FailureCategory::Missing)?;
        exec_probe(resolved.to_string_lossy().into_owned(), "--help").await
    })
    .await
}

/// Whether the configured speaker is a program that runs here.
///
/// The transcriber probe's twin, down to splitting through the same `split_command` for the same
/// reason — a probe that parses its subject differently from the code that spawns it raises the alarm
/// on exactly the configuration it was added to bless.
///
/// One difference, and it is the whole reason this is a separate row rather than a second check
/// inside `voice_probe`: a missing SPEAKER is not a missing pillar. Voice with no TTS still hears the
/// question and still answers it, in writing. Folding the two together would paint a working
/// conversation red for lacking a voice, and this module's header says what that costs — it teaches
/// people to ignore the readout when it matters.
///
/// Not configured is `Ok`. A núcleo that does not speak is a choice, not a fault.
///
/// **A resident speaker (`tts_url`) is `Ok` here too, and is not probed.** There is no program to
/// resolve, and the alternative — an HTTP probe — would be a new pattern in this module for one
/// loopback service while Ollama, the other one, has none. A row that graded the operator's Piper
/// server and said nothing about their Ollama would be describing their setup rather than this
/// daemon's. The failure still surfaces: `HttpSpeaker` reports a refused connection, `voice.rs` turns
/// it into a 502, and the window says the speaker failed.
async fn speaker_probe(configured: bool, command: String) -> SubsystemReadout {
    run_probe("voice_speaker", async move {
        if !configured {
            return Ok(HealthState::Ok);
        }
        let program = crate::transcribe::split_command(&command)
            .into_iter()
            .next()
            .unwrap_or_default();
        let resolved =
            tokio::task::spawn_blocking(move || resolve_program(std::ffi::OsStr::new(&program)))
                .await
                .map_err(classify_error)?
                .ok_or(FailureCategory::Missing)?;
        exec_probe(resolved.to_string_lossy().into_owned(), "--help").await
    })
    .await
}

/// Whether the GitHub pillar could act if it were asked to.
///
/// **`asked_for` is `enabled` AND the file existing, and both halves are load-bearing.**
/// `GithubConfig::enabled` defaults to TRUE so that a machine with no `~/.nucleos/github.yaml` is capable
/// of everything and autonomous in nothing — which means `enabled` alone can no longer distinguish
/// "the owner wants this" from "the owner has never heard of it". Grading on `enabled` alone would
/// put a red row on every installation that has never touched GitHub, and this module's own header
/// says what that costs: it teaches readers to ignore the readout when it matters.
///
/// Once it HAS been asked for, the two failures are kept apart, because they send a person to two
/// different places:
///
/// - no `gh` on PATH is `Missing` — a thing this computer cannot do;
/// - no token is `PermissionDenied` — a thing it could do if somebody pasted a credential. Reported
///   any other way it reads like a broken repository, and somebody loses an hour.
///
/// It probes the TOKEN and never `gh auth status`, and that is the whole point of asking this
/// question here: the CLI's own login lives in the interactive session's keyring, so a green
/// `gh auth status` would say healthy while the daemon — a scheduled task — could not act.
async fn github_probe(asked_for: bool, binary: String) -> SubsystemReadout {
    run_probe("github", async move {
        if !asked_for {
            return Ok(HealthState::Disabled);
        }
        // The two facts `execute` needs, in the order it needs them, and the CATEGORY comes from
        // `github::Failure` rather than being chosen again here. One vocabulary, defined where the
        // failures are, so the readout and the refusal a caller gets cannot come to disagree.
        // The name off the runtime rather than the literal `gh`, so this probe and
        // `github::execute` cannot be looking for two different files — the second chance to
        // disagree that `cli_probe` refuses to take.
        let failure =
            if tokio::task::spawn_blocking(move || resolve_program(std::ffi::OsStr::new(&binary)))
                .await
                .map_err(classify_error)?
                .is_none()
            {
                Some(crate::github::Failure::MissingCli)
            } else if crate::github::load_token().await.is_none() {
                Some(crate::github::Failure::MissingToken)
            } else {
                None
            };
        match failure {
            None => Ok(HealthState::Ok),
            Some(failure) => Err(failure.category()),
        }
    })
    .await
}

async fn credential_manager_probe() -> SubsystemReadout {
    run_probe("credential_manager", async {
        tokio::task::spawn_blocking(|| crate::secrets::load_secret(DAEMON_TOKEN_KEY))
            .await
            .map_err(classify_error)?
            .map(|_| HealthState::Ok)
            .map_err(classify_error)
    })
    .await
}

async fn disk_probe() -> SubsystemReadout {
    let mut readout = run_probe("worktree_disk", async {
        tokio::task::spawn_blocking(worktree_available_space)
            .await
            .map_err(classify_error)?
            .map(|available| {
                if available < LOW_DISK_BYTES {
                    HealthState::Degraded
                } else {
                    HealthState::Ok
                }
            })
            .map_err(category_from_io)
    })
    .await;

    if readout.status == HealthState::Degraded {
        readout.reason = Some(FailureCategory::LowDiskSpace);
    } else if readout.reason == Some(FailureCategory::NotConfigured) {
        readout.status = HealthState::Disabled;
    }
    readout
}

async fn telegram_sidecar_probe() -> SubsystemReadout {
    let configured = run_probe("telegram_sidecar", async {
        tokio::task::spawn_blocking(|| crate::secrets::load_secret(TELEGRAM_TOKEN_KEY))
            .await
            .map_err(classify_error)?
            .map_err(classify_error)
            .map(|secret| {
                if secret.is_some() {
                    HealthState::Ok
                } else {
                    HealthState::Disabled
                }
            })
    })
    .await;

    if configured.status == HealthState::Disabled || configured.status == HealthState::Down {
        return configured;
    }
    sidecar_probe("telegram_sidecar", crate::sidecar::TELEGRAM, true).await
}

/// PURE: what the supervisor's last observation of a sidecar means for its row.
///
/// The `configured` gate runs first and is never reached past: a pillar nobody turned on is a state,
/// not a fault, and must not be dragged into `down` by having no registry entry.
///
/// `None` shares an arm with `Restarting` on purpose. Nothing ever supervised this name — either the
/// daemon is still starting, which is honest for the seconds it lasts, or something refused to start
/// it. The email sidecar is the case that matters: `main.rs` returns before `supervise` when the
/// hook barrier cannot be proven, and this row used to stay green on the strength of an `.exe`
/// sitting on disk while the pillar was silently off.
fn sidecar_row(
    name: &'static str,
    configured: bool,
    liveness: Option<Liveness>,
) -> SubsystemReadout {
    if !configured {
        return SubsystemReadout::disabled(name, FailureCategory::NotConfigured);
    }
    match liveness {
        Some(Liveness::Running) => SubsystemReadout::ok(name),
        Some(Liveness::Restarting) | None => {
            SubsystemReadout::down(name, FailureCategory::NotRunning)
        }
        Some(Liveness::FailedToSpawn(kind)) => {
            SubsystemReadout::down(name, category_from_spawn(kind))
        }
    }
}

/// The row name and the supervisor's key are different things, so the call site passes both.
async fn sidecar_probe(
    name: &'static str,
    supervised_as: &'static str,
    configured: bool,
) -> SubsystemReadout {
    sidecar_row(name, configured, crate::sidecar::liveness_of(supervised_as))
}

fn aggregate_state(subsystems: &[SubsystemReadout]) -> HealthState {
    if subsystems
        .iter()
        .any(|entry| entry.status == HealthState::Down)
    {
        HealthState::Down
    } else if subsystems
        .iter()
        .any(|entry| entry.status == HealthState::Degraded)
    {
        HealthState::Degraded
    } else if subsystems
        .iter()
        .any(|entry| entry.status == HealthState::Ok)
    {
        HealthState::Ok
    } else {
        HealthState::Disabled
    }
}

fn resolve_cli_binary() -> Option<PathBuf> {
    let configured = std::env::var_os("NUCLEOS_CLAUDE_BIN").unwrap_or_else(|| "claude".into());
    resolve_program(&configured)
}

/// Finds `program` the way a shell would: as a path when it looks like one, else along `PATH`.
///
/// Extracted from `resolve_cli_binary` when the voice transcriber became a second configured program
/// needing the same lookup. Both probes report configuration readiness only — neither runs anything.
fn resolve_program(program: &std::ffi::OsStr) -> Option<PathBuf> {
    let path = PathBuf::from(program);

    if path.components().count() > 1 || path.is_absolute() {
        return binary_candidate(&path);
    }

    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|directory| binary_candidate(&directory.join(program)))
}

fn binary_candidate(path: &Path) -> Option<PathBuf> {
    let mut candidates = vec![path.to_path_buf()];
    if path.extension().is_none() {
        candidates.push(path.with_extension("exe"));
    }
    candidates.into_iter().find(|candidate| {
        candidate.is_file()
            && !candidate
                .extension()
                .is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("cmd"))
    })
}

type ExecVerdict = Result<HealthState, FailureCategory>;

/// Keyed by the program itself, not by the row: the verdict is a fact about a specific binary, so
/// re-pointing `stt_command` at a different transcriber must not inherit the old one's answer.
static EXEC_CACHE: LazyLock<Mutex<HashMap<String, (Instant, ExecVerdict)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// What actually happened when the probe tried to run the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecOutcome {
    Exited { success: bool },
    TimedOut,
    SpawnFailed(io::ErrorKind),
}

/// PURE: what an attempt to run the program means for the row.
fn exec_verdict(outcome: ExecOutcome) -> ExecVerdict {
    match outcome {
        ExecOutcome::Exited { success: true } => Ok(HealthState::Ok),
        // Deliberately not `down`. `--help` and `--version` conventions differ between CLIs, and a
        // probe that raises the alarm on a working install is worse than no probe — the lesson this
        // module already learned from the quoted-path bug in its tests.
        ExecOutcome::Exited { success: false } | ExecOutcome::TimedOut => Ok(HealthState::Degraded),
        ExecOutcome::SpawnFailed(kind) => Err(category_from_spawn(kind)),
    }
}

/// PURE: a cached verdict, but only while it is fresh.
fn cached_verdict(entry: Option<(Instant, ExecVerdict)>, now: Instant) -> Option<ExecVerdict> {
    entry
        .filter(|(stamped, _)| now.duration_since(*stamped) < EXEC_CACHE_TTL)
        .map(|(_, verdict)| verdict)
}

/// A cached verdict on whether `program` runs here, refreshed behind the readout rather than during
/// it.
///
/// The readout never waits for the program. The shell polls `/health` every three seconds and
/// [`AGGREGATE_TIMEOUT`] is one second, so executing on the request path would spawn two processes
/// every three seconds and report `down: timeout` for any CLI that takes longer than a second to
/// answer — which a Node CLI routinely does.
///
/// A cold read answers `Ok`, because the caller only gets here once the path has resolved: that is
/// precisely the check this probe used to do on its own, and it is honest about being a config
/// check. The executed verdict replaces it one poll later.
async fn exec_probe(program: String, flag: &'static str) -> ExecVerdict {
    let entry = EXEC_CACHE
        .lock()
        .ok()
        .and_then(|cache| cache.get(&program).cloned());
    if let Some(verdict) = cached_verdict(entry, Instant::now()) {
        return verdict;
    }

    // Claim the slot before spawning, or every poll until the refresh lands starts another one.
    if let Ok(mut cache) = EXEC_CACHE.lock() {
        cache.insert(program.clone(), (Instant::now(), Ok(HealthState::Ok)));
    }
    tokio::spawn(refresh_exec_cache(program, flag));
    Ok(HealthState::Ok)
}

async fn refresh_exec_cache(program: String, flag: &'static str) {
    let verdict = exec_verdict(run_program_once(&program, flag).await);
    if let Ok(mut cache) = EXEC_CACHE.lock() {
        cache.insert(program, (Instant::now(), verdict));
    }
}

/// Runs the program with a harmless flag, purely to see whether it runs at all.
///
/// This is what execution buys over resolving a path: wrong architecture, a missing DLL, a corrupt
/// file, permission bits, an `.exe` that is really text — all of them resolve fine and fail here.
///
/// stdio is closed and `kill_on_drop` set so a transcriber that ignores `--help` and sits waiting
/// for audio is killed at the timeout instead of accumulating orphans.
async fn run_program_once(program: &str, flag: &'static str) -> ExecOutcome {
    let mut command = tokio::process::Command::new(program);
    command
        .arg(flag)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);

    match command.spawn() {
        Err(error) => ExecOutcome::SpawnFailed(error.kind()),
        Ok(mut child) => match tokio::time::timeout(EXEC_PROBE_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) => ExecOutcome::Exited {
                success: status.success(),
            },
            Ok(Err(error)) => ExecOutcome::SpawnFailed(error.kind()),
            Err(_) => ExecOutcome::TimedOut,
        },
    }
}

/// A spawn failure's `NotFound` means the program is absent, so it is `Missing` — where
/// [`category_from_io`] maps the same `ErrorKind` to `NotConfigured` for the disk probe, which is
/// asking a different question of the same error.
fn category_from_spawn(kind: io::ErrorKind) -> FailureCategory {
    match kind {
        io::ErrorKind::NotFound => FailureCategory::Missing,
        io::ErrorKind::PermissionDenied => FailureCategory::PermissionDenied,
        _ => FailureCategory::Unknown,
    }
}

fn worktree_available_space() -> io::Result<u64> {
    let project_root = std::env::current_dir().map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "a worktree root needs a current project directory",
        )
    })?;
    free_space_for_worktrees(&project_root)
}

/// How much room is left on the volume that would hold this project's worktrees.
///
/// Takes the project root rather than reading the daemon's own directory, and that is the whole
/// reason it is a function of its own. `worktree_root` lives INSIDE each project root by default —
/// `<project>/.nucleos/worktrees`, not a sibling of it — so two projects can still sit on two
/// volumes; a reading taken from wherever the daemon happens to be running answers about a third.
/// The probe above keeps the old behaviour because a health readout is about the machine, not about
/// one project.
///
/// Blocking, and deliberately not wrapped in `spawn_blocking` here: the underlying call —
/// `GetDiskFreeSpaceExW` on Windows, `statvfs` on Unix — is a metadata read against an
/// already-mounted volume, and every caller is either already on a blocking thread or paying
/// microseconds.
pub fn free_space_for_worktrees(project_root: &Path) -> io::Result<u64> {
    let root = worktree::worktree_root(project_root);
    let existing_root = root
        .ancestors()
        .find(|candidate| candidate.exists())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no existing worktree root ancestor",
            )
        })?;
    disk_free_space(existing_root)
}

#[cfg(windows)]
fn disk_free_space(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;

    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            directory_name: *const u16,
            available_to_caller: *mut u64,
            total_bytes: *mut u64,
            total_free_bytes: *mut u64,
        ) -> i32;
    }

    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0;
    // `path` is NUL-terminated and remains alive for the whole Win32 call; the other pointers
    // point at writable stack values of the documented `ULARGE_INTEGER` width.
    let success = unsafe {
        GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if success == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(available)
    }
}

/// `f_bavail` and not `f_bfree`: `f_bfree` counts every block free on the filesystem, including
/// the reserve only root may spend, while `f_bavail` is what an unprivileged process can actually
/// take — which is the honest answer to "is there room" for a daemon that runs as nobody special.
///
/// The `#[allow]` is on the function because the cast is unnecessary on exactly the platforms
/// where `f_bavail`/`f_frsize` are already `u64` (Linux) and load-bearing where they are `u32`
/// (macOS); one `cfg`-free spelling has to be wrong for one of them, and widening is the safe way
/// to be wrong.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)]
fn disk_free_space(path: &Path) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is NUL-terminated and alive for the call; `stat` is a writable `statvfs`.
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

#[cfg(not(any(windows, unix)))]
fn disk_free_space(_path: &Path) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "disk-space probe is implemented with the Windows filesystem API",
    ))
}

fn category_from_io(error: io::Error) -> FailureCategory {
    match error.kind() {
        io::ErrorKind::PermissionDenied => FailureCategory::PermissionDenied,
        io::ErrorKind::NotFound => FailureCategory::NotConfigured,
        io::ErrorKind::ConnectionRefused
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::NotConnected
        | io::ErrorKind::AddrInUse
        | io::ErrorKind::AddrNotAvailable => FailureCategory::Unreachable,
        _ => FailureCategory::Unknown,
    }
}

/// Deliberately discards error text before it can cross the health-response boundary.
fn classify_error<E>(_error: E) -> FailureCategory {
    FailureCategory::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn breached_readout() -> HealthReadout {
        HealthReadout {
            status: HealthState::Down,
            subsystems: vec![SubsystemReadout::down(
                "cli_binary",
                FailureCategory::Missing,
            )],
        }
    }

    /// Every migration applied, like `autopilot.rs`'s `test_pool`. One connection, because each
    /// `sqlite::memory:` connection is a database of its own.
    async fn migrated_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    #[test]
    fn the_router_row_follows_the_file_and_then_the_router() {
        assert_eq!(router_plan(None), RouterPlan::Off);
        assert_eq!(
            router_plan(Some(
                "mode: off
"
            )),
            RouterPlan::Off
        );
        assert_eq!(
            router_plan(Some(
                "mode: shadow
: [
"
            )),
            RouterPlan::Refused
        );
        assert_eq!(
            router_plan(Some(
                "mode: shadow
url: http://example.com:1
"
            )),
            RouterPlan::Refused
        );
        let ask = router_plan(Some(
            "mode: shadow
url: http://127.0.0.1:9
",
        ));
        assert!(matches!(ask, RouterPlan::Ask { .. }));

        let off = router_row(&RouterPlan::Off, false);
        assert_eq!(
            (off.name, off.status),
            ("llm_router", HealthState::Disabled)
        );
        let refused = router_row(&RouterPlan::Refused, false);
        assert_eq!(refused.status, HealthState::Down);
        assert!(refused.reason.is_some());
        assert_eq!(router_row(&ask, true).status, HealthState::Ok);
        let unreachable = router_row(&ask, false);
        assert_eq!(unreachable.status, HealthState::Degraded);
        assert_eq!(unreachable.reason, Some(FailureCategory::Unreachable));
    }

    /// The probe's one question, against a real listener: a router that answers `targets` is green,
    /// and a port nobody listens on is amber.
    #[tokio::test]
    async fn a_router_that_answers_its_targets_is_green_and_a_silent_one_is_amber() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buffer = [0u8; 1024];
                let _ = socket.read(&mut buffer).await;
                let body = r#"{"targets":[]}"#;
                let reply = format!(
                    "HTTP/1.1 200 OK
content-type: application/json
content-length: {}
connection: close

{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });
        let ask = |port: u16| {
            router_plan(Some(&format!(
                "mode: shadow
url: http://127.0.0.1:{port}
"
            )))
        };
        assert_eq!(router_answer(ask(port)).await.status, HealthState::Ok);
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = free.local_addr().unwrap().port();
        drop(free);
        assert_eq!(router_answer(ask(dead)).await.status, HealthState::Degraded);
    }

    #[test]
    fn an_interpreter_that_does_not_resolve_is_a_red_row() {
        let missing = hook_interpreter_row(None, true);
        assert_eq!(missing.status, HealthState::Down);
        assert_eq!(missing.reason, Some(FailureCategory::Missing));

        let resolved = hook_interpreter_row(Some(PathBuf::from("x")), true);
        assert_eq!(resolved.status, HealthState::Ok);
        assert_eq!(resolved.reason, None);
    }

    /// Owner decision, 2026-09-14: nobody wires the hook, so a missing interpreter harms nothing.
    #[test]
    fn a_missing_interpreter_is_disabled_when_no_project_wires_the_hook() {
        let missing = hook_interpreter_row(None, false);
        assert_eq!(missing.status, HealthState::Disabled);
        assert_eq!(missing.reason, Some(FailureCategory::NotConfigured));

        let resolved = hook_interpreter_row(Some(PathBuf::from("x")), false);
        assert_eq!(resolved.status, HealthState::Ok);
        assert_eq!(resolved.reason, None);
    }

    /// Why not `down`: a `down` row takes the whole readout down, and with `health_breach_intent`
    /// on that writes a breach record for a hole nobody has.
    #[tokio::test]
    async fn an_unwired_missing_interpreter_leaves_the_aggregate_up_and_writes_no_breach() {
        let subsystems = vec![
            SubsystemReadout::ok("sqlite_pool"),
            hook_interpreter_row(None, false),
        ];
        let readout = HealthReadout {
            status: aggregate_state(&subsystems),
            subsystems,
        };
        assert_eq!(readout.status, HealthState::Ok);
        assert!(!readout.is_breach());

        let pool = migrated_pool().await;
        let rules = crate::config::AutopilotRules {
            health_breach_intent: true,
            ..Default::default()
        };
        assert!(
            !record_breach_intent(&pool, "alpha", &rules, &readout)
                .await
                .unwrap()
        );
        assert_eq!(breach_lines(&pool).await, 0);

        // The control: wired somewhere, the same missing interpreter still takes the readout down.
        assert_eq!(
            aggregate_state(&[
                SubsystemReadout::ok("sqlite_pool"),
                hook_interpreter_row(None, true),
            ]),
            HealthState::Down
        );
    }

    /// A project counts only when the hook `wire_classifier_hook` writes is really at its root.
    #[tokio::test]
    async fn the_roster_is_wired_only_when_some_project_wires_the_hook() {
        let pool = migrated_pool().await;
        assert!(
            !hook_wired_in_any_project(&pool).await,
            "an empty roster wires nothing"
        );

        let bare = tempfile::tempdir().unwrap();
        sqlx::query("INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('bare', 'shadow', ?)")
            .bind(bare.path().to_string_lossy().into_owned())
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES ('rootless', 'off')")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            !hook_wired_in_any_project(&pool).await,
            "no rostered root has the hook"
        );

        let wired = tempfile::tempdir().unwrap();
        crate::autopilot::wire_classifier_hook(wired.path()).unwrap();
        sqlx::query("INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('wired', 'active', ?)")
            .bind(wired.path().to_string_lossy().into_owned())
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            hook_wired_in_any_project(&pool).await,
            "one wired project is enough"
        );
    }

    /// Conservative on failure: this pool has no `autopilot_state` table at all.
    #[tokio::test]
    async fn an_unreadable_roster_counts_as_wired() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        assert!(hook_wired_in_any_project(&pool).await);
    }

    /// How many breach lines the feed holds.
    async fn breach_lines(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = ?")
            .bind(BREACH_INTENT_KIND)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn health_breach_intent_is_inert_when_the_rule_is_off() {
        let pool = migrated_pool().await;
        let result = record_breach_intent(
            &pool,
            "alpha",
            &crate::config::AutopilotRules::default(),
            &breached_readout(),
        )
        .await
        .unwrap();

        assert!(!result);
        assert_eq!(breach_lines(&pool).await, 0);
    }

    /// One feed line, on the project's own feed, naming what breached — and nothing written into
    /// any project folder, which is where the old ledger used to be.
    #[tokio::test]
    async fn an_enabled_health_breach_writes_one_feed_line() {
        let pool = migrated_pool().await;
        let rules = crate::config::AutopilotRules {
            health_breach_intent: true,
            ..Default::default()
        };

        assert!(
            record_breach_intent(&pool, "alpha", &rules, &breached_readout())
                .await
                .unwrap()
        );

        let rows: Vec<(Option<String>, String)> =
            sqlx::query_as("SELECT project_id, summary FROM feed WHERE kind = ?")
                .bind(BREACH_INTENT_KIND)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0.as_deref(), Some("alpha"));
        assert!(rows[0].1.contains("cli_binary"), "{}", rows[0].1);
        assert!(rows[0].1.contains("nothing was queued"), "{}", rows[0].1);
    }

    #[test]
    fn disabled_subsystems_do_not_drag_the_aggregate_down() {
        let entries = [
            SubsystemReadout::ok("pool"),
            SubsystemReadout::disabled("email", FailureCategory::NotConfigured),
        ];
        assert_eq!(aggregate_state(&entries), HealthState::Ok);
    }

    #[test]
    fn aggregate_prioritises_down_then_degraded() {
        assert_eq!(
            aggregate_state(&[
                SubsystemReadout::ok("cli"),
                SubsystemReadout::down("sqlite_pool", FailureCategory::Unknown),
            ]),
            HealthState::Down
        );
        assert_eq!(
            aggregate_state(&[
                SubsystemReadout::ok("pool"),
                SubsystemReadout::degraded("disk", FailureCategory::LowDiskSpace),
            ]),
            HealthState::Degraded
        );
    }

    #[tokio::test]
    async fn a_timed_out_probe_is_down_without_hanging_the_readout() {
        let pending = std::future::pending::<Result<HealthState, FailureCategory>>();
        let readout = run_probe_with_timeout("slow", Duration::ZERO, pending).await;
        assert_eq!(readout.status, HealthState::Down);
        assert_eq!(readout.reason, Some(FailureCategory::Timeout));
    }

    #[tokio::test]
    async fn raw_probe_errors_are_reduced_to_categories_before_serialization() {
        let raw_error = "imap://user:password@host.example.test/INBOX";
        let readout = run_probe("mail", async {
            Err::<HealthState, _>(classify_error(raw_error))
        })
        .await;
        let encoded = serde_json::to_string(&readout).unwrap();
        assert!(!encoded.contains("password"));
        assert!(!encoded.contains("user:password@host"));
    }

    /// A resident transcriber has no program to look for, and must not be graded as if it had.
    ///
    /// `speaker_probe`'s twin, arrived at the same way and for the same reason. The regression is
    /// concrete: `stt_url` alone arms the pillar, so without this the row would resolve the empty
    /// string as a program, fail, and paint `voice_transcriber` red on a machine configured entirely
    /// correctly -- and configured for the FASTER path at that. An HTTP probe is not the answer
    /// either: this module probes programs this daemon spawns, and a row that graded the operator's
    /// whisper-server while saying nothing about their Ollama would be describing their setup rather
    /// than this daemon's. The failure still surfaces where it is actionable -- `HttpTranscriber`
    /// reports a refused connection, `voice.rs` turns it into a 502.
    #[test]
    fn a_resident_engine_is_not_probed_as_a_program() {
        // A command, on an armed pillar: there is a program, so it is graded.
        assert!(probes_a_program(true, "whisper-cli -m model.bin"));
        // `stt_url` alone. Armed, and nothing to resolve.
        assert!(!probes_a_program(true, ""));
        assert!(!probes_a_program(true, "   "));
        // Off is off, whatever is written beside it.
        assert!(!probes_a_program(false, "whisper-cli"));
    }

    /// A working transcriber behind a quoted path must not be reported as missing.
    ///
    /// The regression this pins: the probe used to split on whitespace while `transcribe.rs` split on
    /// quotes, so a `stt_command` naming `"C:\Program Files\...\whisper-cli.exe"` made the probe
    /// look for a program whose name STARTS WITH a quote character. It reported `down` for the exact
    /// configuration that quoting was added to support — a probe raising the alarm on a working setup,
    /// which is worse than no probe. Both now go through one function.
    ///
    /// The assertion is `not Missing` rather than `== Ok` since the probe started executing the
    /// program: a resolvable program can now answer `ok` or `degraded` depending on what IT does
    /// with `--help`, which is the program's business and not this regression's. Pinning `== Ok`
    /// would make a test about SPLITTING fail over an exit code — the same category of misdirected
    /// alarm the bug itself was.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_quoted_transcriber_path_probes_the_program_and_not_the_quote() {
        // `cmd` exists on every Windows host and needs no arguments to resolve.
        let readout = voice_probe(true, "\"cmd\" -m model.bin".to_string()).await;

        assert_ne!(
            readout.reason,
            Some(FailureCategory::Missing),
            "a quoted path that resolves must not be reported missing, got {:?}",
            readout.status
        );
    }

    /// And the unquoted form, which every config written before quoting existed uses.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_quoted_transcriber_path_probes_the_program_and_not_the_quote_on_unix() {
        // `sh` exists on every Unix host.
        let readout = voice_probe(true, "\"sh\" -m model.bin".to_string()).await;

        assert_ne!(
            readout.reason,
            Some(FailureCategory::Missing),
            "a quoted path that resolves must not be reported missing, got {:?}",
            readout.status
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn an_unquoted_transcriber_path_still_probes_its_first_token() {
        let readout = voice_probe_within(
            true,
            "cmd -m model.bin".to_string(),
            Duration::from_secs(10),
        )
        .await;
        assert_ne!(readout.reason, Some(FailureCategory::Missing));

        let missing = voice_probe_within(
            true,
            "definitely-not-a-program-anywhere -x".to_string(),
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(
            missing.status,
            HealthState::Down,
            "a transcriber that does not exist has to be reported"
        );
        assert_eq!(missing.reason, Some(FailureCategory::Missing));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unquoted_transcriber_path_still_probes_its_first_token_on_unix() {
        // `sh` exists on every Unix host.
        let readout =
            voice_probe_within(true, "sh -m model.bin".to_string(), Duration::from_secs(10)).await;
        assert_ne!(readout.reason, Some(FailureCategory::Missing));

        let missing = voice_probe_within(
            true,
            "/nonexistent-nucleos/definitely-not-a-program -x".to_string(),
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(
            missing.status,
            HealthState::Down,
            "a transcriber that does not exist has to be reported"
        );
        assert_eq!(missing.reason, Some(FailureCategory::Missing));
    }

    /// A row now reports what the supervisor saw, not what is sitting on disk.
    #[test]
    fn a_sidecar_row_reports_what_the_supervisor_saw() {
        let running = sidecar_row("echo_sidecar", true, Some(Liveness::Running));
        assert_eq!(running.status, HealthState::Ok);
        assert_eq!(running.reason, None);

        let restarting = sidecar_row("echo_sidecar", true, Some(Liveness::Restarting));
        assert_eq!(restarting.status, HealthState::Down);
        assert_eq!(restarting.reason, Some(FailureCategory::NotRunning));

        // A failed spawn proves the binary's absence better than an `is_file()` ever did: it proves
        // it at the moment it mattered, by the mechanism that mattered.
        let never_built = sidecar_row(
            "echo_sidecar",
            true,
            Some(Liveness::FailedToSpawn(io::ErrorKind::NotFound)),
        );
        assert_eq!(never_built.status, HealthState::Down);
        assert_eq!(never_built.reason, Some(FailureCategory::Missing));

        let refused = sidecar_row(
            "echo_sidecar",
            true,
            Some(Liveness::FailedToSpawn(io::ErrorKind::PermissionDenied)),
        );
        assert_eq!(refused.reason, Some(FailureCategory::PermissionDenied));
    }

    /// The case that motivated the work. When the email hook barrier fails, `main.rs` returns before
    /// `supervise` is ever called — so there is no registry entry at all. The row used to stay green
    /// because the `.exe` was on disk, which is the readout being green precisely when it should
    /// not be.
    #[test]
    fn a_configured_sidecar_nobody_started_is_down_rather_than_green() {
        let row = sidecar_row("email_sidecar", true, None);
        assert_eq!(row.status, HealthState::Down);
        assert_eq!(row.reason, Some(FailureCategory::NotRunning));
    }

    /// The counterpart that must NOT change: a pillar nobody turned on is a state, not a fault.
    #[test]
    fn a_pillar_nobody_turned_on_stays_disabled() {
        let row = sidecar_row("email_sidecar", false, None);
        assert_eq!(row.status, HealthState::Disabled);
        assert_eq!(row.reason, Some(FailureCategory::NotConfigured));
    }

    /// The supervisor writes two fields; `liveness` is the single reading of them. This pins the
    /// pairing that is easy to get wrong: `down` means two different things depending on whether a
    /// spawn error is sitting beside it.
    #[test]
    fn the_supervisors_two_fields_read_as_one_verdict() {
        let mut entry = crate::sidecar::SidecarState {
            name: "echo".to_string(),
            state: "running",
            started_at: Some("now".to_string()),
            last_failure: None,
            last_failure_at: None,
            restarts: 0,
            last_line: None,
            last_line_at: None,
            spawn_error: None,
        };
        assert_eq!(entry.liveness(), Liveness::Running);

        entry.state = "down";
        entry.last_failure = Some("exited: exit code: 1".to_string());
        assert_eq!(entry.liveness(), Liveness::Restarting);

        entry.spawn_error = Some(io::ErrorKind::NotFound);
        assert_eq!(
            entry.liveness(),
            Liveness::FailedToSpawn(io::ErrorKind::NotFound)
        );

        // And the clearing matters as much as the setting: a sidecar that could not start, then
        // started, must stop being reported as a missing binary.
        entry.state = "running";
        entry.spawn_error = None;
        assert_eq!(entry.liveness(), Liveness::Running);
    }

    /// Non-zero exit is `degraded`, never `down`. `--help` and `--version` conventions differ across
    /// CLIs, and the one unacceptable outcome for this probe is crying wolf over a working install.
    #[test]
    fn an_exec_probe_is_only_down_when_the_program_will_not_start() {
        assert_eq!(
            exec_verdict(ExecOutcome::Exited { success: true }),
            Ok(HealthState::Ok)
        );
        assert_eq!(
            exec_verdict(ExecOutcome::Exited { success: false }),
            Ok(HealthState::Degraded)
        );
        assert_eq!(
            exec_verdict(ExecOutcome::TimedOut),
            Ok(HealthState::Degraded)
        );
        assert_eq!(
            exec_verdict(ExecOutcome::SpawnFailed(io::ErrorKind::NotFound)),
            Err(FailureCategory::Missing)
        );
    }

    /// The cache is what keeps a five-second program off a one-second readout, so its expiry is
    /// worth a test of its own rather than being trusted to a comparison written once.
    #[test]
    fn a_stale_exec_verdict_is_not_reused() {
        let now = Instant::now();
        let fresh = now.checked_sub(EXEC_CACHE_TTL / 2).unwrap();
        let stale = now.checked_sub(EXEC_CACHE_TTL * 2).unwrap();

        assert_eq!(
            cached_verdict(Some((fresh, Ok(HealthState::Degraded))), now),
            Some(Ok(HealthState::Degraded))
        );
        assert_eq!(
            cached_verdict(Some((stale, Ok(HealthState::Ok))), now),
            None
        );
        assert_eq!(cached_verdict(None, now), None);
    }

    /// A spawn failure and a disk error carry the same `ErrorKind` and mean different things, which
    /// is exactly the kind of shared vocabulary that gets collapsed by a later tidy-up.
    #[test]
    fn a_missing_program_and_a_missing_directory_are_not_the_same_answer() {
        assert_eq!(
            category_from_spawn(io::ErrorKind::NotFound),
            FailureCategory::Missing
        );
        assert_eq!(
            category_from_io(io::Error::from(io::ErrorKind::NotFound)),
            FailureCategory::NotConfigured
        );
    }

    /// The probe measures a real filesystem off Windows too.
    ///
    /// The temp directory is writable on any host that can run this suite, and a writable
    /// directory sits on a mounted filesystem with some room left on it. So an `Unsupported`
    /// error here is the platform gap — `disk_free_space` is implemented against the Windows
    /// filesystem API alone — and never a property of the host the test ran on, which is also
    /// why the assertion is `> 0` rather than a threshold.
    ///
    /// It lives in this module because `disk_free_space` is private to it, and widening that
    /// visibility to test it from outside would change production code to suit a test.
    #[cfg(unix)]
    #[test]
    fn the_disk_probe_measures_free_space_on_unix() {
        let free = disk_free_space(&std::env::temp_dir());
        assert!(
            matches!(&free, Ok(bytes) if *bytes > 0),
            "the volume holding the temp directory has free space to report, got {free:?}"
        );
    }

    fn devtime_status(
        enabled: bool,
        lines_read: i64,
        lines_failed: i64,
    ) -> crate::devtime_store::IngestStatus {
        crate::devtime_store::IngestStatus {
            enabled,
            cycle_at: "2026-10-04T10:00:00.000Z".to_string(),
            lines_read,
            lines_failed,
            failure_amber_rate: 0.02,
            failure_min_lines: 200,
            ..Default::default()
        }
    }

    #[test]
    fn devtime_row_is_disabled_when_ingestion_is_off() {
        let off = devtime_row(Some(devtime_status(false, 0, 0)));
        assert_eq!(
            (off.name, off.status, off.reason),
            (
                "devtime_ingest",
                HealthState::Disabled,
                Some(FailureCategory::NotConfigured)
            )
        );
        // Nothing has run yet: green, and with nothing to count.
        let fresh = devtime_row(None);
        assert_eq!(
            (fresh.name, fresh.status),
            ("devtime_ingest", HealthState::Ok)
        );
        assert!(fresh.counts.is_none());
    }

    #[test]
    fn devtime_row_turns_degraded_past_the_failure_threshold() {
        // Too few lines to judge a rate, however bad it looks.
        assert_eq!(
            devtime_row(Some(devtime_status(true, 100, 100))).status,
            HealthState::Ok
        );
        // Exactly at the rate is not past it; one more failure is.
        assert_eq!(
            devtime_row(Some(devtime_status(true, 1000, 20))).status,
            HealthState::Ok
        );
        let bad = devtime_row(Some(devtime_status(true, 1000, 21)));
        assert_eq!(bad.status, HealthState::Degraded);
        assert_eq!(bad.reason, Some(FailureCategory::Unknown));
    }

    #[test]
    fn devtime_row_carries_the_unmapped_count() {
        let mut status = devtime_status(true, 10, 1);
        status.unmapped_sessions = 3;
        status.skipped_daemon_sessions = 5;
        status.unknown_records = 2;
        status.files_failed = 4;
        let row = devtime_row(Some(status));
        let counts = row.counts.as_ref().expect("a status gives counts");
        assert_eq!(counts.get("unmapped"), Some(&3));
        assert_eq!(counts.get("skipped_daemon"), Some(&5));
        assert_eq!(counts.get("unknown_records"), Some(&2));
        assert_eq!(counts.get("files_failed"), Some(&4));
        assert_eq!(counts.get("lines_read"), Some(&10));
        assert_eq!(counts.get("lines_failed"), Some(&1));
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(json["counts"]["unmapped"], 3);
        let plain = serde_json::to_value(SubsystemReadout::ok("x")).unwrap();
        assert!(plain.get("counts").is_none());
    }

    #[test]
    fn distiller_row_counts_the_queue() {
        let row = distiller_row(DistillTally {
            pending: 3,
            failed_24h: 0,
            last_done_unix: Some(1_759_000_000),
        });
        assert_eq!((row.name, row.status), ("distiller", HealthState::Ok));
        let counts = row.counts.as_ref().expect("a tally gives counts");
        assert_eq!(counts.get("pending"), Some(&3));
        assert_eq!(counts.get("failed_24h"), Some(&0));
        assert_eq!(counts.get("last_done_unix"), Some(&1_759_000_000));
        // Nothing has ever been distilled: the instant is absent, not zero.
        let fresh = distiller_row(DistillTally {
            pending: 0,
            failed_24h: 0,
            last_done_unix: None,
        });
        let counts = fresh.counts.as_ref().expect("a tally gives counts");
        assert!(!counts.contains_key("last_done_unix"));
        assert_eq!(counts.get("pending"), Some(&0));
    }

    #[test]
    fn distiller_row_turns_degraded_on_a_recent_failure() {
        let row = distiller_row(DistillTally {
            pending: 0,
            failed_24h: 1,
            last_done_unix: None,
        });
        assert_eq!(row.status, HealthState::Degraded);
        assert_eq!(row.reason, Some(FailureCategory::Unknown));
    }

    #[tokio::test]
    async fn distiller_tally_reads_the_queue() {
        let pool = migrated_pool().await;
        let now = chrono::Utc::now();
        let t1 = now - chrono::Duration::hours(5);
        let t2 = now - chrono::Duration::hours(2);
        let rows: [(i64, &str, Option<chrono::DateTime<chrono::Utc>>); 6] = [
            (1, crate::distill::STATUS_PENDING, None),
            (2, crate::distill::STATUS_RUNNING, None),
            (3, crate::distill::STATUS_DONE, Some(t1)),
            (4, crate::distill::STATUS_DONE, Some(t2)),
            (
                5,
                crate::distill::STATUS_FAILED,
                Some(now - chrono::Duration::hours(1)),
            ),
            (
                6,
                crate::distill::STATUS_FAILED,
                Some(now - chrono::Duration::hours(48)),
            ),
        ];
        for (job_id, status, finished) in rows {
            sqlx::query(
                "INSERT INTO distill_queue \
                 (cause, project_id, job_id, status, created_at, finished_at) \
                 VALUES ('job_landed', 'p', ?, ?, ?, ?)",
            )
            .bind(job_id)
            .bind(status)
            .bind(now.to_rfc3339())
            .bind(finished.map(|at| at.to_rfc3339()))
            .execute(&pool)
            .await
            .unwrap();
        }
        let tally = distiller_tally(&pool, now).await.unwrap();
        // A pending and a running row both still wait; only the failure inside 24h counts.
        assert_eq!(tally.pending, 2);
        assert_eq!(tally.failed_24h, 1);
        assert_eq!(tally.last_done_unix, Some(t2.timestamp()));
    }
}
