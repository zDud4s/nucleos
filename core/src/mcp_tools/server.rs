//! The MCP server itself: `McpBox`, `NucleosTools`, its `ServerHandler` and `run_stdio` with its arguments.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpBox {
    All,
    JobNode(i64),
    /// An interactive (IDE/terminal) session's box: it serves `EVERY_BOX_TOOLS` and nothing else,
    /// and every call is held to this absolute worktree path. See `worktree_for_call`.
    Worktree(String),
    /// A team agent's NODE run id; the box serves `TEAM_BASE` and `TEAM_EXTRAS`.
    Team(i64),
}

pub struct NucleosTools {
    pub(super) client: crate::daemon_client::DaemonClient,
    #[expect(dead_code, reason = "tool_handler macro accesses this router field")]
    pub(super) tool_router: ToolRouter<Self>,
    /// Which box this instance serves, including the scope id needed to answer its tools.
    ///
    /// The id lives HERE, on the server, and is never a tool argument. The stdio process is launched
    /// already serving one scope; if the model could say which job it meant, one scope could name
    /// another's by asking, and the only defence left would be the model not trying — which is a
    /// hope rather than a fence.
    ///
    /// It is also the box: `JobNode(id)` serves its own named surface, while `All` serves everything
    /// except the tools that require a job. One field rather than two, because a box and the scope it
    /// serves are the same fact — a narrowed server with no scope behind it would advertise tools
    /// that cannot answer.
    pub(super) served: McpBox,
    /// The value that closes this turn's untrusted-content boundary (`<<<untrusted:NONCE>>>`).
    ///
    /// Owned by the server, so it lives exactly as long as the turn this server was built for: the
    /// MCP subprocess is one per `claude` process, a `LocalToolBox` holds one per local turn. Every
    /// place that wraps content (`filter_outgoing`, `LocalToolBox::call`) and the one place that
    /// tells the model the value (`get_info`'s legend) reads THIS field, so they cannot disagree.
    nonce: String,
}

impl NucleosTools {
    /// The server for one box, carrying the scope id needed to answer that box's tools.
    ///
    /// **`All` must keep meaning "everything except the named job-node tools".** `run_stdio` serves
    /// the cloud assistant and council today and neither passes a box; a default that quietly
    /// filtered further would take tools away with nothing failing loudly, and the symptom — half
    /// the app going silent — reads as the model behaving oddly. `sem_caixa_o_servidor_serve_tudo`
    /// is that guard.
    pub fn for_box(client: crate::daemon_client::DaemonClient, served: McpBox) -> Self {
        Self {
            client,
            tool_router: Self::tool_router(),
            served,
            nonce: fresh_nonce(),
        }
    }

    /// This turn's boundary value. See the field.
    pub(super) fn boundary_nonce(&self) -> &str {
        &self.nonce
    }

    /// Adopts another server's boundary value, for a box derived inside the same turn: one turn
    /// has one boundary, and a second value would make the model's legend wrong for half of it.
    pub(super) fn sharing_boundary_with(mut self, other: &Self) -> Self {
        self.nonce.clone_from(&other.nonce);
        self
    }

    /// The whole router: the two `#[tool_router]` impls, `tools.rs` and `ops_tools.rs`, added
    /// together. Split in two only to keep each file under the size ceiling; every caller asks
    /// for this one.
    pub(super) fn tool_router() -> ToolRouter<Self> {
        Self::work_tools() + Self::ops_tools() + Self::task_tools()
    }

    /// Whether this instance will announce and dispatch one name.
    pub(super) fn serves(&self, tool: &str) -> bool {
        served_in_box(&self.served, tool)
    }

