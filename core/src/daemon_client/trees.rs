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
    /// dropped instead and nothing is relaunched (`{answer, drop: true}`). A `widen` list is sent as
    /// `widen_paths` and widens the path fence of a branch parked on a fence refusal.
    pub async fn decision_answer(
        &self,
        id: &str,
        answer: &str,
        drop: bool,
        widen: Option<&[String]>,
    ) -> Result<Value, String> {
        let path = format!(
            "/tree-decisions/{}/answer",
            super::nodes::encode_segment(id)
        );
        let response = self
            .request(reqwest::Method::POST, &path)
            .json(&decision_answer_body(answer, drop, widen))
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

/// The body of `POST /tree-decisions/{id}/answer`. `widen_paths` is present only when the caller
/// sent a non-empty list, so an answer without one reads the same as before.
fn decision_answer_body(answer: &str, drop: bool, widen: Option<&[String]>) -> Value {
    let mut body = serde_json::json!({ "answer": answer, "drop": drop });
    if let Some(paths) = widen.filter(|paths| !paths.is_empty()) {
        body["widen_paths"] = serde_json::json!(paths);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_widen_list_is_sent_as_widen_paths() {
        let paths = vec!["core/src/hooks/pretool.rs".to_owned()];
        let body = decision_answer_body("yes", false, Some(&paths));
        assert_eq!(body["widen_paths"], serde_json::json!(paths));
        assert_eq!(body["answer"], "yes");
        assert_eq!(body["drop"], false);
    }

    #[test]
    fn no_widen_list_leaves_widen_paths_out_of_the_body() {
        assert!(
            decision_answer_body("yes", false, None)
                .get("widen_paths")
                .is_none()
        );
        assert!(
            decision_answer_body("yes", false, Some(&[]))
                .get("widen_paths")
                .is_none()
        );
    }
}
