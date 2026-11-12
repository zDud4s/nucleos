//! The six operations the supervisor prompt names verbatim.

use super::{ROUTES, authority_of};

#[derive(Clone, Debug)]
pub struct HotOp {
    pub name: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    pub example: serde_json::Value,
}

pub fn hot_ops() -> Vec<HotOp> {
    vec![
        HotOp {
            name: "create_run",
            method: "POST",
            path: "/runs",
            example: serde_json::json!({ "prompt": "Summarise the open TODOs in this project." }),
        },
        HotOp {
            name: "create_job",
            method: "POST",
            path: "/jobs",
            example: serde_json::json!({ "project_id": "<project id>", "prompt": "Fix the failing build." }),
        },
        HotOp {
            name: "message_run",
            method: "POST",
            path: "/runs/{id}/message",
            example: serde_json::json!({ "message": "Also update the changelog." }),
        },
        HotOp {
            name: "list_runs",
            method: "GET",
            path: "/runs",
            example: serde_json::json!({}),
        },
        HotOp {
            name: "approve_proposal",
            method: "POST",
            path: "/proposals/{id}/approve",
            example: serde_json::json!({}),
        },
        HotOp {
            name: "cancel_run",
            method: "POST",
            path: "/runs/{id}/cancel",
            example: serde_json::json!({}),
        },
    ]
}

/// Curated one-line purpose of each hot operation, by name.
fn curated_purpose(name: &str) -> &'static str {
    match name {
        "create_run" => "Start a run on a prompt.",
        "create_job" => "Start a job in a project.",
        "message_run" => "Send a mid-run message to a steerable run.",
        "list_runs" => "List recent runs.",
        "approve_proposal" => "Approve a pending proposal.",
        "cancel_run" => "Stop a running run.",
        _ => "",
    }
}

/// The prompt text built from the index entries plus each example.
pub fn render_prompt_block() -> String {
    let mut out = String::new();
    for op in hot_ops() {
        out.push_str(&format!(
            "- {} {} - {}",
            op.method,
            op.path,
            curated_purpose(op.name)
        ));
        if let Some(entry) = ROUTES
            .iter()
            .find(|e| e.method == op.method && e.path == op.path)
        {
            out.push_str(&format!(" (authority: {})", authority_of(entry).as_str()));
        }
        if op.method == "POST" && op.example.as_object().is_some_and(|o| !o.is_empty()) {
            out.push_str(&format!(" body: {}", op.example));
        }
        out.push('\n');
    }
    out
}
