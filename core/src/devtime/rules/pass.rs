//! The per-cycle development-time rules pass.

use super::*;

/// What one rules pass did, for the caller's log. No health row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RulesPassStats {
    pub sessions_ruled: usize,
    pub sessions_failed: usize,
    pub families_panicked: usize,
}

/// The per-cycle rules pass over the sessions this cycle touched plus a capped backlog of stale ones,
/// skipping `skip` (sessions whose lane rebuild failed). Touched sessions always run; the backlog (a
/// session ruled under another `rules_version`, or never ruled) is capped by `sessions_per_cycle`.
/// One session's error or panic is logged and counted, and the next session goes on. Afterwards each
/// project a session was ruled for gets its cross-session findings replaced.
pub async fn run_rules_pass(
    pool: &SqlitePool,
    cfg: &DevtimeRulesConfig,
    sources: AdapterSources,
    touched: &BTreeSet<String>,
    skip: &BTreeSet<String>,
) -> RulesPassStats {
    let fingerprint = rules_fingerprint(cfg);
    run_rules_pass_gated(pool, cfg, sources, touched, skip, FAMILIES, |project_id| {
        cross_due_now(project_id, &fingerprint)
    })
    .await
}

/// How long a project's cross-session findings stand before a cycle recomputes them.
///
/// The F families read every main-lane attempt of the project inside `f_window_days` (31 496 rows,
/// 1.5–17 s, on this repository on 2026-10-17), and any session that moved touches its project —
/// so with one live session the read ran every cycle, every minute, for patterns that span a
/// month and cannot change meaningfully in one. A new rules fingerprint is a new key, so a config
/// change recomputes at once.
pub(super) const CROSS_PASS_EVERY: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Whether `project_id`'s cross-session findings are due under `fingerprint`, recording now if so.
fn cross_due_now(project_id: &str, fingerprint: &str) -> bool {
    type Last = std::collections::HashMap<(String, String), std::time::Instant>;
    static LAST: std::sync::OnceLock<std::sync::Mutex<Last>> = std::sync::OnceLock::new();
    let Ok(mut last) = LAST.get_or_init(Default::default).lock() else {
        return true;
    };
    cross_due(
        &mut last,
        project_id,
        fingerprint,
        std::time::Instant::now(),
    )
}

/// [`cross_due_now`] over an explicit table and clock.
pub(super) fn cross_due(
    last: &mut std::collections::HashMap<(String, String), std::time::Instant>,
    project_id: &str,
    fingerprint: &str,
    now: std::time::Instant,
) -> bool {
    let key = (project_id.to_string(), fingerprint.to_string());
    match last.get(&key) {
        Some(at) if now.saturating_duration_since(*at) < CROSS_PASS_EVERY => false,
        _ => {
            last.insert(key, now);
            true
        }
    }
}

/// What ruling one session produced, for the pass's tally.
pub(super) struct Ruled {
    pub(super) project_id: String,
    pub(super) families_panicked: usize,
}

