//! Lets a test unit skip the clippy its check unit already ran.
//!
//! A group like `core` has `check: cargo clippy ...` and `command: bash scripts/gates.sh core`,
//! and that script runs the same clippy before the tests. A caller that submits the check kind
//! first and the test kind once it passed (the tree-job gate) would pay for clippy twice. So a
//! test unit whose group's check unit already passed for the same tree is run with
//! `NUCLEOS_GATE_SKIP_CLIPPY=1`, and the script leaves its clippy step out.
//!
//! "The same tree" is the unit's fingerprint: the hash of the files the group reads, the group's
//! map entry and the map version, which is kind independent, so a group's check and test units
//! carry the same one. The worktree must match too, because the fingerprint says nothing about
//! the toolchain or the target dir the other worktree built with.
//!
//! The flag is deliberately NOT part of the fingerprint or the cache key. A test unit's pass must
//! still mean "clippy passed for this tree", and it does: the flag is only ever set when a check
//! unit passed (or was reused from the cache, which only holds green rows) for that exact
//! fingerprint, so a cached test pass cannot hide a clippy failure. Anything that does not read the
//! same evidence (the post-merge gate, the daemon's probe, a recheck, a bisection) keeps clippy:
//! they exist to doubt an earlier green.

use sqlx::SqlitePool;

use crate::verify::runs::{
    REQUESTED_BY_BISECT, REQUESTED_BY_FLAKE_CHECK, REQUESTED_BY_POSTGATE, REQUESTED_BY_PROBE,
    STATUS_PASSED, STATUS_SKIPPED_CACHED,
};

/// The variable `scripts/gates.sh` reads.
pub const SKIP_CLIPPY_ENV: &str = "NUCLEOS_GATE_SKIP_CLIPPY";

