#![allow(dead_code)]

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::autopilot::{Mode, ProjectSummary};

/// The header that says WHICH NODE is calling.
///
/// A team's key names the run, not the node — a director and its specialists hold the identical
/// token, which is right, because the key belongs to the run. Some questions are about the node
/// anyway ("is this the director?", "which item asked for this?"), and this is how the answer
/// travels.
///
/// **It is not the model naming itself.** The value comes from `NUCLEOS_RUN_ID`, which the daemon
/// writes into the node's environment (`runs::run_env`) and which this client — the daemon's own
/// code — reads and sends. A model calls a tool with the parameters that tool declares; it never
/// builds the HTTP request. Anything able to forge this header already holds
/// `NUCLEOS_DAEMON_TOKEN`, which arrives by the identical route, so it buys an attacker nothing
/// they did not have. The daemon still checks that the node named belongs to the run the key
/// authenticated, so a stale or foreign value resolves to nobody rather than to somebody else.
pub const RUN_ID_HEADER: &str = "x-nucleos-run-id";

/// The port a NucleOS daemon binds when nothing says otherwise.
///
/// It lived as a literal in nine places, which was honest while a machine ran exactly one daemon.
pub const DEFAULT_PORT: u16 = 8791;

/// The environment variable that moves a daemon off `DEFAULT_PORT`.
pub const PORT_VAR: &str = "NUCLEOS_PORT";

/// The environment variable that moves a daemon's database and logs somewhere of their own.
pub const DATA_DIR_VAR: &str = "NUCLEOS_DATA_DIR";

/// PURE: the port to bind, given what the environment said.
///
/// Anything that is not a usable port falls back to the default rather than refusing to start —
/// see this function's test for why a daemon that will not come up is the worse failure. Zero is
/// refused along with the nonsense: to the OS it means "any free port", and a daemon whose address
/// cannot be predicted is unreachable by every client that was told the default.
pub fn port_from(configured: Option<&str>) -> u16 {
    configured
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .unwrap_or(DEFAULT_PORT)
}