/// Rules one session: reads its rows, builds the facts, evaluates, projects and writes the result in
/// one transaction. The pure part runs under `catch_unwind`, so a panic anywhere in it fails only this
/// session.
pub(super) async fn rule_session(
    pool: &SqlitePool,
    cfg: &DevtimeRulesConfig,
    sources: AdapterSources,
    families: &[(char, FamilyFn)],
    fingerprint: &str,
    session_id: &str,
) -> Result<Ruled, String> {
    let head = devtime_store::session_head(pool, session_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the session has no row".to_string())?;
    let rows = devtime_store::session_rows(pool, session_id)
        .await
        .map_err(|error| error.to_string())?;
    let spans = devtime_store::session_spans(pool, session_id)
        .await
        .map_err(|error| error.to_string())?;
    let (write, families_panicked) = catch_unwind(AssertUnwindSafe(|| {
        let facts = SessionFacts::build(&head, &rows, &spans, cfg, sources);
        let (output, panicked) = evaluate_counted(&facts, cfg, families);
        let (span_marks, attempt_marks) = project(&facts, &output.findings, &output.verdicts);
        let turn_stats = devtime_unexplained::turn_stats(&facts, &span_marks, cfg);
        let findings = finding_rows(&facts, &output.findings, fingerprint);
        (
            RuleWrite {
                findings,
                span_marks,
                attempt_marks,
                turn_stats,
            },
            panicked,
        )
    }))
    .map_err(|_| "the rule engine panicked".to_string())?;
    devtime_store::write_rule_results(pool, &head, &write, fingerprint)
        .await
        .map_err(|error| error.to_string())?;
    Ok(Ruled {
        project_id: head.project_id,
        families_panicked,
    })
}

/// [`run_rules_pass`] with the families injected, so a test can run the pass over its own.
pub(super) async fn run_rules_pass_with(
    pool: &SqlitePool,
    cfg: &DevtimeRulesConfig,
    sources: AdapterSources,
    touched: &BTreeSet<String>,
    skip: &BTreeSet<String>,
    families: &[(char, FamilyFn)],
) -> RulesPassStats {
    run_rules_pass_gated(pool, cfg, sources, touched, skip, families, |_| true).await
}

/// [`run_rules_pass_with`] with the cross-session read asked first: a project `cross_due` turns
/// down keeps the findings it has.
pub(super) async fn run_rules_pass_gated(
    pool: &SqlitePool,
    cfg: &DevtimeRulesConfig,
    sources: AdapterSources,
    touched: &BTreeSet<String>,
    skip: &BTreeSet<String>,
    families: &[(char, FamilyFn)],
    mut cross_due: impl FnMut(&str) -> bool,
) -> RulesPassStats {
    let fingerprint = rules_fingerprint(cfg);
    let mut stats = RulesPassStats::default();

    let mut work: Vec<String> = touched
        .iter()
        .filter(|id| !skip.contains(*id))
        .cloned()
        .collect();
    let mut queued: HashSet<String> = work.iter().cloned().collect();
    match devtime_store::sessions_needing_rules(pool, &fingerprint, cfg.sessions_per_cycle).await {
        Ok(backlog) => {
            for id in backlog {
                if !skip.contains(&id) && queued.insert(id.clone()) {
                    work.push(id);
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "devtime: could not list the sessions the rules owe work");
        }
    }

    let mut projects: BTreeSet<String> = BTreeSet::new();
    for session_id in &work {
        match rule_session(pool, cfg, sources, families, &fingerprint, session_id).await {
            Ok(ruled) => {
                stats.sessions_ruled += 1;
                stats.families_panicked += ruled.families_panicked;
                projects.insert(ruled.project_id);
            }
            Err(error) => {
                stats.sessions_failed += 1;
                tracing::warn!(session = %session_id, %error, "devtime: ruling a session failed");
            }
        }
    }

    let since = format_ms(
        Utc::now().timestamp_millis() - i64::from(cfg.thresholds.f_window_days) * MS_PER_DAY,
    );
    for project_id in projects.iter().filter(|id| cross_due(id)) {
        let rows = match devtime_store::cross_session_rows(pool, project_id, &since).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(project = %project_id, %error, "devtime: could not read the cross-session rows");
                continue;
            }
        };
        let found = match catch_unwind(AssertUnwindSafe(|| devtime_rules_f::run_cross(&rows, cfg)))
        {
            Ok(found) => found,
            Err(_) => {
                stats.families_panicked += 1;
                tracing::warn!(project = %project_id, family = "F", "devtime: a rule family panicked");
                continue;
            }
        };
        let written = cross_rows(project_id, &found, &fingerprint);
        if let Err(error) =
            devtime_store::replace_cross_findings(pool, project_id, CROSS_RULE_IDS, &written).await
        {
            tracing::warn!(project = %project_id, %error, "devtime: could not store the cross-session findings");
        }
    }
    stats
}
