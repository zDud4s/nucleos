//! Work-node methods: the hierarchical task tree an agent opens, updates, closes and rehydrates.
//!
//! The daemon owns every limit (depth, open siblings, path ownership, summary length), so a refusal
//! (409/422/404) is returned as readable text and not swallowed: the model has to read why.

use super::*;

impl DaemonClient {
    /// Opens a node under `parent_id`, or a new root when it is absent. Body: `{parent_id?, kind,
    /// title, goal, owned_paths[], budget_tokens, task_id?, session_id?, chat_id?}`.
    pub async fn node_open(&self, body: &Value) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/nodes")
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, "opening a work node").await
    }

    /// Patches a node: `{status?, decision?, context_peak?}`.
    pub async fn node_update(&self, id: &str, body: &Value) -> Result<Value, String> {
        let response = self
            .request(
                reqwest::Method::PATCH,
                &format!("/nodes/{}", encode_segment(id)),
            )
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, &format!("updating work node {id}")).await
    }

    /// Closes a node: `{status, summary, handoff_path}`.
    pub async fn node_close(&self, id: &str, body: &Value) -> Result<Value, String> {
        let response = self
            .request(
                reqwest::Method::POST,
                &format!("/nodes/{}/close", encode_segment(id)),
            )
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, &format!("closing work node {id}")).await
    }

    /// The tree under `root_id`; `view` is "brief" or "full".
    pub async fn node_tree(&self, root_id: &str, view: &str) -> Result<Value, String> {
        let response = self
            .request(
                reqwest::Method::GET,
                &format!(
                    "/nodes/tree/{}?view={}",
                    encode_segment(root_id),
                    encode_segment(view)
                ),
            )
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, &format!("reading work tree {root_id}")).await
    }

    /// The root of the open tree for a session or a chat; a 404 comes back as a refusal.
    pub async fn node_active(
        &self,
        session_id: Option<&str>,
        chat_id: Option<&str>,
    ) -> Result<Value, String> {
        // reqwest is built without its `query` feature, so the query string is written by hand.
        let mut query: Vec<String> = Vec::new();
        if let Some(session_id) = session_id {
            query.push(format!("session_id={}", encode_segment(session_id)));
        }
        if let Some(chat_id) = chat_id {
            query.push(format!("chat_id={}", encode_segment(chat_id)));
        }
        let path = if query.is_empty() {
            "/nodes/active".to_owned()
        } else {
            format!("/nodes/active?{}", query.join("&"))
        };
        let response = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, "looking up the active work tree").await
    }
}

/// Percent-encodes one path segment so an id with `/`, `?` or spaces cannot change the route.
fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::encode_segment;

    #[test]
    fn a_path_segment_cannot_escape_its_route() {
        assert_eq!(encode_segment("n-1_a.b~"), "n-1_a.b~");
        assert_eq!(encode_segment("a/b?c d"), "a%2Fb%3Fc%20d");
    }
}
