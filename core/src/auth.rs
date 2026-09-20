use axum::{
    extract::{Request, State},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::Response,
};
use rand::RngExt;
use subtle::ConstantTimeEq;

use crate::state::AppState;

#[derive(Clone)]
pub struct Token(pub String);

pub fn generate_token() -> String {
    rand::rng()
        .sample_iter(&rand::distr::Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

/// Generates an RFC 9562 UUID version 4 without adding a second randomness dependency.
pub fn generate_uuid_v4() -> String {
    let mut bytes = rand::rng().random::<[u8; 16]>();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

/// What a presented bearer token is allowed to reach.
///
/// The daemon had exactly one key, and `runs::run_env` handed it to every spawned CLI. `worktree`
/// and `shadow` runs launch with `ToolPolicy::Unrestricted`, so they own a Bash tool, and the
/// classifier calls `echo $NUCLEOS_DAEMON_TOKEN` a `read-local` action — allowed even in shadow
/// mode, whose entire premise is watching a run work without letting it act. That run could print
/// the key that approves its own proposals and disengages the kill switch, and every decision the
/// hook made after that was advisory. A gate is only a gate while what is behind it cannot be
/// reached another way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The human's key: the shell reads it from Credential Manager, the telegram sidecar is
    /// launched with it because it is that shell reached from a phone, and orchestrator turns carry
    /// it because they act for the user. Every route.
    ///
    /// An orchestrator turn is the one agent that holds this, and it can because
    /// `ToolPolicy::McpOnly` leaves it no Bash, no Read and no Write — it has no way to look at its
    /// own environment. That is the property that makes it safe, not the mode's name.
    Control,
    /// One autonomous run's key. Minted when the run is created, dead the moment the run stops
    /// running, and good for exactly three routes, all of them one conversation about its own tool
    /// calls: asking the safety gate beforehand, waiting for a person's answer when the gate cannot
    /// decide alone, and reporting back what actually happened.
    ///
    /// Nothing is lost by keeping it that narrow — only orchestrator turns are given an
    /// `--mcp-config`, so a `worktree`, `shadow` or triage run has no daemon tool to call in the
    /// first place. Everything such a run wants to do goes through the gate, which is the point.
    Run(i64),
    /// A sidecar's key, good for the routes that sidecar's pillar owns and nothing else.
    Service(Service),
    /// One team run's key, good for `TEAM_ROUTES` and dead once the run stops being live.
    ///
    /// It looks like `Run` and behaves like `Service`, and the reason is the one difference between
    /// a department and a council. `Service` would have been the exact sibling — the enum
    /// enumerates "a subprocess of ours that must not hold the controls", and a team run qualifies
    /// — except that `mint_service_token` stores one row per NAME, minted at startup and shared by
    /// every seat, because a seat needs no identity of its own. A team run does: `read_team_file`
    /// has to resolve WHICH folder, and a token shared by every run names none of them.
    ///
    /// `String` and not `i64` because `team_runs.id` is TEXT.
    TeamRun(String),
    /// A durable caller key, limited to the access level chosen when it was minted.
    ApiToken(ApiTokenLevel),
}

/// The three durable API-key levels, named for what a holder may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApiTokenLevel {
    ReadOnly,
    RunCreating,
    Admin,
}

impl ApiTokenLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::RunCreating => "run-creating",
            Self::Admin => "admin",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "read-only" => Some(Self::ReadOnly),
            "run-creating" => Some(Self::RunCreating),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }
}

/// The first route a run token opens, and the reason a run token exists.
const HOOK_ROUTE: &str = "/hooks/pretooluse-decision";

/// The second door of the same conversation, and the only other one a run had until this route
/// grew a third.
///
/// The gate above answers in milliseconds because the hook can only wait five seconds. A tool call
/// somebody has to say yes to cannot be answered in five seconds, so the gate says `asking` and the
/// hook comes back here to wait — which is a call that BLOCKS, and therefore had to be its own
/// route rather than a slower version of the first.
///
/// A run reaches only its own question: `hooks::wait_for_run` finds the ask by the run id the key
/// names, and a run has at most one tool call in flight because the hook that asks is synchronous.
const ASK_WAIT_ROUTE: &str = "/hooks/ask-wait";

/// The third door: the OUTCOME of the same tool call the first two routes decided about and waited
/// on, reported after the fact.
///
/// It is not a narrower version of `HOOK_ROUTE` — the handler behind it never returns a `Decision`
/// and can never block anything, because by the time it fires the tool has already run (or already
/// failed). It sits in the same `Scope::Run` arm as the other two rather than in a table of its
/// own for the same reason they do: this is still one run asking about one tool call of its own,
/// now completing the loop instead of opening it.
const POSTTOOLUSE_ROUTE: &str = "/hooks/posttooluse";

/// The email sidecar's whole daemon surface: report what it fetched, and ask where it got to.
///
/// Two routes, and that is not a simplification — `daemon/client.go` builds a URL in exactly two
/// places, so this list is complete by construction rather than by inspection. The process on the
/// other end parses MIME written by strangers, which is the best reason in the system to keep the
/// blast radius of a parsing bug down to "it can tell the daemon about mail".
const EMAIL_ROUTES: &[(Method, &str)] = &[
    (Method::GET, "/email/cursor"),
    (Method::POST, "/email/incoming"),
];

/// Routes that expose state without changing it.
///
/// This is deliberately not "every GET": adding a route must not silently disclose it to every
/// read-only key. Kill-switch and budget readouts stay administrative because the packet treats
/// those control surfaces as a family, and token listing stays administrative because it reveals
/// durable credential metadata.
///
/// `GET /files/download` is here and the four routes that CHANGE that folder — `POST /files/folder`,
/// `/files/upload`, `/files/move` and `DELETE /files` — are in no table at all, which leaves them to
/// Admin and the control token. Reading a filed file discloses what `GET /email/{id}/attachments/…`
/// already does, so refusing it would protect nothing; rearranging or deleting somebody's folder is
/// a different act, and the folder holds the only copy of an attachment once the mail it came from
/// has expired.
const READ_ONLY_ROUTES: &[(Method, &str)] = &[
    (Method::GET, "/status"),
    (Method::GET, "/health/readout"),
    (Method::GET, "/backups"),
    (Method::GET, "/autopilot/state"),
    (Method::GET, "/projects"),
    // How much work fits and what is in flight. A state that changes nothing, and `/projects`'
    // companion — but written in by hand, because this table is not "every GET" and the comment
    // above says why.
    (Method::GET, "/concurrency"),
    // Watching a run work is watching. It returns the same bytes `GET /runs/{id}` already hands a
    // read-only key in `stdout`, only sooner — so withholding it would protect nothing and would
    // make the live view the one thing a reader had to be an admin to see.
    (Method::GET, "/runs/{id}/tail"),
    // The rules in force, beside `/concurrency` for the same reason it is here: it describes the
    // shape of the fleet and changes nothing. Asking for one is Admin's; reading which exist is not.
    (Method::GET, "/fleet/exclusions"),
    (Method::GET, "/fleet/exclusions/requests"),
    (Method::GET, "/projects/{id}/ls"),
    (Method::GET, "/projects/{id}/cat"),
    (Method::GET, "/projects/{id}/grep"),
    (Method::GET, "/projects/{id}/diff"),
    // The rest of the same family, added when the workspace gave them callers. Each is strictly
    // less than one already here: `blame` is metadata about lines `cat` returns whole, `log` and
    // `branches` are the history behind the `diff`, and `changed` is a file list plus a count. A
    // reader that may read the files and may not learn who last touched a line is a distinction
    // with nothing behind it.
    //
    // `readings` is thirty days of this project's own tallies — cost, gate, tokens, time to land —
    // and belongs beside `/projects` for the reason `/concurrency` does: it describes the shape of
    // the work and changes none of it.
    //
    // `worktree` hands back an absolute path on this machine. Listed anyway, and the argument is
    // the same one `/runs/{id}/tail` is listed under: the holder can already read every file in
    // that checkout through `cat?run=`, so withholding where it sits protects nothing and would
    // make the door to an editor the one thing a reader had to be an admin to open.
    //
    // `ownership` is the write boundary, read. Knowing which files the app would write is not
    // permission to write one — `POST /projects/{id}/write` is in no table at all — and a fence
    // only a privileged caller can see is a fence nobody can argue with.
    //
    // `map` lists every source path in the project and the import graph between them — and, since
    // the junction moved onto this route, the text of every approved decision read against that
    // graph. A reader who already reaches `cat`, `grep` and `blame` can already read the contents
    // of those same files, so a list of their names grants nothing it did not already have; and
    // the decisions are the grant `map/decisions` is justified under in the next paragraph, on an
    // argument that does not weaken by their being approved — either way they are sentences lifted
    // out of a spec sitting in a folder this reader can `cat` whole. So the permission does not
    // change and no row is added: `map` was already in this table. The sentence is corrected
    // rather than left standing, because a comment that still read "and nothing else" is how the
    // next reader concludes the decisions arrived on this route without anybody weighing them.
    //
    // `map/decisions` is the pile nobody has read yet: sentences a model lifted out of a document
    // already sitting in that project's folder. A reader who reaches `map` and `cat` can read the
    // spec they came from whole, so the extract of it grants nothing they did not already have.
    // Only the GET. `POST /projects/{id}/map/extract`, `POST /projects/{id}/map/decisions/{n}`,
    // `POST /projects/{id}/map/stamps` and `POST /projects/{id}/map/triage` are in NO table, here
    // or in `RUN_CREATING_ROUTES`, which leaves them to Admin and the control token by
    // default-deny — and all four belong there. Extracting spends a model, which a read-only key
    // never bought; approving is what puts a line in the map. Neither is a read, and neither is a
    // thing done on the owner's behalf by a weaker key.
    //
    // The fourth is the extraction's argument doubled and is listed rather than left implied,
    // because an enumeration that stopped at three is how the next reader concludes the triage
    // route was weighed and allowed: it spends a model PER DECISION, so a read-only key that could
    // reach it could spend a whole backlog's worth of somebody else's money in one request. It
    // takes no id at all, which makes the exposure the entire project rather than one line.
    //
    // The third has the sharpest claim of the four, and it is worth writing down because the
    // absence of a row is the entire enforcement and an absence explains nothing to whoever reads
    // this next. A stamp is the owner's VERDICT — *está como quero* — and §6 of the map spec takes
    // that one act away from the model on purpose: the model may compress a spec and may say a node
    // deserves a look, and may never put anything green. §6.1 gives the reason in one sentence:
    // returning that authority "pela porta da renderização" turns the map back into false confidence,
    // now carrying the authority of a traffic light. A read-only key writing one would be that same
    // door with a different handle — an automated caller minting the owner's answer, on the one
    // question this whole feature exists because only they can answer. Every other write here is a
    // thing the owner could have done themselves in a moment; this one is the moment.
    //
    // `map/specs` is the list of filenames a reader can already see through `ls` — it names
    // nothing `cat` could not already show. Naming them here is what lets the extraction button
    // offer a choice instead of a text box; it spends no model and settles nothing, so it costs
    // this key no more than `map` beside it does.
    //
    // `map/silenced` is the same grant a third time and needs no new argument: §6.2's pile is the
    // decisions a model saw nothing worth the owner's time in, each one a sentence lifted out of a
    // spec sitting in that project's folder — which a reader holding `map`, `map/decisions` and
    // `cat` can already read whole — plus the reason the triager gave and the name of the brain
    // that gave it. Neither of those two is a fact about anything outside this project.
    //
    // `map/orphan` says which file carried a `§` and in which commit it stopped, for one section
    // of one document. Every fact in that answer — a path, a commit id, a committer date, a commit
    // subject — is one `log` and `blame` beside it already hand over, out of the same repository,
    // and the section it is asked about is a heading from a spec `map/decisions` already grants.
    // It reads history where the routes above read the present, and that is a different git call
    // rather than a different grant.
    //
    // Listed rather than left implied, because §6.2 requires the pile to be *"sempre acessível"*
    // and a route that answered `403` to a key holding `map` would be a filter on that
    // accessibility wearing the shape of a permission. The `POST …/map/triage` beside it stays in
    // NO table for the reason given four paragraphs up, and the pair is worth reading together:
    // this is the pile a run left behind, and that one is what spends a model per decision to
    // fill it.
    (Method::GET, "/projects/{id}/readings"),
    (Method::GET, "/projects/{id}/log"),
    (Method::GET, "/projects/{id}/branches"),
    (Method::GET, "/projects/{id}/blame"),
    (Method::GET, "/projects/{id}/changed"),
    (Method::GET, "/projects/{id}/worktree"),
    (Method::GET, "/projects/{id}/ownership"),
    (Method::GET, "/projects/{id}/map"),
    (Method::GET, "/projects/{id}/map/decisions"),
    (Method::GET, "/projects/{id}/map/specs"),
    (Method::GET, "/projects/{id}/map/silenced"),
    (Method::GET, "/projects/{id}/map/orphan"),
    (Method::GET, "/feed"),
    // The same lines `/feed?scope=all` already hands this key, read as a window instead of the newest
    // fifty — so refusing it would protect nothing. Where the owner stopped reading is a number
    // about those lines. Moving it is `POST /feed/seen`, in no table: what the owner has seen is
    // theirs to say, and a key that could mark everything read could hide what the page surfaces.
    (Method::GET, "/feed/timeline"),
    (Method::GET, "/feed/seen"),
    (Method::GET, "/runs"),
    (Method::GET, "/presets"),
    (Method::GET, "/presets/{id}"),
    (Method::GET, "/runs/awaiting-approval"),
    (Method::GET, "/runs/{id}"),
    // A stop report reads rows that already happened: the run's own row plus, for a `gate` or
    // `timeout` kind, the `shadow_decisions` leading up to it. It starts nothing and holds no
    // resource, so a read-only key that could not reach it would have to be handed Admin — the
    // scope that CAN launch runs — for a route that launches none. The narrower grant is the
    // safer one.
    (Method::GET, "/runs/{id}/stop"),
    (Method::GET, "/assistant/{turn_id}"),
    (Method::GET, "/jobs"),
    (Method::GET, "/jobs/{id}"),
    (Method::GET, "/proposals"),
    (Method::GET, "/shadow-decisions"),
    (Method::GET, "/scoreboard"),
    (Method::GET, "/email/cursor"),
    (Method::GET, "/email/queue"),
    (Method::GET, "/email/{id}"),
    (Method::GET, "/email/{id}/attachments/{position}"),
    (Method::GET, "/email/{id}/attachments"),
    (Method::GET, "/files"),
    (Method::GET, "/files/download"),
    (Method::GET, "/files/search"),
    // Reading the archive of pages this machine has already fetched is a read of local state, like
    // the mail queue beside it. It reaches no network.
    (Method::GET, "/web/pages"),
    (Method::GET, "/web/pages/{id}"),
    // Reading one queued git operation's state. A ticket says what was asked for and how it ended;
    // it starts nothing, runs nothing and holds no repository.
    //
    // `GET /vcs/requests/{id}/wait` is deliberately NOT here, though it reads the same row and
    // returns the same shape. The argument is least privilege and nothing else: anyone who may read
    // a ticket can poll the route above and learn everything waiting would tell them, so granting
    // the wait buys the holder no capability it lacks — and an unneeded grant is one more thing to
    // be wrong about later.
    //
    // Deliberately NOT argued as load: a key that can poll `{id}` in a tight loop generates more
    // work than one blocked in a 45-second wait, so "it ties the daemon up" would be an argument a
    // disagreeing reader wins. What the exclusion removes is convenience, not capability, and that
    // is exactly why it costs nothing to keep.
    (Method::GET, "/vcs/requests"),
    (Method::GET, "/vcs/requests/{id}"),
    // Searching is listed here because the alternative is worse, not because it is free: it does
    // send a query off this machine. But it starts no run, holds no tools, and returns titles and
    // URLs — and a read-only key that cannot search would push every caller to Admin, which is the
    // scope that CAN start runs. The narrower grant is the safer one.
    (Method::POST, "/web/search"),
];

