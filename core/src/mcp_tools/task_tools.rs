//! The task-claim tools: one shared problem, one owner. See `http/tasks.rs` for the other half.

use super::*;

#[tool_router(router = task_tools, vis = "pub(super)")]
impl NucleosTools {
    #[tool(description = "List the live tasks of this project with owner and age. Check it before fixing \
                          anything outside your own task.")]
    pub(super) async fn task_list(
        &self,
        Parameters(TaskListParams { session_id }): Parameters<TaskListParams>,
    ) -> String {
        json_result(
            self.client
                .task_call("list", &self.task_body(session_id, serde_json::json!({})))
                .await,
        )
    }

    #[tool(description = "Claim a task by key before fixing something that is not yours (a red master, a \
                          failing test). Returns {won, task, holder, similar}. If won is false another \
                          owner has it: do not fix it, carry on with your own work.")]
    pub(super) async fn task_claim(
        &self,
        Parameters(TaskClaimParams {
            key,
            title,
            guard_paths,
            hint_paths,
            session_id,
        }): Parameters<TaskClaimParams>,
    ) -> String {
        let extra = serde_json::json!({ "key": key, "title": title, "guard_paths": guard_paths, "hint_paths": hint_paths });
        json_result(
            self.client
                .task_call("claim", &self.task_body(session_id, extra))
                .await,
        )
    }

    #[tool(description = "Give a claimed task back (status open) when you cannot finish it. note says why.")]
    pub(super) async fn task_release(
        &self,
        Parameters(TaskCloseParams {
            id,
            note,
            session_id,
        }): Parameters<TaskCloseParams>,
    ) -> String {
        let extra = serde_json::json!({ "id": id, "note": note });
        json_result(
            self.client
                .task_call("release", &self.task_body(session_id, extra))
                .await,
        )
    }

    #[tool(description = "Close a task you own once it is fixed. note says how.")]
    pub(super) async fn task_done(
        &self,
        Parameters(TaskCloseParams {
            id,
            note,
            session_id,
        }): Parameters<TaskCloseParams>,
    ) -> String {
        let extra = serde_json::json!({ "id": id, "note": note });
        json_result(
            self.client
                .task_call("done", &self.task_body(session_id, extra))
                .await,
        )
    }
}

impl NucleosTools {
    /// The body of every task call: the worktree of the box (never an argument) plus the stamped
    /// session id. A run or a job node has no worktree field to send: the daemon reads it off the run.
    fn task_body(
        &self,
        session_id: Option<String>,
        mut body: serde_json::Value,
    ) -> serde_json::Value {
        let worktree = match &self.served {
            McpBox::Worktree(own) => Some(own.clone()),
            _ => None,
        };
        body["worktree"] = serde_json::json!(worktree);
        body["session_id"] = serde_json::json!(session_id);
        body
    }
}
