//! VCS, GitHub, and verification delivery methods.

use super::*;
use crate::integrations::github;

impl DaemonClient {
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

    /// Reads something from GitHub, through the daemon that holds the credential.
    ///
    /// The flat parameters become a typed `ReadOp` HERE, before anything is sent, so a bad
    /// repository comes back as a sentence naming the field rather than as a bare 400. The route
    /// validates again on the way in — the same check on every road, which is what
    /// `github`'s validating `Deserialize` exists for.
    pub async fn github_read(
        &self,
        operation: String,
        repo: String,
        id: Option<String>,
    ) -> Result<Value, String> {
        let op = github::ReadOp::from_request(github::ReadRequest {
            operation,
            repo,
            id,
        })?;
        self.github_request(&github::Op::Read(op)).await
    }

    /// Asks GitHub for something that changes it.
    ///
    /// The answer says which of two things happened — `ran`, or `filed_for_approval` with the number
    /// a person will see beside it. **Neither blocks**, and the caller is meant to read the status
    /// rather than assume the first.
    pub async fn github_act(&self, request: github::ActRequest) -> Result<Value, String> {
        let op = github::ActOp::from_request(request)?;
        self.github_request(&github::Op::Act(op)).await
    }

    /// The one request both halves make.
    ///
    /// A refusal here carries a message in its body (`submit_github_request` answers
    /// `(StatusCode, String)`), unlike `/vcs/requests` where the status IS the message — so the body
    /// is read first and the status is only the fallback. Told "no github token is stored", a caller
    /// knows what to do; told "403", it guesses.
    async fn github_request(&self, op: &github::Op) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/github/requests")
            .json(&serde_json::json!({ "op": op }))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(if body.trim().is_empty() {
                format!("the daemon refused the request: {status}")
            } else {
                body
            });
        }
        serde_json::from_str(&body).map_err(|error| error.to_string())
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

    /// Ask the daemon to verify a worktree. The answer is a ticket: finished when the daemon's wait
    /// ceiling allowed, otherwise with progress to be read back through `verify_status`.
    pub async fn verify(
        &self,
        kind: &str,
        scope: &str,
        worktree: Option<&str>,
        files: Option<&[String]>,
        base: Option<&str>,
        task_id: Option<&str>,
    ) -> Result<Value, String> {
        self.verify_request(
            "/verify",
            &serde_json::json!({
                "kind": kind,
                "scope": scope,
                "worktree": worktree,
                "files": files,
                "base": base,
                "task_id": task_id,
                "wait": true,
            }),
        )
        .await
    }

    /// File one workflow ledger record (`POST /workflow/records`). The body is the `record` tool's
    /// arguments as the box resolved them; the daemon validates it and picks the project.
    pub async fn record(&self, body: &Value) -> Result<Value, String> {
        self.verify_request("/workflow/records", body).await
    }

    /// Ask the daemon for a covered verification of a worktree (F3-13): `cover` makes the daemon
    /// pick the base, so the request carries neither `files` nor `base`. The answer is a ticket, as
    /// with `verify`.
    ///
    /// It submits with `wait: false`: the daemon then answers with the ticket id at once and the
    /// caller follows it through `verify_status`. With `wait: true` the daemon holds this POST for
    /// up to its own ceiling, so a caller whose limit is shorter times out with the id never read,
    /// and a ticket that exists cannot be joined.
    pub async fn verify_cover(&self, worktree: &str) -> Result<Value, String> {
        self.verify_request(
            "/verify",
            &serde_json::json!({
                "kind": "test",
                "scope": "scope",
                "worktree": worktree,
                "cover": true,
                "wait": false,
            }),
        )
        .await
    }

    /// One verification ticket's state; `wait` holds the line up to the daemon's ceiling.
    pub async fn verify_status(&self, ticket: i64, wait: bool) -> Result<Value, String> {
        self.verify_request(
            "/verify/status",
            &serde_json::json!({ "ticket": ticket, "wait": wait }),
        )
        .await
    }

    /// Tells the daemon an IDE session's verify box is still there (`POST /verify/box/beat`).
    pub async fn verify_box_beat(&self, worktree: &str) -> Result<Value, String> {
        self.verify_request(
            "/verify/box/beat",
            &serde_json::json!({ "worktree": worktree }),
        )
        .await
    }

    /// The one request every verify call makes (`verify`, `verify_status`, `verify_box_beat`, and
    /// `record`, which shares their keys and their refusal shape). A
    /// refusal carries its reason in the body (the handlers answer `(StatusCode, String)`), so the
    /// body is read first and the status is only the fallback, as in `github_request`.
    async fn verify_request(&self, path: &str, body: &Value) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, path)
            .json(body)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(if text.trim().is_empty() {
                format!("the daemon refused the request: {status}")
            } else {
                text
            });
        }
        serde_json::from_str(&text).map_err(|error| error.to_string())
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
    pub operation: crate::execution::vcs::Op,
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
        operation: crate::execution::vcs::Op::from_request(operation, source, target)?,
    })
}

/// PURE: which of the two ticket routes to call. Extracted so the choice is asserted somewhere —
/// inline, the difference between blocking and not blocking is one path segment nothing reads.
pub(super) fn ticket_path(id: i64, wait: bool) -> String {
    if wait {
        format!("/vcs/requests/{id}/wait")
    } else {
        format!("/vcs/requests/{id}")
    }
}
