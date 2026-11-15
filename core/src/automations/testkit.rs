//! Shared fixtures of the automation tests: a scripted `CiGh` and a project whose `origin` is a GitHub
//! repository. No network and no `gh` binary.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::integrations::github::{
    CiError, CiGh, CommitSha, FailedJob, Repo, RunState, RunSummary, WorkflowName,
};

/// A sha as the real `gh` prints one: 40 hex digits.
pub const SHA_1: &str = "1111111111111111111111111111111111111111";
pub const SHA_2: &str = "2222222222222222222222222222222222222222";

type RunAnswer = Result<Vec<RunSummary>, CiError>;

/// A `CiGh` that answers from a script. The answers of a sha are consumed in order; the last one
/// repeats forever. Every call is counted.
pub struct FakeCiGh {
    runs: Mutex<HashMap<String, VecDeque<RunAnswer>>>,
    polled: Mutex<Vec<String>>,
    reruns: Mutex<Vec<u64>>,
    jobs: Mutex<Option<Result<Vec<FailedJob>, CiError>>>,
    log: Mutex<Option<Result<String, CiError>>>,
}

impl FakeCiGh {
    pub fn new() -> Self {
        Self {
            runs: Mutex::new(HashMap::new()),
            polled: Mutex::new(Vec::new()),
            reruns: Mutex::new(Vec::new()),
            jobs: Mutex::new(None),
            log: Mutex::new(None),
        }
    }

    /// Appends the answers `runs_for_commit` gives for `sha`.
    pub fn script(&self, sha: &str, answers: Vec<RunAnswer>) {
        self.runs
            .lock()
            .unwrap()
            .entry(sha.to_owned())
            .or_default()
            .extend(answers);
    }

    pub fn set_jobs(&self, jobs: Result<Vec<FailedJob>, CiError>) {
        *self.jobs.lock().unwrap() = Some(jobs);
    }

    pub fn set_log(&self, log: Result<String, CiError>) {
        *self.log.lock().unwrap() = Some(log);
    }

    /// How many times `runs_for_commit` was asked, for any sha.
    pub fn polls(&self) -> usize {
        self.polled.lock().unwrap().len()
    }

    /// The shas `runs_for_commit` was asked about, in order.
    pub fn polled_shas(&self) -> Vec<String> {
        self.polled.lock().unwrap().clone()
    }

    /// The runs `rerun_failed` was asked to repeat, in order.
    pub fn reruns(&self) -> Vec<u64> {
        self.reruns.lock().unwrap().clone()
    }
}

#[async_trait]
impl CiGh for FakeCiGh {
    async fn runs_for_commit(
        &self,
        _repo: &Repo,
        sha: &CommitSha,
        _workflow: &WorkflowName,
    ) -> Result<Vec<RunSummary>, CiError> {
        self.polled.lock().unwrap().push(sha.as_str().to_owned());
        let mut scripted = self.runs.lock().unwrap();
        let Some(queue) = scripted.get_mut(sha.as_str()) else {
            return Ok(Vec::new());
        };
        if queue.len() > 1 {
            queue.pop_front().unwrap_or(Ok(Vec::new()))
        } else {
            queue.front().cloned().unwrap_or(Ok(Vec::new()))
        }
    }

    async fn failed_jobs(&self, _repo: &Repo, _run: u64) -> Result<Vec<FailedJob>, CiError> {
        self.jobs.lock().unwrap().clone().unwrap_or_else(|| {
            Ok(vec![FailedJob {
                name: "core (windows-latest)".to_owned(),
                conclusion: "failure".to_owned(),
            }])
        })
    }

    async fn failed_log_tail(
        &self,
        _repo: &Repo,
        _run: u64,
        _max_bytes: usize,
    ) -> Result<String, CiError> {
        self.log
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Ok("error: the build broke".to_owned()))
    }

    async fn rerun_failed(&self, _repo: &Repo, run: u64) -> Result<(), CiError> {
        self.reruns.lock().unwrap().push(run);
        Ok(())
    }
}

/// One run of `sha`, created now.
pub fn run_of(
    sha: &str,
    id: u64,
    state: RunState,
    conclusion: Option<&str>,
    attempt: u32,
) -> RunSummary {
    RunSummary {
        id,
        state,
        conclusion: conclusion.map(str::to_owned),
        head_sha: sha.to_owned(),
        url: format!("https://github.com/acme/widgets/actions/runs/{id}"),
        attempt,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    }
}

/// A completed run of `sha`.
pub fn finished_run(sha: &str, id: u64, conclusion: &str, attempt: u32) -> RunSummary {
    run_of(sha, id, RunState::Completed, Some(conclusion), attempt)
}

/// Puts `project_id` on the roster with a real git repository whose `origin` is a GitHub one, so
/// `project_repo` answers `acme/widgets`. Keep the returned directory alive for the test.
pub async fn github_project(pool: &SqlitePool, project_id: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temp dir");
    for args in [
        vec!["init", "-q"],
        vec!["remote", "add", "origin", "https://github.com/acme/widgets.git"],
    ] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(&args)
            .output()
            .expect("git should start");
        assert!(out.status.success(), "git {args:?} failed");
    }
    sqlx::query(
        "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES (?, 'active', ?)",
    )
    .bind(project_id)
    .bind(dir.path().to_string_lossy().into_owned())
    .execute(pool)
    .await
    .unwrap();
    dir
}

/// Records that an automation pushed `sha` to `branch` (what the CI watch looks for).
pub async fn pushed(pool: &SqlitePool, project_id: &str, branch: &str, sha: &str) {
    super::firings::record(
        pool,
        &super::firings::NewFiring {
            project_id,
            rule: "push-when-green",
            event_key: &format!("green:{sha}"),
            branch,
            sha,
            action: "push",
            outcome: super::firings::PUSHED,
            detail: "",
        },
    )
    .await
    .unwrap()
    .expect("a new push firing");
}
