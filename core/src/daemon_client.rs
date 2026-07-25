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

    #[test]
    fn active_project_resolves_to_worktree_mode() {
        let projects = vec![ProjectSummary {
            project_id: "active-project".into(),
            mode: Mode::Active,
            project_root: Some("C:/projects/active".into()),
            pending: 0,
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
        }];

        assert!(resolve_run_request(&projects, "rootless-project", "do the work").is_err());
    }
}
