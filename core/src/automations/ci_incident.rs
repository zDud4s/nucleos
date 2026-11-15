//! `Action::OpenIncident`: a red CI of a pushed sha opens (or grows) the branch's `ci-red:<branch>`
//! incident, and a later green closes it.
//!
//! One live incident per branch: a further red appends to `detail.reds` instead of opening a second
//! ticket. Log text is written by whoever committed, so it is untrusted: it goes only into
//! `detail.last_red.ci_log_excerpt` (bounded to the tail, marked `untrusted`), never into the title.
//! Nothing here touches `launch_attempts`: the launch cap counts launches, not reruns or reds.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::SqlitePool;

use super::dispatch::Event;
use super::{CiConclusion, firings};
use crate::config::GatePlace;
use crate::integrations::github::{
    CiError, CiGh, CommitSha, FailedJob, ProjectRepo, Repo, RunSummary, WorkflowName,
};
use crate::task_claims::{machine, store};

/// The most of a failed log that goes into an incident: the tail, where the error is.
pub const MAX_LOG_BYTES: usize = 8 * 1024;
/// The most reds one incident remembers; older ones fall off.
const MAX_REDS: usize = 20;

/// The jobs of `.github/workflows/ci.yml` and the `scripts/gates.sh` target each one is. `plan` is
/// not a target. The test `job_names_in_ci_yml_map_to_gate_targets` keeps this in step with the file.
const JOB_TARGETS: [&str; 7] = [
    "core", "sidecars", "shell", "tauri", "hooks", "security", "broker",
];

/// A `CiGh` for the callers that can never reach `open_incident` (the local events): every call is
/// `Unavailable`.
pub struct NoCiGh;

fn unavailable() -> CiError {
    CiError::Unavailable("no GitHub client in this context".to_owned())
}

#[async_trait]
impl CiGh for NoCiGh {
    async fn runs_for_commit(
        &self,
        _repo: &Repo,
        _sha: &CommitSha,
        _workflow: &WorkflowName,
    ) -> Result<Vec<RunSummary>, CiError> {
        Err(unavailable())
    }

    async fn failed_jobs(&self, _repo: &Repo, _run: u64) -> Result<Vec<FailedJob>, CiError> {
        Err(unavailable())
    }

    async fn failed_log_tail(
        &self,
        _repo: &Repo,
        _run: u64,
        _max_bytes: usize,
    ) -> Result<String, CiError> {
        Err(unavailable())
    }

    async fn rerun_failed(&self, _repo: &Repo, _run: u64) -> Result<(), CiError> {
        Err(unavailable())
    }
}

/// One red run, as the incident records it.
#[derive(Debug, Clone)]
pub struct Red {
    pub branch: String,
    pub workflow: String,
    pub sha: String,
    pub run_id: u64,
    pub url: String,
    pub at: String,
    pub failed_jobs: Vec<String>,
    /// The tail of the failed steps' log. Untrusted.
    pub log_excerpt: Option<String>,
    /// Why there is no excerpt, when the log could not be read.
    pub log_error: Option<String>,
}

/// The `gates.sh` target a CI job name stands for: `core (windows-latest)` is `core`; `plan` and
/// any name that is not a known job is `None`.
pub fn job_target(job_name: &str) -> Option<&str> {
    let name = job_name.split(" (").next().unwrap_or(job_name).trim();
    JOB_TARGETS.contains(&name).then_some(name)
}

/// The targets of the failed jobs that `gate_targets` marks `local`: the CI failed where the local
/// gate was meant to have caught it.
pub fn local_targets_in(
    failed_jobs: &[FailedJob],
    gate_targets: Option<&BTreeMap<String, GatePlace>>,
) -> Vec<String> {
    let Some(gate_targets) = gate_targets else {
        return Vec::new();
    };
    let mut local: Vec<String> = Vec::new();
    for job in failed_jobs {
        let Some(target) = job_target(&job.name) else {
            continue;
        };
        if gate_targets.get(target) == Some(&GatePlace::Local) && !local.iter().any(|t| t == target)
        {
            local.push(target.to_owned());
        }
    }
    local
}