/// The current HTTP entry points that create a new run.
///
/// `POST /runs/{id}/message` is deliberately absent and must not be added. Creating a run authorises
/// the prompt supplied at that moment, in advance of the run existing; steering injects text into a
/// live session that already holds tools, past every check its creation went through — a prompt
/// nobody reviewed reaching a process nothing is about to review again. That is precisely what the
/// email pillar's design forbids for content nobody vouches for, so speaking into a run stays its own
/// authorization rather than a consequence of being allowed to start one.
///
/// `POST /jobs/{id}/notes` is deliberately absent for the `POST /runs/{id}/message` reason exactly,
/// and it is the entry most likely to be added here by mistake: `POST /jobs` is on the list below,
/// and a note lives at a URL one segment from it, so filing the two together reads as consistency.
/// It is not. Creating a job authorises the prompt supplied at that moment, before the work exists;
/// a note adds a second author to work already running past every check its creation went through,
/// and the wait between leaving it and its being read is the only difference from steering. Leaving
/// one is Admin's.
///
/// `POST /email/send` is deliberately absent from this table and from `READ_ONLY_ROUTES` both, for
/// the same shape of reason. A run-creating key buys the prompt it supplies at the moment it
/// supplies it; a message leaving this machine under the mailbox owner's own address is not
/// something that key ever bought, and it is the one act in the pillar its owner cannot undo.
/// `EMAIL_ROUTES` is the sharpest case: the sidecar is the process that parses MIME written by
/// strangers, so it must not hold the key to the route that replies to them. Sending is Admin's.
///
/// `POST /web/read` is deliberately absent from both tables, so it needs Admin. `POST /web/search`
/// is not, and the asymmetry is the point: a search returns titles and URLs, while a read pulls a
/// stranger's prose into this machine's store and index, where later callers will meet it. A
/// read-only key naming any URL it likes is a way to plant text for somebody else to read.
///
/// `POST /vcs/requests` is absent from both tables for the `POST /email/send` reason, not the
/// `POST /runs` one, and the distinction is the whole of it. A run-creating key buys a run: work in
/// a disposable worktree that a human reviews before anything of it survives. A queued merge is the
/// opposite end — it is the act that makes work survive, published to a branch other people build
/// on, and like a sent message it is the one thing in its pillar its owner cannot undo. That it is
/// spelled `POST` and mentions a repository makes it look like a sibling of `/runs`; it is a sibling
/// of `/email/send`. Queueing is Admin's.
///
/// `POST /github/requests` inherits that question and its answer without alteration, and it is the
/// clearer instance of the two: a queued merge at least leaves the machine only at the end, and this
/// route IS the leaving. It is absent from both tables, so a read-only key cannot reach it and a run
/// that tried to speak to the API directly cannot either.
///
/// **That absence is what makes ONE route safe for two tools.** The reading half and the acting half
/// share this door, and the partition between them is held by the parameter TYPES at the tool
/// boundary rather than by the transport. A transport-level partition would be worth having if this
/// were reachable by the agent the tools serve; it is not, because of the line above.
const RUN_CREATING_ROUTES: &[(Method, &str)] = &[
    (Method::POST, "/runs"),
    // A job is several runs over one worktree, so it belongs to the scope that buys runs rather
    // than to a scope of its own. What matters more is where it is NOT: `Scope::Run` reaches only
    // `HOOK_ROUTE`, so an autonomous run cannot ask for a job — and it must never be able to. Each
    // job starts runs, and a run that could start jobs would be a self-replication machine that no
    // brake in this house counts, because none of them counts recursion. That is the same escape
    // `runs.rs` describes closing for `POST /runs`.
    (Method::POST, "/jobs"),
    (Method::POST, "/webhooks/push"),
    (Method::POST, "/presets/{id}/run"),
    (Method::POST, "/assistant/message"),
    (Method::POST, "/email/triage"),
];

/// PURE: whether `scope` may perform `method` on `path`.
///
/// One table rather than a capability declared beside each route: this is a safety boundary, and a
/// boundary you have to reconstruct by reading forty route definitions is one nobody audits. A new
/// route is unreachable by a scoped key until someone adds it here on purpose.
///
/// Visible to the crate's tests so a module that ADDS routes can assert its own absence from a
/// scope's table where those routes are written — `team_trigger` does exactly that. The boundary is
/// still decided only here; being readable from a test is what makes forgetting to add a route
/// fail somewhere other than production.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn permits(scope: &Scope, method: &Method, path: &str) -> bool {
    match scope {
        Scope::Control => true,
        Scope::Run(_) => {
            method == Method::POST
                && (path == HOOK_ROUTE || path == ASK_WAIT_ROUTE || path == POSTTOOLUSE_ROUTE)
        }
        Scope::Service(Service::Email) => route_is_listed(EMAIL_ROUTES, method, path),
        Scope::Service(Service::Council) => route_is_listed(COUNCIL_ROUTES, method, path),
        Scope::TeamRun(_) => route_is_listed(TEAM_ROUTES, method, path),
        Scope::ApiToken(ApiTokenLevel::ReadOnly) => route_is_listed(READ_ONLY_ROUTES, method, path),
        Scope::ApiToken(ApiTokenLevel::RunCreating) => {
            route_is_listed(READ_ONLY_ROUTES, method, path)
                || route_is_listed(RUN_CREATING_ROUTES, method, path)
        }
        Scope::ApiToken(ApiTokenLevel::Admin) => true,
    }
}

fn route_is_listed(routes: &[(Method, &str)], method: &Method, path: &str) -> bool {
    routes
        .iter()
        .any(|(allowed, pattern)| allowed == method && path_matches(pattern, path))
}

fn path_matches(pattern: &str, path: &str) -> bool {
    let mut pattern = pattern.trim_matches('/').split('/');
    let mut actual = path.trim_matches('/').split('/');

    loop {
        match (pattern.next(), actual.next()) {
            (None, None) => return true,
            (Some(expected), Some(found))
                if expected == found
                    || (expected.starts_with('{')
                        && expected.ends_with('}')
                        && !found.is_empty()) => {}
            _ => return false,
        }
    }
}

/// A run's key and the secret to store for it: `<run_id>.<secret>`.
///
/// The id travels in the token so the lookup is by primary key rather than by the secret itself,
/// which keeps the comparison in Rust — and constant-time — instead of in SQLite's `=`.
pub fn mint_run_token(id: i64) -> (String, String) {
    let secret = generate_token();
    (format!("{id}.{secret}"), secret)
}

/// A durable API key and the secret stored for it: `api:<name>.<secret>`.
pub fn mint_api_token(name: &str) -> (String, String) {
    let secret = generate_token();
    (format!("api:{name}.{secret}"), secret)
}

/// Every route a council seat's tools reach, and nothing else.
///
/// This is the third of three independent reasons a seat cannot act, and the only one that holds
/// without anybody's cooperation. `ToolPolicy::McpOnly` is the CLI refusing itself every tool but
/// this server's; `hooks.rs` is the daemon refusing every name outside `mcp_tools::COUNCIL_TOOLS`
/// — and that second one is COOPERATIVE, because the `PreToolUse` hook fires only if the
/// `.claude/settings.json` resolved from the run's working directory registers it. A seat runs with
/// no working directory of its own. So the question "what if the hook never fires" has to have an
/// answer, and this table is it: with this key, `POST /runs` is 403 whatever the model decided.
///
/// It is why a seat gets a key of its own rather than the control token an orchestrator turn
/// carries. That turn holds the controls because approving a proposal on the owner's word is its
/// JOB; a council answers a question, and the design's third decision — reads only, never acts — is
/// a promise this list is what actually keeps.
///
/// Every entry is a GET of the owner's own state, which is the same content
/// `mcp_tools::COUNCIL_TOOLS` advertises. `GET /vcs/requests/{id}/wait` is absent for the reason
/// `READ_ONLY_ROUTES` gives for excluding it, and so is `vcs_ticket` from the tool list: it is the
/// read-back half of `vcs_request`, and a seat that cannot queue an operation has nothing to read
/// back.
const COUNCIL_ROUTES: &[(Method, &str)] = &[
    (Method::GET, "/projects"),
    (Method::GET, "/runs/{id}"),
    (Method::GET, "/proposals"),
    // Both are administrative for an API key and reachable here, and the difference is who is
    // asking: an API key is a credential somebody pasted into a script, while this one is minted at
    // startup, never leaves the daemon's own subprocesses, and reads back to the owner's own
    // question. What a council costs and whether autonomy is switched off are two of the things a
    // person convenes one to ask about.
    (Method::GET, "/autopilot/budget"),
    (Method::GET, "/autopilot/kill"),
    (Method::GET, "/email/queue"),
    (Method::GET, "/email/{id}"),
    (Method::GET, "/files"),
];

/// Every route a team agent's tools reach, and nothing else.
///
/// The argument is `COUNCIL_ROUTES`', repeated because the position is the same: `team.rs` launches
/// with `cwd: None`, so no `.claude/settings.json` of the owner's resolves and the `PreToolUse`
/// hook may never fire. This table is the answer to "what if the hook never fires" — with this key,
/// `POST /runs` is 403 whatever the model decided. Complete by construction, not by inspection.
///
/// It is pairs and not paths, and that is load-bearing: `http.rs` registers
/// `.route("/files", get(get_files).delete(delete_file))`, so a list of paths alone would hand a
/// department the deleting of the owner's folder along with the listing of it.
///
/// **Three deliberate differences from the council's list.**
///
/// `POST /web/search` and `POST /web/read` are here and are not there. A council answers from the
/// state of the machine; a department investigates the world, and one that cannot open a page
/// answers from what it half-remembers. The warning above `RUN_CREATING_ROUTES` — that a read
/// "pulls a stranger's prose into this machine's store and index, where later callers will meet it"
/// — is accepted rather than waved away, and it is what `web.rs` forcing `Requester::Autonomous`
/// for this scope pays for: what a department plants arrives quarantined.
///
/// `GET /autopilot/budget` and `GET /autopilot/kill` are there and are not here. The council's
/// reason — "what a council costs and whether autonomy is switched off are two of the things a
/// person convenes one to ask about" — does not transfer: a department is not convened to answer
/// about the machine, it is put to work in a domain.
///
/// `GET /projects`, `GET /runs/{id}` and `GET /proposals` are absent for that same reason. They are
/// the state of the house, which is a council's subject and not a marketing department's.
///
/// And one thing that is absent from BOTH, deliberately: `POST /hooks/pretooluse-decision`.
/// Including it would be worse than useless — `hooks::pretooluse_decision` checks the body's
/// `run_id` against the token only for `Scope::Run`, so a new scope on that route could name
/// somebody else's run, and every branch below reads that id's `mode`.
///
/// The list is NOT derived from `ToolEffect`. `ReadsOwn`/`ReadsUntrusted` answer "what does this do
/// to the turn that called it"; `permits` answers "what does this key reach". Deriving one from the
/// other would hand a department more reach than `ApiTokenLevel::ReadOnly` has — the kill switch,
/// the budget and the blocking `wait` would all arrive.
const TEAM_ROUTES: &[(Method, &str)] = &[
    // The run's own folder, and the only route of the teams design that this scope opens. Which
    // folder is decided by the token, never by an argument — see `team.rs`.
    (Method::POST, "/team-files/read"),
    // The first of the three routes in this table that are not reads. It does not perform anything:
    // it records what the department asked for, and the core acts later if a human agrees
    // (`team::propose_action`). That is what keeps this table from growing one entry per action a
    // department might want — a `POST /email/send` here would have been the first of six, and by the
    // sixth this scope would no longer be describable in a sentence.
    //
    // Its GET twin, which lists the queue, is deliberately absent: a department may ask, and may not
    // read what every other department has asked for.
    //
    // **This comment used to end "and the only one there will ever be", and the line below it used
    // to open "The second and last".** Both were written as promises and neither survived the next
    // feature. What they were reaching for is worth keeping and is said here once instead: a route
    // is added to this table only when it lets a department SAY something — ask for an action, ask
    // for a colleague, tell a colleague — never when it lets one DO something. That rule has held
    // three times; the counting has not.
    (Method::POST, "/team-actions"),
    // Like the one above it, it records a request rather than performing one: nobody joins the
    // catalogue until a person says so. Unlike it, the daemon answers differently depending on WHICH
    // NODE of the run is calling — a specialist is refused — and that is decided inside the handler
    // against `team_runs.director_run_id`, because `permits` answers about keys and a key belongs to
    // the run rather than to a node.
    (Method::POST, "/team-recruits"),
    // Words for a colleague in the same department. The one route here whose effect is entirely
    // INSIDE the run that calls it: nothing leaves the machine, nobody is asked to approve, and what
    // it writes is read by another node holding this very same key.
    //
    // That containment is why the tool behind it is graded `WritesOwn` while the two above are
    // `Acts` — see `mcp_tools::TOOL_EFFECTS`. It is also why this route needs no ceiling in
    // `permits`: what bounds it is a per-run count the handler reads, and a scope cannot count.
    (Method::POST, "/team-notes"),
    // Words for the OWNER, in the one conversation this run was pointed at. It reaches outside the
    // department, which every other entry in this table does not — and it is still a SAYING and not
    // a DOING, which is the rule this table keeps: nothing runs, nothing is spent, and the daemon
    // chooses the destination from a column the department cannot read or set.
    (Method::POST, "/team-reports"),
    (Method::GET, "/email/queue"),
    (Method::GET, "/email/{id}"),
    (Method::GET, "/files"),
    (Method::POST, "/web/search"),
    (Method::POST, "/web/read"),
];

