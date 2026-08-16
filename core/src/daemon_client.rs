#![allow(dead_code)]

use serde::Serialize;
use serde_json::Value;

use crate::autopilot::{Mode, ProjectSummary};

pub struct DaemonClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl DaemonClient {
    pub fn new(base_url: String, token: String) -> Self {
        Self {
            base_url,
            token,
            http: reqwest::Client::new(),
        }
    }

    pub fn from_env() -> Result<Self, String> {
        let base_url =
            std::env::var("NUCLEOS_DAEMON_URL").unwrap_or_else(|_| "http://127.0.0.1:8791".into());
        let token = std::env::var("NUCLEOS_DAEMON_TOKEN")
            .map_err(|_| "NUCLEOS_DAEMON_TOKEN not set".to_owned())?;
        if token.is_empty() {
            return Err("NUCLEOS_DAEMON_TOKEN not set".into());
        }

        Ok(Self::new(base_url, token))
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base_url))
            .bearer_auth(&self.token)
    }

    pub async fn list_projects(&self) -> Result<Vec<ProjectSummary>, String> {
        self.request(reqwest::Method::GET, "/projects")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn create_run(&self, project_id: &str, prompt: &str) -> Result<i64, String> {
        let projects = self.list_projects().await?;
        let body = resolve_run_request(&projects, project_id, prompt)?;
        let response: Value = self
            .request(reqwest::Method::POST, "/runs")
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;

        response["id"]
            .as_i64()
            .ok_or_else(|| "create run response missing id".into())
    }

    /// Asks for a job: several runs over one worktree, rather than one context window.
    ///
    /// Refused here as well as at the route, and by **the same function** the route uses. Two
    /// copies of that table would eventually disagree, and the way they would disagree is the
    /// dangerous one: a client that let `shadow` through would produce a job that does nothing at
    /// all while reporting that it is working.
    ///
    /// What this deliberately does NOT do is send what it resolved. The `ResolvedStart` is dropped
    /// on the floor — the daemon resolves the root from its own state, because a client that named
    /// a root would be naming a directory the daemon then creates a worktree in and writes to. This
    /// call is a courtesy that saves a round trip and hands back a better sentence, never an
    /// authority.
    pub async fn create_job(
        &self,
        project_id: &str,
        prompt: &str,
        budget_usd: Option<f64>,
        max_rounds: Option<i64>,
    ) -> Result<i64, String> {
        let projects = self.list_projects().await?;
        crate::job::resolve_start(&projects, project_id)
            .map_err(|refusal| refusal.reason(project_id))?;

        let response: Value = self
            .request(reqwest::Method::POST, "/jobs")
            .json(&serde_json::json!({
                "project_id": project_id,
                "prompt": prompt,
                "budget_usd": budget_usd,
                "max_rounds": max_rounds,
            }))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;

        response["job_id"]
            .as_i64()
            .ok_or_else(|| "create job response missing job_id".into())
    }

    pub async fn get_run(&self, id: i64) -> Result<Value, String> {
        self.request(reqwest::Method::GET, &format!("/runs/{id}"))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn cancel_run(&self, id: i64) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, &format!("/runs/{id}/cancel"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_null(response).await
    }

    pub async fn list_proposals(&self) -> Result<Value, String> {
        self.request(reqwest::Method::GET, "/proposals")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn approve_proposal(&self, id: i64) -> Result<Value, String> {
        self.request(reqwest::Method::POST, &format!("/proposals/{id}/approve"))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn reject_proposal(&self, id: i64) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, &format!("/proposals/{id}/reject"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_null(response).await
    }

    pub async fn get_budget(&self) -> Result<Value, String> {
        self.request(reqwest::Method::GET, "/autopilot/budget")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// Triage whatever mail is waiting, now. Returns what was started, not the verdicts — those
    /// arrive in the feed a few minutes later.
    pub async fn triage_email(&self) -> Result<Value, String> {
        self.request(reqwest::Method::POST, "/email/triage")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// What the pillar knows about: pending mail first, then the newest verdicts.
    pub async fn get_email_queue(&self) -> Result<Value, String> {
        self.request(reqwest::Method::GET, "/email/queue")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// One message in full, body included.
    pub async fn get_email(&self, id: i64) -> Result<Value, String> {
        self.request(reqwest::Method::GET, &format!("/email/{id}"))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// Search the web, and what this machine has already read, for one query.
    ///
    /// Returns titles, URLs and snippets — never page content. The trust decision (spec §5) is made
    /// over a URL before anything is fetched, and a search that returned content would make that
    /// decision arrive too late to mean anything.
    pub async fn web_search(&self, query: &str, limit: Option<i64>) -> Result<Value, String> {
        self.request(reqwest::Method::POST, "/web/search")
            .json(&serde_json::json!({ "query": query, "limit": limit }))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// Read one page.
    ///
    /// Reading only. There is deliberately NO client method here that submits a form, posts, logs
    /// in, or sends anything — the same asymmetry `list_files` has, for a sharper reason: this is
    /// the method that fills an agent's context with text a stranger wrote, and any write sitting
    /// beside it becomes something those words can try to aim.
    pub async fn web_read(&self, url: &str) -> Result<Value, String> {
        self.request(reqwest::Method::POST, "/web/read")
            .json(&serde_json::json!({ "url": url }))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// Read one file out of the calling team run's own workspace.
    ///
    /// **The run is not an argument, and that is the whole design of this call.** Which folder gets
    /// opened comes from the `Scope::TeamRun` that authenticated the request, so the caller names a
    /// path inside its delivery and nothing else. A `team_run_id` parameter would be the caller
    /// naming what it may read, which is not a permission anything here grants itself.
    ///
    /// Reading only, like `list_files` and `web_read` beside it: the specialists do not write their
    /// answers, `team.rs` does. A write verb here would reintroduce the collision between two
    /// specialists choosing the same filename that naming the files from the core removes by
    /// construction.
    pub async fn read_team_file(&self, path: &str) -> Result<Value, String> {
        self.request(reqwest::Method::POST, "/team-files/read")
            .json(&serde_json::json!({ "path": path }))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// What is in the files folder. Reading only — there is deliberately no client method here for
    /// creating, writing, moving, deleting or downloading, so an agent cannot reach those even by
    /// mistake. The folder grew a whole file manager on the shell side; this stayed one verb.
    pub async fn list_files(&self, path: &str) -> Result<Value, String> {
        self.request(
            reqwest::Method::GET,
            &format!("/files?path={}", urlencoding_encode(path)),
        )
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())
    }

    pub async fn get_kill(&self) -> Result<Value, String> {
        self.request(reqwest::Method::GET, "/autopilot/kill")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn set_kill(&self, engaged: bool) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/autopilot/kill")
            .json(&serde_json::json!({"engaged": engaged}))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_null(response).await
    }

    /// Queues one operation and waits for it, up to the daemon's own ceiling.
    ///
    /// Two calls rather than one: the submit answers as soon as the row is committed, and the wait is
    /// a separate route so a caller that only wants a ticket is not made to block for it. What this
    /// method does is spend the wait on the caller's behalf, which is spec decision 3's hybrid —
    /// block for a while, then hand back a ticket rather than a timeout.
    pub async fn vcs_request(
        &self,
        project_id: &str,
        operation: &str,
        source: Option<&str>,
        target: Option<&str>,
    ) -> Result<Value, String> {
        let body = vcs_submit_body(project_id, operation, source, target)?;
        let response = self
            .request(reqwest::Method::POST, "/vcs/requests")
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        // A refusal is a bare status with an empty body (`submit_vcs_request` returns
        // `Result<Json<Ticket>, StatusCode>`), so `.json()` on it would fail with a decoding error
        // that names nothing useful. The status IS the message: 404 is an unknown project, 422 a
        // project whose recorded root is not a repository or a body the queue will not accept, 403 a
        // token that may not queue.
        if !response.status().is_success() {
            return Err(format!(
                "the daemon refused the request: {}",
                response.status()
            ));
        }
        let submitted: Value = response.json().await.map_err(|e| e.to_string())?;

        match submitted["id"].as_i64() {
            Some(id) => self.vcs_ticket(id, true).await,
            None => Ok(submitted),
        }
    }

    /// One queued operation's ticket: what was asked for and how it ended. `wait` spends up to the
    /// daemon's ceiling waiting for it to finish; without it the answer is whatever the row says now.
    pub async fn vcs_ticket(&self, id: i64, wait: bool) -> Result<Value, String> {
        self.request(reqwest::Method::GET, &ticket_path(id, wait))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }
}

/// The submit body, built and validated before anything is sent.
///
/// Named `VcsSubmitBody` and not `VcsRequestBody` on purpose: `http.rs` has a private type by that
/// second name for the *receiving* side of the same wire shape, and two identically named structs at
/// opposite ends of one request are a trap for whoever changes one of them.
#[derive(Serialize)]
pub struct VcsSubmitBody {
    pub project_id: String,
    pub operation: crate::vcs::Op,
}

/// PURE: the body for one queue submission, or why the request is not one.
pub fn vcs_submit_body(
    project_id: &str,
    operation: &str,
    source: Option<&str>,
    target: Option<&str>,
) -> Result<VcsSubmitBody, String> {
    Ok(VcsSubmitBody {
        project_id: project_id.to_owned(),
        operation: crate::vcs::Op::from_request(operation, source, target)?,
    })
}

/// PURE: which of the two ticket routes to call. Extracted so the choice is asserted somewhere —
/// inline, the difference between blocking and not blocking is one path segment nothing reads.
fn ticket_path(id: i64, wait: bool) -> String {
    if wait {
        format!("/vcs/requests/{id}/wait")
    } else {
        format!("/vcs/requests/{id}")
    }
}

/// Percent-encodes a query value.
///
/// Written out rather than pulled in: a folder name carrying `&`, `#` or `..` would otherwise
/// arrive as a different request than the one intended — and `..` reaching the daemon's path guard
/// as a *separate parameter* rather than part of the path is exactly how a check gets skipped.
fn urlencoding_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[derive(Serialize)]
pub struct CreateRunBody {
    pub prompt: String,
    pub project_id: String,
    pub cwd: String,
    pub mode: String,
}

pub fn resolve_run_request(
    projects: &[ProjectSummary],
    project_id: &str,
    prompt: &str,
) -> Result<CreateRunBody, String> {
    let project = projects
        .iter()
        .find(|project| project.project_id == project_id)
        .ok_or_else(|| format!("unknown project: {project_id}"))?;
    let cwd = project
        .project_root
        .clone()
        .ok_or_else(|| format!("project {project_id} has no root (mode off?)"))?;
    let mode = match project.mode {
        Mode::Active => "worktree",
        Mode::Shadow => "shadow",
        Mode::Off => return Err(format!("project {project_id} is off")),
    };

    Ok(CreateRunBody {
        prompt: prompt.into(),
        project_id: project_id.into(),
        cwd,
        mode: mode.into(),
    })
}

async fn json_or_null(response: reqwest::Response) -> Result<Value, String> {
    let body = response.bytes().await.map_err(|e| e.to_string())?;
    if body.is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_slice(&body).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder name reaches the daemon as a query value. Left raw, `&` would start a second
    /// parameter and `..` would arrive somewhere the path guard never inspects — which is how a
    /// check gets skipped rather than defeated.
    #[test]
    fn a_folder_name_cannot_rewrite_the_request_it_travels_in() {
        assert_eq!(urlencoding_encode("BACMAT/2026"), "BACMAT%2F2026");
        assert_eq!(urlencoding_encode(".."), "..");
        assert_eq!(urlencoding_encode("../../etc"), "..%2F..%2Fetc");
        assert_eq!(urlencoding_encode("a&path=b"), "a%26path%3Db");
        assert_eq!(urlencoding_encode("com espaço"), "com%20espa%C3%A7o");
        // Unreserved characters are left alone, or every path would be unreadable in a log.
        assert_eq!(
            urlencoding_encode("relatorio-2026_v1.docx"),
            "relatorio-2026_v1.docx"
        );
    }

    /// Resolving a JOB is not resolving a run, and this pins the two apart.
    ///
    /// `create_job` calls `job::resolve_start` rather than `resolve_run_request` beside it, and the
    /// difference is the shadow row. A run may be plan-only, so shadow is a legitimate run mode. A
    /// job may not: its plan node has to WRITE the plan.json its queue comes from, so a job
    /// accepted for a shadow project would spend the night doing nothing and reporting that it was
    /// working. If the two are ever "unified" for looking alike, this assertion is what says no.
    ///
    /// No network in either direction: both are pure functions over a roster.
    #[test]
    fn resolver_um_job_nao_e_resolver_um_run() {
        let projects = vec![
            summary_for("shadow-project", Mode::Shadow, Some("C:/projects/shadow")),
            summary_for("live-project", Mode::Active, Some("C:/projects/live")),
        ];

        // The run path accepts shadow, and is right to.
        assert_eq!(
            resolve_run_request(&projects, "shadow-project", "inspect")
                .unwrap()
                .mode,
            "shadow"
        );
        // The job path refuses it, and says which state it found.
        let refusal = crate::job::resolve_start(&projects, "shadow-project")
            .expect_err("a job must not be accepted for a plan-only project");
        assert!(refusal.reason("shadow-project").contains("shadow"));

        // An unknown project fails on both paths, so the courtesy check in `create_job` cannot be
        // the thing that lets one through.
        assert!(resolve_run_request(&projects, "nao-existe", "x").is_err());
        assert_eq!(
            crate::job::resolve_start(&projects, "nao-existe"),
            Err(crate::job::StartRefusal::UnknownProject)
        );

        // And the one that must still work, without which the assertions above pass by refusing
        // everything.
        assert_eq!(
            crate::job::resolve_start(&projects, "live-project")
                .unwrap()
                .project_root,
            "C:/projects/live"
        );
    }

    fn summary_for(project_id: &str, mode: Mode, root: Option<&str>) -> ProjectSummary {
        ProjectSummary {
            project_id: project_id.into(),
            mode,
            project_root: root.map(str::to_string),
            pending: 0,
            classes_ready: 0,
            classes_total: 0,
            withheld_classes_ready: 0,
            promotable: false,
            open_proposals: 0,
            wip_limit: Some(3),
            queue_full: false,
        }
    }

    #[test]
    fn a_request_body_carries_the_typed_operation() {
        let body = vcs_submit_body("nucleos", "merge", Some("feature"), Some("master")).unwrap();

        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({
                "project_id": "nucleos",
                "operation": {"op": "merge", "source": "feature", "target": "master"}
            })
        );
    }

    /// The validation lives in `vcs::Op`, and this asserts the client actually consults it rather than
    /// posting whatever it was handed — a caller that named an operation the queue does not have is told
    /// so without a round trip, and the daemon never sees a request it would only reject.
    #[test]
    fn an_operation_the_queue_does_not_have_never_reaches_the_daemon() {
        // One the queue has decided against, rather than one it has not got to: nothing is deferred
        // any more, so a name that fails here fails for a reason the caller can act on.
        assert!(vcs_submit_body("nucleos", "pull", Some("feature"), Some("origin")).is_err());
        assert!(vcs_submit_body("nucleos", "tag", Some("main"), Some("-d")).is_err());
        assert!(vcs_submit_body("nucleos", "merge", Some("-f"), Some("master")).is_err());
        // The same guard on the operation the queue DID learn, because a second variant is a second
        // route to argv and inherits none of the first one's checks by being next to it.
        assert!(vcs_submit_body("nucleos", "push", Some("main"), Some("--exec=x")).is_err());
    }

    /// `source` is what moves and `target` is where it goes, for both operations — so a push reads
    /// "branch, then remote". Asserted on the wire shape because that convention is the one thing a
    /// caller cannot infer from the parameter names alone, and swapping the two arguments at the
    /// call site would otherwise produce a request that queues and pushes the wrong thing.
    #[test]
    fn a_push_body_names_the_branch_as_source_and_the_remote_as_target() {
        let body = vcs_submit_body("nucleos", "push", Some("main"), Some("origin")).unwrap();

        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({
                "project_id": "nucleos",
                "operation": {"op": "push", "remote": "origin", "branch": "main"}
            })
        );
    }

    /// The two ticket routes differ only in a suffix, and nothing else in the suite would notice if they
    /// were swapped: both return the same shape, and one merely blocks longer.
    #[test]
    fn waiting_and_not_waiting_are_different_routes() {
        assert_eq!(ticket_path(7, false), "/vcs/requests/7");
        assert_eq!(ticket_path(7, true), "/vcs/requests/7/wait");
    }

    #[test]
    fn active_project_resolves_to_worktree_mode() {
        let projects = vec![ProjectSummary {
            project_id: "active-project".into(),
            mode: Mode::Active,
            project_root: Some("C:/projects/active".into()),
            pending: 0,
            classes_ready: 0,
            classes_total: 0,
            withheld_classes_ready: 0,
            promotable: false,
            open_proposals: 0,
            wip_limit: Some(3),
            queue_full: false,
        }];

        let body = resolve_run_request(&projects, "active-project", "do the work").unwrap();

        assert_eq!(body.mode, "worktree");
        assert_eq!(body.cwd, "C:/projects/active");
    }

    #[test]
    fn shadow_project_resolves_to_shadow_mode() {
        let projects = vec![ProjectSummary {
            project_id: "shadow-project".into(),
            mode: Mode::Shadow,
            project_root: Some("C:/projects/shadow".into()),
            pending: 0,
            classes_ready: 0,
            classes_total: 0,
            withheld_classes_ready: 0,
            promotable: false,
            open_proposals: 0,
            wip_limit: Some(3),
            queue_full: false,
        }];

        let body = resolve_run_request(&projects, "shadow-project", "inspect the work").unwrap();

        assert_eq!(body.mode, "shadow");
        assert_eq!(body.cwd, "C:/projects/shadow");
    }

    #[test]
    fn off_project_is_rejected() {
        let projects = vec![ProjectSummary {
            project_id: "off-project".into(),
            mode: Mode::Off,
            project_root: Some("C:/projects/off".into()),
            pending: 0,
            classes_ready: 0,
            classes_total: 0,
            withheld_classes_ready: 0,
            promotable: false,
            open_proposals: 0,
            wip_limit: Some(3),
            queue_full: false,
        }];

        assert!(resolve_run_request(&projects, "off-project", "do the work").is_err());
    }

    #[test]
    fn unknown_project_is_rejected() {
        let projects = vec![ProjectSummary {
            project_id: "known-project".into(),
            mode: Mode::Active,
            project_root: Some("C:/projects/known".into()),
            pending: 0,
            classes_ready: 0,
            classes_total: 0,
            withheld_classes_ready: 0,
            promotable: false,
            open_proposals: 0,
            wip_limit: Some(3),
            queue_full: false,
        }];

        assert!(resolve_run_request(&projects, "unknown-project", "do the work").is_err());
    }

    #[test]
    fn project_without_root_is_rejected() {
        let projects = vec![ProjectSummary {
            project_id: "rootless-project".into(),
            mode: Mode::Active,
            project_root: None,
            pending: 0,
            classes_ready: 0,
            classes_total: 0,
            withheld_classes_ready: 0,
            promotable: false,
            open_proposals: 0,
            wip_limit: Some(3),
            queue_full: false,
        }];

        assert!(resolve_run_request(&projects, "rootless-project", "do the work").is_err());
    }
}
