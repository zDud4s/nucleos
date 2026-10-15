//! Browser-session methods exposed by the daemon client.

use super::*;

impl DaemonClient {
    // The browser pillar's five agent verbs (spec §6.1).
    //
    // # What is NOT here, and why each absence is load-bearing
    //
    // **No profile argument on `browser_open`.** The agent chooses WHAT to look at; the núcleo
    // chooses WHERE it happens (spec §5.3, §6.1). A parameter here would be that boundary escaping
    // to the wrong side of the wire, and what it decides is whether a stranger's page runs inside the
    // profile holding the owner's logins.
    //
    // **No run id either.** Letting a tool name one would let an agent join the browser of a run
    // that is not its own. The cost is real and worth stating: a session opened through this tool
    // gets a throwaway of its own rather than sharing its run's, so a run that opens three pages
    // gets three browsers.
    //
    // **No `browser_screenshot`, and `browser_look` is not it.** The screenshot route answers the
    // shell: a full-page PNG of whatever is there, for a person's window. What the agent gets is a
    // LOOK — viewport only, labelled with its own refs, and refused outright once a person has the
    // wheel. Two routes rather than one with a flag, because the audience decides everything else.
    //
    // The reason there was no picture at all still stands and is now a price paid on purpose:
    // `filter_outgoing` in `mcp_tools.rs` redacts TEXT, so a key drawn on a canvas crosses it. The
    // argument, and the containment, are written at the image branch itself.
    //
    // **No `browser_grant`, and no route to write one against.** The site list grows when a person
    // finishes a login and keeps the chain, and by no other means (spec §5.2).

    /// Open a browsing session. The profile is chosen by the daemon, never named here.
    pub async fn browser_open(
        &self,
        project_id: &str,
        url: &str,
        visible: bool,
    ) -> Result<Value, String> {
        self.request(reqwest::Method::POST, "/browser/open")
            .json(&serde_json::json!({ "project_id": project_id, "url": url, "visible": visible }))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// The accessibility view of a page: what is there and what it is called.
    pub async fn browser_snapshot(
        &self,
        session_id: i64,
        changes_only: bool,
        text_from: i64,
        controls_from: i64,
        find: &str,
    ) -> Result<Value, String> {
        self.request(reqwest::Method::POST, "/browser/snapshot")
            .json(&serde_json::json!({
                "session_id": session_id,
                "changes_only": changes_only,
                "text_from": text_from,
                "controls_from": controls_from,
                "find": find,
            }))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// One action against a ref from the last snapshot.
    ///
    /// A refusal by the fence comes back as an ordinary answer carrying `outcome: "refused"`, and
    /// stays one all the way to the agent (spec §6.2). Turning it into an error here would make it
    /// indistinguishable from a crashed browser, and the response to a crash is a retry.
    pub async fn browser_act(
        &self,
        session_id: i64,
        kind: &str,
        element_ref: &str,
        text: Option<String>,
        filename: Option<String>,
    ) -> Result<Value, String> {
        self.request(reqwest::Method::POST, "/browser/act")
            .json(&serde_json::json!({
                "session_id": session_id,
                "kind": kind,
                "ref": element_ref,
                "text": text.unwrap_or_default(),
                "filename": filename.unwrap_or_default(),
            }))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// The annotated picture. Answers with `image`, `mime` and the `labels` drawn on it.
    pub async fn browser_look(&self, session_id: i64) -> Result<Value, String> {
        self.request(reqwest::Method::POST, "/browser/look")
            .json(&serde_json::json!({ "session_id": session_id }))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// Ask for the wheel. Raises a proposal; it hands nothing over (spec §4.4 rule 3).
    pub async fn browser_handoff(&self, session_id: i64, reason: &str) -> Result<Value, String> {
        self.request(reqwest::Method::POST, "/browser/handoff")
            .json(&serde_json::json!({ "session_id": session_id, "reason": reason }))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// Close a session. The only one of the five that reads nothing from the page.
    pub async fn browser_close(&self, session_id: i64) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/browser/close")
            .json(&serde_json::json!({ "session_id": session_id }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status();
        Ok(serde_json::json!({ "closed": status.is_success(), "status": status.as_u16() }))
    }
}