/// `[(NUCLEOS_GATE_SKIP_CLIPPY, "1")]` when unit `unit_id` is a test unit with a passed check
/// unit of the same group, worktree and fingerprint; empty otherwise, and on any read error (a
/// unit that keeps its clippy is never wrong).
pub async fn env(pool: &SqlitePool, unit_id: i64) -> Vec<(String, String)> {
    let found: Result<bool, sqlx::Error> = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM verify_runs t JOIN verify_runs c \
           ON c.project_id IS t.project_id AND c.worktree = t.worktree \
          AND c.group_name = t.group_name AND c.fingerprint = t.fingerprint \
         WHERE t.id = ?1 AND t.kind = 'test' AND t.fingerprint IS NOT NULL \
           AND t.requested_by NOT IN (?2, ?3, ?4, ?5) \
           AND c.kind = 'check' AND c.status IN (?6, ?7))",
    )
    .bind(unit_id)
    .bind(REQUESTED_BY_POSTGATE)
    .bind(REQUESTED_BY_PROBE)
    .bind(REQUESTED_BY_FLAKE_CHECK)
    .bind(REQUESTED_BY_BISECT)
    .bind(STATUS_PASSED)
    .bind(STATUS_SKIPPED_CACHED)
    .fetch_one(pool)
    .await;
    match found {
        Ok(true) => vec![(SKIP_CLIPPY_ENV.to_owned(), "1".to_owned())],
        Ok(false) => Vec::new(),
        Err(error) => {
            tracing::warn!(%error, unit_id, "verify executor: cannot look up the check unit; clippy stays");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::runs::{self, Request, Row};

    struct Spec<'a> {
        kind: &'a str,
        status: &'a str,
        fingerprint: Option<&'a str>,
        group: &'a str,
        worktree: &'a str,
    }

    const CHECK_PASSED: Spec = Spec {
        kind: "check",
        status: STATUS_PASSED,
        fingerprint: Some("fp1"),
        group: "core",
        worktree: "/wt",
    };

    /// A finished row, as the executor or a cache hit leaves it.
    async fn finished(pool: &SqlitePool, spec: &Spec<'_>) {
        runs::record(
            pool,
            &Row {
                project_id: Some("proj"),
                worktree: spec.worktree,
                sha: None,
                scope: runs::SCOPE_FULL,
                origin: runs::ORIGIN_VERIFY,
                origin_id: Some(1),
                ordinal: None,
                requested_by: "job",
                group_name: Some(spec.group),
                kind: Some(spec.kind),
                argv: "[]",
                fingerprint: spec.fingerprint,
                status: spec.status,
                exit_code: None,
                duration_ms: 0,
                started_at: "t",
                finished_at: "t",
                output_tail: None,
            },
        )
        .await
        .unwrap();
    }

    /// The test unit about to run, queued by `requested_by`.
    async fn queued_test(pool: &SqlitePool, requested_by: &str, fingerprint: &str) -> i64 {
        runs::enqueue(
            pool,
            &Request {
                project_id: Some("proj".to_owned()),
                worktree: "/wt".to_owned(),
                scope: runs::SCOPE_FULL.to_owned(),
                origin: runs::ORIGIN_VERIFY.to_owned(),
                origin_id: Some(2),
                requested_by: requested_by.to_owned(),
                group_name: Some("core".to_owned()),
                kind: Some("test".to_owned()),
                argv: vec!["bash".to_owned(), "scripts/gates.sh".to_owned()],
                fingerprint: Some(fingerprint.to_owned()),
                priority: runs::PRIORITY_INTERACTIVE,
                weight: 1,
                timeout_ms: 0,
            },
            0,
        )
        .await
        .unwrap()
        .id
    }

    fn skips(env: &[(String, String)]) -> bool {
        env.len() == 1 && env[0] == (SKIP_CLIPPY_ENV.to_owned(), "1".to_owned())
    }

    #[tokio::test]
    async fn a_test_unit_with_a_passed_check_at_the_same_fingerprint_skips_clippy() {
        let pool = crate::testdb::fresh_pool().await;
        finished(&pool, &CHECK_PASSED).await;
        let unit = queued_test(&pool, "job", "fp1").await;
        assert!(skips(&env(&pool, unit).await));
    }

    #[tokio::test]
    async fn a_check_served_from_the_cache_counts_as_passed() {
        let pool = crate::testdb::fresh_pool().await;
        finished(
            &pool,
            &Spec {
                status: STATUS_SKIPPED_CACHED,
                ..CHECK_PASSED
            },
        )
        .await;
        let unit = queued_test(&pool, "job", "fp1").await;
        assert!(skips(&env(&pool, unit).await));
    }

    #[tokio::test]
    async fn a_test_unit_without_a_check_keeps_clippy() {
        let pool = crate::testdb::fresh_pool().await;
        let unit = queued_test(&pool, "job", "fp1").await;
        assert!(env(&pool, unit).await.is_empty());
    }

    #[tokio::test]
    async fn a_failed_or_errored_check_keeps_clippy() {
        for status in ["failed", "errored", "queued", "running", "superseded"] {
            let pool = crate::testdb::fresh_pool().await;
            finished(
                &pool,
                &Spec {
                    status,
                    ..CHECK_PASSED
                },
            )
            .await;
            let unit = queued_test(&pool, "job", "fp1").await;
            assert!(env(&pool, unit).await.is_empty(), "{status}");
        }
    }

    #[tokio::test]
    async fn a_check_at_another_tree_group_or_worktree_keeps_clippy() {
        let others = [
            Spec {
                fingerprint: Some("fp0"),
                ..CHECK_PASSED
            },
            Spec {
                fingerprint: None,
                ..CHECK_PASSED
            },
            Spec {
                group: "shell",
                ..CHECK_PASSED
            },
            Spec {
                worktree: "/elsewhere",
                ..CHECK_PASSED
            },
            Spec {
                kind: "test",
                ..CHECK_PASSED
            },
        ];
        for other in &others {
            let pool = crate::testdb::fresh_pool().await;
            finished(&pool, other).await;
            let unit = queued_test(&pool, "job", "fp1").await;
            assert!(env(&pool, unit).await.is_empty());
        }
    }

    #[tokio::test]
    async fn the_postgate_a_probe_and_the_rechecks_keep_clippy() {
        for requested_by in [
            REQUESTED_BY_POSTGATE,
            REQUESTED_BY_PROBE,
            REQUESTED_BY_FLAKE_CHECK,
            REQUESTED_BY_BISECT,
        ] {
            let pool = crate::testdb::fresh_pool().await;
            finished(&pool, &CHECK_PASSED).await;
            let unit = queued_test(&pool, requested_by, "fp1").await;
            assert!(env(&pool, unit).await.is_empty(), "{requested_by}");
        }
    }

    #[tokio::test]
    async fn a_check_unit_itself_never_gets_the_flag() {
        let pool = crate::testdb::fresh_pool().await;
        finished(&pool, &CHECK_PASSED).await;
        let id = runs::enqueue(
            &pool,
            &Request {
                project_id: Some("proj".to_owned()),
                worktree: "/wt".to_owned(),
                scope: runs::SCOPE_FULL.to_owned(),
                origin: runs::ORIGIN_VERIFY.to_owned(),
                origin_id: Some(2),
                requested_by: "job".to_owned(),
                group_name: Some("core".to_owned()),
                kind: Some("check".to_owned()),
                argv: vec!["cargo".to_owned(), "clippy".to_owned()],
                fingerprint: Some("fp1".to_owned()),
                priority: runs::PRIORITY_INTERACTIVE,
                weight: 1,
                timeout_ms: 0,
            },
            0,
        )
        .await
        .unwrap()
        .id;
        assert!(env(&pool, id).await.is_empty());
    }
}
