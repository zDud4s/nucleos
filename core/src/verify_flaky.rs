//! Whether a red verification unit is flakiness (spec 2026-10-05 §6.2 step 1,
//! metric 5). Pure apart from `history`. The evidence key is
//! `(group_name, sha, fingerprint)` and all three are required: a `verify own`
//! runs on a dirty worktree whose `sha` is HEAD, so a row without a fingerprint
//! proves nothing. Called by nothing until F3-3.

use std::collections::BTreeMap;

use crate::verify_runs::{STATUS_ERRORED, STATUS_FAILED, STATUS_PASSED};

/// Observations a group needs before its rate can mark a red `Suspect`.
pub const MIN_OBSERVED: u32 = 5;
/// Flaky keys per thousand observed keys at or above which a group is suspect (10%).
pub const SUSPECT_PER_MILLE: u32 = 100;

/// The columns of one `verify_runs` row that flakiness reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: i64,
    pub group_name: Option<String>,
    pub sha: Option<String>,
    pub fingerprint: Option<String>,
    pub status: String,
    pub requested_by: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Rerun {
    /// Failed, then passed on the same sha: the gate counts it green.
    Flaky,
    Red,
    Inconclusive,
}

/// The verdict of a rerun of a failed unit on the same sha.
pub fn rerun_verdict(rerun_status: &str) -> Rerun {
    if rerun_status == STATUS_PASSED {
        Rerun::Flaky
    } else if rerun_status == STATUS_FAILED {
        Rerun::Red
    } else {
        Rerun::Inconclusive
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct GroupRate {
    /// Distinct `(sha, fingerprint)` keys with evidence.
    pub observed: u32,
    /// Of those, keys that saw both a pass and a failure.
    pub flaky: u32,
}

/// The evidence key of a row and whether it passed; `None` when the row is not evidence.
fn evidence(run: &Run) -> Option<(&str, &str, &str, bool)> {
    let passed = if run.status == STATUS_PASSED {
        true
    } else if run.status == STATUS_FAILED {
        false
    } else {
        return None;
    };
    Some((
        run.group_name.as_deref()?,
        run.sha.as_deref()?,
        run.fingerprint.as_deref()?,
        passed,
    ))
}

/// Per-group flake rates. Rows missing a group, sha or fingerprint, and rows
/// that are neither `passed` nor `failed`, are not evidence.
pub fn group_rates(rows: &[Run]) -> BTreeMap<String, GroupRate> {
    // key -> (saw a pass, saw a failure)
    let mut keys: BTreeMap<(&str, &str, &str), (bool, bool)> = BTreeMap::new();
    for row in rows {
        if let Some((group, sha, fp, passed)) = evidence(row) {
            let seen = keys.entry((group, sha, fp)).or_default();
            if passed {
                seen.0 = true;
            } else {
                seen.1 = true;
            }
        }
    }
    let mut rates: BTreeMap<String, GroupRate> = BTreeMap::new();
    for ((group, _, _), (pass, fail)) in keys {
        let rate = rates.entry(group.to_string()).or_default();
        rate.observed += 1;
        if pass && fail {
            rate.flaky += 1;
        }
    }
    rates
}

#[derive(Debug, PartialEq, Eq)]
pub enum Class {
    Flaky,
    /// Red, in a group whose flake rate is high. Never skips the rerun.
    Suspect,
    Red,
}

/// Classify one red against the history of its project.
pub fn classify(red: &Run, history: &[Run]) -> Class {
    if let (Some(group), Some(sha), Some(fp)) = (
        red.group_name.as_deref(),
        red.sha.as_deref(),
        red.fingerprint.as_deref(),
    ) {
        let passed_on_key = history.iter().any(|h| {
            h.status == STATUS_PASSED
                && h.group_name.as_deref() == Some(group)
                && h.sha.as_deref() == Some(sha)
                && h.fingerprint.as_deref() == Some(fp)
        });
        if passed_on_key {
            return Class::Flaky;
        }
        if let Some(rate) = group_rates(history).get(group)
            && rate.observed >= MIN_OBSERVED
            && rate.flaky * 1000 >= rate.observed * SUSPECT_PER_MILLE
        {
            return Class::Suspect;
        }
    }
    Class::Red
}

/// Terminal rows of one project that ran a group, newest first.
pub async fn history(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    limit: i64,
) -> sqlx::Result<Vec<Run>> {
    let rows: Vec<(
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        String,
    )> = sqlx::query_as(
        "SELECT id, group_name, sha, fingerprint, status, requested_by \
             FROM verify_runs \
             WHERE project_id = ? AND status IN (?, ?, ?) AND group_name IS NOT NULL \
             ORDER BY id DESC LIMIT ?",
    )
    .bind(project_id)
    .bind(STATUS_PASSED)
    .bind(STATUS_FAILED)
    .bind(STATUS_ERRORED)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, group_name, sha, fingerprint, status, requested_by)| Run {
                id,
                group_name,
                sha,
                fingerprint,
                status,
                requested_by,
            },
        )
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify_runs::{self, Row};

    fn run(id: i64, group: Option<&str>, sha: Option<&str>, fp: Option<&str>, status: &str) -> Run {
        Run {
            id,
            group_name: group.map(str::to_string),
            sha: sha.map(str::to_string),
            fingerprint: fp.map(str::to_string),
            status: status.to_string(),
            requested_by: "gate".to_string(),
        }
    }

    fn full(id: i64, group: &str, sha: &str, fp: &str, status: &str) -> Run {
        run(id, Some(group), Some(sha), Some(fp), status)
    }

    #[test]
    fn a_failure_that_passes_on_rerun_is_flaky() {
        assert_eq!(rerun_verdict(verify_runs::STATUS_PASSED), Rerun::Flaky);
    }

    #[test]
    fn a_failure_that_fails_again_is_red() {
        assert_eq!(rerun_verdict(verify_runs::STATUS_FAILED), Rerun::Red);
    }

    #[test]
    fn an_errored_rerun_is_inconclusive() {
        assert_eq!(
            rerun_verdict(verify_runs::STATUS_ERRORED),
            Rerun::Inconclusive
        );
        assert_eq!(rerun_verdict("anything else"), Rerun::Inconclusive);
    }

    #[test]
    fn a_pass_and_a_fail_on_one_key_count_one_flake() {
        let rows = vec![
            full(1, "core", "s1", "f1", "passed"),
            full(2, "core", "s1", "f1", "failed"),
            full(3, "core", "s1", "f1", "failed"),
            // A second key in the same group, only passing.
            full(4, "core", "s2", "f2", "passed"),
        ];
        let rates = group_rates(&rows);
        assert_eq!(rates.len(), 1);
        assert_eq!(
            rates["core"],
            GroupRate {
                observed: 2,
                flaky: 1
            }
        );
    }

    #[test]
    fn rows_without_sha_group_or_fingerprint_are_not_evidence() {
        let rows = vec![
            run(1, None, Some("s1"), Some("f1"), "passed"),
            run(2, None, Some("s1"), Some("f1"), "failed"),
            run(3, Some("core"), None, Some("f1"), "passed"),
            run(4, Some("core"), None, Some("f1"), "failed"),
            run(5, Some("core"), Some("s1"), None, "passed"),
            run(6, Some("core"), Some("s1"), None, "failed"),
        ];
        assert!(group_rates(&rows).is_empty());
    }

    #[test]
    fn cached_and_errored_rows_are_not_evidence() {
        let rows = vec![
            full(1, "core", "s1", "f1", "passed"),
            full(2, "core", "s1", "f1", verify_runs::STATUS_SKIPPED_CACHED),
            full(3, "core", "s1", "f1", verify_runs::STATUS_ERRORED),
        ];
        let rates = group_rates(&rows);
        assert_eq!(
            rates["core"],
            GroupRate {
                observed: 1,
                flaky: 0
            }
        );
    }

    #[test]
    fn a_red_with_a_pass_on_its_key_is_flaky() {
        let red = full(9, "core", "s1", "f1", "failed");
        let history = vec![full(1, "core", "s1", "f1", "passed")];
        assert_eq!(classify(&red, &history), Class::Flaky);
        // A pass on another key proves nothing.
        let other = vec![full(1, "core", "s1", "f2", "passed")];
        assert_eq!(classify(&red, &other), Class::Red);
        // A red without a full key can never be Flaky.
        let keyless = run(9, Some("core"), Some("s1"), None, "failed");
        assert_eq!(classify(&keyless, &history), Class::Red);
    }

    #[test]
    fn a_red_in_a_group_above_the_rate_is_suspect() {
        assert_eq!(MIN_OBSERVED, 5);
        assert_eq!(SUSPECT_PER_MILLE, 100);
        let red = full(99, "core", "sX", "fX", "failed");
        // Five observed keys, one flaky: 200 per mille, at or above 100.
        let mut history = vec![
            full(1, "core", "s1", "f1", "passed"),
            full(2, "core", "s1", "f1", "failed"),
        ];
        for (i, s) in ["s2", "s3", "s4", "s5"].iter().enumerate() {
            history.push(full(10 + i as i64, "core", s, "f", "passed"));
        }
        assert_eq!(group_rates(&history)["core"].observed, 5);
        assert_eq!(classify(&red, &history), Class::Suspect);

        // Same flake count but below MIN_OBSERVED: plain red.
        let few = vec![
            full(1, "core", "s1", "f1", "passed"),
            full(2, "core", "s1", "f1", "failed"),
        ];
        assert_eq!(classify(&red, &few), Class::Red);

        // Enough observations, rate under 10%: plain red (1 flake in 20 keys = 50 per mille).
        let mut low = vec![
            full(1, "core", "s1", "f1", "passed"),
            full(2, "core", "s1", "f1", "failed"),
        ];
        for i in 0..19 {
            low.push(full(100 + i, "core", &format!("t{i}"), "f", "passed"));
        }
        assert_eq!(classify(&red, &low), Class::Red);

        // A different group's rate does not count.
        let other_group = full(99, "web", "sX", "fX", "failed");
        assert_eq!(classify(&other_group, &history), Class::Red);
    }

    async fn insert(
        pool: &sqlx::SqlitePool,
        project: &str,
        group: Option<&str>,
        status: &str,
    ) -> i64 {
        verify_runs::record(
            pool,
            &Row {
                project_id: Some(project),
                worktree: "/wt",
                sha: Some("s1"),
                scope: verify_runs::SCOPE_FULL,
                origin: verify_runs::ORIGIN_RUN,
                origin_id: Some(1),
                ordinal: None,
                requested_by: verify_runs::REQUESTED_BY_GATE,
                group_name: group,
                kind: None,
                argv: "x",
                fingerprint: Some("f1"),
                status,
                exit_code: Some(0),
                duration_ms: 1,
                started_at: "2026-01-01T00:00:00Z",
                finished_at: "2026-01-01T00:00:01Z",
                output_tail: None,
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn history_reads_terminal_rows_of_one_project_newest_first() {
        let pool = crate::testdb::fresh_pool().await;
        let first = insert(&pool, "alpha", Some("core"), "passed").await;
        insert(&pool, "alpha", Some("core"), "skipped_cached").await;
        insert(&pool, "alpha", None, "failed").await;
        insert(&pool, "beta", Some("core"), "failed").await;
        let last = insert(&pool, "alpha", Some("core"), "failed").await;

        let rows = history(&pool, "alpha", 10).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, last);
        assert_eq!(rows[0].status, "failed");
        assert_eq!(rows[1].id, first);
        assert_eq!(rows[1].status, "passed");
        assert_eq!(rows[0].group_name.as_deref(), Some("core"));
        assert_eq!(rows[0].requested_by, "gate");

        // The limit applies after ordering.
        let one = history(&pool, "alpha", 1).await.unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].id, last);
    }
}
