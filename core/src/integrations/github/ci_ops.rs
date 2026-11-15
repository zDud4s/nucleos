//! The daemon's own typed `gh` operations for watching a CI run.
//!
//! **Deliberately outside `Op`/`ReadOp`/`ActOp`.** Those enums are what a model may ask for: they are guarded
//! by the policy tables, the ceilings, `gh_map` and the MCP tools. These four operations are called by the
//! daemon itself (the CI watch of the project automations) and authorised by the owner switching the rule on;
//! no tool, proposal or request body can name them. Extending `ReadOp::RunList` instead would have changed
//! serde that proposals persist and opened a route a model could declare.
//!
//! Everything that reaches a command line is typed and goes through `spawn_gh_argv`, so the module's rules
//! hold unchanged: no shell, the token by environment, a `--flag=value` spelling or `--` before a positional,
//! the output capped and redacted.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use super::exec::{Failure, OPERATION_TIMEOUT, Outcome, load_token, spawn_gh_argv};
use super::ops::{argv, repo_flag};
use super::policy::GithubRuntime;
use super::types::{Repo, WorkflowName};

/// A commit sha as `gh run list --commit` takes it: 7 to 64 hex digits, so it can never read as a flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitSha(String);

impl CommitSha {
    pub fn new(value: &str) -> Result<Self, String> {
        let value = value.trim();
        if !(7..=64).contains(&value.len()) || !value.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("a commit sha is 7 to 64 hex digits: {value}"));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Queued,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub id: u64,
    pub state: RunState,
    /// `None` until the run completed.
    pub conclusion: Option<String>,
    pub head_sha: String,
    pub url: String,
    /// 1 for the first execution, +1 for every re-run.
    pub attempt: u32,
    /// ISO-8601, as `gh` prints it.
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedJob {
    pub name: String,
    pub conclusion: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiError {
    /// The pillar is off, `gh` is missing or there is no token: not a fault of the CI.
    Unavailable(String),
    Failed(String),
}

impl std::fmt::Display for CiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CiError::Unavailable(reason) => write!(formatter, "unavailable: {reason}"),
            CiError::Failed(reason) => write!(formatter, "failed: {reason}"),
        }
    }
}

impl std::error::Error for CiError {}

/// The test seam of the CI watch: the real implementation shells out to `gh`.
#[async_trait]
pub trait CiGh: Send + Sync {
    async fn runs_for_commit(
        &self,
        repo: &Repo,
        sha: &CommitSha,
        workflow: &WorkflowName,
    ) -> Result<Vec<RunSummary>, CiError>;
    async fn failed_jobs(&self, repo: &Repo, run: u64) -> Result<Vec<FailedJob>, CiError>;
    /// The last `max_bytes` of the failed steps' log, cut on a UTF-8 boundary. Untrusted text.
    async fn failed_log_tail(
        &self,
        repo: &Repo,
        run: u64,
        max_bytes: usize,
    ) -> Result<String, CiError>;
    async fn rerun_failed(&self, repo: &Repo, run: u64) -> Result<(), CiError>;
}

/// The four operations, as data, so their argv is one tested function.
#[derive(Debug, Clone)]
pub(super) enum CiOp {
    RunsForCommit {
        repo: Repo,
        sha: CommitSha,
        workflow: WorkflowName,
    },
    RunJobs {
        repo: Repo,
        id: u64,
    },
    RunLogFailed {
        repo: Repo,
        id: u64,
    },
    RerunFailed {
        repo: Repo,
        id: u64,
    },
}

impl CiOp {
    pub(super) fn kind(&self) -> &'static str {
        match self {
            CiOp::RunsForCommit { .. } => "runs_for_commit",
            CiOp::RunJobs { .. } => "run_jobs",
            CiOp::RunLogFailed { .. } => "run_log_failed",
            CiOp::RerunFailed { .. } => "rerun_failed",
        }
    }
}