/// A team run's key and the secret to store for it: `team:<team_run_id>.<secret>`.
///
/// The prefix is not decoration. `resolve` picks the table by prefix — `api:` for durable keys, a
/// service name for sidecars, and anything numeric for a run — and a TEXT team-run id would either
/// collide with the numeric branch or resolve nowhere. `team:` follows the shape `api:` already
/// established, so the comment saying the prefix "picks the table without a second marker" now
/// names four families instead of three.
pub fn mint_team_token(team_run_id: &str) -> (String, String) {
    let secret = generate_token();
    (format!("team:{team_run_id}.{secret}"), secret)
}

/// A process the daemon launches and hands a key of its own, rather than the control token.
///
/// The telegram sidecar deliberately keeps the control token: it is the user's remote control — it
/// approves proposals, works the kill switch and cancels runs, the same surface the shell has — so
/// an allowlist for it would be all of Control minus a handful of routes, which reads like a
/// boundary without being one. Narrowing it means first deciding what a chat message is allowed to
/// do, and that is a product decision, not a plumbing one.
///
/// `Council` is not a sidecar and belongs here anyway, because what this enum actually enumerates is
/// "a subprocess of ours that must not hold the controls". A council seat is an agent CLI the daemon
/// spawns, and the argument for scoping its key is stronger than the email sidecar's: there are up
/// to eight of them at once, each one a model deciding what to call next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    Email,
    Council,
}

impl Service {
    /// The `service_tokens.name` this is stored under, and the prefix in its key. Never numeric, so
    /// it cannot be confused with a run id.
    fn name(self) -> &'static str {
        match self {
            Service::Email => "email",
            Service::Council => "council",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "email" => Some(Service::Email),
            "council" => Some(Service::Council),
            _ => None,
        }
    }
}

/// Mints a sidecar's key and stores it, replacing whatever the previous daemon left behind.
///
/// Returns what goes in the sidecar's environment. Called before the sidecar is spawned — a key
/// stored afterwards would 401 whatever the sidecar did first.
pub async fn mint_service_token(
    pool: &sqlx::SqlitePool,
    service: Service,
) -> Result<String, sqlx::Error> {
    let secret = generate_token();
    sqlx::query("INSERT OR REPLACE INTO service_tokens (name, token) VALUES (?, ?)")
        .bind(service.name())
        .bind(&secret)
        .execute(pool)
        .await?;
    Ok(format!("{}.{secret}", service.name()))
}

/// Mints a conversation's key and stores it, retiring whatever the process before it was given.
///
/// Async and storing, like `mint_service_token` and unlike `mint_run_token`, because there is no row
/// of its own to hang the secret on: a conversation outlives every one of its turns, and the turn
/// rows come and go underneath it.
///
/// Called before the CLI is spawned. A key stored afterwards would 401 whatever the process did
/// first, and the first thing a rooted turn does is ask the gate about its first tool call.
pub async fn mint_chat_token(
    pool: &sqlx::SqlitePool,
    chat_id: &str,
) -> Result<String, sqlx::Error> {
    let secret = generate_token();
    sqlx::query("INSERT OR REPLACE INTO chat_tokens (chat_id, token) VALUES (?, ?)")
        .bind(chat_id)
        .bind(&secret)
        .execute(pool)
        .await?;
    Ok(format!("chat:{chat_id}.{secret}"))
}

