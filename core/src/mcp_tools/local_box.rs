//! `LocalToolBox`, the in-process tool box, and its bounded reads of project files.

use super::*;

/// The `LOCAL_TOOLS` subset of this server, reachable by the in-process loop in `local_agent.rs`.
///
/// It holds a `NucleosTools` and calls the very same methods the MCP subprocess exposes, rather
/// than reimplementing them against `DaemonClient`. The tool bodies are small, but they are where
/// `create_run` resolves a project name and `vcs_ticket` decides what waiting means, and a second
/// copy of those would be a second set of answers to the same questions.
///
/// It reaches the daemon over loopback HTTP even though it runs INSIDE the daemon. That looks
/// wasteful and is the point: it is the same request the MCP subprocess makes, through the same
/// handler, with the same authorisation — so a local turn and a cloud turn cannot diverge in what a
/// tool does, only in which tools they are offered.
pub struct LocalToolBox {
    pub(super) tools: NucleosTools,
    /// Read directly, not through a tool, because the budget check below has to HAPPEN rather than
    /// be requested. See `spend_is_permitted`.
    pub(super) pool: sqlx::SqlitePool,
    /// Which names this box advertises and will dispatch: `LOCAL_TOOLS` for a chat turn,
    /// `COUNCIL_TOOLS` for a council seat.
    ///
    /// A field rather than a second type, because everything else about the two is identical — the
    /// same router, the same dispatch, the same budget gate — and a second type would be a copy of
    /// all of it kept in step by hand. What differs between a chat and a seat is exactly one list,
    /// so exactly one list is what varies.
    pub(super) allowed: &'static [&'static str],
    /// The loadout-driven doors (`BOX_ONLY_TOOLS`) this box offers on top of `allowed`. Empty
    /// unless `with_loadout` found the run's resolved tools listing them.
    ///
    /// A separate field because `allowed` is a compile-time list shared by every run of one kind,
    /// where whether THIS run holds `read_context` or `request_tool` is the run's own
    /// `run_loadout` row. Names are `'static` because they are picked out of `BOX_ONLY_TOOLS`, never
    /// taken from the loadout's strings.
    pub(super) granted: Vec<&'static str>,
}

impl LocalToolBox {
    /// Whether this box advertises and will dispatch one name: the list it was built with, or a
    /// loadout door it was granted.
    pub(super) fn offers(&self, name: &str) -> bool {
        self.allowed.contains(&name) || self.granted.contains(&name)
    }

    /// Offers the loadout doors (`BOX_ONLY_TOOLS`: `read_context`, `request_tool`) that `tools`, the
    /// run's resolved loadout, lists, and no others.
    ///
    /// This is the local twin of the Team box, which serves a loadout tool only while the run's
    /// `run_loadout` row lists it. Only `BOX_ONLY_TOOLS` can be granted this way: the rest of the
    /// team surface stays the fixed `TEAM_TOOLS`, whose members were graded one by one for a path
    /// that never passes `hooks.rs`. The daemon admits both doors by that same row on its side
    /// (`auth::loadout_admits`), so a name offered here and absent from the row would be refused
    /// there as well.
    pub fn with_loadout(mut self, tools: &[String]) -> Self {
        self.granted = BOX_ONLY_TOOLS
            .iter()
            .copied()
            .filter(|door| tools.iter().any(|listed| listed == door))
            .collect();
        self
    }