/// PURE: the fixed argv of an operation. Caller values are only ever `--flag=value` or after `--`.
pub(super) fn ci_argv(op: &CiOp) -> Vec<String> {
    match op {
        CiOp::RunsForCommit {
            repo,
            sha,
            workflow,
        } => argv(
            &["run", "list"],
            [
                repo_flag(repo),
                format!("--commit={}", sha.as_str()),
                format!("--workflow={}", workflow.as_str()),
                "--limit=20".to_owned(),
                "--json=databaseId,status,conclusion,headSha,url,attempt,createdAt".to_owned(),
            ],
            &[],
        ),
        CiOp::RunJobs { repo, id } => argv(
            &["run", "view"],
            [repo_flag(repo), "--json=jobs".to_owned()],
            &[id.to_string().as_str()],
        ),
        CiOp::RunLogFailed { repo, id } => argv(
            &["run", "view"],
            [repo_flag(repo), "--log-failed".to_owned()],
            &[id.to_string().as_str()],
        ),
        CiOp::RerunFailed { repo, id } => argv(
            &["run", "rerun"],
            [repo_flag(repo), "--failed".to_owned()],
            &[id.to_string().as_str()],
        ),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRun {
    database_id: u64,
    #[serde(default)]
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    head_sha: String,
    #[serde(default)]
    url: String,
    #[serde(default = "first_attempt")]
    attempt: u32,
    #[serde(default)]
    created_at: String,
}

fn first_attempt() -> u32 {
    1
}

#[derive(Deserialize)]
struct RawJobs {
    #[serde(default)]
    jobs: Vec<RawJob>,
}

#[derive(Deserialize)]
struct RawJob {
    #[serde(default)]
    name: String,
    #[serde(default)]
    conclusion: Option<String>,
}

/// PURE: `gh run list --json` output to summaries. `gh` prints an empty conclusion for a run still going.
fn parse_runs(json: &str) -> Result<Vec<RunSummary>, CiError> {
    let raw: Vec<RawRun> = serde_json::from_str(json)
        .map_err(|error| CiError::Failed(format!("could not read the run list: {error}")))?;
    Ok(raw
        .into_iter()
        .map(|run| RunSummary {
            id: run.database_id,
            state: match run.status.as_str() {
                "completed" => RunState::Completed,
                "in_progress" => RunState::InProgress,
                _ => RunState::Queued,
            },
            conclusion: run.conclusion.filter(|conclusion| !conclusion.is_empty()),
            head_sha: run.head_sha,
            url: run.url,
            attempt: run.attempt,
            created_at: run.created_at,
        })
        .collect())
}

/// PURE: `gh run view --json=jobs` output to the jobs that failed (`failure` or `timed_out`).
fn parse_failed_jobs(json: &str) -> Result<Vec<FailedJob>, CiError> {
    let raw: RawJobs = serde_json::from_str(json)
        .map_err(|error| CiError::Failed(format!("could not read the run's jobs: {error}")))?;
    Ok(raw
        .jobs
        .into_iter()
        .filter_map(|job| {
            let conclusion = job.conclusion.unwrap_or_default();
            matches!(conclusion.as_str(), "failure" | "timed_out").then_some(FailedJob {
                name: job.name,
                conclusion,
            })
        })
        .collect())
}

/// PURE: the last `max_bytes` of `text`, moved forward to the next UTF-8 boundary.
fn tail_utf8(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut start = text.len() - max_bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// PURE: why `gh` did not run, in the watch's two-way vocabulary.
fn map_failure(failure: Failure) -> CiError {
    match failure {
        Failure::NotConfigured | Failure::MissingCli | Failure::MissingToken => {
            CiError::Unavailable(failure.to_string())
        }
        other => CiError::Failed(other.to_string()),
    }
}

/// What `gh` prints (lowercase) when the token is revoked or expired, or when the repository is not
/// visible to it: not a fault of the CI, so the watch reports `ci_unavailable` and stays on.
const AUTH_MARKERS: [&str; 7] = [
    "http 401",
    "http 403",
    "http 404",
    "authentication",
    "gh auth login",
    "bad credentials",
    "could not resolve to a repository",
];

/// PURE: a non-zero `gh` exit, in the watch's two-way vocabulary. An auth-ish message is
/// `Unavailable`; anything else is a `Failed`.
fn classify_exit(kind: &str, exit_code: Option<i32>, output: &str) -> CiError {
    let message = format!(
        "gh {kind} exited with {exit_code:?}: {}",
        tail_utf8(output.trim(), 1024)
    );
    let lowered = output.to_lowercase();
    if AUTH_MARKERS.iter().any(|marker| lowered.contains(marker)) {
        CiError::Unavailable(message)
    } else {
        CiError::Failed(message)
    }
}

pub struct RealCiGh {
    pub runtime: Arc<GithubRuntime>,
}

impl RealCiGh {
    async fn run(&self, op: &CiOp) -> Result<Outcome, CiError> {
        if !self.runtime.enabled {
            return Err(map_failure(Failure::NotConfigured));
        }
        let token = load_token()
            .await
            .ok_or_else(|| map_failure(Failure::MissingToken))?;
        self.run_with(op, &token, OPERATION_TIMEOUT).await
    }

    /// Everything after the keyring and the production deadline, which a test cannot have.
    async fn run_with(
        &self,
        op: &CiOp,
        token: &str,
        deadline: std::time::Duration,
    ) -> Result<Outcome, CiError> {
        let outcome = spawn_gh_argv(&self.runtime, op.kind(), ci_argv(op), token, deadline)
            .await
            .map_err(map_failure)?;
        if outcome.exit_code != Some(0) {
            return Err(classify_exit(
                op.kind(),
                outcome.exit_code,
                &outcome.output_tail,
            ));
        }
        Ok(outcome)
    }
}

#[async_trait]
impl CiGh for RealCiGh {
    async fn runs_for_commit(
        &self,
        repo: &Repo,
        sha: &CommitSha,
        workflow: &WorkflowName,
    ) -> Result<Vec<RunSummary>, CiError> {
        let outcome = self
            .run(&CiOp::RunsForCommit {
                repo: repo.clone(),
                sha: sha.clone(),
                workflow: workflow.clone(),
            })
            .await?;
        parse_runs(&outcome.stdout)
    }

    async fn failed_jobs(&self, repo: &Repo, run: u64) -> Result<Vec<FailedJob>, CiError> {
        let outcome = self
            .run(&CiOp::RunJobs {
                repo: repo.clone(),
                id: run,
            })
            .await?;
        parse_failed_jobs(&outcome.stdout)
    }

    async fn failed_log_tail(
        &self,
        repo: &Repo,
        run: u64,
        max_bytes: usize,
    ) -> Result<String, CiError> {
        let outcome = self
            .run(&CiOp::RunLogFailed {
                repo: repo.clone(),
                id: run,
            })
            .await?;
        Ok(tail_utf8(&outcome.stdout, max_bytes).to_owned())
    }

    async fn rerun_failed(&self, repo: &Repo, run: u64) -> Result<(), CiError> {
        self.run(&CiOp::RerunFailed {
            repo: repo.clone(),
            id: run,
        })
        .await
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::super::ops::{ActOp, ReadOp};
    use super::super::policy::Policy;
    use super::*;

    fn repo() -> Repo {
        Repo::new("owner/name").expect("a repository")
    }

    fn sha() -> CommitSha {
        CommitSha::new("0123456789abcdef0123456789abcdef01234567").expect("a sha")
    }

    fn workflow() -> WorkflowName {
        WorkflowName::new("gates").expect("a workflow")
    }

    #[test]
    fn ci_argv_is_fixed_and_never_interpreted() {
        assert!(CommitSha::new("abc;rm").is_err());
        assert!(CommitSha::new("abcdef0 1").is_err());
        assert!(CommitSha::new("--help12").is_err());
        assert!(CommitSha::new("abc").is_err(), "shorter than 7");

        let sha = sha();
        let hex = sha.as_str();
        assert_eq!(
            ci_argv(&CiOp::RunsForCommit {
                repo: repo(),
                sha: sha.clone(),
                workflow: workflow()
            }),
            vec![
                "run".to_owned(),
                "list".to_owned(),
                "--repo=owner/name".to_owned(),
                format!("--commit={hex}"),
                "--workflow=gates".to_owned(),
                "--limit=20".to_owned(),
                "--json=databaseId,status,conclusion,headSha,url,attempt,createdAt".to_owned(),
            ]
        );
        assert_eq!(
            ci_argv(&CiOp::RunJobs {
                repo: repo(),
                id: 42
            }),
            [
                "run",
                "view",
                "--repo=owner/name",
                "--json=jobs",
                "--",
                "42"
            ]
        );
        assert_eq!(
            ci_argv(&CiOp::RunLogFailed {
                repo: repo(),
                id: 42
            }),
            [
                "run",
                "view",
                "--repo=owner/name",
                "--log-failed",
                "--",
                "42"
            ]
        );
        assert_eq!(
            ci_argv(&CiOp::RerunFailed {
                repo: repo(),
                id: 42
            }),
            ["run", "rerun", "--repo=owner/name", "--failed", "--", "42"]
        );
    }

    #[test]
    fn ci_ops_are_not_in_the_model_surface() {
        let kinds: Vec<&str> = ReadOp::all()
            .iter()
            .map(|op| op.kind())
            .chain(ActOp::all().iter().map(|op| op.kind()))
            .collect();
        for internal in [
            "run_jobs",
            "run_log_failed",
            "rerun_failed",
            "runs_for_commit",
        ] {
            assert!(
                !kinds.contains(&internal),
                "{internal} must stay out of the model surface"
            );
        }
    }

    #[test]
    fn runs_for_commit_parses_gh_json() {
        let json = r#"[
            {"databaseId": 11, "status": "completed", "conclusion": "failure", "headSha": "abc1234",
             "url": "https://github.com/o/n/actions/runs/11", "attempt": 2, "createdAt": "2026-11-15T10:00:00Z"},
            {"databaseId": 12, "status": "in_progress", "conclusion": "", "headSha": "abc1234",
             "url": "https://github.com/o/n/actions/runs/12", "attempt": 1, "createdAt": "2026-11-15T10:05:00Z"}
        ]"#;
        let runs = parse_runs(json).expect("parses");
        assert_eq!(
            runs[0],
            RunSummary {
                id: 11,
                state: RunState::Completed,
                conclusion: Some("failure".to_owned()),
                head_sha: "abc1234".to_owned(),
                url: "https://github.com/o/n/actions/runs/11".to_owned(),
                attempt: 2,
                created_at: "2026-11-15T10:00:00Z".to_owned(),
            }
        );
        assert_eq!(runs[1].state, RunState::InProgress);
        assert_eq!(runs[1].conclusion, None);
        assert_eq!(runs[1].attempt, 1);
    }

    #[test]
    fn failed_jobs_keeps_only_failed_conclusions() {
        let json = r#"{"jobs": [
            {"name": "build", "conclusion": "success"},
            {"name": "test", "conclusion": "failure"},
            {"name": "slow", "conclusion": "timed_out"},
            {"name": "docs", "conclusion": "skipped"},
            {"name": "pending", "conclusion": null}
        ]}"#;
        let jobs = parse_failed_jobs(json).expect("parses");
        assert_eq!(
            jobs,
            vec![
                FailedJob {
                    name: "test".to_owned(),
                    conclusion: "failure".to_owned()
                },
                FailedJob {
                    name: "slow".to_owned(),
                    conclusion: "timed_out".to_owned()
                },
            ]
        );
    }

    #[test]
    fn unparsable_json_is_a_failed_not_a_panic() {
        assert!(matches!(parse_runs("not json"), Err(CiError::Failed(_))));
        assert!(matches!(
            parse_failed_jobs("[1, 2"),
            Err(CiError::Failed(_))
        ));
    }

    #[test]
    fn missing_cli_and_missing_token_are_unavailable() {
        for failure in [
            Failure::NotConfigured,
            Failure::MissingCli,
            Failure::MissingToken,
        ] {
            assert!(matches!(map_failure(failure), CiError::Unavailable(_)));
        }
        for failure in [Failure::TimedOut, Failure::Unknown("boom".to_owned())] {
            assert!(matches!(map_failure(failure), CiError::Failed(_)));
        }
    }

    /// Fails against the old code, which made every non-zero exit a `Failed`.
    #[test]
    fn an_auth_failure_exit_is_unavailable_and_any_other_exit_is_failed() {
        for stderr in [
            "HTTP 401: Bad credentials (https://api.github.com/)",
            "gh: HTTP 403: Resource not accessible by personal access token",
            "To get started with GitHub CLI, please run:  gh auth login",
            "authentication required",
            "GraphQL: Could not resolve to a Repository with the name 'o/n'.",
            "HTTP 404: Not Found",
        ] {
            assert!(
                matches!(
                    classify_exit("run_jobs", Some(1), stderr),
                    CiError::Unavailable(_)
                ),
                "{stderr}"
            );
        }
        for stderr in ["", "HTTP 500: server error", "run 7 cannot be rerun"] {
            assert!(
                matches!(
                    classify_exit("rerun_failed", Some(1), stderr),
                    CiError::Failed(_)
                ),
                "{stderr}"
            );
        }
    }

    #[test]
    fn log_tail_keeps_the_end_and_is_bounded() {
        let text = format!("x{}", "é".repeat(10_000));
        assert_eq!(text.len(), 20_001);
        // 8191 would start inside a two-byte character: the cut moves forward to the boundary.
        let tail = tail_utf8(&text, 8191);
        assert!(tail.len() <= 8191);
        assert!(tail.len() >= 8190);
        assert!(text.ends_with(tail));
        assert_eq!(tail_utf8(&text, 8192).len(), 8192);
        assert_eq!(tail_utf8("short", 8192), "short");
    }

    /// Spawns `echo` as a program, so on Windows it needs Git's `usr/bin` on PATH.
    #[tokio::test]
    async fn real_ci_gh_passes_the_fixed_argv_to_the_binary() {
        let runtime = GithubRuntime {
            enabled: true,
            configured: true,
            binary: "echo".to_owned(),
            policy: Policy::empty(),
        };
        let real = RealCiGh {
            runtime: Arc::new(runtime),
        };
        let op = CiOp::RerunFailed {
            repo: repo(),
            id: 7,
        };
        let outcome = real
            .run_with(&op, "test-token", std::time::Duration::from_secs(20))
            .await
            .expect("echo exits zero");
        assert_eq!(
            outcome.stdout.trim(),
            "run rerun --repo=owner/name --failed -- 7"
        );
        assert_eq!(outcome.kind, "rerun_failed");

        let list = CiOp::RunsForCommit {
            repo: repo(),
            sha: sha(),
            workflow: workflow(),
        };
        let outcome = real
            .run_with(&list, "test-token", std::time::Duration::from_secs(20))
            .await
            .expect("echo exits zero");
        assert_eq!(outcome.stdout.trim(), ci_argv(&list).join(" "));
    }
}
