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