    /// Whether a tool that starts work may run.
    ///
    /// This is a precondition in the daemon and not an instruction in the prompt, and the
    /// difference is the whole point. The obvious design tells the model to call `get_budget`
    /// before `create_run` — but nothing can make a model call a tool, so that is a hope with the
    /// shape of a rule. `job.rs` already gates autonomous work on this exact function; a chat that
    /// can start a run is autonomous work with a person's sentence in front of it.
    ///
    /// It guards only the local path because that is the path this change adds. A cloud turn can
    /// still start a run without passing here, which is the behaviour it has today and a separate
    /// decision to change — one that would affect the desktop app, where somebody is watching.
    pub(super) async fn spend_is_permitted(&self) -> Result<(), String> {
        let now = chrono::Utc::now();
        match crate::governance::budget::budget_permits_new_run(&self.pool, now).await {
            crate::governance::budget::BudgetDecision::Pause { reason, .. } => Err(reason),
            crate::governance::budget::BudgetDecision::Allow => {
                // LocalToolBox has no runtime, so this reads stored quota and never warns.
                match crate::quota::quota_permits_new_run(
                    &self.pool,
                    None,
                    crate::config::models_config_now().active_runner(),
                    now,
                )
                .await
                {
                    crate::quota::QuotaDecision::Allow => Ok(()),
                    crate::quota::QuotaDecision::Pause { reason, .. } => Err(reason),
                }
            }
        }
    }

    /// A chat turn's box: `LOCAL_TOOLS`.
    pub fn new(base_url: String, token: String, pool: sqlx::SqlitePool) -> Self {
        Self::with_tools(base_url, token, pool, LOCAL_TOOLS)
    }

    /// A council seat's box: `COUNCIL_TOOLS`, which carries nothing that acts.
    pub fn for_council(base_url: String, token: String, pool: sqlx::SqlitePool) -> Self {
        Self::with_tools(base_url, token, pool, COUNCIL_TOOLS)
    }

    /// A hosted turn's box: `HOSTED_TOOLS`, the explicit allowlist for a third-party model reached
    /// over OpenRouter.
    pub fn for_hosted(base_url: String, token: String, pool: sqlx::SqlitePool) -> Self {
        Self::with_tools(base_url, token, pool, HOSTED_TOOLS)
    }

    /// A team agent's box: `TEAM_TOOLS`, and the `token` is that RUN's team key.
    ///
    /// **This box is the only barrier on the local path, and the reason it cannot be `new`.** A
    /// local turn never passes through `hooks.rs` — `local_agent::run_turn` applies only
    /// `ToolBox::permitted_after_untrusted` — while `LOCAL_TOOLS` carries `create_run` and
    /// `create_job`, both `Acts`. A specialist handed a chat's box would start runs.
    ///
    /// **Which folder `read_team_file` opens is decided by the token passed here**, never by the
    /// arguments the model supplies, and that is why the local path is loopback HTTP like every
    /// other box rather than an in-process read of the folder. A second implementation over the
    /// directory would be a second answer to "what may this run read", kept in step by hand — which
    /// is exactly what this type's doc says it exists to avoid.
    /// `run_id` is the NODE, where the token is the RUN. A local turn knows it directly — it is
    /// running inside the daemon — where a cloud turn's MCP subprocess reads it out of the
    /// environment. Both then send it the same way, so a director's authority does not depend on
    /// which machine answers. See `daemon_client::RUN_ID_HEADER`.
    pub fn for_team(base_url: String, token: String, pool: sqlx::SqlitePool, run_id: i64) -> Self {
        Self {
            pool: pool.clone(),
            allowed: TEAM_TOOLS,
            granted: Vec::new(),
            tools: NucleosTools::for_box(
                crate::daemon_client::DaemonClient::as_run(base_url, token, run_id),
                McpBox::All,
            ),
        }
    }

    pub(super) fn with_tools(
        base_url: String,
        token: String,
        pool: sqlx::SqlitePool,
        allowed: &'static [&'static str],
    ) -> Self {
        Self {
            pool,
            allowed,
            granted: Vec::new(),
            tools: NucleosTools::for_box(
                crate::daemon_client::DaemonClient::new(base_url, token),
                McpBox::All,
            ),
        }
    }
}

