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
    run_rules_pass_with(pool, cfg, sources, touched, skip, FAMILIES).await
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
    for project_id in &projects {
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