/// PURE: the URL a daemon on `port` is reached at.
///
/// Derived and never written beside the port. Two literals for one fact is what lets a daemon bind
/// one port and tell everything it launches to call back on another, which presents as every tool
/// being broken at once.
pub fn url_for(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// PURE: whether this process is the machine's daemon, or a second one run beside it.
///
/// A secondary must not register the logon task — that would point the machine's autostart at
/// whatever build is under test — and must not start the sidecars, which would put a second copy
/// of every integration on the same accounts.
///
/// An empty value is not an override: it is what a shell leaves behind when a variable is exported
/// and never given one, and reading it as "secondary" would quietly take autostart and sidecars
/// away from a real daemon.
pub fn is_primary(port: Option<&str>, data_dir: Option<&str>) -> bool {
    let stated = |value: Option<&str>| value.is_some_and(|value| !value.is_empty());
    !stated(port) && !stated(data_dir)
}

/// The port this process binds, read from the environment.
pub fn port() -> u16 {
    port_from(std::env::var(PORT_VAR).ok().as_deref())
}

/// The URL this daemon hands to everything it launches.
///
/// One reader for the whole process, so a secondary instance cannot bind one port and advertise
/// another.
pub fn daemon_url() -> String {
    url_for(port())
}

#[derive(Clone)]
pub struct DaemonClient {
    base_url: String,
    token: String,
    /// The node this client speaks for, when it speaks for one. `None` for the desktop app and for
    /// every caller whose identity is fully described by its token.
    run_id: Option<i64>,
    http: reqwest::Client,
}

impl DaemonClient {
    pub fn new(base_url: String, token: String) -> Self {
        Self {
            base_url,
            token,
            run_id: None,
            http: reqwest::Client::new(),
        }
    }

    /// A client that speaks for one node of one run. See `RUN_ID_HEADER`.
    pub fn as_run(base_url: String, token: String, run_id: i64) -> Self {
        Self {
            run_id: Some(run_id),
            ..Self::new(base_url, token)
        }
    }

    /// The same client, speaking for one run from now on.
    ///
    /// The id comes from the daemon's own `runs` row, never from a model. See `RUN_ID_HEADER`.
    pub fn for_run(&self, run_id: i64) -> Self {
        Self {
            base_url: self.base_url.clone(),
            token: self.token.clone(),
            run_id: Some(run_id),
            http: self.http.clone(),
        }
    }

    pub fn from_env() -> Result<Self, String> {
        // `NUCLEOS_DAEMON_URL` still wins: the daemon writes it into everything it launches, and
        // it is the only value that survives a client running somewhere the daemon's own
        // environment does not reach. The fallback is derived rather than written out again, so a
        // secondary instance's tools reach the secondary rather than the machine's real daemon.
        let base_url = std::env::var("NUCLEOS_DAEMON_URL").unwrap_or_else(|_| daemon_url());
        let token = std::env::var("NUCLEOS_DAEMON_TOKEN")
            .map_err(|_| "NUCLEOS_DAEMON_TOKEN not set".to_owned())?;
        if token.is_empty() {
            return Err("NUCLEOS_DAEMON_TOKEN not set".into());
        }

        Ok(Self {
            // Absent for the runs that have no use for it, and unparseable is the same as absent:
            // a malformed value must not become a node id that happens to be valid.
            run_id: std::env::var("NUCLEOS_RUN_ID")
                .ok()
                .and_then(|id| id.parse().ok()),
            ..Self::new(base_url, token)
        })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let request = self
            .http
            .request(method, format!("{}{path}", self.base_url))
            .bearer_auth(&self.token);
        match self.run_id {
            Some(run_id) => request.header(RUN_ID_HEADER, run_id.to_string()),
            None => request,
        }
    }

    pub async fn list_projects(&self) -> Result<Vec<ProjectSummary>, String> {
        self.request(reqwest::Method::GET, "/projects")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// The catalogue of departments, handed back as the daemon writes it.
    ///
    /// `Value` rather than a typed roster on purpose. `ProjectSummary` above is typed because
    /// `resolve_run_request` and `job::resolve_start` DECIDE on its fields; nothing in this client
    /// decides on a team's, so a struct here would be a second copy of `team::TeamView` that could
    /// only drift from it. Whether the named team exists is answered by `job::start`, at the row.
    pub async fn list_teams(&self) -> Result<Value, String> {
        self.request(reqwest::Method::GET, "/teams")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn create_run(&self, project_id: &str, prompt: &str) -> Result<i64, String> {
        let projects = self.list_projects().await?;
        let body = resolve_run_request(&projects, project_id, prompt)?;
        let response: Value = self
            .request(reqwest::Method::POST, "/runs")
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;

        response["id"]
            .as_i64()
            .ok_or_else(|| "create run response missing id".into())
    }

    /// Asks for a job: several runs over one worktree, rather than one context window.
    ///
    /// Refused here as well as at the route, and by **the same function** the route uses. Two
    /// copies of that table would eventually disagree, and the way they would disagree is the
    /// dangerous one: a client that let `shadow` through would produce a job that does nothing at
    /// all while reporting that it is working.
    ///
    /// What this deliberately does NOT do is send what it resolved. The `ResolvedStart` is dropped
    /// on the floor — the daemon resolves the root from its own state, because a client that named
    /// a root would be naming a directory the daemon then creates a worktree in and writes to. This
    /// call is a courtesy that saves a round trip and hands back a better sentence, never an
    /// authority.
    pub async fn create_job(
        &self,
        project_id: &str,
        prompt: &str,
        budget_usd: Option<f64>,
        max_rounds: Option<i64>,
        team_id: Option<&str>,
    ) -> Result<i64, String> {
        let projects = self.list_projects().await?;
        crate::job::resolve_start(&projects, project_id)
            .map_err(|refusal| refusal.reason(project_id))?;

        // Whether the team EXISTS is deliberately not checked here, unlike the project above, and
        // the asymmetry is the same one `job::start` states at the route: the catalogue can change
        // between a request being written and it landing, so the answer has to be read where the row
        // is made. A courtesy check here would only be able to disagree with it.
        //
        // Built up rather than written as one literal because `team_id` is OMITTED when absent
        // instead of travelling as `null`. `CreateJobRequest::team_id` is `#[serde(default)]` and
        // reads the two the same way, so this buys exactly one thing: a job asked for with no team
        // sends the body it sent before teams existed, byte for byte. That turns "this parameter
        // changed nothing for callers who do not use it" from a claim into something a test holds.
        let mut body = serde_json::json!({
            "project_id": project_id,
            "prompt": prompt,
            "budget_usd": budget_usd,
            "max_rounds": max_rounds,
        });
        if let Some(team_id) = team_id {
            body["team_id"] = Value::String(team_id.to_owned());
        }

        let response: Value = self
            .request(reqwest::Method::POST, "/jobs")
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;

        response["job_id"]
            .as_i64()
            .ok_or_else(|| "create job response missing job_id".into())
    }

    pub async fn get_run(&self, id: i64) -> Result<Value, String> {
        self.request(reqwest::Method::GET, &format!("/runs/{id}"))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn cancel_run(&self, id: i64) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, &format!("/runs/{id}/cancel"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_null(response).await
    }

    // Looking at a job, and stopping one. `create_job` has been on this client since jobs existed
    // and nothing here could ever look at what it started — so a caller with no screen could open a
    // night's work and then had no way to ask how it went, or to end it.
    //
    // The middle one matters more than it reads. A job that a brake has parked writes its reason on
    // its own row (`jobs.wait_reason`, set by `job::park`) and says it once in the feed. Without a
    // read of that row, "waiting" and "doing nothing" are the same silence to anyone not at the app.

    /// One job: its status, its wait reason if a brake parked it, and its queue.
    pub async fn get_job(&self, id: i64) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::GET, &format!("/jobs/{id}"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, &format!("reading job {id}")).await
    }

    /// The project's jobs, newest first. `live` narrows it to the ones still going.
    ///
    /// The project is optional because the route's is: `GET /jobs` with no `project_id` lists every
    /// project's, which is the right answer to "what is running anywhere" — the question somebody
    /// away from the machine actually asks.
    pub async fn list_jobs(&self, project_id: Option<&str>, live: bool) -> Result<Value, String> {
        // Assembled from a list rather than pushed onto a string, so that "no narrowing at all"
        // comes out as `/jobs` and not as `/jobs?` with a dangling separator. An empty pair is a
        // parse the route should never be asked to make: `JobsQuery::live` is an `Option<bool>`,
        // and `live=` with nothing after it is not a bool.
        let mut params: Vec<String> = Vec::new();
        if live {
            params.push("live=true".to_owned());
        }
        if let Some(project_id) = project_id {
            params.push(format!("project_id={}", urlencoding_encode(project_id)));
        }
        let route = if params.is_empty() {
            "/jobs".to_owned()
        } else {
            format!("/jobs?{}", params.join("&"))
        };
        let response = self
            .request(reqwest::Method::GET, &route)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, "listing jobs").await
    }

    /// Stops a job. The item in flight finishes; nothing else starts.
    pub async fn cancel_job(&self, id: i64) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, &format!("/jobs/{id}/cancel"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_null(response).await
    }

    // The four project reads (spec: orchestrator eyes). Each reaches the calling person's OWN
    // checkout — never a run's worktree, so none of them takes a `run` parameter, unlike the
    // sibling routes in `http.rs` that answer both questions. Every interpolated segment and every
    // query value goes through `urlencoding_encode`, because a path or a query string chosen by
    // whoever wrote the request that reaches here must travel as ONE value and never rewrite the
    // request around it — `um_caminho_com_e_comercial_viaja_codificado` is what pins that.
    //
    // `project_ls` and `project_grep` answer `Value` rather than a typed shape (`inspect::Entry`,
    // `inspect::Match`), and that is deliberate rather than lazy: a recording test-daemon answers
    // `{}` to every route it does not otherwise handle, and a typed `Vec<_>` would fail to
    // deserialize that shape and fail the test that relies on it, where `Value` does not care.

    /// A directory listing inside the project's own checkout.
    pub async fn project_ls(&self, project_id: &str, path: &str) -> Result<Value, String> {
        let route = format!(
            "/projects/{}/ls?path={}",
            urlencoding_encode(project_id),
            urlencoding_encode(path)
        );
        self.request(reqwest::Method::GET, &route)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// One file's contents out of the project's own checkout.
    ///
    /// Bare text, not JSON: `get_project_cat` in `http.rs` answers `Result<String, StatusCode>`, so
    /// the body is read with `text_or_refusal` rather than `.json()` — a `.json()` read of a
    /// plain-text body would fail on the first file that is not valid JSON, which is nearly every
    /// file there is. And not a bare `.text()` either: a refusal from that route is a `StatusCode`
    /// with an EMPTY body, which `.text()` reads as `Ok(String::new())` regardless of status —
    /// indistinguishable from an empty file, and specifically indistinguishable from the
    /// `safe_join` refusal a path like `../../../etc/passwd` gets. Checking status first is what
    /// turns that refusal back into an `Err`.
    pub async fn project_cat(&self, project_id: &str, path: &str) -> Result<String, String> {
        let route = format!(
            "/projects/{}/cat?path={}",
            urlencoding_encode(project_id),
            urlencoding_encode(path)
        );
        let response = self
            .request(reqwest::Method::GET, &route)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        text_or_refusal(response, &format!("reading {path:?}")).await
    }

    /// A text search inside the project's own checkout.
    ///
    /// The query parameter on the wire is named `q`, matching `GrepQuery` in `http.rs` — the Rust
    /// parameter keeps the friendlier name `query` because nothing here requires the two to match.
    pub async fn project_grep(
        &self,
        project_id: &str,
        query: &str,
        path: &str,
    ) -> Result<Value, String> {
        let route = format!(
            "/projects/{}/grep?q={}&path={}",
            urlencoding_encode(project_id),
            urlencoding_encode(query),
            urlencoding_encode(path)
        );
        self.request(reqwest::Method::GET, &route)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// The uncommitted diff of the project's own checkout. Bare text, like `project_cat` above,
    /// and read through `text_or_refusal` for the same reason: a refusal here is also an empty
    /// body on a non-2xx status, and a bare `.text()` cannot tell that apart from a project with
    /// nothing uncommitted.
    pub async fn project_diff(&self, project_id: &str, path: &str) -> Result<String, String> {
        let route = format!(
            "/projects/{}/diff?path={}",
            urlencoding_encode(project_id),
            urlencoding_encode(path)
        );
        let response = self
            .request(reqwest::Method::GET, &route)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        text_or_refusal(response, &format!("reading the diff at {path:?}")).await
    }

    // The two shadow reads (the promotion door). `list_projects` above already answers WHETHER a
    // project may leave shadow — `ProjectSummary` carries `classes_ready`, `classes_total`,
    // `withheld_classes_ready` and `promotable`. These two answer WHY NOT: which action class is
    // short of the bar, and what is sitting in the queue waiting to be judged.
    //
    // Both routes are already in the read table in `auth.rs` (`GET /scoreboard` and
    // `GET /shadow-decisions`, beside the `GET /proposals` this file already calls), so nothing
    // here widens what this token may reach. The verdict route one segment deeper —
    // `POST /shadow-decisions/{id}/verdict` — is deliberately given no method in this file; the
    // reason is written where the tools are, in `mcp_tools.rs`.
    //
    // `Value` rather than `Vec<ClassTally>` / `Vec<ShadowDecision>`, for the reason the project
    // reads above give: the recording test-daemon answers `{}` to every route it does not otherwise
    // handle, and a typed `Vec<_>` fails to deserialize that shape where `Value` does not care.

    /// The per-class shadow scoreboard for one project: what the classifier would have decided, how
    /// much of it a human has reviewed, and how often they agreed.
    pub async fn shadow_scoreboard(&self, project_id: &str) -> Result<Value, String> {
        let route = format!("/scoreboard?project_id={}", urlencoding_encode(project_id));
        self.request(reqwest::Method::GET, &route)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// The decisions of one project still waiting on a human verdict.
    pub async fn shadow_queue(&self, project_id: &str) -> Result<Value, String> {
        let route = format!(
            "/shadow-decisions?project_id={}",
            urlencoding_encode(project_id)
        );
        self.request(reqwest::Method::GET, &route)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn list_proposals(&self) -> Result<Value, String> {
        self.request(reqwest::Method::GET, "/proposals")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn approve_proposal(&self, id: i64) -> Result<Value, String> {
        self.request(reqwest::Method::POST, &format!("/proposals/{id}/approve"))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn reject_proposal(&self, id: i64) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, &format!("/proposals/{id}/reject"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_null(response).await
    }

    /// The newest models per vendor, as the daemon's catalogue serves them. A refused vendor comes
    /// back as the daemon's own message, which names the valid ones.
    pub async fn latest_models(&self, vendor: Option<&str>) -> Result<Value, String> {
        // Encoded by hand: this reqwest is built without its `query` feature, and the value is an
        // agent's own string.
        let path = match vendor {
            Some(vendor) => {
                let mut encoded = String::new();
                for byte in vendor.bytes() {
                    match byte {
                        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                            encoded.push(byte as char)
                        }
                        other => encoded.push_str(&format!("%{other:02X}")),
                    }
                }
                format!("/models/latest?vendor={encoded}")
            }
            None => "/models/latest".to_string(),
        };
        let response = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let ok = response.status().is_success();
        let body: Value = response.json().await.map_err(|e| e.to_string())?;
        if ok {
            Ok(body)
        } else {
            Err(body["error"]
                .as_str()
                .unwrap_or("the daemon refused the request")
                .to_string())
        }
    }

    pub async fn get_budget(&self) -> Result<Value, String> {
        self.request(reqwest::Method::GET, "/autopilot/budget")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// Triage whatever mail is waiting, now. Returns what was started, not the verdicts — those
    /// arrive in the feed a few minutes later.
    pub async fn triage_email(&self) -> Result<Value, String> {
        self.request(reqwest::Method::POST, "/email/triage")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// What the pillar knows about: pending mail first, then the newest verdicts.
    pub async fn get_email_queue(&self) -> Result<Value, String> {
        self.request(reqwest::Method::GET, "/email/queue")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// One message in full, body included.
    pub async fn get_email(&self, id: i64) -> Result<Value, String> {
        self.request(reqwest::Method::GET, &format!("/email/{id}"))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// Search the web, and what this machine has already read, for one query.
    ///
    /// Returns titles, URLs and snippets — never page content. The trust decision (spec §5) is made
    /// over a URL before anything is fetched, and a search that returned content would make that
    /// decision arrive too late to mean anything.
    pub async fn web_search(&self, query: &str, limit: Option<i64>) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/web/search")
            .json(&serde_json::json!({ "query": query, "limit": limit }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, "a web search").await
    }

    /// Read one page.
    ///
    /// Reading only. There is deliberately NO client method here that submits a form, posts, logs
    /// in, or sends anything — the same asymmetry `list_files` has, for a sharper reason: this is
    /// the method that fills an agent's context with text a stranger wrote, and any write sitting
    /// beside it becomes something those words can try to aim.
    pub async fn web_read(&self, url: &str) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/web/read")
            .json(&serde_json::json!({ "url": url }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, "reading a web page").await
    }

    /// Read one file out of the calling team run's own workspace.
    ///
    /// **The run is not an argument, and that is the whole design of this call.** Which folder gets
    /// opened comes from the `Scope::TeamRun` that authenticated the request, so the caller names a
    /// path inside its delivery and nothing else. A `team_run_id` parameter would be the caller
    /// naming what it may read, which is not a permission anything here grants itself.
    ///
    /// Reading only, like `list_files` and `web_read` beside it: the specialists do not write their
    /// answers, `team.rs` does. A write verb here would reintroduce the collision between two
    /// specialists choosing the same filename that naming the files from the core removes by
    /// construction.
    pub async fn read_team_file(&self, path: &str) -> Result<Value, String> {
        self.request(reqwest::Method::POST, "/team-files/read")
            .json(&serde_json::json!({ "path": path }))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// Ask the owner for one NucleOS tool this box can serve but was not given.
    ///
    /// The run is not an argument: `request` sets the run header, so a run can only ask for itself.
    /// A refusal is read back as the error text, the way `declare_refinement` does, because it says
    /// why.
    pub async fn request_tool(&self, tool: &str, reason: &str) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/loadout/tool-requests")
            .json(&serde_json::json!({ "tool": tool, "reason": reason }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            let status = response.status();
            let said = response.text().await.unwrap_or_default();
            return Err(if said.trim().is_empty() {
                format!("the core refused: {status}")
            } else {
                said
            });
        }
        response.json().await.map_err(|e| e.to_string())
    }

    /// Read one context file the calling run was given.
    ///
    /// The run is not an argument: `request` sets the run header, so a run can only read the refs
    /// of its own loadout. A refusal is read back as the error text because it says why.
    pub async fn read_context(&self, path: &str) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/context/read")
            .json(&serde_json::json!({ "path": path }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            let status = response.status();
            let said = response.text().await.unwrap_or_default();
            return Err(if said.trim().is_empty() {
                format!("the core refused: {status}")
            } else {
                said
            });
        }
        response.json().await.map_err(|e| e.to_string())
    }

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

    /// Ask the core to do something on the calling department's behalf.
    ///
    /// **The run is not an argument**, for `read_team_file`'s reason one method up: the department
    /// is named by the key that authenticated the call, never by the body.
    ///
    /// Unlike every other method here, a refusal is READ AND RETURNED rather than reduced to a
    /// status. Every refusal on this route carries a sentence written for the model — "this
    /// department may not send email", "you already have five waiting for approval" — and each one
    /// leads somewhere different: rewrite the request, ask for something else, or say plainly in the
    /// deliverable that it could not be done. A bare `403` leads to a retry.
    /// A turn declares what it learned. It is a proposal, never a fact.
    ///
    /// **`origin_run_id` is deliberately not a parameter.** The daemon reads which run is speaking
    /// off `RUN_ID_HEADER`, which `request` above sets from the run this client was built for — so
    /// a run can name itself and has no way to name anybody else. A field here would be a field a
    /// model could fill in, and "which run taught this?" would stop being evidence.
    /// The declaration's project is deliberately not a parameter for the same reason: the daemon
    /// derives it from that run, so the caller cannot name another project or widen it to the whole
    /// machine.
    pub async fn declare_refinement(
        &self,
        kind: &str,
        title: &str,
        body: &str,
        reasoning: &str,
    ) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/knowledge")
            .json(&serde_json::json!({
                "kind": kind,
                "title": title,
                "body": body,
                "reasoning": reasoning,
            }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            let status = response.status();
            let said = response.text().await.unwrap_or_default();
            return Err(if said.trim().is_empty() {
                format!("the core refused: {status}")
            } else {
                said
            });
        }
        response.json().await.map_err(|e| e.to_string())
    }

    /// Ask for approved knowledge in the calling run's scope.
    ///
    /// Scope is not a parameter: the daemon reads it from `RUN_ID_HEADER`, which `request` sets
    /// from the run this client represents.
    pub async fn recall(&self, query: &str, layer: Option<&str>) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/knowledge/recall")
            .json(&serde_json::json!({ "query": query, "layer": layer }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            let status = response.status();
            let said = response.text().await.unwrap_or_default();
            return Err(if said.trim().is_empty() {
                format!("the core refused: {status}")
            } else {
                said
            });
        }
        response.json().await.map_err(|e| e.to_string())
    }

    /// Leave a finding in the calling job's scope for its next node.
    ///
    /// The run id in `RUN_ID_HEADER` identifies the sender; the daemon resolves the job from its
    /// scoped key rather than accepting a scope in this body.
    pub async fn note_finding(
        &self,
        fact: &str,
        evidence: &[serde_json::Value],
    ) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/knowledge/findings")
            .json(&serde_json::json!({ "fact": fact, "evidence": evidence }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            let status = response.status();
            let said = response.text().await.unwrap_or_default();
            return Err(if said.trim().is_empty() {
                format!("the core refused: {status}")
            } else {
                said
            });
        }
        response.json().await.map_err(|e| e.to_string())
    }

    /// Convene a council, and hand back the id it will be readable by.
    ///
    /// The id and not the answer, because there is no answer yet: `POST /council` is a `202` and
    /// the deliberation runs for minutes afterwards. A caller gets the id now and reads the result
    /// with [`Self::get_council`] on a later turn — which is why the tool that calls this has a
    /// sibling, and why one tool would have been useless.
    ///
    /// Refusals come back as the daemon's own sentence. Every error arm of `post_council` is a
    /// `(StatusCode, String)` written in prose — which panel refused, which seat was wrong, what
    /// the budget had left — and a caller told "the core refused: 400" instead would have to guess
    /// at what to do differently.
    pub async fn ask_council(
        &self,
        question: &str,
        rounds: Option<u32>,
        roles: Option<BTreeMap<String, String>>,
    ) -> Result<String, String> {
        let response = self
            .request(reqwest::Method::POST, "/council")
            .json(&council_ask_body(question, rounds, roles))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            let status = response.status();
            let said = response.text().await.unwrap_or_default();
            return Err(if said.trim().is_empty() {
                format!("the core refused: {status}")
            } else {
                said
            });
        }
        let body: Value = response.json().await.map_err(|e| e.to_string())?;
        body["id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "council response missing id".into())
    }

    /// One council in full: its status, its phase, every seat's answer, and the synthesis if there
    /// is one yet.
    ///
    /// No roster is sent by this client, deliberately. The override exists and the shell offers it,
    /// but a turn convening a council has just been handed a question it could not answer alone —
    /// letting it also choose who gets asked would let it assemble a panel that agrees with it.
    pub async fn get_council(&self, council_id: &str) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::GET, &format!("/council/{council_id}"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            let status = response.status();
            let said = response.text().await.unwrap_or_default();
            return Err(if said.trim().is_empty() {
                format!("the core refused: {status}")
            } else {
                said
            });
        }
        response.json().await.map_err(|e| e.to_string())
    }

    pub async fn propose_action(
        &self,
        kind: &str,
        payload: &Value,
        why: &str,
    ) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/team-actions")
            .json(&serde_json::json!({ "kind": kind, "payload": payload, "why": why }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            let status = response.status();
            let said = response.text().await.unwrap_or_default();
            return Err(if said.trim().is_empty() {
                format!("the core refused: {status}")
            } else {
                said
            });
        }
        response.json().await.map_err(|e| e.to_string())
    }

    /// A director says something to the owner, in the conversation its run was pointed at.
    ///
    /// **No destination parameter, and that absence is the governance of this feature.** Where a
    /// department reports is `team_runs.report_to_chat_id`, chosen by whoever started the run; the
    /// department has no tool that takes a conversation, no way to list the ones on this machine,
    /// and no way to reach one it was not handed. A `chat_id` here would undo all three.
    ///
    /// Which node is speaking comes from `RUN_ID_HEADER`, as everywhere else on this client, and the
    /// daemon refuses a specialist — a department speaks to its owner with one voice.
    ///
    /// Through `json_or_refusal`: the refusal that matters most is "this department was not pointed
    /// at a conversation", which is not an error at all but the ordinary state of nearly every run,
    /// and a model that reads it should put the words in its delivery rather than retry.
    pub async fn report_to_owner(&self, body: &str) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/team-reports")
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, "reporting to the owner").await
    }

    /// One member of a department leaves words for another.
    ///
    /// **Neither the run nor the sender is an argument**, for `read_team_file`'s reason: the
    /// department is named by the key that authenticated the call, and WHICH NODE is calling comes
    /// from `RUN_ID_HEADER`, added by `request()` from an id this process cannot alter. A body field
    /// naming the sender would let a specialist sign a colleague's name to its own finding, which is
    /// the one thing the receiving node cannot check.
    ///
    /// `to` names an `agents.id` and the daemon resolves it against the run's own roster. Naming
    /// somebody who is not on it is not a broken call — it is the ordinary way a model gets a name
    /// slightly wrong, and the refusal says so in a sentence it can act on while it still has the
    /// roster in front of it.
    ///
    /// Through `json_or_refusal`, like `send_to_chat` and unlike the two `propose_*` methods above:
    /// every refusal on this route is already a sentence the daemon wrote for this failure, so the
    /// generic wrapper says everything a bespoke status check would.
    pub async fn send_team_note(&self, to: &str, body: &str) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/team-notes")
            .json(&serde_json::json!({ "to": to, "body": body }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, "leaving a note for a colleague").await
    }

    /// A director asks the owner for a specialist its department does not have.
    ///
    /// The whole request travels as one object rather than as seven parameters, because it is one
    /// object at both ends — `team::RecruitRequest` here, `agent::AgentRequest` after a person has
    /// edited it — and unpacking it in the middle would be a third place the field list is written.
    ///
    /// Refusals are read and returned like `propose_action`'s, and here it matters more: a
    /// specialist calling this is told to put it in its answer instead, and "403" does not say that.
    pub async fn propose_teammate(&self, request: &Value) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/team-recruits")
            .json(request)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            let status = response.status();
            let said = response.text().await.unwrap_or_default();
            return Err(if said.trim().is_empty() {
                format!("the core refused: {status}")
            } else {
                said
            });
        }
        response.json().await.map_err(|e| e.to_string())
    }

    /// Hands a message to a different conversation than the one this run is answering in.
    ///
    /// `chat_id` names the DESTINATION and travels in the URL, matching every other
    /// `/assistant/chats/{id}/...` route on this server; `sending_run_id` — which conversation is
    /// doing the relaying — is never a parameter here or on the wire, because this run already
    /// states it, on every request, the same way it does for `read_team_file`: as `RUN_ID_HEADER`,
    /// added by `request()` above from an id this process cannot alter, since nothing on the local
    /// path can read its own environment. Naming a destination the daemon later refuses is not a
    /// broken call — see `json_or_refusal`, below.
    ///
    /// Through `json_or_refusal` rather than the hand-written status check `propose_action` and
    /// `propose_teammate` use above: those predate it, and what this route refuses with is already
    /// a short, specific slug the daemon wrote for exactly this failure
    /// (`http::relay_refusal_response`), not a sentence that needs composing — the generic
    /// "the daemon refused …: <status>: <body>" wrapper says everything a bespoke one would here.
    pub async fn send_to_chat(&self, chat_id: &str, text: &str) -> Result<Value, String> {
        let response = self
            .request(
                reqwest::Method::POST,
                &format!("/assistant/chats/{chat_id}/relay"),
            )
            .json(&serde_json::json!({ "text": text }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, "relaying a message to another conversation").await
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

    /// What is in the files folder. Reading only — there is deliberately no client method here for
    /// creating, writing, moving, deleting or downloading, so an agent cannot reach those even by
    /// mistake. The folder grew a whole file manager on the shell side; this stayed one verb.
    pub async fn list_files(&self, path: &str) -> Result<Value, String> {
        self.request(
            reqwest::Method::GET,
            &format!("/files?path={}", urlencoding_encode(path)),
        )
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())
    }

    pub async fn get_kill(&self) -> Result<Value, String> {
        self.request(reqwest::Method::GET, "/autopilot/kill")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn set_kill(&self, engaged: bool) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/autopilot/kill")
            .json(&serde_json::json!({"engaged": engaged}))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_null(response).await
    }

    /// Queues one operation and waits for it, up to the daemon's own ceiling.
    ///
    /// Two calls rather than one: the submit answers as soon as the row is committed, and the wait is
    /// a separate route so a caller that only wants a ticket is not made to block for it. What this
    /// method does is spend the wait on the caller's behalf, which is spec decision 3's hybrid —
    /// block for a while, then hand back a ticket rather than a timeout.
    pub async fn vcs_request(
        &self,
        project_id: &str,
        operation: &str,
        source: Option<&str>,
        target: Option<&str>,
    ) -> Result<Value, String> {
        let body = vcs_submit_body(project_id, operation, source, target)?;
        let response = self
            .request(reqwest::Method::POST, "/vcs/requests")
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        // A refusal is a bare status with an empty body (`submit_vcs_request` returns
        // `Result<Json<Ticket>, StatusCode>`), so `.json()` on it would fail with a decoding error
        // that names nothing useful. The status IS the message: 404 is an unknown project, 422 a
        // project whose recorded root is not a repository or a body the queue will not accept, 403 a
        // token that may not queue.
        if !response.status().is_success() {
            return Err(format!(
                "the daemon refused the request: {}",
                response.status()
            ));
        }
        let submitted: Value = response.json().await.map_err(|e| e.to_string())?;

        match submitted["id"].as_i64() {
            Some(id) => self.vcs_ticket(id, true).await,
            None => Ok(submitted),
        }
    }

    /// Reads something from GitHub, through the daemon that holds the credential.
    ///
    /// The flat parameters become a typed `ReadOp` HERE, before anything is sent, so a bad
    /// repository comes back as a sentence naming the field rather than as a bare 400. The route
    /// validates again on the way in — the same check on every road, which is what
    /// `github`'s validating `Deserialize` exists for.
    pub async fn github_read(
        &self,
        operation: String,
        repo: String,
        id: Option<String>,
    ) -> Result<Value, String> {
        let op = crate::github::ReadOp::from_request(crate::github::ReadRequest {
            operation,
            repo,
            id,
        })?;
        self.github_request(&crate::github::Op::Read(op)).await
    }

    /// Asks GitHub for something that changes it.
    ///
    /// The answer says which of two things happened — `ran`, or `filed_for_approval` with the number
    /// a person will see beside it. **Neither blocks**, and the caller is meant to read the status
    /// rather than assume the first.
    pub async fn github_act(&self, request: crate::github::ActRequest) -> Result<Value, String> {
        let op = crate::github::ActOp::from_request(request)?;
        self.github_request(&crate::github::Op::Act(op)).await
    }

    /// The one request both halves make.
    ///
    /// A refusal here carries a message in its body (`submit_github_request` answers
    /// `(StatusCode, String)`), unlike `/vcs/requests` where the status IS the message — so the body
    /// is read first and the status is only the fallback. Told "no github token is stored", a caller
    /// knows what to do; told "403", it guesses.
    async fn github_request(&self, op: &crate::github::Op) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/github/requests")
            .json(&serde_json::json!({ "op": op }))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(if body.trim().is_empty() {
                format!("the daemon refused the request: {status}")
            } else {
                body
            });
        }
        serde_json::from_str(&body).map_err(|error| error.to_string())
    }

    /// One queued operation's ticket: what was asked for and how it ended. `wait` spends up to the
    /// daemon's ceiling waiting for it to finish; without it the answer is whatever the row says now.
    pub async fn vcs_ticket(&self, id: i64, wait: bool) -> Result<Value, String> {
        self.request(reqwest::Method::GET, &ticket_path(id, wait))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }

    /// Ask the daemon to verify a worktree. The answer is a ticket: finished when the daemon's wait
    /// ceiling allowed, otherwise with progress to be read back through `verify_status`.
    pub async fn verify(
        &self,
        kind: &str,
        scope: &str,
        worktree: Option<&str>,
        files: Option<&[String]>,
        base: Option<&str>,
    ) -> Result<Value, String> {
        self.verify_request(
            "/verify",
            &serde_json::json!({
                "kind": kind,
                "scope": scope,
                "worktree": worktree,
                "files": files,
                "base": base,
                "wait": true,
            }),
        )
        .await
    }

    /// Ask the daemon for a covered verification of a worktree (F3-13): `cover` makes the daemon
    /// pick the base, so the request carries neither `files` nor `base`. The answer is a ticket, as
    /// with `verify`.
    ///
    /// It submits with `wait: false`: the daemon then answers with the ticket id at once and the
    /// caller follows it through `verify_status`. With `wait: true` the daemon holds this POST for
    /// up to its own ceiling, so a caller whose limit is shorter times out with the id never read,
    /// and a ticket that exists cannot be joined.
    pub async fn verify_cover(&self, worktree: &str) -> Result<Value, String> {
        self.verify_request(
            "/verify",
            &serde_json::json!({
                "kind": "test",
                "scope": "scope",
                "worktree": worktree,
                "cover": true,
                "wait": false,
            }),
        )
        .await
    }

    /// One verification ticket's state; `wait` holds the line up to the daemon's ceiling.
    pub async fn verify_status(&self, ticket: i64, wait: bool) -> Result<Value, String> {
        self.verify_request(
            "/verify/status",
            &serde_json::json!({ "ticket": ticket, "wait": wait }),
        )
        .await
    }

    /// Tells the daemon an IDE session's verify box is still there (`POST /verify/box/beat`).
    pub async fn verify_box_beat(&self, worktree: &str) -> Result<Value, String> {
        self.verify_request(
            "/verify/box/beat",
            &serde_json::json!({ "worktree": worktree }),
        )
        .await
    }

    /// The one request every verify call makes (`verify`, `verify_status`, `verify_box_beat`). A
    /// refusal carries its reason in the body (the handlers answer `(StatusCode, String)`), so the
    /// body is read first and the status is only the fallback, as in `github_request`.
    async fn verify_request(&self, path: &str, body: &Value) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, path)
            .json(body)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(if text.trim().is_empty() {
                format!("the daemon refused the request: {status}")
            } else {
                text
            });
        }
        serde_json::from_str(&text).map_err(|error| error.to_string())
    }
}

/// The body of an answer, or the status it was refused with.
///
/// A refusal from a route that returns `Result<_, StatusCode>` is a bare status with an empty body,
/// so `.json()` on one fails with a decoding error that names nothing useful. The status IS the
/// message. `context` says what the caller was attempting, because the status alone does not.
///
/// The web routes refuse differently — a status AND a sentence — and go through here too.
/// Everything this returns is read by a model deciding what to tell a person, and the two failures
/// are answered oppositely: a search that came back empty is a fact about the world, and a search
/// that never happened is a fact about the machine. Told "error decoding response body", a model has
/// neither, and what it reports is that it looked and found nothing.
async fn json_or_refusal(response: reqwest::Response, context: &str) -> Result<Value, String> {
    let status = response.status();
    if !status.is_success() {
        // The body when there is one, because the web routes put the actionable half there — "the
        // web pillar is off: set enabled: true in ~/.nucleos/web.yaml" is a sentence somebody can act on
        // and `503` alone is not. Truncated because this string lands in a model's context and the
        // thing most likely to answer a request with kilobytes of body is a proxy, not the daemon.
        let detail = response.text().await.unwrap_or_default();
        return Err(refusal_message(status, &detail, context));
    }
    json_or_null(response).await
}

/// Bare-text sibling of `json_or_refusal`, for the two routes that answer a plain string on
/// success rather than JSON (`project_cat`, `project_diff`).
///
/// Both of those refuse the same way `get_project_ls`/`get_project_grep` do in `http.rs`: a bare
/// `StatusCode` with an EMPTY body (`Result<String, StatusCode>`, same as the `Value` routes'
/// `Result<Json<_>, StatusCode>`). The `Value` routes are safe from this by accident — `.json()` on
/// an empty body fails to parse and surfaces an `Err` on its own — but `.text()` has no such
/// accident to lean on: it happily turns an empty, refused body into `Ok(String::new())`, which
/// reads as "the file is empty" rather than "that path was refused". Checking status before ever
/// calling `.text()` on the success path, the same way `json_or_refusal` does, is what keeps a
/// refusal an `Err` instead of a false-empty answer.
async fn text_or_refusal(response: reqwest::Response, context: &str) -> Result<String, String> {
    let status = response.status();
    if !status.is_success() {
        let detail = response.text().await.unwrap_or_default();
        return Err(refusal_message(status, &detail, context));
    }
    response.text().await.map_err(|e| e.to_string())
}

/// The shared refusal-message formatting for `json_or_refusal` and `text_or_refusal`: the body
/// when there is one, because the web routes put the actionable half there — "the web pillar is
/// off: set enabled: true in ~/.nucleos/web.yaml" is a sentence somebody can act on and `503` alone is
/// not. Truncated because this string lands in a model's context and the thing most likely to
/// answer a request with kilobytes of body is a proxy, not the daemon.
fn refusal_message(status: reqwest::StatusCode, detail: &str, context: &str) -> String {
    let detail = detail.trim();
    match detail.chars().take(REFUSAL_DETAIL_LIMIT + 1).count() {
        0 => format!("the daemon refused {context}: {status}"),
        n if n > REFUSAL_DETAIL_LIMIT => {
            let cut: String = detail.chars().take(REFUSAL_DETAIL_LIMIT).collect();
            format!("the daemon refused {context}: {status}: {cut}…")
        }
        _ => format!("the daemon refused {context}: {status}: {detail}"),
    }
}

/// How much of a refusal's body travels back with it, in characters.
///
/// Long enough for every sentence the daemon itself writes, short enough that an HTML error page
/// from something sitting between here and it cannot become the turn's context.
const REFUSAL_DETAIL_LIMIT: usize = 300;

/// The submit body, built and validated before anything is sent.
///
/// Named `VcsSubmitBody` and not `VcsRequestBody` on purpose: `http.rs` has a private type by that
/// second name for the *receiving* side of the same wire shape, and two identically named structs at
/// opposite ends of one request are a trap for whoever changes one of them.
#[derive(Serialize)]
pub struct VcsSubmitBody {
    pub project_id: String,
    pub operation: crate::vcs::Op,
}

/// PURE: the body for one queue submission, or why the request is not one.
pub fn vcs_submit_body(
    project_id: &str,
    operation: &str,
    source: Option<&str>,
    target: Option<&str>,
) -> Result<VcsSubmitBody, String> {
    Ok(VcsSubmitBody {
        project_id: project_id.to_owned(),
        operation: crate::vcs::Op::from_request(operation, source, target)?,
    })
}

/// PURE: the `POST /council` body for one question. `rounds` and `roles` become keys only when
/// given. An absent key is the daemon's documented "use the file's default"; a `null` reads the same
/// today only because of how serde treats an `Option`, and a wire contract should not lean on that.
pub fn council_ask_body(
    question: &str,
    rounds: Option<u32>,
    roles: Option<BTreeMap<String, String>>,
) -> Value {
    let mut body = serde_json::json!({ "question": question });
    if let Some(rounds) = rounds {
        body["rounds"] = serde_json::json!(rounds);
    }
    if let Some(roles) = roles {
        body["roles"] = serde_json::json!(roles);
    }
    body
}

/// PURE: which of the two ticket routes to call. Extracted so the choice is asserted somewhere —
/// inline, the difference between blocking and not blocking is one path segment nothing reads.
fn ticket_path(id: i64, wait: bool) -> String {
    if wait {
        format!("/vcs/requests/{id}/wait")
    } else {
        format!("/vcs/requests/{id}")
    }
}

/// Percent-encodes a query value.
///
/// Written out rather than pulled in: a folder name carrying `&`, `#` or `..` would otherwise
/// arrive as a different request than the one intended — and `..` reaching the daemon's path guard
/// as a *separate parameter* rather than part of the path is exactly how a check gets skipped.
pub fn urlencoding_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[derive(Serialize)]
pub struct CreateRunBody {
    pub prompt: String,
    pub project_id: String,
    pub cwd: String,
    pub mode: String,
}

pub fn resolve_run_request(
    projects: &[ProjectSummary],
    project_id: &str,
    prompt: &str,
) -> Result<CreateRunBody, String> {
    let project = projects
        .iter()
        .find(|project| project.project_id == project_id)
        .ok_or_else(|| format!("unknown project: {project_id}"))?;
    let cwd = project
        .project_root
        .clone()
        .ok_or_else(|| format!("project {project_id} has no root (mode off?)"))?;
    let mode = match project.mode {
        Mode::Active => "worktree",
        Mode::Shadow => "shadow",
        Mode::Off => return Err(format!("project {project_id} is off")),
    };

    Ok(CreateRunBody {
        prompt: prompt.into(),
        project_id: project_id.into(),
        cwd,
        mode: mode.into(),
    })
}

async fn json_or_null(response: reqwest::Response) -> Result<Value, String> {
    let body = response.bytes().await.map_err(|e| e.to_string())?;
    if body.is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_slice(&body).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod instance_tests {
    use super::*;

    /// The port a second daemon binds, and what happens to a value that is not a port.
    ///
    /// A machine runs ONE NucleOS, so the port was a literal in nine places and that was honest
    /// while it was true. It stopped being true the moment somebody needed to exercise a change
    /// end to end without stopping the daemon that is already serving — the alternative being to
    /// run the new build against the live database, which is the one experiment nobody can undo.
    ///
    /// Unparseable falls back to the default rather than refusing to start. This value arrives
    /// from a shell, it is only ever set on purpose, and a daemon that will not come up because
    /// somebody typed `NUCLEOS_PORT=879l` is a worse answer than one that comes up where it always
    /// does. Zero is rejected with them: it means "any free port" to the OS, and a daemon whose
    /// address nothing can predict is unreachable by every client that was told the default.
    #[test]
    fn a_port_is_read_from_what_the_environment_said_or_falls_back() {
        assert_eq!(port_from(None), DEFAULT_PORT);
        assert_eq!(port_from(Some("8792")), 8792);
        assert_eq!(port_from(Some("")), DEFAULT_PORT);
        assert_eq!(port_from(Some("879l")), DEFAULT_PORT);
        assert_eq!(port_from(Some("0")), DEFAULT_PORT);
        assert_eq!(port_from(Some("99999")), DEFAULT_PORT);
    }

    /// The URL is derived from the port and never written beside it.
    ///
    /// Two literals for one fact is the shape that lets a daemon bind one port and tell everything
    /// it launches to call back on another — a failure that looks like every tool being broken.
    #[test]
    fn the_url_a_daemon_hands_out_names_the_port_it_binds() {
        assert_eq!(url_for(port_from(None)), "http://127.0.0.1:8791");
        assert_eq!(url_for(port_from(Some("8792"))), "http://127.0.0.1:8792");
    }

    /// Whether this process is the machine's daemon or a second one run beside it.
    ///
    /// Asked because two things must not happen on a secondary: registering the logon task, which
    /// would point the machine's autostart at whatever build happened to be under test, and
    /// starting the sidecars, which would have a second copy of every integration talking to the
    /// same accounts.
    ///
    /// Either override makes it secondary, and that is deliberate rather than lazy. A second
    /// daemon on the default port cannot bind at all, and one on the real data directory is
    /// writing the live database — which is the case this whole mechanism exists to avoid. Neither
    /// is a primary; both are somebody testing.
    #[test]
    fn a_daemon_told_a_port_or_a_directory_of_its_own_is_not_the_machines_daemon() {
        assert!(is_primary(None, None));
        assert!(!is_primary(Some("8792"), None));
        assert!(!is_primary(None, Some("C:/tmp/nucleos-test")));
        assert!(!is_primary(Some("8792"), Some("C:/tmp/nucleos-test")));
        // An empty value is not an override. It is what a shell leaves behind when a variable is
        // exported and never given a value, and reading it as "secondary" would silently take a
        // real daemon's autostart and sidecars away from it.
        assert!(is_primary(Some(""), Some("")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder name reaches the daemon as a query value. Left raw, `&` would start a second
    /// parameter and `..` would arrive somewhere the path guard never inspects — which is how a
    /// check gets skipped rather than defeated.
    #[test]
    fn a_folder_name_cannot_rewrite_the_request_it_travels_in() {
        assert_eq!(urlencoding_encode("BACMAT/2026"), "BACMAT%2F2026");
        assert_eq!(urlencoding_encode(".."), "..");
        assert_eq!(urlencoding_encode("../../etc"), "..%2F..%2Fetc");
        assert_eq!(urlencoding_encode("a&path=b"), "a%26path%3Db");
        assert_eq!(urlencoding_encode("com espaço"), "com%20espa%C3%A7o");
        // Unreserved characters are left alone, or every path would be unreadable in a log.
        assert_eq!(
            urlencoding_encode("relatorio-2026_v1.docx"),
            "relatorio-2026_v1.docx"
        );
    }

    /// Resolving a JOB is not resolving a run, and this pins the two apart.
    ///
    /// `create_job` calls `job::resolve_start` rather than `resolve_run_request` beside it, and the
    /// difference is the shadow row. A run may be plan-only, so shadow is a legitimate run mode. A
    /// job may not: its plan node has to WRITE the plan.json its queue comes from, so a job
    /// accepted for a shadow project would spend the night doing nothing and reporting that it was
    /// working. If the two are ever "unified" for looking alike, this assertion is what says no.
    ///
    /// No network in either direction: both are pure functions over a roster.
    #[test]
    fn resolver_um_job_nao_e_resolver_um_run() {
        let projects = vec![
            summary_for("shadow-project", Mode::Shadow, Some("C:/projects/shadow")),
            summary_for("live-project", Mode::Active, Some("C:/projects/live")),
        ];

        // The run path accepts shadow, and is right to.
        assert_eq!(
            resolve_run_request(&projects, "shadow-project", "inspect")
                .unwrap()
                .mode,
            "shadow"
        );
        // The job path refuses it, and says which state it found.
        let refusal = crate::job::resolve_start(&projects, "shadow-project")
            .expect_err("a job must not be accepted for a plan-only project");
        assert!(refusal.reason("shadow-project").contains("shadow"));

        // An unknown project fails on both paths, so the courtesy check in `create_job` cannot be
        // the thing that lets one through.
        assert!(resolve_run_request(&projects, "nao-existe", "x").is_err());
        assert_eq!(
            crate::job::resolve_start(&projects, "nao-existe"),
            Err(crate::job::StartRefusal::UnknownProject)
        );

        // And the one that must still work, without which the assertions above pass by refusing
        // everything.
        assert_eq!(
            crate::job::resolve_start(&projects, "live-project")
                .unwrap()
                .project_root,
            "C:/projects/live"
        );
    }

    fn summary_for(project_id: &str, mode: Mode, root: Option<&str>) -> ProjectSummary {
        ProjectSummary {
            project_id: project_id.into(),
            mode,
            project_root: root.map(str::to_string),
            pending: 0,
            classes_ready: 0,
            classes_total: 0,
            withheld_classes_ready: 0,
            promotable: false,
            open_review_items: 0,
            open_proposals: 0,
            open_shadow_decisions: 0,
            wip_limit: Some(3),
            queue_full: false,
            last_gate: None,
            last_gate_at: None,
        }
    }

    #[test]
    fn a_request_body_carries_the_typed_operation() {
        let body = vcs_submit_body("nucleos", "merge", Some("feature"), Some("master")).unwrap();

        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({
                "project_id": "nucleos",
                "operation": {"op": "merge", "source": "feature", "target": "master"}
            })
        );
    }

    /// The validation lives in `vcs::Op`, and this asserts the client actually consults it rather than
    /// posting whatever it was handed — a caller that named an operation the queue does not have is told
    /// so without a round trip, and the daemon never sees a request it would only reject.
    #[test]
    fn an_operation_the_queue_does_not_have_never_reaches_the_daemon() {
        // One the queue has decided against, rather than one it has not got to: nothing is deferred
        // any more, so a name that fails here fails for a reason the caller can act on.
        assert!(vcs_submit_body("nucleos", "pull", Some("feature"), Some("origin")).is_err());
        assert!(vcs_submit_body("nucleos", "tag", Some("main"), Some("-d")).is_err());
        assert!(vcs_submit_body("nucleos", "merge", Some("-f"), Some("master")).is_err());
        // The same guard on the operation the queue DID learn, because a second variant is a second
        // route to argv and inherits none of the first one's checks by being next to it.
        assert!(vcs_submit_body("nucleos", "push", Some("main"), Some("--exec=x")).is_err());
    }

    /// `source` is what moves and `target` is where it goes, for both operations — so a push reads
    /// "branch, then remote". Asserted on the wire shape because that convention is the one thing a
    /// caller cannot infer from the parameter names alone, and swapping the two arguments at the
    /// call site would otherwise produce a request that queues and pushes the wrong thing.
    #[test]
    fn a_push_body_names_the_branch_as_source_and_the_remote_as_target() {
        let body = vcs_submit_body("nucleos", "push", Some("main"), Some("origin")).unwrap();

        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({
                "project_id": "nucleos",
                "operation": {"op": "push", "remote": "origin", "branch": "main"}
            })
        );
    }

    /// The two ticket routes differ only in a suffix, and nothing else in the suite would notice if they
    /// were swapped: both return the same shape, and one merely blocks longer.
    #[test]
    fn waiting_and_not_waiting_are_different_routes() {
        assert_eq!(ticket_path(7, false), "/vcs/requests/7");
        assert_eq!(ticket_path(7, true), "/vcs/requests/7/wait");
    }

    #[test]
    fn active_project_resolves_to_worktree_mode() {
        let projects = vec![ProjectSummary {
            project_id: "active-project".into(),
            mode: Mode::Active,
            project_root: Some("C:/projects/active".into()),
            pending: 0,
            classes_ready: 0,
            classes_total: 0,
            withheld_classes_ready: 0,
            promotable: false,
            open_review_items: 0,
            open_proposals: 0,
            open_shadow_decisions: 0,
            wip_limit: Some(3),
            queue_full: false,
            last_gate: None,
            last_gate_at: None,
        }];

        let body = resolve_run_request(&projects, "active-project", "do the work").unwrap();

        assert_eq!(body.mode, "worktree");
        assert_eq!(body.cwd, "C:/projects/active");
    }

    /// What a model is told when the web is off, and why the wording is the whole task.
    ///
    /// `/web/search` answers a disabled pillar with `503` and a sentence in plain text. Neither web
    /// method looked at the status, so the sentence never arrived: `.json()` choked on it and the
    /// model received `{"error":"error decoding response body"}`. That reads as a glitch in the
    /// plumbing, and the failure it produces is the model reporting that it searched and found
    /// nothing, which is a lie.
    ///
    /// A model told the search did not happen can say so. A model told the response would not parse
    /// has nothing to report and will fill the gap itself.
    #[tokio::test]
    async fn a_web_call_the_daemon_refused_says_the_web_did_not_answer() {
        let url = refusing_daemon(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "the web pillar is off: set enabled: true in ~/.nucleos/web.yaml",
        )
        .await;
        let client = DaemonClient::new(url, "test-token".to_string());

        let searched = client.web_search("golf 2.0 tdi", Some(5)).await;
        let read = client.web_read("https://stand.example/golf").await;

        for (what, outcome) in [("web_search", &searched), ("web_read", &read)] {
            let refusal = outcome
                .as_ref()
                .expect_err("a refusal must not be reported as an answer");
            assert!(
                refusal.contains("503") || refusal.contains("Service Unavailable"),
                "{what} said {refusal:?}, which does not say the daemon refused it"
            );
            assert!(
                !refusal.contains("decoding"),
                "{what} said {refusal:?}, which reads as a parse fault rather than a refusal"
            );
            // The actionable half. `503` says the call did not happen; only the sentence says what
            // would make it happen, and this one is a line in a config file.
            assert!(
                refusal.contains("web.yaml"),
                "{what} said {refusal:?}, dropping the one part somebody can act on"
            );
        }
    }

    /// A refusal is read by a model, so its length is a cost. The daemon's own sentences are short;
    /// the thing likely to answer a request with kilobytes is a proxy between here and it, and
    /// handing that to the turn as context is how an unrelated error page becomes what the model
    /// thinks it learned.
    #[tokio::test]
    async fn a_refusal_that_arrives_as_a_wall_of_text_is_cut_down() {
        let url = refusing_daemon(
            axum::http::StatusCode::BAD_GATEWAY,
            concat!(
                "<html><body>",
                include_str!("../Cargo.toml"),
                "</body></html>"
            ),
        )
        .await;

        let refusal = DaemonClient::new(url, "test-token".to_string())
            .web_search("golf", None)
            .await
            .expect_err("a 502 is not an answer");

        assert!(
            refusal.chars().count() < REFUSAL_DETAIL_LIMIT * 2,
            "the refusal is {} characters long",
            refusal.chars().count()
        );
        assert!(refusal.contains('…'), "a cut refusal must say it was cut");
        assert!(
            refusal.contains("502"),
            "and must still say what happened: {refusal:?}"
        );
    }

    /// The direction that is worse than an unhelpful message: a refusal delivered as success.
    ///
    /// Any route that refuses with a JSON body would have been read straight through as the answer,
    /// and since the refusal slugs landed there is one of those in this daemon. A caller cannot tell
    /// an empty result set from a rejection when both arrive as `Ok`.
    #[tokio::test]
    async fn a_refusal_that_happens_to_be_json_is_still_a_refusal() {
        let url =
            refusing_daemon(axum::http::StatusCode::FORBIDDEN, r#"{"error":"blocked"}"#).await;
        let client = DaemonClient::new(url, "test-token".to_string());

        assert!(
            client.web_read("https://192.168.1.1/admin").await.is_err(),
            "a 403 carrying JSON must not be handed back as the page"
        );
    }

    #[tokio::test]
    async fn a_refused_declaration_hands_the_refusal_back_to_the_model() {
        let url = refusing_daemon(
            axum::http::StatusCode::BAD_REQUEST,
            r#"{"refusal":"missing_run_id"}"#,
        )
        .await;

        let refusal = DaemonClient::new(url, "test-token".to_string())
            .declare_refinement("memory", "t", "b", "r")
            .await
            .expect_err("a refused declaration must not be reported as knowledge");

        assert!(refusal.contains("missing_run_id"), "said {refusal:?}");
    }

    /// A daemon that answers one canned refusal to everything.
    async fn refusing_daemon(status: axum::http::StatusCode, body: &'static str) -> String {
        let app = axum::Router::new().fallback(move || async move { (status, body) });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}")
    }

    /// **A team asked for reaches the daemon.**
    ///
    /// The assertion is on the BODY that travelled and not on the return value, and that is the
    /// whole of the test. `create_job` hands back a job id either way, so a version of this that
    /// checked the id would pass with `team_id` dropped on the floor — which is precisely the state
    /// this change exists to end, and it is a state whose only symptom is work running sequentially
    /// while reporting `completed`.
    #[tokio::test]
    async fn uma_equipa_pedida_viaja_ate_ao_corpo_do_pedido() {
        let (url, seen) = job_recording_daemon().await;
        let client = DaemonClient::new(url, "test-token".to_string());

        let job_id = client
            .create_job("live-project", "build the thing", None, None, Some("crew"))
            .await
            .expect("the fake daemon accepts the job");
        assert_eq!(job_id, 7);

        let body = seen
            .lock()
            .unwrap()
            .clone()
            .expect("no POST /jobs was sent");
        assert_eq!(
            body["team_id"].as_str(),
            Some("crew"),
            "the team never left this client: {body}"
        );
    }

    /// And the half that says the change cost nobody anything: no team named, and the body is the
    /// one this client sent before teams existed.
    ///
    /// `team_id` ABSENT rather than `null`. Both deserialize to `None` at the route, so this is not
    /// a claim about the daemon — it is the claim that adding the parameter changed nothing for the
    /// callers who do not use it, which is worth nothing unless something checks it.
    ///
    /// `.get()` and not `[]`: indexing a missing key yields `Null`, so `body["team_id"].is_null()`
    /// would pass for both an omitted field and a `null` one — the exact confusion the test is here
    /// to rule out.
    #[tokio::test]
    async fn sem_equipa_o_corpo_sai_como_sempre_saiu() {
        let (url, seen) = job_recording_daemon().await;
        let client = DaemonClient::new(url, "test-token".to_string());

        client
            .create_job("live-project", "build the thing", None, None, None)
            .await
            .expect("the fake daemon accepts the job");

        let body = seen
            .lock()
            .unwrap()
            .clone()
            .expect("no POST /jobs was sent");
        assert!(
            body.get("team_id").is_none(),
            "a job with no team must not mention one at all: {body}"
        );
        // The rest of the body, or "nothing else changed" is being asserted about a single key.
        assert_eq!(body["project_id"].as_str(), Some("live-project"));
        assert_eq!(body["prompt"].as_str(), Some("build the thing"));
        assert!(body["budget_usd"].is_null());
        assert!(body["max_rounds"].is_null());
    }

    /// A daemon that answers the two calls `create_job` makes and keeps the JSON body of the second.
    ///
    /// `recording_daemon` beside it records the URI, which is the right question for a read whose
    /// argument travels in the query string and the wrong one here: `POST /jobs` always has the same
    /// path, and everything this test is about is inside the body.
    async fn job_recording_daemon() -> (String, std::sync::Arc<std::sync::Mutex<Option<Value>>>) {
        let seen: std::sync::Arc<std::sync::Mutex<Option<Value>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let recorded = seen.clone();
        let roster = serde_json::to_string(&[summary_for(
            "live-project",
            Mode::Active,
            Some("C:/projects/live"),
        )])
        .expect("a roster serializes");

        let app = axum::Router::new()
            .route(
                "/projects",
                axum::routing::get(move || {
                    let roster = roster.clone();
                    async move {
                        (
                            [(axum::http::header::CONTENT_TYPE, "application/json")],
                            roster,
                        )
                    }
                }),
            )
            .route(
                "/jobs",
                axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                    let recorded = recorded.clone();
                    async move {
                        *recorded.lock().unwrap() = Some(body);
                        (
                            axum::http::StatusCode::CREATED,
                            axum::Json(serde_json::json!({"job_id": 7})),
                        )
                    }
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), seen)
    }

    /// A daemon that answers `{}` to everything and remembers the exact path+query of the last
    /// request it received — so a test can inspect what actually travelled on the wire rather than
    /// trusting that the client built what it meant to.
    async fn recording_daemon() -> (String, std::sync::Arc<std::sync::Mutex<Option<String>>>) {
        let seen: std::sync::Arc<std::sync::Mutex<Option<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let recorded = seen.clone();
        let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
            let recorded = recorded.clone();
            async move {
                *recorded.lock().unwrap() = Some(uri.to_string());
                (axum::http::StatusCode::OK, "{}")
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), seen)
    }

    /// A folder name reaching a project read as a query value must stay ONE value, whatever
    /// characters it holds — the same property `a_folder_name_cannot_rewrite_the_request_it_\
    /// travels_in` pins for `urlencoding_encode` in isolation, checked here end-to-end so a future
    /// `project_ls` that builds its query by hand (`format!("...?path={path}")`, skipping the
    /// encoder that already exists in this file) is caught rather than assumed away.
    ///
    /// `&` is the sharp case: raw, it starts a second query parameter, so a path an attacker chose
    /// could smuggle in a parameter the daemon reads as if the caller had sent it. `..` travels
    /// inside the same single value — `urlencoding_encode` leaves `.` unescaped by design, so the
    /// guard this test wants is not "no `..` reaches the daemon" but "it never reaches the daemon
    /// as anything other than part of the one `path` value".
    #[tokio::test]
    async fn um_caminho_com_e_comercial_viaja_codificado() {
        let (url, seen) = recording_daemon().await;
        let client = DaemonClient::new(url, "test-token".to_string());

        client
            .project_ls("nucleos", "a&path=b/../etc")
            .await
            .expect("the recording daemon answers 200 to everything");

        let uri = seen
            .lock()
            .unwrap()
            .clone()
            .expect("project_ls sent no request to the daemon at all");
        let query = uri
            .split_once('?')
            .map(|(_, query)| query)
            .unwrap_or_else(|| panic!("no query string was sent at all: {uri}"));

        assert_eq!(
            query.matches("path=").count(),
            1,
            "the path value split into more than one query parameter: {uri}"
        );
        assert_eq!(
            query.split('&').count(),
            1,
            "an unescaped & in the path started a second query parameter: {uri}"
        );
    }

    /// The bug `text_or_refusal` exists to close: `get_project_cat` in `http.rs` refuses a path
    /// like this with a bare `StatusCode` — an EMPTY body — and a plain `.text()` read does not
    /// care about status codes at all, so it turned that refusal into `Ok(String::new())`:
    /// indistinguishable from "the file is empty" when it is really "that path was refused". Reuses
    /// `refusing_daemon`, the fixture the two `json_or_refusal` tests above already use for a
    /// canned non-2xx answer, rather than touching the frozen `recording_daemon` (which only ever
    /// answers 200).
    #[tokio::test]
    async fn a_project_cat_refusal_does_not_arrive_as_an_empty_file() {
        let url = refusing_daemon(axum::http::StatusCode::BAD_REQUEST, "").await;
        let client = DaemonClient::new(url, "test-token".to_string());

        let refusal = client
            .project_cat("nucleos", "../../../etc/passwd")
            .await
            .expect_err("a 400 with an empty body must not read as an empty file");

        assert!(
            refusal.contains("400") || refusal.contains("Bad Request"),
            "refusal should say what happened: {refusal:?}"
        );
    }

    /// Same bug, same fix, on `project_diff`'s side: an empty-bodied refusal must not read as an
    /// empty (i.e. "nothing uncommitted") diff.
    #[tokio::test]
    async fn a_project_diff_refusal_does_not_arrive_as_an_empty_diff() {
        let url = refusing_daemon(axum::http::StatusCode::NOT_FOUND, "").await;
        let client = DaemonClient::new(url, "test-token".to_string());

        let refusal = client
            .project_diff("nucleos", "../../../etc/passwd")
            .await
            .expect_err("a 404 with an empty body must not read as an empty diff");

        assert!(
            refusal.contains("404") || refusal.contains("Not Found"),
            "refusal should say what happened: {refusal:?}"
        );
    }

    /// A project id reaching the two shadow reads is a QUERY value, not a path segment, and that is
    /// the difference this pins. The four project reads put the id in the path, where a stray `&`
    /// is inert; here a raw one would start a second query parameter and the daemon would read a
    /// parameter the caller never sent. `urlencoding_encode` is already applied to both — this is
    /// what catches the future rewrite that drops it (`format!("/scoreboard?project_id={id}")`
    /// reads perfectly well and is wrong).
    #[tokio::test]
    async fn um_projeto_com_e_comercial_viaja_codificado_nas_leituras_de_shadow() {
        for (label, call) in [("shadow_scoreboard", 0usize), ("shadow_queue", 1usize)] {
            let (url, seen) = recording_daemon().await;
            let client = DaemonClient::new(url, "test-token".to_string());
            let project = "nucleos&project_id=outro";

            if call == 0 {
                client.shadow_scoreboard(project).await
            } else {
                client.shadow_queue(project).await
            }
            .expect("the recording daemon answers 200 to everything");

            let uri = seen
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| panic!("{label} sent no request to the daemon at all"));
            let query = uri
                .split_once('?')
                .map(|(_, query)| query)
                .unwrap_or_else(|| panic!("{label} sent no query string at all: {uri}"));

            assert_eq!(
                query.split('&').count(),
                1,
                "{label}: an unescaped & in the project id started a second query parameter: {uri}"
            );
            assert_eq!(
                query.matches("project_id=").count(),
                1,
                "{label}: the project id split into more than one query parameter: {uri}"
            );
        }
    }

    /// The job listing builds its own query, so the two ways it can be built wrong are checked
    /// here: a project id that rewrites the request around itself, and a separator left dangling
    /// when nothing narrows the list.
    ///
    /// `&` is the sharp case for the first. For the second, `/jobs?` with an empty pair after it is
    /// a `live=` the route has to parse as an `Option<bool>`, which it is not — so "list
    /// everything" has to come out as a bare path.
    #[tokio::test]
    async fn a_lista_de_jobs_monta_a_sua_query_sem_separador_a_solta() {
        let (url, seen) = recording_daemon().await;
        let client = DaemonClient::new(url, "test-token".to_string());

        client
            .list_jobs(Some("nucleos&live=true"), false)
            .await
            .expect("the recording daemon answers 200 to everything");
        let uri = seen.lock().unwrap().clone().expect("no request was sent");
        let query = uri
            .split_once('?')
            .map(|(_, query)| query)
            .unwrap_or_else(|| panic!("no query string was sent at all: {uri}"));
        assert_eq!(
            query.split('&').count(),
            1,
            "an unescaped & in the project id started a second query parameter: {uri}"
        );

        client
            .list_jobs(None, false)
            .await
            .expect("the recording daemon answers 200 to everything");
        let uri = seen.lock().unwrap().clone().expect("no request was sent");
        assert!(
            !uri.contains('?'),
            "listing every job narrowed by nothing must not send a query string at all: {uri}"
        );

        client
            .list_jobs(None, true)
            .await
            .expect("the recording daemon answers 200 to everything");
        let uri = seen.lock().unwrap().clone().expect("no request was sent");
        assert!(
            uri.ends_with("live=true"),
            "the live flag alone must be the whole query, with nothing after it: {uri}"
        );
    }

    #[test]
    fn shadow_project_resolves_to_shadow_mode() {
        let projects = vec![ProjectSummary {
            project_id: "shadow-project".into(),
            mode: Mode::Shadow,
            project_root: Some("C:/projects/shadow".into()),
            pending: 0,
            classes_ready: 0,
            classes_total: 0,
            withheld_classes_ready: 0,
            promotable: false,
            open_review_items: 0,
            open_proposals: 0,
            open_shadow_decisions: 0,
            wip_limit: Some(3),
            queue_full: false,
            last_gate: None,
            last_gate_at: None,
        }];

        let body = resolve_run_request(&projects, "shadow-project", "inspect the work").unwrap();

        assert_eq!(body.mode, "shadow");
        assert_eq!(body.cwd, "C:/projects/shadow");
    }

    #[test]
    fn off_project_is_rejected() {
        let projects = vec![ProjectSummary {
            project_id: "off-project".into(),
            mode: Mode::Off,
            project_root: Some("C:/projects/off".into()),
            pending: 0,
            classes_ready: 0,
            classes_total: 0,
            withheld_classes_ready: 0,
            promotable: false,
            open_review_items: 0,
            open_proposals: 0,
            open_shadow_decisions: 0,
            wip_limit: Some(3),
            queue_full: false,
            last_gate: None,
            last_gate_at: None,
        }];

        assert!(resolve_run_request(&projects, "off-project", "do the work").is_err());
    }

    #[test]
    fn unknown_project_is_rejected() {
        let projects = vec![ProjectSummary {
            project_id: "known-project".into(),
            mode: Mode::Active,
            project_root: Some("C:/projects/known".into()),
            pending: 0,
            classes_ready: 0,
            classes_total: 0,
            withheld_classes_ready: 0,
            promotable: false,
            open_review_items: 0,
            open_proposals: 0,
            open_shadow_decisions: 0,
            wip_limit: Some(3),
            queue_full: false,
            last_gate: None,
            last_gate_at: None,
        }];

        assert!(resolve_run_request(&projects, "unknown-project", "do the work").is_err());
    }

    #[test]
    fn project_without_root_is_rejected() {
        let projects = vec![ProjectSummary {
            project_id: "rootless-project".into(),
            mode: Mode::Active,
            project_root: None,
            pending: 0,
            classes_ready: 0,
            classes_total: 0,
            withheld_classes_ready: 0,
            promotable: false,
            open_review_items: 0,
            open_proposals: 0,
            open_shadow_decisions: 0,
            wip_limit: Some(3),
            queue_full: false,
            last_gate: None,
            last_gate_at: None,
        }];

        assert!(resolve_run_request(&projects, "rootless-project", "do the work").is_err());
    }
}