/// The last `max` bytes of `text`, cut on a character boundary.
fn tail(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// The incident's `detail`. `prev` is the live ticket's detail: `merge_detail` is shallow and the new
/// value wins, so the grown `reds` list is built here.
pub fn build_detail(prev: Option<&Value>, red: Red, auto_fix: bool, local: &[String]) -> Value {
    let summary = json!({
        "sha": red.sha,
        "run_id": red.run_id,
        "url": red.url,
        "at": red.at,
        "failed_jobs": red.failed_jobs,
    });
    let mut reds = prev
        .and_then(|detail| detail.get("reds"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let known = reds.iter().any(|seen| {
        seen.get("sha") == summary.get("sha") && seen.get("run_id") == summary.get("run_id")
    });
    if !known {
        reds.push(summary.clone());
    }
    if reds.len() > MAX_REDS {
        reds.drain(..reds.len() - MAX_REDS);
    }

    let mut last_red = summary.as_object().cloned().unwrap_or_default();
    last_red.insert(
        "ci_log_excerpt".to_owned(),
        red.log_excerpt
            .as_deref()
            .map_or(Value::Null, |text| Value::from(tail(text, MAX_LOG_BYTES))),
    );
    if let Some(reason) = &red.log_error {
        last_red.insert("ci_log_error".to_owned(), Value::from(reason.as_str()));
    }
    last_red.insert("untrusted".to_owned(), Value::Bool(true));

    let mut detail = json!({
        "branch": red.branch,
        "auto_fix": auto_fix,
        "workflow": red.workflow,
        "last_red": Value::Object(last_red),
        "reds": reds,
    });
    if !local.is_empty() {
        detail["environment_difference"] = json!({
            "targets": local,
            "note": "CI failed in target(s) marked local in gate_targets — this should have been \
                     caught locally; the local and CI environments differ.",
        });
    }
    detail
}

fn short(sha: &str, len: usize) -> &str {
    sha.get(..len).unwrap_or(sha)
}

/// Carries out an `open_incident` firing: `(outcome, detail)` for the firing log. Only a failed run
/// opens anything.
pub async fn run(
    pool: &SqlitePool,
    project_id: &str,
    rule: &str,
    event: &Event,
    auto_fix: bool,
    gh: &dyn CiGh,
) -> (&'static str, String) {
    let Event::CiFinished {
        sha,
        branch,
        workflow,
        conclusion: CiConclusion::Failure,
        run_id,
    } = event
    else {
        return (firings::OK, "no incident: the run did not fail".to_owned());
    };
    let Some(key) = machine::ci_red_key(branch) else {
        return (
            firings::FAILED,
            format!("`{branch}` is not a branch a ci-red incident can be keyed on"),
        );
    };
    let deadline = std::time::Instant::now() + crate::execution::git_exec::OPERATION_TIMEOUT;
    let repo = match crate::integrations::github::project_repo(pool, project_id, deadline).await {
        Ok(Some(ProjectRepo::Known { repo, .. })) => repo,
        Ok(_) => {
            return (
                firings::CI_UNAVAILABLE,
                "the project's origin is not a GitHub repository".to_owned(),
            );
        }
        Err(error) => {
            return (
                firings::FAILED,
                format!("could not read the project's repository: {error}"),
            );
        }
    };

    // The jobs and the log only enrich the incident, but without `gh` nothing confirms the red.
    let failed_jobs = match gh.failed_jobs(&repo, *run_id).await {
        Ok(jobs) => jobs,
        Err(CiError::Unavailable(reason)) => return (firings::CI_UNAVAILABLE, reason),
        Err(CiError::Failed(reason)) => {
            tracing::warn!(project_id, rule, %reason, "could not list the failed jobs; opening the incident without them");
            Vec::new()
        }
    };
    let (log_excerpt, log_error) = match gh.failed_log_tail(&repo, *run_id, MAX_LOG_BYTES).await {
        Ok(text) => (Some(text), None),
        Err(CiError::Unavailable(reason)) => return (firings::CI_UNAVAILABLE, reason),
        // An incident without an excerpt is worth more than none.
        Err(CiError::Failed(reason)) => (None, Some(reason)),
    };

    let gate_targets = crate::project::rules::autopilot(project_id)
        .ok()
        .and_then(|rules| rules.gate_targets);
    let local = local_targets_in(&failed_jobs, gate_targets.as_ref());

    let now = Utc::now();
    let prev = match store::live_by_key(pool, project_id, &key, now).await {
        Ok(row) => row.map(|row| row.detail),
        Err(error) => {
            return (
                firings::FAILED,
                format!("could not read the {} incident: {error}", key.as_str()),
            );
        }
    };
    let red = Red {
        branch: branch.clone(),
        workflow: workflow.clone(),
        sha: sha.clone(),
        run_id: *run_id,
        url: format!("https://github.com/{}/actions/runs/{run_id}", repo.as_str()),
        at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        failed_jobs: failed_jobs.iter().map(|job| job.name.clone()).collect(),
        log_excerpt,
        log_error,
    };
    let detail = build_detail(prev.as_ref(), red, auto_fix, &local);
    let title = format!("the CI of {branch} is red");
    match machine::open_ci_red_ticket(pool, project_id, branch, title, detail, now).await {
        Ok(_) => (
            firings::OK,
            format!(
                "CI red on {branch} at {}: incident opened/updated (auto_fix={auto_fix})",
                short(sha, 8)
            ),
        ),
        Err(error) => (
            firings::FAILED,
            format!("could not open the ci-red incident: {error}"),
        ),
    }
}

/// A green run closes the branch's live `ci-red` incident. Runs before the rules are evaluated and
/// whether or not a rule that opens one is on: the live ticket decides. The watch only polls the
/// newest push of a branch, which is later than any red, so no ancestry check is needed; a green of
/// the very sha that is red does not close.
pub async fn close_on_green(
    pool: &SqlitePool,
    project_id: &str,
    event: &Event,
) -> sqlx::Result<bool> {
    let Event::CiFinished {
        sha,
        branch,
        conclusion: CiConclusion::Success,
        ..
    } = event
    else {
        return Ok(false);
    };
    let Some(key) = machine::ci_red_key(branch) else {
        return Ok(false);
    };
    let now = Utc::now();
    let Some(row) = store::live_by_key(pool, project_id, &key, now).await? else {
        return Ok(false);
    };
    let red_sha = row.detail.pointer("/last_red/sha").and_then(Value::as_str);
    if red_sha == Some(sha.as_str()) {
        return Ok(false);
    }
    machine::close_ci_red_ticket(
        pool,
        project_id,
        branch,
        &format!("CI green at {}", short(sha, 12)),
        now,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automations::dispatch_with;
    use crate::automations::testkit::{FakeCiGh, SHA_1, SHA_2, github_project};
    use crate::project::rules::testing::{publish_automations, publish_rules};
    use crate::task_claims::TaskRow;
    use crate::task_claims::launch::{Due, KeyHistory, due};
    use chrono::Duration;

    fn rule(enabled: bool, auto_fix: bool, when: &str) -> String {
        format!(
            "rules:\n  - name: corrigir-ci-vermelha\n    enabled: {enabled}\n    on: {{event: ci_finished, workflow: ci.yml}}\n{when}    do: {{action: open_incident, auto_fix: {auto_fix}}}\n"
        )
    }

    fn ci(sha: &str, run_id: u64, conclusion: CiConclusion) -> Event {
        Event::CiFinished {
            sha: sha.into(),
            branch: "integ".into(),
            workflow: "ci.yml".into(),
            conclusion,
            run_id,
        }
    }

    fn red(sha: &str, run_id: u64) -> Event {
        ci(sha, run_id, CiConclusion::Failure)
    }

    async fn live(pool: &SqlitePool, project: &str) -> Option<TaskRow> {
        store::live_by_key(
            pool,
            project,
            &machine::ci_red_key("integ").unwrap(),
            Utc::now(),
        )
        .await
        .unwrap()
    }

    async fn rows_of_the_key(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM task_claims WHERE task_key = 'ci-red:integ'")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn failed(name: &str) -> FailedJob {
        FailedJob {
            name: name.to_owned(),
            conclusion: "failure".to_owned(),
        }
    }

    fn a_red(sha: &str, run_id: u64, log: Option<String>) -> Red {
        Red {
            branch: "integ".into(),
            workflow: "ci.yml".into(),
            sha: sha.into(),
            run_id,
            url: "https://example.test/run".into(),
            at: "2026-11-15T10:00:00Z".into(),
            failed_jobs: vec!["core (windows-latest)".into()],
            log_excerpt: log,
            log_error: None,
        }
    }

    // ---- pure ----------------------------------------------------------------------------------

    #[test]
    fn job_target_strips_the_os_matrix() {
        assert_eq!(job_target("core (windows-latest)"), Some("core"));
        assert_eq!(job_target("hooks (ubuntu-latest)"), Some("hooks"));
        assert_eq!(job_target("shell"), Some("shell"));
        assert_eq!(job_target("something-new"), None);
    }

    #[test]
    fn plan_is_not_a_gate_target() {
        assert_eq!(job_target("plan"), None);
    }

    #[test]
    fn job_names_in_ci_yml_map_to_gate_targets() {
        let ci_yml = include_str!("../../../.github/workflows/ci.yml");
        let document: serde_yaml::Value = serde_yaml::from_str(ci_yml).expect("ci.yml parses");
        let jobs = document
            .get("jobs")
            .and_then(serde_yaml::Value::as_mapping)
            .expect("ci.yml has jobs");
        assert!(!jobs.is_empty());
        for key in jobs.keys() {
            let name = key.as_str().expect("a job key is a string");
            if name == "plan" {
                continue;
            }
            assert!(
                job_target(name).is_some(),
                "the CI job `{name}` has no gate target: add it to JOB_TARGETS and scripts/gates.sh"
            );
        }
    }

    #[test]
    fn local_targets_in_names_only_jobs_marked_local() {
        let targets: BTreeMap<String, GatePlace> = [
            ("core".to_owned(), GatePlace::Local),
            ("shell".to_owned(), GatePlace::Ci),
        ]
        .into();
        let jobs = [failed("core (windows-latest)"), failed("shell")];
        assert_eq!(local_targets_in(&jobs, Some(&targets)), vec!["core"]);
        assert!(local_targets_in(&jobs, None).is_empty());
        let twice = [
            failed("core (windows-latest)"),
            failed("core (ubuntu-latest)"),
        ];
        assert_eq!(
            local_targets_in(&twice, Some(&targets)),
            vec!["core"],
            "a target is named once"
        );
    }

    #[test]
    fn build_detail_appends_reds_and_keeps_one_ticket_shape() {
        let first = build_detail(None, a_red(SHA_1, 1, None), true, &[]);
        assert_eq!(first["auto_fix"], Value::Bool(true));
        assert_eq!(first["branch"], "integ");
        assert_eq!(first["last_red"]["sha"], SHA_1);
        assert_eq!(first["reds"].as_array().unwrap().len(), 1);
        let second = build_detail(Some(&first), a_red(SHA_2, 2, None), false, &[]);
        assert_eq!(
            second["auto_fix"],
            Value::Bool(false),
            "the last red's auto_fix wins"
        );
        assert_eq!(second["last_red"]["sha"], SHA_2);
        let reds = second["reds"].as_array().unwrap();
        assert_eq!(reds.len(), 2);
        assert_eq!(reds[0]["sha"], SHA_1);
        assert_eq!(reds[1]["sha"], SHA_2);
        let again = build_detail(Some(&second), a_red(SHA_2, 2, None), false, &[]);
        assert_eq!(
            again["reds"].as_array().unwrap().len(),
            2,
            "the same run is not added twice"
        );
    }

    #[test]
    fn build_detail_marks_the_log_untrusted_and_bounded() {
        let log = format!("{}TAIL!", "x".repeat(20 * 1024));
        let detail = build_detail(None, a_red(SHA_1, 1, Some(log)), true, &[]);
        let excerpt = detail["last_red"]["ci_log_excerpt"].as_str().unwrap();
        assert!(excerpt.len() <= MAX_LOG_BYTES);
        assert!(excerpt.ends_with("TAIL!"), "the tail is kept, not the head");
        assert_eq!(detail["last_red"]["untrusted"], Value::Bool(true));
        // A cut inside a multi-byte character moves forward to a boundary.
        let wide = "é".repeat(MAX_LOG_BYTES);
        let detail = build_detail(None, a_red(SHA_1, 1, Some(wide)), true, &[]);
        assert!(detail["last_red"]["ci_log_excerpt"].as_str().unwrap().len() <= MAX_LOG_BYTES);
    }

    #[test]
    fn build_detail_states_the_environment_difference_only_for_local_targets() {
        let without = build_detail(None, a_red(SHA_1, 1, None), true, &[]);
        assert!(without.get("environment_difference").is_none());
        let with = build_detail(None, a_red(SHA_1, 1, None), true, &["core".to_owned()]);
        assert_eq!(with["environment_difference"]["targets"], json!(["core"]));
        assert!(
            with["environment_difference"]["note"]
                .as_str()
                .unwrap()
                .contains("should have been caught locally")
        );
        assert!(
            !with["last_red"]["ci_log_excerpt"].is_string(),
            "no log, no excerpt"
        );
    }

    // ---- with a database ------------------------------------------------------------------------

    #[tokio::test]
    async fn a_red_ci_opens_one_ci_red_incident_with_sha_run_jobs_and_excerpt() {
        let project = "ci-inc-open";
        publish_automations(project, &rule(true, true, ""));
        let pool = crate::testdb::fresh_pool().await;
        let _repo = github_project(&pool, project).await;
        let gh = FakeCiGh::new();

        let fired = dispatch_with(&pool, project, red(SHA_1, 11), &gh).await;

        assert_eq!(fired.len(), 1, "{fired:?}");
        assert_eq!(fired[0].action, "open_incident");
        assert_eq!(fired[0].outcome, "ok", "{fired:?}");
        let ticket = live(&pool, project).await.expect("an incident is open");
        assert_eq!(ticket.title, "the CI of integ is red");
        assert_eq!(ticket.detail["auto_fix"], Value::Bool(true));
        assert_eq!(ticket.detail["last_red"]["sha"], SHA_1);
        assert_eq!(ticket.detail["last_red"]["run_id"], 11);
        assert_eq!(
            ticket.detail["last_red"]["failed_jobs"],
            json!(["core (windows-latest)"])
        );
        assert_eq!(ticket.detail["last_red"]["untrusted"], Value::Bool(true));
        assert!(
            ticket.detail["last_red"]["ci_log_excerpt"]
                .as_str()
                .unwrap()
                .contains("the build broke")
        );
        assert!(
            !ticket.title.contains("broke"),
            "log text never reaches the title"
        );
    }

    #[tokio::test]
    async fn a_second_red_on_the_same_branch_appends_and_opens_no_second_ticket() {
        let project = "ci-inc-append";
        publish_automations(project, &rule(true, true, ""));
        let pool = crate::testdb::fresh_pool().await;
        let _repo = github_project(&pool, project).await;
        let gh = FakeCiGh::new();

        dispatch_with(&pool, project, red(SHA_1, 11), &gh).await;
        dispatch_with(&pool, project, red(SHA_2, 12), &gh).await;

        let ticket = live(&pool, project).await.unwrap();
        assert_eq!(ticket.report_count, 2);
        assert_eq!(ticket.detail["reds"].as_array().unwrap().len(), 2);
        assert_eq!(ticket.detail["last_red"]["sha"], SHA_2);
        assert_eq!(rows_of_the_key(&pool).await, 1);
    }

    #[tokio::test]
    async fn auto_fix_true_makes_the_ticket_launchable_and_false_does_not() {
        let project = "ci-inc-autofix";
        publish_automations(project, &rule(true, true, ""));
        let pool = crate::testdb::fresh_pool().await;
        let _repo = github_project(&pool, project).await;
        let gh = FakeCiGh::new();
        let later = Utc::now() + Duration::minutes(11);

        dispatch_with(&pool, project, red(SHA_1, 11), &gh).await;
        let launchable = live(&pool, project).await.unwrap();
        assert_eq!(
            due(&launchable, &KeyHistory::default(), false, later),
            Due::Launch
        );

        // The owner turns auto_fix off between two reds: the same ticket stops being launchable.
        publish_automations(project, &rule(true, false, ""));
        dispatch_with(&pool, project, red(SHA_2, 12), &gh).await;
        let reporting = live(&pool, project).await.unwrap();
        assert_eq!(reporting.detail["auto_fix"], Value::Bool(false));
        assert_eq!(
            due(&reporting, &KeyHistory::default(), false, later),
            Due::No
        );
    }

    #[tokio::test]
    async fn a_red_in_a_local_target_says_so_in_the_detail() {
        let project = "ci-inc-local";
        publish_automations(project, &rule(true, true, ""));
        publish_rules(project, "gate_targets: {core: local, shell: ci}\n");
        let pool = crate::testdb::fresh_pool().await;
        let _repo = github_project(&pool, project).await;
        let gh = FakeCiGh::new();

        dispatch_with(&pool, project, red(SHA_1, 11), &gh).await;

        let ticket = live(&pool, project).await.unwrap();
        assert_eq!(
            ticket.detail["environment_difference"]["targets"],
            json!(["core"])
        );
    }

    #[tokio::test]
    async fn a_green_ci_after_a_red_closes_the_incident() {
        let project = "ci-inc-green";
        publish_automations(project, &rule(true, true, ""));
        let pool = crate::testdb::fresh_pool().await;
        let _repo = github_project(&pool, project).await;
        let gh = FakeCiGh::new();
        let green = ci(SHA_2, 13, CiConclusion::Success);

        assert!(
            !close_on_green(&pool, project, &green).await.unwrap(),
            "a green with no live ticket closes nothing"
        );
        dispatch_with(&pool, project, red(SHA_1, 11), &gh).await;
        assert!(live(&pool, project).await.is_some());

        dispatch_with(&pool, project, green, &gh).await;

        assert!(live(&pool, project).await.is_none(), "the green closed it");
    }

    #[tokio::test]
    async fn a_green_of_the_same_sha_that_is_red_does_not_close() {
        let project = "ci-inc-same-sha";
        publish_automations(project, &rule(true, true, ""));
        let pool = crate::testdb::fresh_pool().await;
        let _repo = github_project(&pool, project).await;
        let gh = FakeCiGh::new();

        dispatch_with(&pool, project, red(SHA_1, 11), &gh).await;
        dispatch_with(&pool, project, ci(SHA_1, 12, CiConclusion::Success), &gh).await;

        assert!(live(&pool, project).await.is_some());
    }

    #[tokio::test]
    async fn the_rerun_does_not_consume_the_launch_cap() {
        let project = "ci-inc-cap";
        publish_automations(project, &rule(true, true, ""));
        let pool = crate::testdb::fresh_pool().await;
        let _repo = github_project(&pool, project).await;
        let gh = FakeCiGh::new();

        gh.rerun_failed(&Repo::new("acme/widgets").unwrap(), 11)
            .await
            .unwrap();
        dispatch_with(&pool, project, red(SHA_1, 11), &gh).await;

        let attempts: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(launch_attempts), 0) FROM task_claims WHERE task_key = 'ci-red:integ'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(attempts, 0, "neither the rerun nor the open is a launch");
        // Only two real launches park it.
        let ticket = live(&pool, project).await.unwrap();
        let spent = KeyHistory {
            launches_7d: 2,
            last_launch_at: Some("2000-01-01T00:00:00Z".to_owned()),
        };
        assert_eq!(due(&ticket, &spent, false, Utc::now()), Due::Park);
    }

    #[tokio::test]
    async fn the_engine_dispatches_open_incident_end_to_end() {
        let project = "ci-inc-e2e";
        publish_automations(
            project,
            &rule(true, true, "    when: {conclusion: failure}\n"),
        );
        let pool = crate::testdb::fresh_pool().await;
        let _repo = github_project(&pool, project).await;
        let gh = FakeCiGh::new();

        let fired = dispatch_with(&pool, project, red(SHA_1, 11), &gh).await;
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].outcome, "ok");
        assert_eq!(fired[0].event_key, format!("{SHA_1}:11"));
        assert!(live(&pool, project).await.is_some());

        assert!(
            dispatch_with(&pool, project, red(SHA_1, 11), &gh)
                .await
                .is_empty(),
            "the same event fires once"
        );
        let firings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM automation_firings")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(firings, 1);

        // A rule that is off opens nothing.
        let off = "ci-inc-e2e-off";
        publish_automations(off, &rule(false, true, ""));
        let _off_repo = github_project(&pool, off).await;
        assert!(
            dispatch_with(&pool, off, red(SHA_2, 12), &gh)
                .await
                .is_empty()
        );
        assert!(live(&pool, off).await.is_none());
    }

    #[tokio::test]
    async fn a_manual_push_run_never_reaches_open_incident() {
        let project = "ci-inc-manual";
        publish_automations(
            project,
            &rule(
                true,
                true,
                "    when: {pushed_by: automation, conclusion: failure}\n",
            ),
        );
        let pool = crate::testdb::fresh_pool().await;
        let _repo = github_project(&pool, project).await;
        let gh = FakeCiGh::new();

        // No automation pushed SHA_1, so this red is the owner's own push.
        assert!(
            dispatch_with(&pool, project, red(SHA_1, 11), &gh)
                .await
                .is_empty()
        );
        assert!(live(&pool, project).await.is_none());
    }

    #[tokio::test]
    async fn gh_being_unavailable_opens_no_incident_and_says_so() {
        let project = "ci-inc-unavailable";
        publish_automations(project, &rule(true, true, ""));
        let pool = crate::testdb::fresh_pool().await;
        let _repo = github_project(&pool, project).await;
        let gh = FakeCiGh::new();
        gh.set_jobs(Err(CiError::Unavailable("gh is not installed".to_owned())));

        let fired = dispatch_with(&pool, project, red(SHA_1, 11), &gh).await;

        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].outcome, "ci_unavailable");
        assert!(live(&pool, project).await.is_none());
    }

    #[tokio::test]
    async fn a_failed_log_read_still_opens_the_incident_without_an_excerpt() {
        let project = "ci-inc-nolog";
        publish_automations(project, &rule(true, true, ""));
        let pool = crate::testdb::fresh_pool().await;
        let _repo = github_project(&pool, project).await;
        let gh = FakeCiGh::new();
        gh.set_log(Err(CiError::Failed("log expired".to_owned())));

        dispatch_with(&pool, project, red(SHA_1, 11), &gh).await;

        let ticket = live(&pool, project).await.unwrap();
        assert!(ticket.detail["last_red"]["ci_log_excerpt"].is_null());
        assert_eq!(ticket.detail["last_red"]["ci_log_error"], "log expired");
    }
}