#[async_trait::async_trait]
impl crate::models::local_agent::ToolBox for LocalToolBox {
    /// Derived from the router's own list, so a tool's description and schema reach a local model
    /// exactly as they reach a cloud one. Writing them out by hand here is how the two would come
    /// to disagree about what `create_job` is for.
    fn schemas(&self) -> Vec<serde_json::Value> {
        NucleosTools::tool_router()
            .list_all()
            .into_iter()
            .filter(|tool| self.offers(tool.name.as_ref()))
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description.unwrap_or_default(),
                        "parameters": tool.input_schema,
                    }
                })
            })
            .collect()
    }

    fn for_run(&self, run_id: i64) -> Option<Box<dyn crate::models::local_agent::ToolBox>> {
        Some(Box::new(LocalToolBox {
            tools: NucleosTools::for_box(self.tools.client.for_run(run_id), McpBox::All)
                .sharing_boundary_with(&self.tools),
            pool: self.pool.clone(),
            allowed: self.allowed,
            granted: self.granted.clone(),
        }))
    }

    /// An unknown name resolves to `Acts` in `tool_effect`, so it is refused here — the fail-closed
    /// direction, and the same one the cloud path takes.
    fn permitted_after_untrusted(&self, name: &str) -> bool {
        tool_effect(name) != ToolEffect::Acts
    }

    async fn call(
        &self,
        name: &str,
        arguments: &serde_json::Value,
    ) -> crate::models::local_agent::ToolAnswer {
        // A name outside the offered set is refused here rather than dispatched, because the model
        // is the only thing that chose it: `self.offers` is what was advertised, and anything else
        // is a hallucinated name or a tool this turn was deliberately not given.
        if !self.offers(name) {
            return crate::models::local_agent::ToolAnswer::own(error_json(format!(
                "{name} is not a tool this conversation can use"
            )));
        }

        // Classified ONCE, before the tool runs, and the same answer gates both the filtering below
        // and the turn's latch. It used to be asked twice — once here for redaction and once by the
        // loop afterwards for the taint — against a `runs` row that can be deleted in between, so
        // the two could disagree and leave a triage run's stdout in a turn that still counted clean.
        let effect = effect_of_call(&self.pool, name, arguments).await;

        macro_rules! parsed {
            ($type:ty) => {
                match serde_json::from_value::<$type>(arguments.clone()) {
                    Ok(value) => value,
                    Err(error) => {
                        return crate::models::local_agent::ToolAnswer {
                            text: error_json(format!("bad arguments for {name}: {error}")),
                            untrusted: effect == ToolEffect::ReadsUntrusted,
                        };
                    }
                }
            };
        }

        let answer = match name {
            "list_projects" => self.tools.list_projects().await,
            "list_proposals" => self.tools.list_proposals().await,
            "list_teams" => self.tools.list_teams().await,
            "latest_models" => {
                self.tools
                    .latest_models(Parameters(parsed!(LatestModelsParams)))
                    .await
            }
            "get_budget" => self.tools.get_budget().await,
            "get_kill" => self.tools.get_kill().await,
            "get_run" => self.tools.get_run(Parameters(parsed!(IdParams))).await,
            // The fifth list a tool on `LOCAL_TOOLS` has to join, and the one nothing about adding
            // a tool reminds you of: this box dispatches to the methods DIRECTLY rather than
            // through the router, so a name offered here and unhandled here is a tool the model is
            // shown and then told does not exist. `every_offered_tool_can_be_dispatched` is what
            // caught it, exactly as the `list_files` note above says it was written to.
            "get_job" => self.tools.get_job(Parameters(parsed!(IdParams))).await,
            "list_jobs" => {
                self.tools
                    .list_jobs(Parameters(parsed!(JobsListParams)))
                    .await
            }
            "get_email_queue" => self.tools.get_email_queue().await,
            "get_email" => self.tools.get_email(Parameters(parsed!(IdParams))).await,
            // `list_files` is on `COUNCIL_TOOLS` and had no arm here, so a local seat that called
            // the tool it was offered was told the tool did not exist. It went unseen because the
            // test below walked `LOCAL_TOOLS` alone — a list `list_files` is deliberately absent
            // from — and it now walks every list this box is ever built with.
            "list_files" => self.tools.list_files(Parameters(parsed!(PathParams))).await,
            "read_team_file" => {
                self.tools
                    .read_team_file(Parameters(parsed!(PathParams)))
                    .await
            }
            // No `spend_is_permitted` guard, unlike `create_run` below: asking for an action starts
            // no model and costs nothing. What governs it is the alçada and the queue ceiling, both
            // read by the daemon on the other side of this call.
            "declare_refinement" => {
                self.tools
                    .declare_refinement(Parameters(parsed!(DeclareRefinementParams)))
                    .await
            }
            "recall" => self.tools.recall(Parameters(parsed!(RecallParams))).await,
            "propose_action" => {
                self.tools
                    .propose_action(Parameters(parsed!(ProposeActionParams)))
                    .await
            }
            "propose_teammate" => {
                self.tools
                    .propose_teammate(Parameters(parsed!(ProposeTeammateParams)))
                    .await
            }
            "report_to_owner" => {
                self.tools
                    .report_to_owner(Parameters(parsed!(ReportParams)))
                    .await
            }
            "suggest_model" => {
                self.tools
                    .suggest_model(Parameters(parsed!(SuggestModelParams)))
                    .await
            }
            // No `spend_is_permitted` guard, for `propose_action`'s reason: leaving words for a
            // colleague starts no model and costs nothing. What governs it is the per-run ceiling,
            // read by the daemon on the other side of this call.
            "send_team_note" => {
                self.tools
                    .send_team_note(Parameters(parsed!(TeamNoteParams)))
                    .await
            }
            "web_search" => {
                self.tools
                    .web_search(Parameters(parsed!(SearchParams)))
                    .await
            }
            "web_read" => self.tools.web_read(Parameters(parsed!(UrlParams))).await,
            // The two loadout doors, reachable only through `granted`. Both go over the same
            // loopback route as the MCP box, as this run (`RUN_ID_HEADER`), so the daemon decides
            // the scope: `read_context` opens only the refs of the run's own loadout, and
            // `request_tool` files a request for the run's own agent. `read_context` grades
            // `ReadsUntrusted`, so `effect` above already fenced it and marks the turn tainted.
            "read_context" => {
                self.tools
                    .read_context(Parameters(parsed!(PathParams)))
                    .await
            }
            "request_tool" => {
                self.tools
                    .request_tool(Parameters(parsed!(RequestToolParams)))
                    .await
            }
            "vcs_ticket" => {
                self.tools
                    .vcs_ticket(Parameters(parsed!(VcsTicketParams)))
                    .await
            }
            "create_run" | "create_job" => {
                if let Err(reason) = self.spend_is_permitted().await {
                    // Answered as a tool result rather than as a failure, so the model can tell the
                    // person WHY nothing started instead of falling silent or trying again.
                    return crate::models::local_agent::ToolAnswer::own(error_json(format!(
                        "no work can start right now: {reason}"
                    )));
                }
                match name {
                    "create_run" => self.tools.create_run(Parameters(parsed!(RunParams))).await,
                    _ => self.tools.create_job(Parameters(parsed!(JobParams))).await,
                }
            }
            // Unreachable while this match covers `LOCAL_TOOLS`, which
            // `every_local_tool_can_be_dispatched` is what proves.
            other => error_json(format!("{other} has no local dispatch")),
        };

        // Filtered by the same function `filter_outgoing` uses on the MCP path — not by a second
        // copy of its rule, which is how the two paths came to disagree twice in a row. Every
        // result goes through it, untrusted or not: `get_run` hands back a whole run row, stdout
        // included, where a transcript can carry a token it echoed, and the answer reaches Telegram
        // either way.
        let text = redact_rendered(&answer);

        // And fenced by the same function too, for the same reason the line above shares one.
        // `answer.text` is handed straight back to the local model as a tool result, so a page's
        // words arrive here exactly as undelimited as they would over MCP — a boundary on one path
        // and not the other would be the third time these two drifted apart.
        //
        // Keyed on `effect`, which is `effect_of_call` and not the table: this side holds the pool,
        // so it knows a triage run's stdout is a stranger's words even though `get_run` reads
        // `ReadsOwn` by name. That is the same rule the MCP side wants and cannot reach, not a
        // different one.
        let untrusted = effect == ToolEffect::ReadsUntrusted;
        let text = if untrusted {
            fence_untrusted(&text, self.tools.boundary_nonce())
        } else {
            text
        };

        crate::models::local_agent::ToolAnswer { text, untrusted }
    }
}