    /// What this server ANNOUNCES, in characters of JSON.
    ///
    /// The tool schemas are the largest thing this daemon puts into a run's prompt without writing
    /// a word of it by hand: `#[tool(...)]` on each method, `schemars` on each parameter struct, and
    /// the whole block re-sent with the tool list on every request the CLI makes. Nothing in the
    /// CLI's stream reports its size — `extract_usage` reads four totals, and the `init` event's
    /// `tools` field is a list of NAMES — so if it is to be priced at all it is priced here, off the
    /// same router that answers `list_tools`.
    ///
    /// Pure — no pool, no I/O, no process. `Self::tool_router()` is the static router the
    /// `#[tool_router]` macro builds, the same one `list_tools` answers from.
    ///
    /// `serde_json::to_string(&tool).len()` rather than a hand-rolled sum of name, description and
    /// schema: the wire form is what is paid for, and its punctuation and key names are a real part
    /// of it. A tool that somehow fails to serialise counts as nothing instead of panicking — this
    /// is an estimate feeding a display, and no reading here is worth taking a daemon down for.
    pub fn advertised_schema_chars(job: Option<i64>) -> usize {
        Self::advertised_box_schema_chars(&job.map_or(McpBox::All, McpBox::JobNode))
    }

    /// [`Self::advertised_schema_chars`] for any box, which is how a Team box is priced: its scope
    /// is a node run id and not a job, so `Option<i64>` cannot name it.
    pub fn advertised_box_schema_chars(served: &McpBox) -> usize {
        Self::tool_router()
            .list_all()
            .into_iter()
            .filter(|tool| served_in_box(served, tool.name.as_ref()))
            .filter_map(|tool| serde_json::to_string(&tool).ok())
            .map(|json| json.len())
            .sum()
    }
}

#[tool_handler(name = "nucleos")]
impl ServerHandler for NucleosTools {
    /// What the server says about itself, and the only place the boundary convention is EXPLAINED.
    ///
    /// Hand-written for the same reason `call_tool` below is — `#[tool_handler]` skips generating a
    /// method the impl already defines — but the reason it has to be is different and specific: the
    /// macro's `instructions` is a string literal, and this text has to carry a value drawn
    /// per turn. That is not a detail. `filter_outgoing` wraps a stranger's words in markers a page
    /// cannot forge, and until this existed nothing told the model what those markers MEANT. A
    /// delimiter the reader has no legend for is a decoration: the mechanism was sound and the
    /// convention was private to the code that emitted it.
    ///
    /// **The value is declared here on purpose, and the trade-off is worth stating because it looks
    /// like a leak.** The model already sees the nonce on every wrapped result — that is what a
    /// boundary is — so naming it here opens no channel that was not already open. What it buys is
    /// that a forged PAIR is recognisable: a page that emits its own opening and closing markers
    /// makes the text after them look like it came from us, and a model holding a declared value can
    /// reject that mechanically instead of having to remember which value opened first.
    ///
    /// The image sentence is not padding. `browser_look` returns a picture beside its text, blocks
    /// are siblings rather than nested, and no marker can enclose one — so for a picture the
    /// boundary announces rather than delimits, and the only thing that can close that gap is
    /// saying so.
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new(
            "nucleos",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(boundary_legend(&self.nonce))
    }

    /// Every tool result leaves through here, and that is the entire point of writing it by hand.
    ///
    /// `#[tool_handler]` generates this method only when the impl does not already define one, so
    /// providing it costs nothing and takes ownership of the one path every call returns through.
    /// The alternative — calling a filter at the end of each `#[tool]` body — is a rule enforced by
    /// remembering, and the tool that forgets it is the tool nobody notices, because a missing
    /// redaction looks exactly like text that had nothing to redact.
    ///
    /// The router is built by `Self::tool_router()` here for the same reason the macro does it:
    /// that is the expression the generated body uses, and diverging from it would mean this
    /// method dispatches against a different router than `list_tools` advertises.
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        if !self.serves(&request.name) {
            return Ok(rmcp::model::CallToolResult::error(vec![
                rmcp::model::ContentBlock::text(format!(
                    "{} is not a tool this box can use",
                    request.name,
                )),
            ]));
        }
        // Read before the request is moved into the context, and that is the whole of this line:
        // `filter_outgoing` has to know WHICH tool answered, and by the line below the name is gone.
        let called = request.name.clone();
        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let result = Self::tool_router().call(tcc).await?;
        Ok(filter_outgoing(&called, result, &self.nonce))
    }

    /// What this instance announces, which is the whole router unless it is serving a box.
    ///
    /// Hand-written for the reason `call_tool` above is: `#[tool_handler]` builds its list from the
    /// static `Self::tool_router()` and cannot see instance state, so a per-instance box is not
    /// something the macro can express. The list is otherwise the macro's own, cursor included.
    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        Ok(rmcp::model::ListToolsResult {
            tools: Self::tool_router()
                .list_all()
                .into_iter()
                .filter(|tool| self.serves(&tool.name))
                .collect(),
            meta: None,
            next_cursor: None,
        })
    }
}