/// Resolves a presented bearer to a scope, or `None` if it authenticates nothing.
async fn resolve(state: &AppState, presented: &str) -> Option<Scope> {
    // Constant-time compare: `==` on the token short-circuits at the first differing byte, timing
    // which leaks the secret's content one byte at a time. `ct_eq` only short-circuits on a length
    // mismatch, and the length is not the secret.
    if bool::from(presented.as_bytes().ct_eq(state.token.0.as_bytes())) {
        return Some(Scope::Control);
    }

    // Read whole, before the split every other family shares, and split from the RIGHT. The
    // others take everything before the FIRST dot as their prefix, which is safe for a fixed
    // service name and for an integer, and is not safe for a conversation id: ids are UUIDs today,
    // but a dotted one would silently become somebody else's lookup rather than a failed one. The
    // secret is `generate_token`'s alphanumerics and can never hold a dot, so the last one is
    // always the separator.
    if let Some(rest) = presented.strip_prefix("chat:") {
        let (chat_id, secret) = rest.rsplit_once('.')?;
        let stored: String = sqlx::query_scalar("SELECT token FROM chat_tokens WHERE chat_id = ?")
            .bind(chat_id)
            .fetch_optional(&state.pool)
            .await
            .ok()??;
        // The secret first, and the conversation's state only after it matches. Resolving the run
        // first would answer a caller holding no secret at all — whether this conversation is
        // busy — by how long the refusal took.
        if !bool::from(secret.as_bytes().ct_eq(stored.as_bytes())) {
            return None;
        }
        // Resolved when presented rather than when minted, which is the whole reason this family
        // exists. A CLI kept alive across turns is handed its environment once, at spawn, so a key
        // naming the turn that spawned it would still name that turn five turns later — wrong
        // attribution, and a run key that never dies.
        //
        // Nothing running means nothing to name. That is the rule `runs.token` already follows,
        // kept rather than weakened: between turns this opens no door.
        return sqlx::query_scalar::<_, i64>(
            "SELECT id FROM runs WHERE chat_id = ? AND status = 'running' ORDER BY id DESC LIMIT 1",
        )
        .bind(chat_id)
        .fetch_optional(&state.pool)
        .await
        .ok()?
        .map(Scope::Run);
    }

    let (prefix, secret) = presented.split_once('.')?;

    // `api:<name>` contains a colon: it is neither the integer prefix of a run nor any service
    // name (service names are fixed enum values), so the three credential families cannot collide.
    if let Some(name) = prefix.strip_prefix("api:") {
        let (stored, level) = sqlx::query_as::<_, (String, String)>(
            "SELECT token, access_level FROM api_tokens WHERE name = ?",
        )
        .bind(name)
        .fetch_optional(&state.pool)
        .await
        .ok()??;
        let level = ApiTokenLevel::from_str(&level)?;
        return bool::from(secret.as_bytes().ct_eq(stored.as_bytes()))
            .then_some(Scope::ApiToken(level));
    }

    // `team:<id>` contains a colon for the same reason `api:<name>` does, and the id is TEXT, so
    // without the prefix it would either be read as a service name or fail the integer parse below.
    if let Some(team_run_id) = prefix.strip_prefix("team:") {
        let (stored, run_state) = sqlx::query_as::<_, (String, String)>(
            "SELECT token, state FROM team_runs WHERE id = ?",
        )
        .bind(team_run_id)
        .fetch_optional(&state.pool)
        .await
        .ok()??;

        // A team token dies with its run, for the reason the run branch below gives and more so: it
        // lives for hours and crosses dozens of subprocesses, which makes it the longest-lived key
        // in the house and the one that most needs the rule. `team::is_live` rather than a fourth
        // copy of the state list.
        if !crate::team::is_live(&run_state) {
            return None;
        }
        return bool::from(secret.as_bytes().ct_eq(stored.as_bytes()))
            .then_some(Scope::TeamRun(team_run_id.to_owned()));
    }

    // A service name never parses as an integer and a run id always does, so the remaining prefix
    // picks the table without a second marker to keep in sync.
    if let Some(service) = Service::from_name(prefix) {
        let stored: String = sqlx::query_scalar("SELECT token FROM service_tokens WHERE name = ?")
            .bind(prefix)
            .fetch_optional(&state.pool)
            .await
            .ok()??;
        return bool::from(secret.as_bytes().ct_eq(stored.as_bytes()))
            .then_some(Scope::Service(service));
    }

    let id: i64 = prefix.parse().ok()?;
    let (stored, status) = sqlx::query_as::<_, (Option<String>, String)>(
        "SELECT token, status FROM runs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .ok()??;

    // A run token dies with its run. Otherwise a finished run's environment — still sitting in a
    // log, a crash dump, or a child process that outlived the CLI — would stay a working key long
    // after the run it belonged to stopped being governed by anything.
    if status != "running" {
        return None;
    }
    // `None` is every row written before migration 0022 and every orchestrator turn: no stored
    // secret, so nothing to match, so the token authenticates nothing.
    bool::from(secret.as_bytes().ct_eq(stored?.as_bytes())).then_some(Scope::Run(id))
}

pub async fn require_token(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?
        .to_owned();

    let scope = resolve(&state, &presented)
        .await
        .ok_or(StatusCode::UNAUTHORIZED)?;

    // 403 rather than 401: the caller authenticated, it is simply not allowed here. Warned rather
    // than silently refused, because a run reaching for a control route is the exact signature of
    // the thing this scope exists to stop, and it should be visible when it happens.
    if !permits(&scope, req.method(), req.uri().path()) {
        tracing::warn!(
            ?scope,
            method = %req.method(),
            path = %req.uri().path(),
            "refused a request outside the caller's scope"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    // Handlers that care WHICH run is calling read this rather than believing the request body;
    // `hooks::pretooluse_decision` is the one that does.
    req.extensions_mut().insert(scope);
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeCommandRunner;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use axum::routing::{get, post};
    use std::sync::Arc;
    use tower::ServiceExt;

    #[test]
    fn generated_uuid_v4_has_the_required_format_version_and_variant() {
        let uuid = generate_uuid_v4();
        let bytes = uuid.as_bytes();

        assert_eq!(uuid.len(), 36);
        assert!(uuid.is_ascii());
        assert!(
            [8, 13, 18, 23]
                .into_iter()
                .all(|index| bytes[index] == b'-')
        );
        assert!(
            bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit())
        );
        assert_eq!(bytes[14], b'4');
        assert!(matches!(bytes[19], b'8' | b'9' | b'a' | b'b'));
        assert_eq!(uuid, uuid.to_ascii_lowercase());
    }

    async fn test_state(token: &str) -> AppState {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        // Run tokens are resolved against the `runs` table, so this can no longer be a bare pool.
        sqlx::migrate!().run(&pool).await.unwrap();
        AppState {
            token: Token(token.to_string()),
            pool,
            telegram_doctrine: None,
            runner: Arc::new(FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            assistants: std::sync::Arc::new(crate::assistants::NoAssistants),
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_tails: Default::default(),
            files_root: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            browser: std::sync::Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: std::sync::Arc::new(crate::github::GithubRuntime::default()),
            web: std::sync::Arc::new(crate::web::WebRuntime::disabled()),
            quota: std::sync::Arc::new(crate::quota::QuotaRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    fn protected_router(state: AppState) -> Router {
        Router::new()
            .route("/secret", get(|| async { "top secret" }))
            .route("/status", get(|| async {}))
            .route("/health/readout", get(|| async {}))
            .route("/backup", post(|| async {}))
            .route("/backups", get(|| async {}))
            .route("/backups/{name}/restore", post(|| async {}))
            .route("/autopilot/state", get(|| async {}).post(|| async {}))
            .route("/autopilot/kill", get(|| async {}).post(|| async {}))
            .route("/autopilot/budget", get(|| async {}).post(|| async {}))
            .route("/projects", get(|| async {}))
            .route("/projects/{id}/cat", get(|| async {}))
            .route("/feed", get(|| async {}))
            .route("/runs", get(|| async {}).post(|| async {}))
            .route("/runs/{id}", get(|| async {}))
            // A stand-in, like the gate route below: this file's tests are about who may reach a
            // path, and the path is what `permits` matches on.
            .route("/runs/{id}/stop", get(|| async {}))
            .route("/jobs", get(|| async {}).post(|| async {}))
            .route("/webhooks/push", post(|| async {}))
            .route("/presets", get(|| async {}).post(|| async {}))
            .route("/presets/{id}/run", post(|| async {}))
            .route("/assistant/message", post(|| async {}))
            .route("/assistant/{turn_id}", get(|| async {}))
            .route("/proposals", get(|| async {}))
            // A stand-in for the real gate route: these tests are about who may reach it, and the
            // path is what `permits` matches on.
            .route(HOOK_ROUTE, post(|| async { "decided" }))
            .route(ASK_WAIT_ROUTE, post(|| async { "waited" }))
            .route(POSTTOOLUSE_ROUTE, post(|| async { "recorded" }))
            .route("/proposals/{id}/approve", post(|| async {}))
            .route("/worktrees/{run_id}/release", post(|| async {}))
            // All three, because the point of the test below is that they are graded differently:
            // reading a ticket or the queue is a read, submitting work is not, and waiting is a read
            // that is refused anyway for holding the connection.
            .route("/vcs/requests", get(|| async {}).post(|| async {}))
            .route("/vcs/requests/{id}", get(|| async {}))
            .route("/vcs/requests/{id}/wait", get(|| async {}))
            .route("/shadow-decisions", get(|| async {}))
            .route("/shadow-decisions/{id}/verdict", post(|| async {}))
            .route("/scoreboard", get(|| async {}))
            .route("/email/cursor", get(|| async { "" }).post(|| async { "" }))
            .route("/email/incoming", post(|| async { "" }))
            .route("/email/triage", post(|| async {}))
            .route("/email/{id}/attachments", get(|| async {}))
            // The five a team run reaches beside `/files`, plus the one it must never reach.
            // `/email/{id}` sits beside `/email/{id}/attachments` on purpose: the table is matched
            // segment by segment, so a scope holding the shorter pattern must not inherit the
            // longer one.
            .route("/email/queue", get(|| async {}))
            .route("/email/{id}", get(|| async {}))
            .route("/email/send", post(|| async {}))
            .route("/web/search", post(|| async {}))
            .route("/web/read", post(|| async {}))
            .route("/team-files/read", post(|| async {}))
            // Registered with both methods, so the negative assertion below — a department may POST
            // an action and may not LIST the queue — is answered by `permits` rather than by the
            // router not knowing the path.
            .route("/team-actions", post(|| async {}).get(|| async {}))
            .route("/team-notes", post(|| async {}))
            .route("/team-reports", post(|| async {}))
            .route("/team-recruits", post(|| async {}).get(|| async {}))
            .route("/files", get(|| async {}).delete(|| async {}))
            .route("/files/folder", post(|| async {}))
            .route("/files/download", get(|| async {}))
            .route("/files/search", get(|| async {}))
            .route("/files/upload", post(|| async {}))
            .route("/files/move", post(|| async {}))
            .route("/api-tokens", get(|| async {}).post(|| async {}))
            .route("/api-tokens/{name}", axum::routing::delete(|| async {}))
            // The browser pillar, mounted so the refusals below are refusals of a route that
            // exists. Without these the assertions would pass against a 404 that never reached the
            // classifier, which is the shape of a test that stops noticing.
            .route("/browser/open", post(|| async {}))
            .route("/browser/act", post(|| async {}))
            .route("/browser/look", post(|| async {}))
            .route("/browser/revoke", post(|| async {}))
            .route("/browser/forget", post(|| async {}))
            .route("/browser/handoff", post(|| async {}))
            .route("/browser/return", post(|| async {}))
            .route("/browser/keep", post(|| async {}))
            .route("/browser/sessions", get(|| async {}))
            .route("/browser/readonly", post(|| async {}))
            .route("/browser/sites/{project_id}", get(|| async {}))
            .route("/browser/writes/{project_id}", get(|| async {}))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                require_token,
            ))
            .with_state(state)
    }

    /// A run in the state a real one is in when its CLI calls the gate: `running`, with its minted
    /// secret stored. Returns what the CLI would find in `NUCLEOS_DAEMON_TOKEN`.
    async fn running_run_with_token(state: &AppState) -> (i64, String) {
        let id = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('x', 'running', 'worktree', '2026-01-01T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let (token, secret) = mint_run_token(id);
        sqlx::query("UPDATE runs SET token = ? WHERE id = ?")
            .bind(&secret)
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
        (id, token)
    }

    /// A team run in the state a real one is in while its specialists work, with the whole chain
    /// above it — agent, team, run — written out rather than faked.
    ///
    /// The chain is spelled in full even though this pool has foreign keys off, so that the day
    /// somebody turns them on here these tests do not become the ones that mysteriously fail.
    /// Returns what a specialist's CLI would find in its environment.
    async fn live_team_run_with_token(state: &AppState, id: &str) -> String {
        sqlx::query(
            "INSERT OR IGNORE INTO agents
                 (id, name, speciality, prompt, engine, model, tool_policy, created_at, updated_at)
             VALUES ('director', 'Director', 'plans', 'p', 'claude', NULL, 'mcp_only',
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT OR IGNORE INTO teams
                 (id, name, mission, director_agent_id, max_rounds, max_parallel, budget_usd,
                  created_at, updated_at)
             VALUES ('marketing', 'Marketing', 'sell', 'director', 3, 2, NULL,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let (token, secret) = mint_team_token(id);
        sqlx::query(
            "INSERT INTO team_runs
                 (id, team_id, request, workspace, token, state, created_at, updated_at)
             VALUES (?, 'marketing', 'write the launch post', ?, ?, 'working',
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(format!("teams/marketing/{id}"))
        .bind(&secret)
        .execute(&state.pool)
        .await
        .unwrap();
        token
    }

    async fn stored_api_token(state: &AppState, name: &str, level: ApiTokenLevel) -> String {
        let (token, secret) = mint_api_token(name);
        sqlx::query(
            "INSERT INTO api_tokens (name, token, access_level, created_at)
             VALUES (?, ?, ?, '2026-01-01T00:00:00Z')",
        )
        .bind(name)
        .bind(secret)
        .bind(level.as_str())
        .execute(&state.pool)
        .await
        .unwrap();
        token
    }

    async fn status_of(app: &Router, method: &str, uri: &str, bearer: &str) -> StatusCode {
        app.clone()
            .oneshot(
                HttpRequest::builder()
                    .method(method)
                    .uri(uri)
                    .header("Authorization", format!("Bearer {bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    const ROUTE_FAMILY_CASES: &[(&str, &str)] = &[
        ("GET", "/status"),
        ("GET", "/health/readout"),
        ("POST", "/backup"),
        ("GET", "/autopilot/state"),
        ("GET", "/projects"),
        ("GET", "/feed"),
        ("GET", "/runs"),
        ("GET", "/presets"),
        ("POST", "/assistant/message"),
        ("GET", "/proposals"),
        ("POST", "/worktrees/7/release"),
        ("GET", "/shadow-decisions"),
        ("GET", "/scoreboard"),
        ("GET", "/email/cursor"),
        ("GET", "/files"),
        ("POST", HOOK_ROUTE),
        ("POST", POSTTOOLUSE_ROUTE),
        ("GET", "/api-tokens"),
    ];

    #[tokio::test]
    async fn rejects_missing_token() {
        let app = protected_router(test_state("expected-token").await);
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// The new door, checked the same way `rejects_missing_token` checks every other one: a hook
    /// call reporting an outcome is exactly as unauthenticated without a bearer token as any other
    /// request, and this route must not have grown an exception for it.
    #[tokio::test]
    async fn posttooluse_route_rejects_missing_token() {
        let app = protected_router(test_state("expected-token").await);
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri(POSTTOOLUSE_ROUTE)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_wrong_token() {
        let app = protected_router(test_state("expected-token").await);
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/secret")
                    .header("Authorization", "Bearer wrong-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_token_prefix() {
        let app = protected_router(test_state("expected-token").await);
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/secret")
                    .header("Authorization", "Bearer expected")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_malformed_scheme() {
        for header in ["expected-token", "bearer expected-token", "Bearer"] {
            let app = protected_router(test_state("expected-token").await);
            let response = app
                .oneshot(
                    HttpRequest::builder()
                        .uri("/secret")
                        .header("Authorization", header)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "header {header:?} should not authenticate"
            );
        }
    }

    #[tokio::test]
    async fn accepts_correct_token() {
        let app = protected_router(test_state("expected-token").await);
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/secret")
                    .header("Authorization", "Bearer expected-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// The whole point of the scope. A `worktree` or `shadow` run has a Bash tool and the classifier
    /// permits `echo $NUCLEOS_DAEMON_TOKEN`, so whatever is in its environment must be assumed
    /// published. What it opens is three routes, all of them the same conversation about its own
    /// tool calls.
    #[tokio::test]
    async fn a_run_token_opens_the_gate_route_and_nothing_else() {
        let state = test_state("control-token").await;
        let (_, run_token) = running_run_with_token(&state).await;
        let app = protected_router(state);

        assert_eq!(
            status_of(&app, "POST", HOOK_ROUTE, &run_token).await,
            StatusCode::OK,
            "a run must still be able to ask the gate about its own tool call"
        );
        // The second half of that same question. The gate answers in milliseconds because the hook
        // can only wait five seconds; a call somebody has to say yes to comes back here instead, and
        // this call blocks until they do.
        assert_eq!(
            status_of(&app, "POST", ASK_WAIT_ROUTE, &run_token).await,
            StatusCode::OK,
            "a run must be able to wait for the answer it was told to wait for"
        );
        // The third leg: reporting back what the tool call it asked about actually did.
        assert_eq!(
            status_of(&app, "POST", POSTTOOLUSE_ROUTE, &run_token).await,
            StatusCode::OK,
            "a run must be able to report the outcome of its own tool call"
        );
        // 403, not 401: it authenticated. It is simply not allowed to approve anything.
        assert_eq!(
            status_of(&app, "POST", "/proposals/7/approve", &run_token).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status_of(&app, "GET", "/secret", &run_token).await,
            StatusCode::FORBIDDEN
        );
    }

    /// The door a job route must never open: an autonomous run asking for a job.
    ///
    /// Each job starts runs, so a run that could start jobs is a self-replication machine — and not
    /// one brake in this house counts recursion. The budget counts dollars, the WIP limit counts
    /// unreviewed proposals, and the slot ceiling counts pieces of work in flight.
    ///
    /// **Honest note on what this test is worth.** It passes before `POST /jobs` was added to any
    /// table as well as after, because `permits` gives `Scope::Run` a small, named list of routes
    /// (`HOOK_ROUTE`, `ASK_WAIT_ROUTE`, `POSTTOOLUSE_ROUTE`) and everything else is refused by
    /// construction. So it did not drive the change and it is not evidence the change works — it
    /// is a pin, and its value is the day somebody widens `Scope::Run` further and has to decide,
    /// in front of this assertion, whether jobs are on the list. The test that DID have to fail
    /// first is the one below it.
    #[tokio::test]
    async fn a_run_token_cannot_ask_for_a_job() {
        let state = test_state("control-token").await;
        let (_, run_token) = running_run_with_token(&state).await;
        let app = protected_router(state);

        // 403 rather than 401: it authenticated perfectly well. It is simply not something that
        // gets to ask for a night's work.
        assert_eq!(
            status_of(&app, "POST", "/jobs", &run_token).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status_of(&app, "GET", "/jobs", &run_token).await,
            StatusCode::FORBIDDEN
        );
    }

    /// The agent catalogue is the owner's, and stays that way by being in no scope table. Asserted
    /// rather than left to the absence of a line, because an absence does not fail when it ends.
    #[test]
    fn no_scoped_key_reaches_the_agent_catalogue() {
        for path in ["/agents", "/agents/copywriter"] {
            assert!(!permits(&Scope::Run(1), &Method::GET, path));
            assert!(!permits(
                &Scope::Service(Service::Council),
                &Method::GET,
                path
            ));
            assert!(!permits(
                &Scope::ApiToken(ApiTokenLevel::ReadOnly),
                &Method::GET,
                path
            ));
            assert!(!permits(&Scope::Run(1), &Method::DELETE, path));
        }
    }

    /// The grant that this change actually makes, and the one that failed before it.
    ///
    /// A job is several runs over one worktree, so the key that buys runs buys it. Asserting the
    /// refusals beside it is what stops this from reading as "run-creating became admin".
    #[tokio::test]
    async fn a_run_creating_api_key_may_ask_for_a_job() {
        let state = test_state("control-token").await;
        let token = stored_api_token(&state, "launcher", ApiTokenLevel::RunCreating).await;
        let app = protected_router(state);

        assert_eq!(
            status_of(&app, "POST", "/jobs", &token).await,
            StatusCode::OK,
            "a key that may start runs may ask for the job that starts several"
        );
        assert_eq!(
            status_of(&app, "POST", "/autopilot/kill", &token).await,
            StatusCode::FORBIDDEN,
            "and it is still not an admin key"
        );
    }

    /// A run's environment outlives the run — in a log, a crash dump, a child process that survived
    /// the CLI. The key must not.
    #[tokio::test]
    async fn a_run_token_stops_working_when_its_run_stops_running() {
        let state = test_state("control-token").await;
        let (id, run_token) = running_run_with_token(&state).await;
        let app = protected_router(state.clone());

        assert_eq!(
            status_of(&app, "POST", HOOK_ROUTE, &run_token).await,
            StatusCode::OK
        );

        for status in [
            "completed",
            "failed",
            "timed_out",
            "cancelled",
            "interrupted",
        ] {
            sqlx::query("UPDATE runs SET status = ? WHERE id = ?")
                .bind(status)
                .bind(id)
                .execute(&state.pool)
                .await
                .unwrap();
            assert_eq!(
                status_of(&app, "POST", HOOK_ROUTE, &run_token).await,
                StatusCode::UNAUTHORIZED,
                "a {status} run's token must not still open the gate"
            );
        }
    }

    /// A turn of this conversation, running, the way one is while its CLI is alive.
    async fn running_turn_of(state: &AppState, chat_id: &str) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, chat_id, created_at)
             VALUES ('x', 'running', 'assistant', ?, '2026-01-01T00:00:00Z')",
        )
        .bind(chat_id)
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// A CLI kept alive across turns is handed its environment once, at spawn, so a key naming the
    /// turn that spawned it would still be presented five turns later. This key names the
    /// CONVERSATION and is resolved, when presented, to whatever turn of it is running — so what
    /// the gate is told is what is actually happening, not what was happening at spawn.
    #[tokio::test]
    async fn a_chat_key_names_whichever_turn_of_that_conversation_is_running() {
        let state = test_state("control-token").await;
        let key = mint_chat_token(&state.pool, "c-1").await.unwrap();

        let first = running_turn_of(&state, "c-1").await;
        assert_eq!(resolve(&state, &key).await, Some(Scope::Run(first)));

        // The first turn ends and a second begins, down the same living process, presenting the
        // same key. It must now name the second turn — that is the whole point of the family.
        sqlx::query("UPDATE runs SET status = 'completed' WHERE id = ?")
            .bind(first)
            .execute(&state.pool)
            .await
            .unwrap();
        let second = running_turn_of(&state, "c-1").await;
        assert_eq!(resolve(&state, &key).await, Some(Scope::Run(second)));
    }

    /// The rule `runs.token` follows, kept rather than weakened: between turns there is nothing for
    /// this key to name, so it authenticates nothing. A living process idling between turns holds a
    /// key that opens no door until somebody speaks to it again.
    #[tokio::test]
    async fn a_chat_key_authenticates_nothing_between_turns() {
        let state = test_state("control-token").await;
        let key = mint_chat_token(&state.pool, "c-1").await.unwrap();

        assert_eq!(resolve(&state, &key).await, None);

        let turn = running_turn_of(&state, "c-1").await;
        assert!(resolve(&state, &key).await.is_some());

        sqlx::query("UPDATE runs SET status = 'cancelled' WHERE id = ?")
            .bind(turn)
            .execute(&state.pool)
            .await
            .unwrap();
        assert_eq!(resolve(&state, &key).await, None);
    }

    /// Two conversations answering at once is the ordinary case, not the exotic one. A key that
    /// resolved to "some running assistant turn" would let either of them act as the other.
    #[tokio::test]
    async fn a_chat_key_cannot_reach_another_conversations_turn() {
        let state = test_state("control-token").await;
        let mine = mint_chat_token(&state.pool, "mine").await.unwrap();

        let theirs = running_turn_of(&state, "theirs").await;

        assert_eq!(resolve(&state, &mine).await, None);

        let ours = running_turn_of(&state, "mine").await;
        assert_eq!(resolve(&state, &mine).await, Some(Scope::Run(ours)));
        assert_ne!(ours, theirs);
    }

    /// A process that died leaves its secret in whatever outlived it. The successor's spawn must
    /// take that key out of service, which is why minting replaces rather than adds.
    #[tokio::test]
    async fn minting_a_conversations_key_again_retires_the_one_before_it() {
        let state = test_state("control-token").await;
        let first = mint_chat_token(&state.pool, "c-1").await.unwrap();
        let second = mint_chat_token(&state.pool, "c-1").await.unwrap();
        running_turn_of(&state, "c-1").await;

        assert_ne!(first, second);
        assert_eq!(resolve(&state, &first).await, None);
        assert!(resolve(&state, &second).await.is_some());
    }

    /// Every other family takes what is before the FIRST dot as its prefix. A conversation id is
    /// the one prefix a person could one day choose, so this family splits from the right — and a
    /// dotted id must resolve to itself rather than to a truncated stranger.
    #[tokio::test]
    async fn a_chat_key_survives_a_conversation_id_with_a_dot_in_it() {
        let state = test_state("control-token").await;
        let key = mint_chat_token(&state.pool, "notes.v2").await.unwrap();
        let turn = running_turn_of(&state, "notes.v2").await;

        assert_eq!(resolve(&state, &key).await, Some(Scope::Run(turn)));

        // And the truncation itself must not be a way in: a turn of the conversation the naive
        // split WOULD have named cannot be reached with this key.
        running_turn_of(&state, "notes").await;
        let truncated = format!("chat:notes.{}", key.rsplit_once('.').unwrap().1);
        assert_eq!(resolve(&state, &truncated).await, None);
    }

    /// It resolves to `Scope::Run`, so it inherits that scope's one route and nothing else. Asserted
    /// rather than assumed: this key is carried by turns that DO have Bash, Read and Write, which is
    /// exactly the population that could look at its own environment and try the rest of the house.
    #[tokio::test]
    async fn a_chat_key_opens_the_gate_route_and_nothing_else() {
        let state = test_state("control-token").await;
        let key = mint_chat_token(&state.pool, "c-1").await.unwrap();
        running_turn_of(&state, "c-1").await;
        let app = protected_router(state.clone());

        assert_eq!(
            status_of(&app, "POST", HOOK_ROUTE, &key).await,
            StatusCode::OK
        );
        // 403 and not 401, for the reason the run token's own test gives: it authenticated. Which
        // is the point being asserted — it authenticates as a RUN, and a run may not approve.
        assert_eq!(
            status_of(&app, "POST", "/proposals/7/approve", &key).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status_of(&app, "GET", "/jobs", &key).await,
            StatusCode::FORBIDDEN
        );
    }

    /// Every `runs` row written before migration 0022 has a NULL token, and so does every
    /// orchestrator turn. `NULL` must not be something a caller can match by guessing the shape.
    #[tokio::test]
    async fn a_run_with_no_stored_secret_authenticates_nothing() {
        let state = test_state("control-token").await;
        let id = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('x', 'running', 'worktree', '2026-01-01T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let app = protected_router(state);

        for guess in [format!("{id}."), format!("{id}.null"), format!("{id}.{id}")] {
            assert_eq!(
                status_of(&app, "POST", HOOK_ROUTE, &guess).await,
                StatusCode::UNAUTHORIZED,
                "{guess:?} must not authenticate"
            );
        }
    }

    #[tokio::test]
    async fn a_token_naming_a_run_that_does_not_exist_authenticates_nothing() {
        let state = test_state("control-token").await;
        let app = protected_router(state);

        for bogus in ["999.secret", "abc.secret", "-1.secret", ".", "0.0"] {
            assert_eq!(
                status_of(&app, "POST", HOOK_ROUTE, bogus).await,
                StatusCode::UNAUTHORIZED,
                "{bogus:?} must not authenticate"
            );
        }
    }

    /// One run's key must not open another run's, even though both are live and both are runs.
    #[tokio::test]
    async fn one_runs_token_does_not_become_anothers() {
        let state = test_state("control-token").await;
        let (first_id, first_token) = running_run_with_token(&state).await;
        let (second_id, _) = running_run_with_token(&state).await;
        assert_ne!(first_id, second_id);

        let forged = format!("{second_id}.{}", first_token.split_once('.').unwrap().1);
        let app = protected_router(state);
        assert_eq!(
            status_of(&app, "POST", HOOK_ROUTE, &forged).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn read_only_is_exactly_the_explicit_read_route_table() {
        let scope = Scope::ApiToken(ApiTokenLevel::ReadOnly);
        for (method, pattern) in READ_ONLY_ROUTES {
            let path = pattern
                .split('/')
                .map(|segment| {
                    if segment.starts_with('{') {
                        "7"
                    } else {
                        segment
                    }
                })
                .collect::<Vec<_>>()
                .join("/");
            assert!(
                permits(&scope, method, &path),
                "{method} {path} should be explicitly readable"
            );
        }
        assert!(!permits(&scope, &Method::GET, "/future-sensitive-route"));
        assert!(!permits(&scope, &Method::POST, "/status"));
    }

    /// A stop report is a read of rows that already happened, not an action — so the weakest key
    /// reaches it, the same as `GET /runs/{id}` beside it in the table.
    #[test]
    fn a_read_only_api_key_may_read_why_a_run_stopped() {
        let scope = Scope::ApiToken(ApiTokenLevel::ReadOnly);
        assert!(permits(&scope, &Method::GET, "/runs/{id}/stop"));
    }

    /// §11 item 9, asked of a real token through the real middleware rather than of `permits`.
    ///
    /// The unit test above and this one are not the same assertion twice. `permits` answers about a
    /// PATTERN; a request carries a path, and between them sit the token store, the middleware and
    /// axum's matcher. A route graded correctly in the table and mounted where the matcher never
    /// hands it that pattern would pass the test above and `401` here.
    ///
    /// The write beside it is what makes the read mean anything: the same key must NOT be able to
    /// cancel the run it can read about.
    #[tokio::test]
    async fn a_read_only_key_reads_a_stop_report_over_http_and_still_cannot_act() {
        let state = test_state("control-token").await;
        let token = stored_api_token(&state, "reader", ApiTokenLevel::ReadOnly).await;
        let app = protected_router(state);

        assert_eq!(
            status_of(&app, "GET", "/runs/9/stop", &token).await,
            StatusCode::OK,
            "a stop report starts nothing and holds nothing"
        );
        assert_eq!(
            status_of(&app, "POST", "/runs", &token).await,
            StatusCode::FORBIDDEN,
            "and the same key must not be able to launch one"
        );
    }

    /// Sending is Admin-only by construction: `POST /email/send` is in NEITHER table.
    ///
    /// The same deliberate omission `POST /runs/{id}/message` is documented for above
    /// `RUN_CREATING_ROUTES`, and for the same reason. A run-creating key authorises the prompt it
    /// supplies at the moment it supplies it; a message leaving this machine under the owner's own
    /// address is not something that key ever bought, and it is the one act in this pillar the
    /// mailbox's owner cannot undo. The email sidecar is on the list too, and is the sharpest case:
    /// it is the process that parses MIME written by strangers, so it must not hold the key to the
    /// route that replies to them.
    ///
    /// Asserting the two tables' membership as well as `permits` is the point — the rule here is an
    /// absence, and an absence is what a later edit adds a line to without noticing.
    #[test]
    fn sending_mail_is_out_of_reach_of_every_scope_below_admin() {
        const SEND_ROUTE: &str = "/email/send";

        for scope in [
            Scope::ApiToken(ApiTokenLevel::ReadOnly),
            Scope::ApiToken(ApiTokenLevel::RunCreating),
            Scope::Service(Service::Email),
        ] {
            assert!(
                !permits(&scope, &Method::POST, SEND_ROUTE),
                "{scope:?} must not be able to send mail as the mailbox's owner"
            );
        }

        for scope in [Scope::ApiToken(ApiTokenLevel::Admin), Scope::Control] {
            assert!(
                permits(&scope, &Method::POST, SEND_ROUTE),
                "{scope:?} acts for the person, and sending is theirs to do"
            );
        }

        assert!(
            !READ_ONLY_ROUTES
                .iter()
                .any(|(_, pattern)| *pattern == SEND_ROUTE),
            "sending is not a read"
        );
        assert!(
            !RUN_CREATING_ROUTES
                .iter()
                .any(|(_, pattern)| *pattern == SEND_ROUTE),
            "sending must not ride in on the permission to start a run"
        );
    }

    /// A queue a read-only key can drive is not a brake.
    ///
    /// Written here rather than beside the handlers because `protected_router` and `status_of` are
    /// private to this module — and because nothing else would catch the mistake. Nothing in the
    /// crate links `build_router` to these tables: the exactness test above walks `READ_ONLY_ROUTES`
    /// and never the axum router.
    ///
    /// Note which direction the danger runs. `permits` is default-deny, so a route added to
    /// `http.rs` and forgotten here is *over*-protected — reachable only by Control and Admin, which
    /// is annoying rather than dangerous. The dangerous edit is the opposite one: adding a line to
    /// the table for a route that should not have been graded a read. That is what these assertions
    /// pin, and `/wait` below is the one most likely to attract it.
    #[tokio::test]
    async fn a_read_only_api_key_may_read_a_vcs_ticket_but_not_queue_work() {
        let state = test_state("control-token").await;
        let token = stored_api_token(&state, "reader", ApiTokenLevel::ReadOnly).await;
        let app = protected_router(state);

        assert_eq!(
            status_of(&app, "GET", "/vcs/requests", &token).await,
            StatusCode::OK,
            "listing the queue starts nothing and holds no repository"
        );
        assert_eq!(
            status_of(&app, "GET", "/vcs/requests/7", &token).await,
            StatusCode::OK,
            "reading one ticket, likewise"
        );
        assert_eq!(
            status_of(&app, "POST", "/vcs/requests", &token).await,
            StatusCode::FORBIDDEN,
            "submitting an operation to the queue is not a read"
        );
        // The route the table's comment spends six lines justifying, and the only one whose
        // exclusion is a judgement rather than a category: it reads the same row as `{id}`, so
        // nothing about *what* it returns argues for refusing it. What argues is that it holds the
        // connection for up to 45 seconds. Without this assertion, adding it to READ_ONLY_ROUTES
        // some later afternoon would be a green-suite change.
        assert_eq!(
            status_of(&app, "GET", "/vcs/requests/7/wait", &token).await,
            StatusCode::FORBIDDEN,
            "waiting is a read, but not one worth handing the weakest key a 45s connection for"
        );
    }

    /// Reading capacity is a read; touching the budget is not.
    ///
    /// The two travel together on the canvas — the header shows occupancy and budget side by side —
    /// and that is why it is worth saying they are not the same grade. `GET /autopilot/budget` is
    /// outside this table on purpose, together with reading the kill switch, because the package
    /// treats the control commands as one family.
    #[test]
    fn reading_capacity_is_a_read_and_reading_the_budget_is_not() {
        let scope = Scope::ApiToken(ApiTokenLevel::ReadOnly);
        assert!(permits(&scope, &Method::GET, "/concurrency"));
        assert!(!permits(&scope, &Method::GET, "/autopilot/budget"));
    }

    /// Reading the day and reading where the owner stopped are reads; moving the marker is not.
    ///
    /// The POST changes nothing but a number, and it is still outside the table: what the owner has
    /// seen is the owner's to say, and a script holding a read-only key that could mark everything
    /// read would hide exactly the lines the page exists to surface.
    #[test]
    fn the_feed_timeline_and_its_marker_are_reads_and_moving_the_marker_is_not() {
        let scope = Scope::ApiToken(ApiTokenLevel::ReadOnly);
        assert!(permits(&scope, &Method::GET, "/feed/timeline"));
        assert!(permits(&scope, &Method::GET, "/feed/seen"));
        assert!(!permits(&scope, &Method::POST, "/feed/seen"));
        assert!(!route_is_listed(
            RUN_CREATING_ROUTES,
            &Method::POST,
            "/feed/seen"
        ));
    }

    #[tokio::test]
    async fn a_read_only_api_key_is_refused_on_each_privileged_route() {
        let state = test_state("control-token").await;
        let token = stored_api_token(&state, "reader", ApiTokenLevel::ReadOnly).await;
        let app = protected_router(state);

        for uri in [
            "/status",
            "/runs/7",
            "/projects/demo/cat",
            "/files",
            "/files/download",
            "/files/search",
        ] {
            assert_eq!(
                status_of(&app, "GET", uri, &token).await,
                StatusCode::OK,
                "GET {uri} is an allowlisted read"
            );
        }
        for (method, uri) in [
            ("POST", "/runs"),
            ("POST", "/webhooks/push"),
            ("POST", "/autopilot/kill"),
            ("POST", "/autopilot/budget"),
            ("POST", "/autopilot/state"),
            ("POST", "/proposals/7/approve"),
            ("POST", "/backups/snapshot.db/restore"),
            ("POST", "/api-tokens"),
            ("GET", "/api-tokens"),
            // Reading the folder is an allowlisted read; changing it is not. A read-only key that
            // could empty somebody's folder would be misnamed.
            ("POST", "/files/folder"),
            ("POST", "/files/upload"),
            ("POST", "/files/move"),
            ("DELETE", "/files"),
            // The browser, all of it, including the two reads. `GET /web/pages` beside it IS an
            // allowlisted read, and the difference is what these routes disclose: the pages a
            // machine has fetched, against the list of hosts a person has accounts on and the
            // sessions currently open in their name.
            ("POST", "/browser/open"),
            ("POST", "/browser/act"),
            ("POST", "/browser/look"),
            ("POST", "/browser/revoke"),
            // The wheel. `/keep` is the one that grows the allowlist, and a read-only key reaching
            // it would be a read-only key granting a host permanent access to the profile that
            // holds the owner's logins.
            ("POST", "/browser/forget"),
            ("POST", "/browser/handoff"),
            ("POST", "/browser/return"),
            ("POST", "/browser/keep"),
            ("GET", "/browser/sessions"),
            ("GET", "/browser/sites/demo"),
            // Withdrawing a write grant, and the record of what was written with it. The
            // record is the sharper of the two: the site list says which hosts a person has
            // accounts on, and this says which forms their own identity was used to submit.
            ("POST", "/browser/readonly"),
            ("GET", "/browser/writes/demo"),
        ] {
            assert_eq!(
                status_of(&app, method, uri, &token).await,
                StatusCode::FORBIDDEN,
                "{method} {uri}"
            );
        }
    }

    #[tokio::test]
    async fn a_run_creating_api_key_starts_runs_but_not_admin_actions() {
        let state = test_state("control-token").await;
        let token = stored_api_token(&state, "launcher", ApiTokenLevel::RunCreating).await;
        let app = protected_router(state);

        for uri in [
            "/runs",
            "/webhooks/push",
            "/presets/7/run",
            "/assistant/message",
            "/email/triage",
        ] {
            assert_eq!(
                status_of(&app, "POST", uri, &token).await,
                StatusCode::OK,
                "POST {uri} starts a run"
            );
        }
        for (method, uri) in [
            ("POST", "/autopilot/kill"),
            ("POST", "/autopilot/budget"),
            ("POST", "/autopilot/state"),
            ("POST", "/proposals/7/approve"),
            ("POST", "/backups/snapshot.db/restore"),
            ("POST", "/api-tokens"),
            ("GET", "/api-tokens"),
            ("POST", "/files/folder"),
            ("POST", "/files/upload"),
            ("POST", "/files/move"),
            ("DELETE", "/files"),
        ] {
            assert_eq!(
                status_of(&app, method, uri, &token).await,
                StatusCode::FORBIDDEN,
                "{method} {uri}"
            );
        }
    }

    #[tokio::test]
    async fn unknown_and_revoked_api_keys_authenticate_nothing() {
        let state = test_state("control-token").await;
        let token = stored_api_token(&state, "temporary", ApiTokenLevel::ReadOnly).await;
        let app = protected_router(state.clone());

        assert_eq!(
            status_of(&app, "GET", "/status", "api:missing.unknown").await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status_of(&app, "GET", "/status", &token).await,
            StatusCode::OK
        );

        sqlx::query("DELETE FROM api_tokens WHERE name = 'temporary'")
            .execute(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            status_of(&app, "GET", "/status", &token).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn api_run_and_service_prefixes_do_not_cross_over() {
        let state = test_state("control-token").await;
        let (run_id, run_token) = running_run_with_token(&state).await;
        let service_token = mint_service_token(&state.pool, Service::Email)
            .await
            .unwrap();
        let api_token = stored_api_token(&state, "reader", ApiTokenLevel::ReadOnly).await;
        let app = protected_router(state);

        let run_secret = run_token.split_once('.').unwrap().1;
        let service_secret = service_token.split_once('.').unwrap().1;
        let api_secret = api_token.split_once('.').unwrap().1;
        for forged in [
            format!("{run_id}.{api_secret}"),
            format!("email.{api_secret}"),
            format!("api:reader.{run_secret}"),
            format!("api:reader.{service_secret}"),
        ] {
            assert_eq!(
                status_of(&app, "GET", "/status", &forged).await,
                StatusCode::UNAUTHORIZED,
                "{forged:?}"
            );
        }
    }

    /// The email sidecar's key opens the two routes it builds a URL for, and none of the rest.
    #[tokio::test]
    async fn the_email_sidecars_key_opens_its_two_routes_and_nothing_else() {
        let state = test_state("control-token").await;
        let token = mint_service_token(&state.pool, Service::Email)
            .await
            .unwrap();
        let app = protected_router(state);

        assert_eq!(
            status_of(&app, "GET", "/email/cursor", &token).await,
            StatusCode::OK
        );
        assert_eq!(
            status_of(&app, "POST", "/email/incoming", &token).await,
            StatusCode::OK
        );
        assert_eq!(
            status_of(&app, "POST", "/proposals/7/approve", &token).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status_of(&app, "POST", HOOK_ROUTE, &token).await,
            StatusCode::FORBIDDEN,
            "the gate answers runs, not sidecars"
        );
        // The method is part of the rule: reading the cursor is not writing to it.
        assert_eq!(
            status_of(&app, "POST", "/email/cursor", &token).await,
            StatusCode::FORBIDDEN
        );
    }

    /// The barrier that holds when the other two do not.
    ///
    /// A council seat is refused every action three times over, and two of those refusals depend on
    /// something cooperating: `ToolPolicy::McpOnly` on the CLI enforcing its own restriction, and
    /// the `PreToolUse` hook firing at all — which it only does if the `.claude/settings.json`
    /// resolved from the run's working directory registers it, and a seat has no working directory.
    /// This one depends on nothing. With this key, `POST /runs` is 403 whatever the model decided
    /// and whatever the CLI did or did not enforce.
    #[tokio::test]
    async fn the_councils_key_reads_and_cannot_start_anything() {
        let state = test_state("control-token").await;
        let token = mint_service_token(&state.pool, Service::Council)
            .await
            .unwrap();
        let app = protected_router(state);

        // What `mcp_tools::COUNCIL_TOOLS` advertises, and it must actually work — a tool that
        // always 403s costs a seat a round and tells it something is broken.
        for path in [
            "/projects",
            "/proposals",
            "/autopilot/budget",
            "/autopilot/kill",
            "/email/queue",
            "/runs/7",
            "/files",
        ] {
            assert_ne!(
                status_of(&app, "GET", path, &token).await,
                StatusCode::FORBIDDEN,
                "a seat must be able to read {path}"
            );
        }

        // And nothing that acts, changes or costs money.
        for (method, path) in [
            ("POST", "/runs"),
            ("POST", "/jobs"),
            ("POST", "/council"),
            ("POST", "/proposals/7/approve"),
            ("POST", "/autopilot/kill"),
            ("POST", "/vcs/requests"),
            ("POST", "/github/requests"),
            ("POST", "/email/send"),
            ("POST", "/email/triage"),
            ("POST", "/web/read"),
            ("POST", "/web/search"),
            ("POST", "/browser/open"),
            ("DELETE", "/files"),
        ] {
            assert_eq!(
                status_of(&app, method, path, &token).await,
                StatusCode::FORBIDDEN,
                "{method} {path}"
            );
        }

        // The gate answers runs, not services — a seat's tool decisions come through its run token.
        assert_eq!(
            status_of(&app, "POST", HOOK_ROUTE, &token).await,
            StatusCode::FORBIDDEN
        );
        // A council cannot convene a council. Nothing in the design wants recursion, and no brake
        // in this house counts it.
        assert_eq!(
            status_of(&app, "GET", "/council", &token).await,
            StatusCode::FORBIDDEN
        );
    }

    /// A fresh daemon replaces the key, so the one a previous daemon's sidecar still holds is dead.
    #[tokio::test]
    async fn minting_a_service_key_again_retires_the_previous_one() {
        let state = test_state("control-token").await;
        let first = mint_service_token(&state.pool, Service::Email)
            .await
            .unwrap();
        let second = mint_service_token(&state.pool, Service::Email)
            .await
            .unwrap();
        assert_ne!(first, second);
        let app = protected_router(state);

        assert_eq!(
            status_of(&app, "GET", "/email/cursor", &first).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status_of(&app, "GET", "/email/cursor", &second).await,
            StatusCode::OK
        );
    }

    /// A service name never parses as a run id, which is what lets one prefix pick the right table.
    /// Guessing the other shape must not cross over.
    #[tokio::test]
    async fn a_service_name_and_a_run_id_do_not_cross_over() {
        let state = test_state("control-token").await;
        let (run_id, run_token) = running_run_with_token(&state).await;
        let service_secret = mint_service_token(&state.pool, Service::Email)
            .await
            .unwrap()
            .split_once('.')
            .unwrap()
            .1
            .to_owned();
        let app = protected_router(state);

        // The email secret under a run id, and the run secret under the service name.
        let run_secret = run_token.split_once('.').unwrap().1;
        for forged in [
            format!("{run_id}.{service_secret}"),
            format!("email.{run_secret}"),
        ] {
            assert_eq!(
                status_of(&app, "GET", "/email/cursor", &forged).await,
                StatusCode::UNAUTHORIZED,
                "{forged:?}"
            );
        }
        // And a run's key does not become a sidecar's by naming its route.
        assert_eq!(
            status_of(&app, "POST", "/email/incoming", &run_token).await,
            StatusCode::FORBIDDEN
        );
    }

    /// The shell is unaffected — this narrows what a *run* and a *sidecar* hold, not what the
    /// daemon's own key opens.
    #[tokio::test]
    async fn the_control_token_still_reaches_a_route_from_every_family() {
        let state = test_state("control-token").await;
        let app = protected_router(state);

        for (method, uri) in ROUTE_FAMILY_CASES {
            assert_eq!(
                status_of(&app, method, uri, "control-token").await,
                StatusCode::OK,
                "{method} {uri}"
            );
        }
    }

    /// The whole of `TEAM_ROUTES`, asserted entry by entry.
    ///
    /// Entry-by-entry and not a spot check, for the reason `COUNCIL_ROUTES` and `EMAIL_ROUTES` are
    /// asserted the same way: the table IS the boundary, so any future addition to it has to walk
    /// past this test on purpose. The negatives below are complement and not guard — "no team route
    /// is a run-creating route" covers six entries and would not catch a `POST /email/send`.
    #[tokio::test]
    async fn a_team_token_reaches_exactly_its_route_table() {
        let state = test_state("control-token").await;
        let token = live_team_run_with_token(&state, "run-1").await;
        let app = protected_router(state);

        for (method, pattern) in TEAM_ROUTES {
            let path = pattern.replace("{id}", "7");
            assert_eq!(
                status_of(&app, method.as_str(), &path, &token).await,
                StatusCode::OK,
                "{method} {path} is on the team's list"
            );
        }
    }

    /// Everything a department must not reach, named one by one.
    ///
    /// Three groups, and each is a decision rather than an oversight. The acts — starting a run,
    /// starting a job, sending mail, queueing a merge, deleting the owner's folder. The council's
    /// two that deliberately did not transfer — the budget and the kill switch, because a
    /// department is not convened to answer about the machine. And the gate, which is absent from
    /// both scopes' tables because `hooks::pretooluse_decision` validates the body's `run_id`
    /// against the token only for `Scope::Run`, so a second scope on that route could name somebody
    /// else's run.
    #[tokio::test]
    async fn a_team_token_reaches_nothing_outside_its_table() {
        let state = test_state("control-token").await;
        let token = live_team_run_with_token(&state, "run-1").await;
        let app = protected_router(state);

        for (method, path) in [
            ("POST", "/runs"),
            ("POST", "/jobs"),
            ("POST", "/email/send"),
            ("POST", "/vcs/requests"),
            ("POST", "/github/requests"),
            // The reason `TEAM_ROUTES` is a list of PAIRS. `/files` is registered with both a GET
            // and a DELETE on the same path, so a table of paths alone would have handed a
            // department the deleting of the owner's folder along with the listing of it.
            ("DELETE", "/files"),
            // A department asks for an action and does not read the queue of them. Same path, other
            // method — the pair-shaped table again.
            ("GET", "/team-actions"),
            ("GET", "/team-recruits"),
            ("GET", "/autopilot/budget"),
            ("GET", "/autopilot/kill"),
            ("GET", "/projects"),
            ("GET", "/runs/7"),
            ("GET", "/proposals"),
            ("GET", "/vcs/requests/7/wait"),
            ("POST", HOOK_ROUTE),
        ] {
            assert_eq!(
                status_of(&app, method, path, &token).await,
                StatusCode::FORBIDDEN,
                "{method} {path} must be out of a department's reach"
            );
        }
    }

    /// The route that leaves the machine is in NEITHER table, asserted as membership rather than
    /// through a request, so that adding it to one of them fails here instead of in production.
    ///
    /// Its sibling `POST /vcs/requests` is asserted beside it, because the argument is one argument
    /// and a test that made it about only the new route would let somebody "fix" the old one.
    #[test]
    fn the_routes_that_leave_the_machine_are_in_no_scope_table() {
        for (method, path) in [
            (Method::POST, "/github/requests"),
            (Method::POST, "/vcs/requests"),
            (Method::POST, "/email/send"),
        ] {
            assert!(
                !route_is_listed(READ_ONLY_ROUTES, &method, path)
                    && !route_is_listed(RUN_CREATING_ROUTES, &method, path)
                    && !route_is_listed(TEAM_ROUTES, &method, path)
                    && !route_is_listed(EMAIL_ROUTES, &method, path)
                    && !route_is_listed(COUNCIL_ROUTES, &method, path),
                "{method} {path} must stay out of every scope table"
            );
            assert!(
                !permits(&Scope::Run(7), &method, path),
                "{method} {path} must be unreachable by a run"
            );
            assert!(
                !permits(&Scope::ApiToken(ApiTokenLevel::RunCreating), &method, path),
                "{method} {path} must be unreachable by a run-creating key"
            );
            assert!(
                permits(&Scope::Control, &method, path),
                "{method} {path} must stay reachable by the control token"
            );
        }
    }

    /// Downloading a model is the person's decision, and the agent must not be able to make it.
    ///
    /// The pull routes spend this machine's bandwidth and disk — a model is gigabytes — on a name
    /// in a request body, and they are reachable by the human's own key and nothing else. An
    /// autonomous run that could reach them could fill the disk between one gate check and the
    /// next, and it would do it through a route that looks like a preference rather than an action.
    ///
    /// Asserted as MEMBERSHIP and not through a request, the same way
    /// `the_routes_that_leave_the_machine_are_in_no_scope_table` above is: `permits` is
    /// default-deny, so both routes are already out of reach the moment they are written — which
    /// means the mistake this test catches is not forgetting to add them to a table, but somebody
    /// later adding them to one. `GET` is listed beside `POST` deliberately: reading the progress
    /// of a download is harmless on its own, and that is exactly the argument that would get it
    /// graded a read and put in `READ_ONLY_ROUTES`, one line above the route that starts it.
    #[test]
    fn the_pull_routes_are_out_of_an_agents_reach() {
        // `size` is here for the same reason `GET pull` is. It downloads nothing and reads a public
        // catalogue, which is precisely the argument that would get it graded harmless — and what
        // it actually reports is what this machine has and what it could hold, next door to the
        // route that spends the disk. It is also the door the pull's own refusal consults.
        for (method, path) in [
            (Method::POST, "/assistant/local-model/pull"),
            (Method::GET, "/assistant/local-model/pull"),
            (Method::GET, "/assistant/local-model/size"),
        ] {
            let method = &method;
            assert!(
                !route_is_listed(READ_ONLY_ROUTES, method, path)
                    && !route_is_listed(RUN_CREATING_ROUTES, method, path)
                    && !route_is_listed(TEAM_ROUTES, method, path)
                    && !route_is_listed(EMAIL_ROUTES, method, path)
                    && !route_is_listed(COUNCIL_ROUTES, method, path),
                "{method} {path} must stay out of every scope table"
            );
            assert!(
                !permits(&Scope::Run(7), method, path),
                "{method} {path} must be unreachable by a run"
            );
            assert!(
                !permits(&Scope::TeamRun("team-1".to_owned()), method, path),
                "{method} {path} must be unreachable by a department"
            );
            assert!(
                !permits(&Scope::ApiToken(ApiTokenLevel::RunCreating), method, path),
                "{method} {path} must be unreachable by a run-creating key"
            );
            assert!(
                permits(&Scope::Control, method, path),
                "{method} {path} must stay reachable by the human's own key"
            );
        }
    }

    /// The complement of the table, asserted as membership so that an edit to either list fails
    /// here rather than in production.
    #[test]
    fn no_team_route_starts_work_and_none_is_the_gate() {
        for (method, pattern) in TEAM_ROUTES {
            assert!(
                !RUN_CREATING_ROUTES
                    .iter()
                    .any(|(other, path)| other == method && path == pattern),
                "{method} {pattern} starts work and must not be a department's to call"
            );
            assert_ne!(
                *pattern, HOOK_ROUTE,
                "the gate answers runs, not departments"
            );
        }
    }

    /// The economy and the boundary must cover the same set, and here is where they are held to it.
    ///
    /// `mcp_tools::TEAM_TOOLS` narrows what the model is offered; `TEAM_ROUTES` decides what the key
    /// reaches. Divergence is silent in both directions and expensive in both: a tool offered and
    /// refused is a rain of 403s nobody traces to its cause, and a tool refused but permitted is a
    /// boundary nobody is exercising.
    ///
    /// The map is written out here because there is no tool→route mapping anywhere in the crate to
    /// derive it from — the correspondence lives inside the bodies of `daemon_client.rs`, and for
    /// `vcs_ticket` it is not even a function of the name (`ticket_path(id, wait)` yields two
    /// different routes, one permitted and one refused on purpose). A hand-written map has to be
    /// maintained; the two assertions below are what make forgetting to fail loudly.
    #[test]
    fn every_team_tool_has_a_route_and_every_team_route_has_a_tool() {
        const TOOL_ROUTES: &[(&str, Method, &str)] = &[
            ("get_email", Method::GET, "/email/{id}"),
            ("get_email_queue", Method::GET, "/email/queue"),
            ("list_files", Method::GET, "/files"),
            ("propose_action", Method::POST, "/team-actions"),
            ("propose_teammate", Method::POST, "/team-recruits"),
            ("read_team_file", Method::POST, "/team-files/read"),
            ("report_to_owner", Method::POST, "/team-reports"),
            ("send_team_note", Method::POST, "/team-notes"),
            ("web_read", Method::POST, "/web/read"),
            ("web_search", Method::POST, "/web/search"),
        ];

        for (tool, method, path) in TOOL_ROUTES {
            assert!(
                crate::mcp_tools::TEAM_TOOLS.contains(tool),
                "{tool} is mapped here and is not offered to a department"
            );
            assert!(
                permits(&Scope::TeamRun("run-1".to_owned()), method, path),
                "{tool} is offered to a department and its route is refused to one"
            );
        }

        for tool in crate::mcp_tools::TEAM_TOOLS {
            assert!(
                TOOL_ROUTES.iter().any(|(mapped, ..)| mapped == tool),
                "{tool} is offered to a department and this map does not say which route it calls"
            );
        }
        for (method, pattern) in TEAM_ROUTES {
            assert!(
                TOOL_ROUTES
                    .iter()
                    .any(|(_, mapped, path)| mapped == method && path == pattern),
                "{method} {pattern} is reachable by a department and no tool it holds calls it — \
                 either the tool list is short or the route table is long"
            );
        }
    }

    /// The pin that says this scope did not widen the run token beyond its three named routes.
    ///
    /// `Scope::Run`'s doc claims it is "good for exactly three routes", and other comments in this
    /// file lean on that being true. A new scope is exactly the change that makes somebody widen
    /// the old one by accident.
    #[test]
    fn a_run_token_still_reaches_exactly_the_hook_route() {
        let run = Scope::Run(1);
        assert!(permits(&run, &Method::POST, HOOK_ROUTE));
        assert!(permits(&run, &Method::POST, ASK_WAIT_ROUTE));
        assert!(permits(&run, &Method::POST, POSTTOOLUSE_ROUTE));

        for (method, pattern) in TEAM_ROUTES {
            assert!(
                !permits(&run, method, pattern),
                "a run token must not have inherited {method} {pattern}"
            );
        }
        for (method, pattern) in READ_ONLY_ROUTES {
            assert!(!permits(&run, method, pattern));
        }
    }

    /// Writing a project's own rules is the owner's, and nobody else's.
    ///
    /// The file behind this route is `.ai/autopilot.yaml`, and it carries `gate_command` — the
    /// command whose exit code decides what *green* means for every run in the project. A key that
    /// could rewrite it could set the gate to `true` and pass every gate it will ever face, which
    /// makes this the one write on the project surface where the blast radius is the whole quality
    /// bar rather than one file.
    ///
    /// Asserted as membership rather than through a request, like `POST /email/send` above and for
    /// the same reason: `permits` is default-deny, so the route is safe today by being in no table.
    /// What that does not survive is somebody adding it to `READ_ONLY_ROUTES` beside the six reads
    /// that share its URL prefix, which would read as tidying up. This is the test that says no.
    #[test]
    fn writing_a_projects_rules_is_in_no_scope_table() {
        const WRITE_ROUTE: &str = "/projects/{id}/write";

        assert!(
            !route_is_listed(READ_ONLY_ROUTES, &Method::POST, WRITE_ROUTE)
                && !route_is_listed(RUN_CREATING_ROUTES, &Method::POST, WRITE_ROUTE)
                && !route_is_listed(TEAM_ROUTES, &Method::POST, WRITE_ROUTE)
                && !route_is_listed(EMAIL_ROUTES, &Method::POST, WRITE_ROUTE)
                && !route_is_listed(COUNCIL_ROUTES, &Method::POST, WRITE_ROUTE),
            "deciding what green means must stay out of every scope table"
        );

        for scope in [
            Scope::Run(7),
            Scope::ApiToken(ApiTokenLevel::ReadOnly),
            Scope::ApiToken(ApiTokenLevel::RunCreating),
        ] {
            assert!(
                !permits(&scope, &Method::POST, "/projects/7/write"),
                "{scope:?} must not be able to rewrite a project's gate command"
            );
        }

        // The reads beside it stay reads. A boundary a reader cannot see is a boundary nobody can
        // argue with, and seeing it is not permission to cross it.
        assert!(permits(
            &Scope::ApiToken(ApiTokenLevel::ReadOnly),
            &Method::GET,
            "/projects/7/ownership"
        ));
    }

    /// A project's commands are the owner's, to read and to run.
    ///
    /// The run route spawns a process of the project's own choosing, which is plainly not a read.
    /// The **listing** is out of the table too, and that is the half worth an assertion: it is the
    /// same class of information `GET /projects/{id}/rules` holds — what this project runs on its
    /// own — and that route has always been Admin's. It also names which commands are marked
    /// runnable by an agent, which is a map of the surface rather than a fact about the code.
    ///
    /// Membership rather than a request, like `POST /email/send` and `POST /projects/{id}/write`
    /// above: `permits` is default-deny, so these are safe today by being nowhere. What that does
    /// not survive is somebody filing them beside the seven `/projects/{id}/…` reads that share
    /// their prefix, which would read as consistency.
    #[test]
    fn a_projects_commands_are_in_no_scope_table() {
        for (method, pattern) in [
            (Method::GET, "/projects/{id}/commands"),
            (Method::POST, "/projects/{id}/commands"),
            (Method::DELETE, "/projects/{id}/commands/{command_id}"),
            (Method::POST, "/projects/{id}/commands/{command_id}/run"),
        ] {
            assert!(
                !route_is_listed(READ_ONLY_ROUTES, &method, pattern)
                    && !route_is_listed(RUN_CREATING_ROUTES, &method, pattern)
                    && !route_is_listed(TEAM_ROUTES, &method, pattern)
                    && !route_is_listed(EMAIL_ROUTES, &method, pattern)
                    && !route_is_listed(COUNCIL_ROUTES, &method, pattern),
                "{method} {pattern} must stay out of every scope table"
            );
        }

        // And the one that matters most, asked as a request: a run's key opens exactly the safety
        // gate, so `runnable_by` is belt and braces rather than the only thing standing here.
        for scope in [
            Scope::Run(7),
            Scope::ApiToken(ApiTokenLevel::ReadOnly),
            Scope::ApiToken(ApiTokenLevel::RunCreating),
        ] {
            assert!(
                !permits(&scope, &Method::POST, "/projects/7/commands/1/run"),
                "{scope:?} must not be able to run a project's commands"
            );
        }
        assert!(permits(
            &Scope::Control,
            &Method::POST,
            "/projects/7/commands/1/run"
        ));
    }

    /// The reach a project declares for itself is the owner's, and nobody else's.
    ///
    /// These twelve routes are the only way into the four tables `project_policy.rs` holds: what a
    /// project's worktrees may run unattended, what the GitHub manager may do on its remote, and
    /// what the shared git queue may do on its repository, and where a landing may be sent. **They
    /// are a live autonomy control, not a configuration edit.**
    /// The hook reads those tables per decision and caches nothing, so a single `allow` row written
    /// through `POST /projects/{id}/shell-rules` binds the very next tool call of every in-flight
    /// run of that project — no restart, no second confirmation, and no moment at which a person is
    /// shown what widened. A key that could write one is a key that can rewrite its own alçada,
    /// which is a strictly larger power than anything a scoped key is sold as buying.
    ///
    /// Asserted as membership rather than through a request, like `POST /projects/{id}/write` and
    /// `POST /projects/{id}/commands` above and for the same reason: `permits` is default-deny, so
    /// these routes are safe TODAY by being in no table. What that does not survive is somebody
    /// filing the four GETs beside the `/projects/{id}/…` reads that share their URL prefix, which
    /// would read as tidying up rather than as a decision. This is the test that says no.
    ///
    /// **The three scopes below are not decoration, and `Scope::Run` alone would prove nothing.**
    /// That arm consults no table at all — it is a fixed `match` over two routes — so it would go on
    /// passing with all twelve of these in every table. `ApiTokenLevel::ReadOnly` and
    /// `ApiTokenLevel::RunCreating` are the two that actually read `READ_ONLY_ROUTES`, which is
    /// where a tidying hand would put them.
    #[test]
    fn a_projects_declared_reach_is_in_no_scope_table() {
        const DECLARED_REACH: &[(Method, &str)] = &[
            (Method::GET, "/projects/{id}/shell-rules"),
            (Method::POST, "/projects/{id}/shell-rules"),
            (Method::DELETE, "/projects/{id}/shell-rules"),
            (Method::GET, "/projects/{id}/github-ops"),
            (Method::POST, "/projects/{id}/github-ops"),
            (Method::DELETE, "/projects/{id}/github-ops"),
            (Method::GET, "/projects/{id}/land-targets"),
            (Method::POST, "/projects/{id}/land-targets"),
            (Method::DELETE, "/projects/{id}/land-targets"),
            (Method::GET, "/projects/{id}/git-ops"),
            (Method::POST, "/projects/{id}/git-ops"),
            (Method::DELETE, "/projects/{id}/git-ops"),
        ];

        for (method, pattern) in DECLARED_REACH {
            assert!(
                !route_is_listed(READ_ONLY_ROUTES, method, pattern)
                    && !route_is_listed(RUN_CREATING_ROUTES, method, pattern)
                    && !route_is_listed(TEAM_ROUTES, method, pattern)
                    && !route_is_listed(EMAIL_ROUTES, method, pattern)
                    && !route_is_listed(COUNCIL_ROUTES, method, pattern),
                "{method} {pattern} decides what a project's runs may do unattended and must stay \
                 out of every scope table"
            );
        }

        for scope in [
            Scope::Run(7),
            Scope::ApiToken(ApiTokenLevel::ReadOnly),
            Scope::ApiToken(ApiTokenLevel::RunCreating),
        ] {
            for (method, pattern) in DECLARED_REACH {
                let path = pattern.replace("{id}", "7");
                assert!(
                    !permits(&scope, method, &path),
                    "{scope:?} must not reach {method} {path}"
                );
            }
        }

        // The reads beside them stay reads. A boundary a reader cannot see is a boundary nobody can
        // argue with, and seeing it is not permission to cross it.
        assert!(permits(
            &Scope::ApiToken(ApiTokenLevel::ReadOnly),
            &Method::GET,
            "/projects/7/readings"
        ));
    }

    /// Which repository a project points at is in no scope table, and this is the closest call of
    /// the three.
    ///
    /// `GET /projects/{id}/github-repo` turns a project id into `owner/name`. It is not a secret —
    /// it is written in the repository's own `.git/config` — but it does disclose WHICH repository a
    /// registered project is, which is one sentence away from the class the twelve routes above are
    /// excluded under.
    ///
    /// **The honest case for the other answer, first, because it is a strong one.** The
    /// confidentiality delta of this route is *zero*. A `ReadOnly` key already holds
    /// `GET /projects/{id}/cat`, `inspect::cat` has no `.git` exclusion — only `grep`'s tree walk
    /// carries `SKIP_DIRS` — and `.git/config` is a path of `Normal` components that resolves inside
    /// the root, so `cat?path=.git/config` hands the same holder the same `origin` URL verbatim
    /// today. `GET /projects` even hands it the root to aim at. On the reasoning `worktree` and
    /// `blame` are listed under — *the holder can already read it, so withholding protects nothing* —
    /// this route belongs in the table, and that reading is written down here so the next reader
    /// weighs it rather than rediscovering it and assuming it was missed.
    ///
    /// **What settles it is that the disclosure is not the question.** Since the delta is zero,
    /// nothing is protected either way, and the decision falls to whether the grant has a use. It
    /// has none, and this route's own shape says so more sharply than the catalogue's does: it
    /// exists to name the repository that `POST /github/requests` will be sent, and that route is
    /// out of both tables because it LEAVES THE MACHINE. A key that may not reach GitHub has no use
    /// for the address of the repository to reach. Its only caller is the page, which holds
    /// `Scope::Control`; the three declaration routes it is drawn beside are themselves in no table,
    /// so a key that cannot read what a project has declared about its remote has no use for the
    /// remote's name either. Between a grant with a use and a grant without one, this table's own
    /// header decides it: it is deliberately not "every GET".
    ///
    /// **And it is why the route exists at all rather than riding on a read the page already
    /// makes.** `ProjectSummary` and `GET /projects/{id}/branches` were both candidates and both are
    /// in `READ_ONLY_ROUTES` — joining either would have made this grant by accident, as a side
    /// effect of saving a round trip. The placement decision and this one are the same decision.
    ///
    /// Membership rather than a request, like the twelve and the catalogue: `permits` is default-deny,
    /// so this is safe today by being nowhere, and what that does not survive is somebody filing it
    /// beside the `/projects/{id}/…` reads it shares a prefix with — on exactly the `cat` reasoning
    /// above, which is true and is not the point. That is the edit this test refuses.
    #[test]
    fn the_repository_a_project_points_at_is_in_no_scope_table() {
        const REPO: &str = "/projects/{id}/github-repo";

        assert!(
            !route_is_listed(READ_ONLY_ROUTES, &Method::GET, REPO)
                && !route_is_listed(RUN_CREATING_ROUTES, &Method::GET, REPO)
                && !route_is_listed(TEAM_ROUTES, &Method::GET, REPO)
                && !route_is_listed(EMAIL_ROUTES, &Method::GET, REPO)
                && !route_is_listed(COUNCIL_ROUTES, &Method::GET, REPO),
            "this names the repository a call that leaves the machine would be sent, and no scoped \
             key may make that call"
        );

        for scope in [
            Scope::Run(7),
            Scope::ApiToken(ApiTokenLevel::ReadOnly),
            Scope::ApiToken(ApiTokenLevel::RunCreating),
        ] {
            assert!(
                !permits(&scope, &Method::GET, "/projects/7/github-repo"),
                "{scope:?} must not reach GET /projects/7/github-repo"
            );
        }

        // And the two that act for the person do reach it, which is the half that makes the
        // exclusion a boundary rather than a route nobody can open.
        for scope in [Scope::ApiToken(ApiTokenLevel::Admin), Scope::Control] {
            assert!(
                permits(&scope, &Method::GET, "/projects/7/github-repo"),
                "{scope:?} opens the page this fact is for"
            );
        }

        // The neighbour it must not be filed beside, asserted rather than described: `github-ops` is
        // the other half of this page's reading of a project's remote, and it is out of every table
        // too. The day one of them is listed, they stop being a pair.
        assert!(!permits(
            &Scope::ApiToken(ApiTokenLevel::ReadOnly),
            &Method::GET,
            "/projects/7/github-ops"
        ));
    }

    /// The catalogue of DECLARABLE operations is in no scope table either, and the reason is not
    /// the one its twelve neighbours have.
    ///
    /// `GET /github/declarable-ops` is a read of compiled constants. It takes no project id, it
    /// touches no database, and its answer is a function of the binary — two callers on the same
    /// build get the same bytes. So the argument the twelve are excluded under does NOT transfer:
    /// nothing here decides what a project's runs may do unattended, and nothing here discloses one
    /// project's configuration, one repository, or one secret. On confidentiality alone this could
    /// be filed beside `/fleet/exclusions`, which is listed as describing the shape of the fleet
    /// while changing none of it. That is the honest case for the other answer, and it is written
    /// down so the next reader weighs it rather than rediscovering it.
    ///
    /// What settles it is that the grant would buy nobody anything. The caller this route was added
    /// for is the app, which holds `Scope::Control` and never consults these tables; no scoped key
    /// has asked for it. And its only use is the twelve routes above, every one of which a scoped key
    /// is refused — a key that cannot read what a project HAS declared has no use for the list of
    /// what MAY be declared, while what the list does give a holder is the exact vocabulary to aim
    /// at those routes with. Between a grant with a use and a grant without one, this table's own
    /// header decides it: it is deliberately not "every GET", because adding a route must not
    /// silently disclose it to every read-only key. `GET /vcs/requests/{id}/wait` is excluded on the
    /// same least-privilege reading, and `GET /assistant/tools` — compiled constants served for a
    /// picker, which is this route's twin in every respect — is in no table today.
    ///
    /// Membership rather than a request, like the twelve: `permits` is default-deny, so this is safe
    /// today by being nowhere, and what that does not survive is somebody filing a GET of constants
    /// in `READ_ONLY_ROUTES` because it plainly discloses nothing. That is the edit this test
    /// refuses, and the paragraph above is the argument it refuses it with.
    #[test]
    fn the_declarable_ops_catalogue_is_in_no_scope_table() {
        // `/vcs/declarable-ops` is `/github/declarable-ops`'s git twin, in no table for the same
        // reason: compiled constants, no project id, no database, no use to a scoped key that
        // cannot reach a single one of the routes this catalogue explains.
        const CATALOGUES: &[&str] = &["/github/declarable-ops", "/vcs/declarable-ops"];

        for catalogue in CATALOGUES {
            assert!(
                !route_is_listed(READ_ONLY_ROUTES, &Method::GET, catalogue)
                    && !route_is_listed(RUN_CREATING_ROUTES, &Method::GET, catalogue)
                    && !route_is_listed(TEAM_ROUTES, &Method::GET, catalogue)
                    && !route_is_listed(EMAIL_ROUTES, &Method::GET, catalogue)
                    && !route_is_listed(COUNCIL_ROUTES, &Method::GET, catalogue),
                "the catalogue explains routes no scoped key may reach, so no scoped key needs it"
            );

            for scope in [
                Scope::Run(7),
                Scope::ApiToken(ApiTokenLevel::ReadOnly),
                Scope::ApiToken(ApiTokenLevel::RunCreating),
            ] {
                assert!(
                    !permits(&scope, &Method::GET, catalogue),
                    "{scope:?} must not reach GET {catalogue}"
                );
            }

            // And the two that act for the person do reach it, which is the half that makes the
            // exclusion a boundary rather than a route nobody can open.
            for scope in [Scope::ApiToken(ApiTokenLevel::Admin), Scope::Control] {
                assert!(
                    permits(&scope, &Method::GET, catalogue),
                    "{scope:?} opens the page these constants are for"
                );
            }
        }
    }

    /// A project's workflows are the owner's, to read and to change.
    ///
    /// Six routes, all out of every table, and the reads are the half worth arguing for. A
    /// workflow is the *instructions an agent is given*: which nodes run, on which model, behind
    /// which gate. A run able to read the graph it is being executed by is a run reading the shape
    /// of its own supervision, and one able to write it could switch the review node off. The
    /// library listing goes with them — it names every workflow on the machine, which is a map of
    /// the house rather than a fact about this project.
    ///
    /// Membership rather than a request, like the commands above: `permits` is default-deny, so
    /// these are safe today by being nowhere. What that does not survive is somebody filing the
    /// GETs beside the eight `/projects/{id}/…` reads that share their prefix.
    #[test]
    fn a_projects_workflows_are_in_no_scope_table() {
        for (method, pattern) in [
            (Method::GET, "/workflows/library"),
            (Method::GET, "/projects/{id}/workflows"),
            (Method::POST, "/projects/{id}/workflows"),
            (Method::DELETE, "/projects/{id}/workflows/{name}"),
            (Method::POST, "/projects/{id}/workflows/{name}/eject"),
            (Method::POST, "/projects/{id}/workflows/{name}/update"),
            (Method::GET, "/projects/{id}/workflows/{name}/diff"),
            (Method::GET, "/projects/{id}/workflows/{name}/graph"),
            (Method::POST, "/projects/{id}/workflows/{name}/nodes/{node}"),
            (Method::POST, "/projects/{id}/workflows/adopt"),
            // Reads a path nobody has vouched for — see `detect.rs`. Default-deny leaves it to the
            // key of the person sitting at the machine, which is the only one that should be able
            // to point this daemon at an arbitrary folder.
            (Method::GET, "/projects/detect"),
        ] {
            assert!(
                !route_is_listed(READ_ONLY_ROUTES, &method, pattern)
                    && !route_is_listed(RUN_CREATING_ROUTES, &method, pattern)
                    && !route_is_listed(TEAM_ROUTES, &method, pattern)
                    && !route_is_listed(EMAIL_ROUTES, &method, pattern)
                    && !route_is_listed(COUNCIL_ROUTES, &method, pattern),
                "{method} {pattern} must stay out of every scope table"
            );
        }

        // And the one that would matter most, asked as a request: ejecting writes a whole bundle
        // into the project's folder, which is exactly what §7.5 says may not have a door here that
        // an agent lacks.
        for scope in [
            Scope::Run(7),
            Scope::ApiToken(ApiTokenLevel::ReadOnly),
            Scope::ApiToken(ApiTokenLevel::RunCreating),
        ] {
            assert!(
                !permits(&scope, &Method::POST, "/projects/7/workflows/harness/eject"),
                "{scope:?} must not be able to eject a project's workflow"
            );
            assert!(
                !permits(&scope, &Method::GET, "/projects/7/workflows"),
                "{scope:?} must not be able to read which workflows a project uses"
            );
        }
        assert!(permits(
            &Scope::Control,
            &Method::POST,
            "/projects/7/workflows/harness/eject"
        ));
    }

    /// A department's key lives for hours and crosses dozens of subprocesses, which makes it the
    /// longest-lived key in the house and the one that most needs the death rule.
    #[tokio::test]
    async fn a_team_token_stops_working_when_its_run_stops_being_live() {
        let state = test_state("control-token").await;
        let token = live_team_run_with_token(&state, "run-1").await;
        let app = protected_router(state.clone());

        for live in crate::team::LIVE_STATES {
            sqlx::query("UPDATE team_runs SET state = ? WHERE id = 'run-1'")
                .bind(live)
                .execute(&state.pool)
                .await
                .unwrap();
            assert_eq!(
                status_of(&app, "GET", "/files", &token).await,
                StatusCode::OK,
                "a {live} run's specialists still need their key"
            );
        }

        for terminal in crate::team::TERMINAL_STATES {
            sqlx::query("UPDATE team_runs SET state = ? WHERE id = 'run-1'")
                .bind(terminal)
                .execute(&state.pool)
                .await
                .unwrap();
            assert_eq!(
                status_of(&app, "GET", "/files", &token).await,
                StatusCode::UNAUTHORIZED,
                "a {terminal} run's token must not still open anything"
            );
        }
    }

    /// Four credential families now share one `resolve`, and the prefix is all that separates them.
    /// A secret from one family presented under another's shape must authenticate nothing.
    #[tokio::test]
    async fn the_four_token_families_do_not_cross_over() {
        let state = test_state("control-token").await;
        let team_token = live_team_run_with_token(&state, "run-1").await;
        let (run_id, run_token) = running_run_with_token(&state).await;
        let api_token = stored_api_token(&state, "reader", ApiTokenLevel::ReadOnly).await;
        let service_token = mint_service_token(&state.pool, Service::Email)
            .await
            .unwrap();

        let team_secret = team_token.split_once('.').unwrap().1.to_owned();
        let run_secret = run_token.split_once('.').unwrap().1.to_owned();
        let api_secret = api_token.split_once('.').unwrap().1.to_owned();
        let service_secret = service_token.split_once('.').unwrap().1.to_owned();
        let app = protected_router(state);

        for forged in [
            // The team's own secret worn as each of the other three shapes.
            format!("{run_id}.{team_secret}"),
            format!("api:reader.{team_secret}"),
            format!("email.{team_secret}"),
            // And each of the other three worn as the team's.
            format!("team:run-1.{run_secret}"),
            format!("team:run-1.{api_secret}"),
            format!("team:run-1.{service_secret}"),
            // A team id that exists is not enough, and one that does not is not a way in either.
            "team:run-1.".to_owned(),
            "team:nope.secret".to_owned(),
            "team:.secret".to_owned(),
        ] {
            assert_eq!(
                status_of(&app, "GET", "/files", &forged).await,
                StatusCode::UNAUTHORIZED,
                "{forged:?} must not authenticate"
            );
        }

        // And the real one still does, so the loop above is refusing forgeries rather than
        // everything.
        assert_eq!(
            status_of(&app, "GET", "/files", &team_token).await,
            StatusCode::OK
        );
    }

    /// One department's key must not open another's, even though both are live and both are teams.
    #[tokio::test]
    async fn one_team_runs_token_does_not_become_anothers() {
        let state = test_state("control-token").await;
        let first = live_team_run_with_token(&state, "run-1").await;
        live_team_run_with_token(&state, "run-2").await;

        let forged = format!("team:run-2.{}", first.split_once('.').unwrap().1);
        let app = protected_router(state);
        assert_eq!(
            status_of(&app, "GET", "/files", &forged).await,
            StatusCode::UNAUTHORIZED
        );
    }

    /// The scope names the caller, and that is the property `web.rs` leans on for #15a. Asserted
    /// here because it is the thing that distinguishes this scope from `Service`, and the
    /// distinction is the whole argument for adding a fourth family rather than a fifth service.
    #[tokio::test]
    async fn the_scope_carries_which_run_is_calling() {
        let state = test_state("control-token").await;
        let token = live_team_run_with_token(&state, "run-1").await;
        assert_eq!(
            resolve(&state, &token).await,
            Some(Scope::TeamRun("run-1".to_owned()))
        );
    }

    #[tokio::test]
    async fn an_admin_api_key_reaches_everything_control_reaches() {
        let state = test_state("control-token").await;
        let token = stored_api_token(&state, "administrator", ApiTokenLevel::Admin).await;
        let app = protected_router(state);

        for (method, uri) in ROUTE_FAMILY_CASES {
            assert_eq!(
                status_of(&app, method, uri, &token).await,
                status_of(&app, method, uri, "control-token").await,
                "{method} {uri}"
            );
        }
    }
}
