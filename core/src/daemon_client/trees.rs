//! Tree-job, backlog and wait-advice routes.
use super::*;

impl DaemonClient {
    /// `POST /trees`: body is `TreeBody` (id, goal, feature_branch, project_root, project_id?, contract?, dirty_owner_files?).
    pub async fn tree_launch(&self, body: &Value) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/trees")
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, "launching a tree job").await
    }

    /// `GET /trees/{id}`.
    pub async fn tree_status(&self, id: &str) -> Result<Value, String> {
        let path = format!("/trees/{}", super::nodes::encode_segment(id));
        let response = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, &format!("reading tree job {id}")).await
    }

    /// `POST /trees/{id}/branch_start`: body is `{name, goal, owned_paths, prompt, layer?, base?, node_id}`.
    pub async fn tree_branch_start(&self, id: &str, body: &Value) -> Result<Value, String> {
        let path = format!("/trees/{}/branch_start", super::nodes::encode_segment(id));
        let response = self
            .request(reqwest::Method::POST, &path)
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, &format!("starting a branch of tree job {id}")).await
    }

    /// `GET /backlog`.
    pub async fn spec_backlog(&self) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::GET, "/backlog")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, "reading the spec backlog").await
    }

    /// `POST /backlog/finish`: `{spec_id?}`.
    pub async fn spec_finish(&self, body: &Value) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/backlog/finish")
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, "finishing a spec").await
    }

    /// `GET /cost/wait-advice`.
    pub async fn wait_advice(&self) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::GET, "/cost/wait-advice")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, "reading wait advice").await
    }
}