pub(super) fn json_result<T: Serialize>(result: Result<T, String>) -> String {
    match result {
        Ok(value) => serde_json::to_string(&value).unwrap_or_else(|e| error_json(e.to_string())),
        Err(msg) => error_json(msg),
    }
}

pub(super) fn error_json(msg: String) -> String {
    serde_json::json!({"error": msg}).to_string()
}

/// How much of a project read one tool answer carries, in bytes of text.
///
/// The CLI refuses a tool result over its token ceiling, and hands the agent an error instead of
/// any part of it. Measured on job 25's own reads: a `project_cat` of a 5,348-line file (261,688
/// characters) and a `project_grep` whose matches serialised to 54,833 characters both came back
/// as "exceeds maximum allowed tokens", so the agent that asked learned nothing from either.
/// 20,000 is under that ceiling with room to spare even at two characters a token, which source
/// code full of punctuation comes close to.
pub(super) const READ_BUDGET: usize = 20_000;

/// The longest one grep match's line is quoted. A minified file is a single line holding everything.
pub(super) const MATCH_TEXT: usize = 300;

/// PURE: `text` cut to at most `max` bytes, on a character boundary.
pub(super) fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// PURE: the part of a file one `project_cat` answer carries.
///
/// A file that fits comes back whole and untouched. Otherwise: lines from `offset` (counting from 1),
/// at most `limit` of them, within [`READ_BUDGET`], followed by one line saying which lines those
/// were and the offset to read on from. That last line is the point: a window that ends without
/// saying so reads as the end of the file.
pub(super) fn window_of_file(text: &str, offset: Option<usize>, limit: Option<usize>) -> String {
    if offset.unwrap_or(1) <= 1 && limit.is_none() && text.len() <= READ_BUDGET {
        return text.to_owned();
    }
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let first = offset.unwrap_or(1).max(1);
    if first > total {
        return format!("[the file has {total} lines; offset {first} is past its end]");
    }
    let mut shown = String::new();
    let mut last = first - 1;
    for line in lines
        .iter()
        .skip(first - 1)
        .take(limit.unwrap_or(usize::MAX).max(1))
    {
        if last >= first && shown.len() + line.len() + 1 > READ_BUDGET {
            break;
        }
        // Only ever the first line shown, since any later one breaks above: a line longer than
        // the whole budget is quoted up to it, and marked, rather than skipped as if absent.
        if line.len() > READ_BUDGET {
            shown.push_str(clip(line, READ_BUDGET));
            shown.push_str(" [line cut here]\n");
        } else {
            shown.push_str(line);
            shown.push('\n');
        }
        last += 1;
    }
    let read_on = if last < total {
        format!(
            "; call project_cat again with offset={} to read on",
            last + 1
        )
    } else {
        String::new()
    };
    format!("{shown}[lines {first}-{last} of {total}{read_on}]")
}

/// PURE: a grep answer that fits one tool result.
///
/// Every quoted line is cut to [`MATCH_TEXT`], and matches are kept in order while they fit
/// [`READ_BUDGET`]. When any were left out, the answer becomes an object that says how many there
/// were, because a list that silently stops reads as all of them.
pub(super) fn bounded_matches(matches: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Array(all) = matches else {
        return matches;
    };
    let total = all.len();
    let mut kept = Vec::new();
    let mut used = 2;
    for mut found in all {
        if let Some(text) = found.get("text").and_then(serde_json::Value::as_str)
            && text.len() > MATCH_TEXT
        {
            found["text"] = format!("{}…", clip(text, MATCH_TEXT)).into();
        }
        let size = found.to_string().len() + 1;
        if used + size > READ_BUDGET {
            break;
        }
        used += size;
        kept.push(found);
    }
    if kept.len() == total {
        return serde_json::Value::Array(kept);
    }
    serde_json::json!({
        "matches": kept,
        "shown": kept.len(),
        "total": total,
        "truncated": true,
        "note": "narrow it with `path` or a more specific query to see the rest",
    })
}