/// Which box this process was launched to serve, read from `--box job-node --job <id>`.
///
/// No `--box` is the broad server, excluding only the tools that require a job. That is what the
/// cloud assistant and council are launched with today and must keep getting.
///
/// A `--box` value this server does not know is a STARTUP ERROR and never a quiet fall back to the
/// full list. A launcher that misspells the box would otherwise put `create_run`, `vcs_request` and
/// `set_kill` in a job-node's hands, and nothing anywhere would say so — the failure would be
/// invisible until it was expensive.
pub fn box_from_args(args: &[String]) -> Result<McpBox, String> {
    let Some(kind) = flag_value(args, "--box") else {
        return Ok(McpBox::All);
    };
    match kind {
        "job-node" => {
            let id = flag_value(args, "--job")
                .ok_or_else(|| "--box job-node needs --job <id> to say which job".to_owned())?;
            id.parse::<i64>()
                .map(McpBox::JobNode)
                .map_err(|error| format!("--job {id} is not a job id: {error}"))
        }
        "team" => {
            let id = flag_value(args, "--run")
                .ok_or_else(|| "--box team needs --run <id> to say which run".to_owned())?;
            id.parse::<i64>()
                .map(McpBox::Team)
                .map_err(|error| format!("--run {id} is not a run id: {error}"))
        }
        "worktree" => {
            let path = flag_value(args, "--worktree").ok_or_else(|| {
                "--box worktree needs --worktree <absolute path> to say which worktree".to_owned()
            })?;
            if std::path::Path::new(path).is_absolute() {
                Ok(McpBox::Worktree(path.to_owned()))
            } else {
                Err(format!(
                    "--worktree {path} must be an absolute path (--box worktree)"
                ))
            }
        }
        _ => Err(format!(
            "--box {kind} is not a box this server knows; the boxes are `job-node`, `team` and `worktree`"
        )),
    }
}

/// The argument after `flag`, if the flag is there and something follows it.
pub(super) fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
}

/// Serves this process's stdin/stdout as the NucleOS MCP server.
///
/// `served` is the box and its scope, and `McpBox::All` — everything except the named job-node
/// tools — is what `--mcp-tools` alone means. See `NucleosTools::for_box` for why the default must
/// stay broad.
pub async fn run_stdio(
    served: McpBox,
    client: crate::daemon_client::DaemonClient,
) -> Result<(), String> {
    // The worktree box keeps its session live for the daemon. Errors are ignored and nothing is
    // printed: stdout is the protocol, and a missed beat only lets the liveness expire.
    if let McpBox::Worktree(path) = &served {
        let beat_client = client.clone();
        let path = path.clone();
        tokio::spawn(async move {
            loop {
                let _ = beat_client.verify_box_beat(&path).await;
                tokio::time::sleep(crate::verify::ide_box::BEAT_EVERY).await;
            }
        });
    }
    let tools = NucleosTools::for_box(client, served);
    let service = tools
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await
        .map_err(|e| e.to_string())?;
    service.waiting().await.map_err(|e| e.to_string())?;
    Ok(())
}
