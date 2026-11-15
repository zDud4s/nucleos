//! Tree-job, backlog and wait-advice routes.
use super::*;

impl DaemonClient {
    /// `POST /trees`: body is `TreeBody` (id, goal, feature_branch, project_root, project_id?, contract?, dirty_owner_files?, land?).
    pub async fn tree_launch(&self, body: &Value) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/trees")
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, "launching a tree job").await
    }

    /// `GET /trees/{id}`, passed through as the daemon answers it: the job, its manifest (a parked
    /// branch carries `park.question`, and `park.last_run` after repeated ends), `nodes`,
    /// `start_refusals` and `open_decisions` (`[{id, branch, node_id, question}]`).
    pub async fn tree_status(&self, id: &str) -> Result<Value, String> {
        let path = format!("/trees/{}", super::nodes::encode_segment(id));
        let response = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, &format!("reading tree job {id}")).await
    }

    /// `POST /tree-decisions/{id}/answer`: body is `{answer}`. The answer goes through the shell's
    /// path (`answer_and_relaunch`) with the origin `conductor`; with `drop` the parked branch is
    /// dropped instead and nothing is relaunched (`{answer, drop: true}`).
    pub async fn decision_answer(
        &self,
        id: &str,
        answer: &str,
        drop: bool,
    ) -> Result<Value, String> {
        let path = format!(
            "/tree-decisions/{}/answer",
            super::nodes::encode_segment(id)
        );
        let response = self
            .request(reqwest::Method::POST, &path)
            .json(&serde_json::json!({ "answer": answer, "drop": drop }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, &format!("answering decision {id}")).await
    }

    /// `GET /tree-decisions?tree_id=&status=`: `[{id, tree, node, branch, question, status}]`.
    pub async fn decision_list(
        &self,
        tree_id: Option<&str>,
        status: Option<&str>,
    ) -> Result<Value, String> {
        let query: Vec<String> = [("tree_id", tree_id), ("status", status)]
            .into_iter()
            .filter_map(|(key, value)| {
                value.map(|value| format!("{key}={}", super::nodes::encode_segment(value)))
            })
            .collect();
        let path = if query.is_empty() {
            "/tree-decisions".to_owned()
        } else {
            format!("/tree-decisions?{}", query.join("&"))
        };
        let response = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, "listing tree decisions").await
    }

    /// `POST /trees/{id}/abandon`: body is `{reason, force?}`.
    pub async fn tree_abandon(&self, id: &str, body: &Value) -> Result<Value, String> {
        let path = format!("/trees/{}/abandon", super::nodes::encode_segment(id));
        let response = self
            .request(reqwest::Method::POST, &path)
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, &format!("abandoning tree job {id}")).await
    }

    /// `POST /trees/{id}/branch_start`: body is `{name, goal, owned_paths, prompt, layer?, base?, node_id, kind?, estimate_tokens?}`.
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
