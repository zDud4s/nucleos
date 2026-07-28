use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::{ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router};
use serde::Serialize;

pub struct NucleosTools {
    client: crate::daemon_client::DaemonClient,
    #[expect(dead_code, reason = "tool_handler macro accesses this router field")]
    tool_router: ToolRouter<Self>,
}

impl NucleosTools {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            client: crate::daemon_client::DaemonClient::from_env()?,
            tool_router: Self::tool_router(),
        })
    }
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
struct RunParams {
    project_id: String,
    prompt: String,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
struct IdParams {
    id: i64,
}

#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
struct KillParams {
    engaged: bool,
}

#[tool_router]
impl NucleosTools {
    #[tool(description = "List projects known to the NucleOS daemon")]
    async fn list_projects(&self) -> String {
        json_result(self.client.list_projects().await)
    }

    #[tool(description = "Create a NucleOS run for a project")]
    async fn create_run(
        &self,
        Parameters(RunParams { project_id, prompt }): Parameters<RunParams>,
    ) -> String {
        match self.client.create_run(&project_id, &prompt).await {
            Ok(id) => serde_json::json!({"run_id": id}).to_string(),
            Err(msg) => error_json(msg),
        }
    }

    #[tool(description = "Get a NucleOS run by ID")]
    async fn get_run(&self, Parameters(IdParams { id }): Parameters<IdParams>) -> String {
        json_result(self.client.get_run(id).await)
    }

    #[tool(description = "Cancel a NucleOS run by ID")]
    async fn cancel_run(&self, Parameters(IdParams { id }): Parameters<IdParams>) -> String {
        json_result(self.client.cancel_run(id).await)
    }

    #[tool(
        description = "Triage the email waiting in the mailbox now. Mail is collected in the \
                       background for free, but classifying it costs a run, so it only happens \
                       when asked. Returns how many messages were queued into the run; the \
                       verdicts arrive in the feed a few minutes later."
    )]
    async fn triage_email(&self) -> String {
        json_result(self.client.triage_email().await)
    }

    #[tool(
        description = "Show what the email pillar knows: mail still waiting to be triaged first, \
                       then the most recent verdicts with their class and summary."
    )]
    async fn get_email_queue(&self) -> String {
        json_result(self.client.get_email_queue().await)
    }

    #[tool(description = "List NucleOS proposals")]
    async fn list_proposals(&self) -> String {
        json_result(self.client.list_proposals().await)
    }

    #[tool(description = "Approve a NucleOS proposal by ID")]
    async fn approve_proposal(&self, Parameters(IdParams { id }): Parameters<IdParams>) -> String {
        json_result(self.client.approve_proposal(id).await)
    }

    #[tool(description = "Reject a NucleOS proposal by ID")]
    async fn reject_proposal(&self, Parameters(IdParams { id }): Parameters<IdParams>) -> String {
        json_result(self.client.reject_proposal(id).await)
    }

    #[tool(description = "Get the NucleOS autopilot budget")]
    async fn get_budget(&self) -> String {
        json_result(self.client.get_budget().await)
    }

    #[tool(description = "Get the NucleOS autopilot kill-switch state")]
    async fn get_kill(&self) -> String {
        json_result(self.client.get_kill().await)
    }

    #[tool(description = "Set the NucleOS autopilot kill-switch state")]
    async fn set_kill(&self, Parameters(KillParams { engaged }): Parameters<KillParams>) -> String {
        json_result(self.client.set_kill(engaged).await)
    }
}

#[tool_handler(name = "nucleos", instructions = "NucleOS daemon control")]
impl ServerHandler for NucleosTools {}

fn json_result<T: Serialize>(result: Result<T, String>) -> String {
    match result {
        Ok(value) => serde_json::to_string(&value).unwrap_or_else(|e| error_json(e.to_string())),
        Err(msg) => error_json(msg),
    }
}

fn error_json(msg: String) -> String {
    serde_json::json!({"error": msg}).to_string()
}

pub async fn run_stdio() -> Result<(), String> {
    let tools = NucleosTools::new()?;
    let service = tools
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await
        .map_err(|e| e.to_string())?;
    service.waiting().await.map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_expected_tool_set() {
        let names: Vec<_> = NucleosTools::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();

        assert_eq!(
            names,
            [
                "approve_proposal",
                "cancel_run",
                "create_run",
                "get_budget",
                "get_email_queue",
                "get_kill",
                "get_run",
                "list_projects",
                "list_proposals",
                "reject_proposal",
                "set_kill",
                "triage_email",
            ]
        );
    }
}
