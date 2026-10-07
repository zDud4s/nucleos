//! Tests only: a compact session-script DSL that builds devtime rows, spans and rule facts, so a
//! rule family's tests read as the session they describe. Declared under `#[cfg(test)]` in `main.rs`.
//!
//! # Use
//!
//! ```text
//! use crate::devtime_rules_fixture::{at, attempt, script};
//! let facts = script("...lines...").facts(&DevtimeRulesConfig::default());
//! ```
//!
//! `script(text)` parses the text (panicking, with the line number, on anything it does not
//! understand: an unknown verb, an unknown key for that verb, a bad number or value, a missing
//! required key), and the [`Script`] it returns builds, on demand and without I/O:
//!
//! | accessor | gives |
//! |---|---|
//! | [`Script::rows`] | `SessionRows`: the turns, messages, attempts and markers the ingest would have stored |
//! | [`Script::spans`]`(idle)` / [`Script::spans_default`] | `Vec<SpanRow>` from the real `devtime_lanes::build_spans`, plus any `span` lines; span ids are `1..=n` in the returned order |
//! | [`Script::head`] | the `SessionHead` (session `s1`, project `p1`, bounds taken from the events, `dirty = 1`) |
//! | [`Script::facts`]`(&cfg)` | `SessionFacts` through the real `SessionFacts::build`, with no adapter sources |
//! | [`Script::facts_with`]`(&cfg, sources)` | the same with `AdapterSources` you choose |
//! | [`Script::persist`]`(&pool, project).await` | writes the session through the store's own writers and `replace_spans`, for engine and precision tests |
//! | [`Script::session`]`("s2")`, [`Script::project`]`("p2")` | builders that rename the session (default `s1`) and the project (default `p1`) |
//! | [`at`]`(secs)` | epoch milliseconds of `base + secs` (what `started_ms` holds) |
//! | [`ts`]`(secs)` | the stored UTC text of `base + secs` |
//! | [`attempt`]`(&facts, "aid")` | the `AttemptFact` with that attempt id (panics when absent) |
//! | [`test_pool`]`().await` | an in-memory database with every migration applied |
//!
//! # The grammar
//!
//! One event per line. Blank lines and lines starting with `#` are ignored.
//!
//! ```text
//! <verb> <lane> <start>[+<dur>] key=value key="a value with spaces" ...
//! ```
//!
//! - `<lane>` is the lane name as stored: `main`, `agent:<agent id>` (what a subagent's own records
//!   carry), or `bg:<tool_use_id>`. The lane builder makes `bg:toolu_<aid>` lanes itself for background
//!   shell calls, so you never write one for that. For `turn` the lane is ignored (turns are main-lane);
//!   write `main`.
//! - `<start>` and `<dur>` are seconds on the script clock (fractions allowed: `10.5+0.25`). The clock
//!   starts at the base time `2026-10-04T10:00:00.000Z`, so `0` is the base and `at(0)` its epoch
//!   milliseconds. A line's `dur` is how long it lasts; its default depends on the verb (below).
//! - Values may be double-quoted to hold spaces (`prog="cargo test"`). There are no escapes.
//! - A key a verb does not take is an error, so a typo never goes unnoticed.
//!
//! ## Verbs
//!
//! ### `turn main <start>[+<dur>]`: a human prompt that opens a turn
//!
//! Turns get `seq = 1, 2, ...` in time order. Keys: `interrupted=1` (the turn was interrupted),
//! `correction=1|0` (`opens_with_correction`; **absent means NULL, "not evaluated"**, which is not the
//! same as `0`). Without `+dur` the turn ends at the end of its last main-lane message or attempt
//! before the next turn (at least its own start); with `+dur` it ends `dur` after its start.
//!
//! ### `msg <lane> <start>[+<dur>]`: an assistant message
//!
//! Keys: `id=<message id>` (default `m1`, `m2`, ... in script order of `msg` lines), `tools=1|0`
//! (`has_tool_use`; **default 1 when some attempt references the message with `msg=<id>`, else 0**),
//! `ctx=<tokens>` (the message's `input_tokens`, which is what a model span carries as
//! `context_tokens`; default 0), `model=<name>`, `effort=<name>`. `dur` defaults to 1 s: the message
//! lasts `[start, start+dur]`.
//!
//! ### Tool verbs: `bash`, `ps`, `edit`, `write`, `read`, `grep`, `glob`, `agent`
//!
//! `bash` is tool `Bash`, `ps` is `PowerShell`, `edit` is `Edit`, `write` is `Write`, `read` is `Read`,
//! `grep` is `Grep`, `glob` is `Glob`, `agent` is `Agent` (attempt `kind` `agent`, every other `tool`).
//! Each makes one attempt that starts at `<start>` and ends at `<start>+<dur>` (`ended_at`). **`dur`
//! defaults to 1 s, and to 0 s for a background launch (`bg=1`)**, which answers at once.
//!
//! Keys common to every tool verb:
//!
//! | key | meaning | default |
//! |---|---|---|
//! | `aid=<id>` | the attempt id (the tool_use id is `toolu_<aid>`) | `t<n>`, `n` = 1-based position among the attempt lines of the script. **Two scripts persisted into one database need distinct ids**, so give explicit ones there |
//! | `msg=<id>` | the message the call belongs to (parallel calls share one) | none: the attempt gets its own message `im-<aid>` spanning `[start-2 s, start-1 s]`, `tools=1`, carrying the attempt's own `model`/`effort`. A `msg=<id>` naming no `msg` line makes one the same way (anchored on the earliest attempt that names it) |
//! | `model=`, `effort=` | the attempt's model and effort | the message's (as the parser copies them) |
//! | `out=ok\|error\|interrupted\|launched\|unknown` | the stored outcome. `unknown` also leaves `ended_at` NULL (no result ever came back) | `ok`, or `launched` with `bg=1` |
//! | `exit=<n>` | `exit_code` | NULL |
//! | `err=<class>` | `error_class` (`none` forces NULL; otherwise one of `devtime_parse::ERROR_CLASSES`, e.g. `exit_75`, `timeout`, `hook_block`, `wrong_shell`, `permission_denied`, `edit_not_found`, `tool_error`) | for `out=error`: `exit_nonzero` when `exit` is given and non-zero, else `tool_error`; for `out=interrupted`: `interrupted`; else NULL |
//! | `refs_in=a,b`, `refs_out=c` | the reference hashes (opaque tokens, stored as given) | none |
//! | `pv=1\|2` | the attempt's `parser_version`; `refs_known` is `pv >= 2` | 2 |
//!
//! Verb-specific keys:
//!
//! - `bash`, `ps`: `prog="<program>"` (the stored `cmd_program`, e.g. `cargo test`, `git commit`,
//!   `sleep`, `bash scripts/gates.sh`), `hash=<token>` (`cmd_hash`; default `h-<prog>` with spaces
//!   turned into `_`, so **two runs of the same `prog` share a hash unless you say otherwise**),
//!   `class=test|build|lint` (shorthand that sets `prog` to `cargo test` / `cargo build` /
//!   `cargo clippy` when `prog` is absent; it is ignored when `prog` is given), `timeout=<ms>`
//!   (`timeout_ms`, milliseconds as stored), `files=a,b` (the written files), `bg=1` (a background
//!   launch), `bg_end=<secs>` (the background work's real end, **absolute on the script clock**, which
//!   becomes `bg_ended_at` with confidence `exact`; this is also what makes the lane builder create the
//!   `bg:toolu_<aid>` lane), `bg_status=completed|failed|killed` (what the notification said; the
//!   effective `outcome` of a background attempt comes from it). Without `prog` and `class` the call has
//!   no program and no hash.
//! - `edit`: `path=<path>` (required), `before=<token>`, `after=<token>` (opaque content hashes; default
//!   `b<n>` / `a<n>` with `n` as in the default `aid`, so two edits differ unless you set them equal,
//!   which is how you script an edit and its revert). The attempt's `files` is `[path]`.
//! - `write`: `path=<path>` (required), `after=<token>`; the edit is `before = ""`. `files` is `[path]`.
//! - `read`: `path=<path>` (required), `off=<n>` (the read offset, default 0).
//! - `grep`, `glob`: no extra keys (the pattern is never stored).
//! - `agent`: `type=<agentType>` (`agent_type`; a role comes from it, omit it for a NULL type), `id=<agent
//!   id>` (`agent_id`; the subagent's own lane is then `agent:<id>` and you write its events with that
//!   lane), `bg=1`, `bg_end=<secs>`, `bg_status=...` as for `bash`, and the common keys. A foreground
//!   agent lasts `dur` seconds on the parent's lane.
//!
//! ### `marker <lane> <start>`: a marker
//!
//! `kind=<kind>` is required (one of `devtime_store::MARKER_KINDS`: `interrupt`, `bg_notification`,
//! `bg_ref`, `compact_boundary`, `commit`, ...). `ref=<text>` is the reference; a value starting with
//! `@` is shorthand for a tool_use id, so `ref=@t3` stores `toolu_t3` (a `bg_notification` refers to the
//! launching tool_use id).
//!
//! ### `span <lane> <start>+<dur> kind=<kind> [aid=<attempt id>]`: a raw extra span
//!
//! Appended to what the lane builder produced, for what the builder never makes (`wait_machine`, which
//! only an adapter explains). It claims the attempt `aid` when given. It does not take part in the
//! lane partition. Use it sparingly.
//!
//! ## What you get
//!
//! - `SessionFacts` built by the real `build`: attempts sorted `(started_ms, attempt_id)` and numbered
//!   `idx`, classified (`cmd_class`, `is_*`, `mutating`, `role`), the effective `outcome`, `done_ms`,
//!   `turn_seq` (the turn whose `[start, next start)` holds the attempt), spans from the real lane
//!   builder (so the partition invariant holds), messages, turns and markers. The project's command set
//!   is resolved for `p1` (or whatever `.project()` set).
//! - Rule tests assert on `Finding`s computed from those facts. Compare times with [`at`].
//!
//! # Worked example: a failing `cargo test`, an edit, a green `cargo test`
//!
//! ```text
//! let facts = script("
//!     turn  main 0
//!     msg   main 1+1  id=m1 model=opus
//!     bash  main 2+3  msg=m1 prog=\"cargo test\" out=error exit=101 aid=red
//!     edit  main 6+1  path=core/src/a.rs aid=fix
//!     bash  main 8+3  prog=\"cargo test\" aid=green
//! ").facts(&DevtimeRulesConfig::default());
//! ```
//!
//! The resulting facts, as the real `build` makes them:
//!
//! - `facts.attempts` is `[red, fix, green]` with `idx` 0, 1, 2 (started at `at(2)`, `at(6)`, `at(8)`).
//! - `red`: `is_shell`, `cmd_program = "cargo test"`, `cmd_class = Some(Test)` (from the default test
//!   list), `outcome = Error`, `exit_code = Some(101)`, `error_class = Some("exit_nonzero")`, `model =
//!   Some("opus")` (from message `m1`), `message_id = Some("m1")`, `turn_seq = Some(1)`, `done_ms =
//!   at(5)`, so `a_fail(red)` holds.
//! - `fix`: `is_edit`, `outcome = Ok`, `edits = [EditFact { path: "core/src/a.rs", before: "b2", after:
//!   "a2" }]`, `files = ["core/src/a.rs"]`, its own message `im-fix` over `[at(4), at(5)]`.
//! - `green`: `cmd_class = Some(Test)`, `outcome = Ok`, `exit_code = None`, `cmd_hash` equal to `red`'s
//!   (`"h-cargo_test"`), so `passed(green)` holds. A1 (test fail, edit, same hash, green) sees: `red`
//!   fails, `edits_between(facts, at(5), at(8))` yields `fix`, and `green` passes: a cycle over
//!   `[red.done_ms, green.started_ms]`.
//! - `facts.spans` is the main lane's partition from the first event to the last (a `model` span, then
//!   the `tool` spans of `red`, `fix` and `green` with `model` spans between), `facts.turns` holds one
//!   turn with `opens_with_correction = None`, and `facts.messages` holds `m1`, `im-fix`, `im-green`
//!   (the message of `red` is `m1`, so `red` has no `im-red`).
//!
//! A subagent and its work:
//!
//! ```text
//!     agent main 13+20 type=wf-executor id=ag1 model=sonnet aid=ag
//!     read  agent:ag1 14+1 path=core/src/a.rs aid=r1
//! ```
//!
//! `ag` has `role = Some(Implementer)`, `agent_id = Some("ag1")`, a `subagent` span `[13, 33]` on `main`;
//! `r1` lives on lane `agent:ag1` with its own partition.
//!
//! A background command and its notification:
//!
//! ```text
//!     bash   main 7+0 bg=1 bg_end=20 bg_status=completed prog=\"cargo test\" aid=bgt
//!     marker main 21 kind=bg_notification ref=@bgt
//! ```
//!
//! `bgt` has `background = true`, `outcome = Ok` (completed, exit none), `done_ms = at(20)`, and the
//! spans gain the lane `bg:toolu_bgt`.
#![allow(dead_code)] // a helper no family test happens to call is not an error

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::json;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::config::DevtimeRulesConfig;
use crate::devtime_lanes;
use crate::devtime_rules::{AdapterSources, AttemptFact, SessionFacts, format_ms, parse_ms};
use crate::devtime_store::{
    self, AttemptRow, MarkerRow, MessageRow, SessionHead, SessionRow, SessionRows, SpanRow, TurnRow,
};

/// The parser version a script's rows carry unless a line says `pv=`.
const SCRIPT_PARSER_VERSION: i64 = 2;
/// The idle threshold `spans_default` uses (the one `devtime.yaml` ships).
const DEFAULT_IDLE: Duration = Duration::from_secs(900);

/// Epoch milliseconds of the script clock's origin, `2026-10-04T10:00:00.000Z`.
fn base_ms() -> i64 {
    parse_ms("2026-10-04T10:00:00.000Z").expect("the base time parses")
}

/// Epoch milliseconds of `base + secs`: what a fact's `started_ms` holds for a line at `secs`.
pub fn at(secs: i64) -> i64 {
    base_ms() + secs * 1000
}

/// The stored UTC text of `base + secs`.
pub fn ts(secs: i64) -> String {
    format_ms(at(secs))
}

/// The attempt fact with this attempt id; panics when the script made none.
pub fn attempt<'a>(facts: &'a SessionFacts, attempt_id: &str) -> &'a AttemptFact {
    facts
        .attempts
        .iter()
        .find(|a| a.attempt_id == attempt_id)
        .unwrap_or_else(|| panic!("no attempt `{attempt_id}` in the facts"))
}

/// An in-memory database with every migration applied. One connection, so a transaction and a plain
/// query see the same data.
pub async fn test_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(":memory:")
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}

// ---------------------------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------------------------

/// One line of the script.
#[derive(Debug, Clone)]
struct Ev {
    line: usize,
    verb: String,
    lane: String,
    start_ms: i64,
    dur_ms: Option<i64>,
    kv: BTreeMap<String, String>,
}

/// A mistake in the script: the test cannot go on.
fn script_error(line: usize, what: &str) -> ! {
    panic!("script line {line}: {what}")
}

/// Seconds (maybe fractional) as milliseconds.
fn seconds_to_ms(text: &str, line: usize) -> i64 {
    match text.parse::<f64>() {
        Ok(secs) => (secs * 1000.0).round() as i64,
        Err(_) => script_error(line, &format!("`{text}` is not a number of seconds")),
    }
}

impl Ev {
    fn get(&self, key: &str) -> Option<&str> {
        self.kv.get(key).map(String::as_str)
    }

    fn text(&self, key: &str) -> Option<String> {
        self.get(key).map(str::to_string)
    }

    fn need(&self, key: &str) -> String {
        match self.text(key) {
            Some(value) => value,
            None => script_error(self.line, &format!("`{} ...` needs `{key}=`", self.verb)),
        }
    }

    fn flag(&self, key: &str) -> Option<bool> {
        self.get(key).map(|value| match value {
            "1" | "true" => true,
            "0" | "false" => false,
            other => script_error(self.line, &format!("`{key}={other}` is not 1 or 0")),
        })
    }

    fn int(&self, key: &str) -> Option<i64> {
        self.get(key).map(|value| match value.parse::<i64>() {
            Ok(number) => number,
            Err(_) => script_error(self.line, &format!("`{key}={value}` is not an integer")),
        })
    }

    /// A key holding seconds, as milliseconds.
    fn secs(&self, key: &str) -> Option<i64> {
        self.get(key).map(|value| seconds_to_ms(value, self.line))
    }

    fn list(&self, key: &str) -> Vec<String> {
        self.get(key)
            .map(|value| {
                value
                    .split(',')
                    .filter(|item| !item.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Splits a line into words; a double-quoted run keeps its spaces and loses its quotes.
fn tokens(line: &str, number: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for ch in line.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if quoted {
        script_error(number, "an unterminated quote");
    }
    if started {
        out.push(current);
    }
    out
}

const ATTEMPT_VERBS: [&str; 8] = [
    "bash", "ps", "edit", "write", "read", "grep", "glob", "agent",
];

/// The keys a verb takes; `None` for an unknown verb.
fn allowed_keys(verb: &str) -> Option<&'static [&'static str]> {
    Some(match verb {
        "turn" => &["interrupted", "correction"],
        "msg" => &["id", "tools", "ctx", "model", "effort"],
        "bash" | "ps" => &[
            "msg",
            "refs_in",
            "refs_out",
            "pv",
            "aid",
            "model",
            "effort",
            "out",
            "exit",
            "err",
            "prog",
            "hash",
            "class",
            "timeout",
            "bg",
            "bg_end",
            "bg_status",
            "files",
        ],
        "edit" => &[
            "msg", "refs_in", "refs_out", "pv", "aid", "model", "effort", "out", "exit", "err",
            "path", "before", "after",
        ],
        "write" => &[
            "msg", "refs_in", "refs_out", "pv", "aid", "model", "effort", "out", "exit", "err",
            "path", "after",
        ],
        "read" => &[
            "msg", "refs_in", "refs_out", "pv", "aid", "model", "effort", "out", "exit", "err",
            "path", "off",
        ],
        "grep" | "glob" => &[
            "msg", "refs_in", "refs_out", "pv", "aid", "model", "effort", "out", "exit", "err",
        ],
        "agent" => &[
            "msg",
            "refs_in",
            "refs_out",
            "pv",
            "aid",
            "model",
            "effort",
            "out",
            "exit",
            "err",
            "type",
            "id",
            "bg",
            "bg_end",
            "bg_status",
        ],
        "marker" => &["kind", "ref"],
        "span" => &["kind", "aid"],
        _ => return None,
    })
}

fn parse_event(line: &str, number: usize) -> Ev {
    let words = tokens(line, number);
    if words.len() < 3 {
        script_error(
            number,
            &format!("expected `<verb> <lane> <start>[+<dur>] key=value ...`, got `{line}`"),
        );
    }
    let verb = words[0].clone();
    let Some(allowed) = allowed_keys(&verb) else {
        script_error(number, &format!("unknown verb `{verb}`"))
    };
    let (start_text, dur_text) = match words[2].split_once('+') {
        Some((start, dur)) => (start, Some(dur)),
        None => (words[2].as_str(), None),
    };
    let mut kv = BTreeMap::new();
    for word in &words[3..] {
        let Some((key, value)) = word.split_once('=') else {
            script_error(number, &format!("`{word}` is not key=value"))
        };
        if !allowed.contains(&key) {
            script_error(
                number,
                &format!("`{verb}` takes no key `{key}` (it takes {allowed:?})"),
            );
        }
        kv.insert(key.to_string(), value.to_string());
    }
    Ev {
        line: number,
        lane: words[1].clone(),
        start_ms: base_ms() + seconds_to_ms(start_text, number),
        dur_ms: dur_text.map(|text| seconds_to_ms(text, number)),
        verb,
        kv,
    }
}

/// Parses a session script (see the module doc for the grammar). Panics with the line number on a
/// mistake, since the only caller is a test.
pub fn script(text: &str) -> Script {
    let events = text
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .map(|(index, line)| parse_event(line.trim(), index + 1))
        .collect();
    Script {
        session: "s1".to_string(),
        project: "p1".to_string(),
        events,
    }
}

// ---------------------------------------------------------------------------------------------
// Building rows
// ---------------------------------------------------------------------------------------------

/// A parsed session script.
#[derive(Debug, Clone)]
pub struct Script {
    session: String,
    project: String,
    events: Vec<Ev>,
}

/// A message before it becomes a row.
struct Draft {
    id: String,
    lane: String,
    first: i64,
    last: i64,
    tools: Option<bool>,
    ctx: i64,
    model: Option<String>,
    effort: Option<String>,
    explicit: bool,
}

fn tool_name_of(verb: &str) -> &'static str {
    match verb {
        "bash" => "Bash",
        "ps" => "PowerShell",
        "edit" => "Edit",
        "write" => "Write",
        "read" => "Read",
        "grep" => "Grep",
        "glob" => "Glob",
        _ => "Agent",
    }
}

const OUTCOMES: [&str; 5] = ["ok", "error", "interrupted", "launched", "unknown"];

/// The program a `class=` shorthand stands for.
fn program_of_class(class: &str, line: usize) -> &'static str {
    match class {
        "test" => "cargo test",
        "build" => "cargo build",
        "lint" => "cargo clippy",
        other => script_error(line, &format!("`class={other}` is not test, build or lint")),
    }
}

/// Every instant any row mentions, in milliseconds.
fn instants_of(rows: &SessionRows, spans: &[SpanRow]) -> Vec<i64> {
    let mut all = Vec::new();
    let mut take = |text: &String| all.extend(parse_ms(text));
    for turn in &rows.turns {
        take(&turn.started_at);
        take(&turn.ended_at);
    }
    for message in &rows.messages {
        take(&message.first_at);
        take(&message.last_at);
    }
    for attempt in &rows.attempts {
        take(&attempt.started_at);
        if let Some(ended) = &attempt.ended_at {
            take(ended);
        }
        if let Some(ended) = &attempt.bg_ended_at {
            take(ended);
        }
    }
    for marker in &rows.markers {
        take(&marker.ts);
    }
    for span in spans {
        take(&span.started_at);
        take(&span.ended_at);
    }
    all
}

impl Script {
    /// Renames the session (default `s1`).
    pub fn session(mut self, id: &str) -> Self {
        self.session = id.to_string();
        self
    }

    /// Renames the project the facts are built for (default `p1`).
    pub fn project(mut self, id: &str) -> Self {
        self.project = id.to_string();
        self
    }

    pub fn session_id(&self) -> &str {
        &self.session
    }

    pub fn project_id(&self) -> &str {
        &self.project
    }

    /// The turns, messages, attempts and markers the script describes.
    pub fn rows(&self) -> SessionRows {
        let session = &self.session;

        // Explicit messages first, so an attempt can inherit a message's model whatever the line order.
        let mut drafts: Vec<Draft> = Vec::new();
        for ev in self.events.iter().filter(|ev| ev.verb == "msg") {
            let id = ev
                .text("id")
                .unwrap_or_else(|| format!("m{}", drafts.len() + 1));
            drafts.push(Draft {
                id,
                lane: ev.lane.clone(),
                first: ev.start_ms,
                last: ev.start_ms + ev.dur_ms.unwrap_or(1000),
                tools: ev.flag("tools"),
                ctx: ev.int("ctx").unwrap_or(0),
                model: ev.text("model"),
                effort: ev.text("effort"),
                explicit: true,
            });
        }

        let mut referenced: BTreeSet<String> = BTreeSet::new();
        let mut attempts: Vec<AttemptRow> = Vec::new();
        let attempt_events = self
            .events
            .iter()
            .filter(|ev| ATTEMPT_VERBS.contains(&ev.verb.as_str()));
        for (index, ev) in attempt_events.enumerate() {
            let n = index + 1;
            let aid = ev.text("aid").unwrap_or_else(|| format!("t{n}"));
            let agent = ev.verb == "agent";
            let shell = ev.verb == "bash" || ev.verb == "ps";
            let background = ev.flag("bg") == Some(true);

            // The message the call belongs to.
            let message_id = ev.text("msg").unwrap_or_else(|| format!("im-{aid}"));
            match drafts.iter().position(|draft| draft.id == message_id) {
                Some(at_index) => {
                    let draft = &mut drafts[at_index];
                    if !draft.explicit && ev.start_ms - 2000 < draft.first {
                        draft.first = ev.start_ms - 2000;
                        draft.last = ev.start_ms - 1000;
                    }
                }
                None => drafts.push(Draft {
                    id: message_id.clone(),
                    lane: ev.lane.clone(),
                    first: ev.start_ms - 2000,
                    last: ev.start_ms - 1000,
                    tools: Some(true),
                    ctx: 0,
                    model: ev.text("model"),
                    effort: ev.text("effort"),
                    explicit: false,
                }),
            }
            referenced.insert(message_id.clone());
            let (message_model, message_effort) = drafts
                .iter()
                .find(|draft| draft.id == message_id)
                .map(|draft| (draft.model.clone(), draft.effort.clone()))
                .unwrap_or_default();

            let out = ev
                .text("out")
                .unwrap_or_else(|| String::from(if background { "launched" } else { "ok" }));
            if !OUTCOMES.contains(&out.as_str()) {
                script_error(ev.line, &format!("`out={out}` is not one of {OUTCOMES:?}"));
            }
            let exit_code = ev.int("exit");
            let error_class = match ev.get("err") {
                Some("none") => None,
                Some(class) => Some(class.to_string()),
                None => match out.as_str() {
                    "error" if exit_code.is_some_and(|code| code != 0) => {
                        Some("exit_nonzero".to_string())
                    }
                    "error" => Some("tool_error".to_string()),
                    "interrupted" => Some("interrupted".to_string()),
                    _ => None,
                },
            };
            let bg_status = ev.text("bg_status");
            if let Some(status) = &bg_status
                && !devtime_store::BG_STATUSES.contains(&status.as_str())
            {
                script_error(
                    ev.line,
                    &format!("`bg_status={status}` is not completed, failed or killed"),
                );
            }
            let bg_end = ev.secs("bg_end").map(|secs| base_ms() + secs);

            let mut program: Option<String> = None;
            let mut hash: Option<String> = None;
            let mut files: Vec<String> = Vec::new();
            let mut edits: Vec<serde_json::Value> = Vec::new();
            let mut reads: Vec<serde_json::Value> = Vec::new();
            if shell {
                program = ev.text("prog").or_else(|| {
                    ev.get("class")
                        .map(|class| program_of_class(class, ev.line).to_string())
                });
                hash = ev.text("hash").or_else(|| {
                    program
                        .as_ref()
                        .map(|program| format!("h-{}", program.replace(' ', "_")))
                });
                files = ev.list("files");
            }
            match ev.verb.as_str() {
                "edit" => {
                    let path = ev.need("path");
                    edits.push(json!({
                        "path": path,
                        "before": ev.text("before").unwrap_or_else(|| format!("b{n}")),
                        "after": ev.text("after").unwrap_or_else(|| format!("a{n}")),
                    }));
                    files.push(path);
                }
                "write" => {
                    let path = ev.need("path");
                    edits.push(json!({
                        "path": path,
                        "before": "",
                        "after": ev.text("after").unwrap_or_else(|| format!("a{n}")),
                    }));
                    files.push(path);
                }
                "read" => {
                    reads.push(json!({
                        "path": ev.need("path"),
                        "offset": ev.int("off").unwrap_or(0),
                    }));
                }
                _ => {}
            }

            let dur = ev.dur_ms.unwrap_or(if background { 0 } else { 1000 });
            attempts.push(AttemptRow {
                attempt_id: aid.clone(),
                session_id: session.clone(),
                lane: ev.lane.clone(),
                message_id: Some(message_id),
                tool_use_id: format!("toolu_{aid}"),
                kind: String::from(if agent { "agent" } else { "tool" }),
                tool_name: tool_name_of(&ev.verb).to_string(),
                agent_type: if agent { ev.text("type") } else { None },
                agent_id: if agent { ev.text("id") } else { None },
                model: ev.text("model").or(message_model),
                effort: ev.text("effort").or(message_effort),
                started_at: format_ms(ev.start_ms),
                ended_at: (out != "unknown").then(|| format_ms(ev.start_ms + dur)),
                outcome: out,
                exit_code,
                error_class,
                cmd_program: program,
                cmd_hash: hash,
                timeout_ms: ev.int("timeout"),
                background: i64::from(background),
                bg_task_id: background.then(|| format!("task-{aid}")),
                bg_ended_at: bg_end.map(format_ms),
                bg_confidence: bg_end.map(|_| "exact".to_string()),
                files: serde_json::to_string(&files).unwrap(),
                edits: serde_json::to_string(&edits).unwrap(),
                reads: serde_json::to_string(&reads).unwrap(),
                parser_version: ev.int("pv").unwrap_or(SCRIPT_PARSER_VERSION),
                bg_status,
                refs_in: serde_json::to_string(&ev.list("refs_in")).unwrap(),
                refs_out: serde_json::to_string(&ev.list("refs_out")).unwrap(),
            });
        }

        let messages: Vec<MessageRow> = drafts
            .iter()
            .map(|draft| MessageRow {
                session_id: session.clone(),
                lane: draft.lane.clone(),
                message_id: draft.id.clone(),
                first_at: format_ms(draft.first),
                last_at: format_ms(draft.last),
                model: draft.model.clone(),
                effort: draft.effort.clone(),
                input_tokens: draft.ctx,
                has_tool_use: i64::from(
                    draft
                        .tools
                        .unwrap_or_else(|| referenced.contains(&draft.id)),
                ),
                parser_version: SCRIPT_PARSER_VERSION,
                ..Default::default()
            })
            .collect();

        // Turns: seq in time order; a turn ends where its last main-lane activity does.
        let mut turn_events: Vec<&Ev> = self.events.iter().filter(|ev| ev.verb == "turn").collect();
        turn_events.sort_by_key(|ev| ev.start_ms);
        let mut turns = Vec::new();
        for (index, ev) in turn_events.iter().enumerate() {
            let next = turn_events
                .get(index + 1)
                .map_or(i64::MAX, |following| following.start_ms);
            let inside = |ms: i64| ms >= ev.start_ms && ms < next;
            let message_ends = drafts
                .iter()
                .filter(|draft| draft.lane == "main" && inside(draft.first))
                .map(|draft| draft.last);
            let attempt_ends = attempts
                .iter()
                .filter(|attempt| attempt.lane == "main")
                .filter_map(|attempt| {
                    let started = parse_ms(&attempt.started_at)?;
                    inside(started).then(|| {
                        attempt
                            .ended_at
                            .as_deref()
                            .and_then(parse_ms)
                            .unwrap_or(started)
                    })
                });
            let last_activity = message_ends
                .chain(attempt_ends)
                .max()
                .unwrap_or(ev.start_ms)
                .max(ev.start_ms);
            let ended = ev.dur_ms.map_or(last_activity, |dur| ev.start_ms + dur);
            turns.push(TurnRow {
                session_id: session.clone(),
                seq: i64::try_from(index).unwrap() + 1,
                started_at: format_ms(ev.start_ms),
                ended_at: format_ms(ended),
                interrupted: i64::from(ev.flag("interrupted") == Some(true)),
                opens_with_correction: ev.flag("correction").map(i64::from),
                parser_version: SCRIPT_PARSER_VERSION,
            });
        }

        let markers = self
            .events
            .iter()
            .filter(|ev| ev.verb == "marker")
            .map(|ev| MarkerRow {
                id: 0,
                session_id: session.clone(),
                lane: ev.lane.clone(),
                ts: format_ms(ev.start_ms),
                kind: ev.need("kind"),
                r#ref: ev
                    .text("ref")
                    .map(|reference| match reference.strip_prefix('@') {
                        Some(aid) => format!("toolu_{aid}"),
                        None => reference.clone(),
                    }),
                parser_version: SCRIPT_PARSER_VERSION,
            })
            .collect();

        SessionRows {
            turns,
            messages,
            attempts,
            markers,
        }
    }

    /// The spans the real lane builder makes from [`Script::rows`], plus the script's `span` lines.
    /// Span ids are `1..=n` in the returned order, which is also the order a fresh database assigns
    /// them when [`Script::persist`] inserts them.
    pub fn spans(&self, idle: Duration) -> Vec<SpanRow> {
        let mut spans = devtime_lanes::build_spans(&self.rows(), idle);
        for ev in self.events.iter().filter(|ev| ev.verb == "span") {
            let aid = ev.text("aid");
            let attempt_ids: Vec<&String> = aid.iter().collect();
            spans.push(SpanRow {
                id: 0,
                session_id: self.session.clone(),
                lane: ev.lane.clone(),
                kind: ev.need("kind"),
                started_at: format_ms(ev.start_ms),
                ended_at: format_ms(ev.start_ms + ev.dur_ms.unwrap_or(0)),
                attempt_ids: serde_json::to_string(&attempt_ids).unwrap(),
                attempt_id: aid.clone(),
                context_tokens: None,
                waste: None,
                rule_id: None,
                confidence: "exact".to_string(),
                parser_version: SCRIPT_PARSER_VERSION,
            });
        }
        for (index, span) in spans.iter_mut().enumerate() {
            span.id = i64::try_from(index).unwrap() + 1;
        }
        spans
    }

    /// [`Script::spans`] with a 15-minute idle threshold.
    pub fn spans_default(&self) -> Vec<SpanRow> {
        self.spans(DEFAULT_IDLE)
    }

    /// The session head: bounds from the events, `dirty = 1`, never ruled.
    pub fn head(&self) -> SessionHead {
        let instants = instants_of(&self.rows(), &self.spans_default());
        let first = instants.iter().copied().min().unwrap_or_else(base_ms);
        let last = instants.iter().copied().max().unwrap_or(first);
        SessionHead {
            session_id: self.session.clone(),
            project_id: self.project.clone(),
            started_at: Some(format_ms(first)),
            ended_at: Some(format_ms(last)),
            updated_at: format_ms(last),
            dirty: 1,
            rules_version: None,
        }
    }

    /// The facts the real `SessionFacts::build` makes from the script, with no adapter source present.
    pub fn facts(&self, cfg: &DevtimeRulesConfig) -> SessionFacts {
        self.facts_with(cfg, AdapterSources::default())
    }

    /// [`Script::facts`] with the adapter sources you choose.
    pub fn facts_with(&self, cfg: &DevtimeRulesConfig, sources: AdapterSources) -> SessionFacts {
        SessionFacts::build(
            &self.head(),
            &self.rows(),
            &self.spans_default(),
            cfg,
            sources,
        )
    }

    /// Writes the session into `pool` through the store's own writers, as `project`, and its spans
    /// through `replace_spans`. The session is left `dirty = 1`, as an ingest leaves it.
    pub async fn persist(&self, pool: &SqlitePool, project: &str) -> sqlx::Result<()> {
        let rows = self.rows();
        let spans = self.spans_default();
        let instants = instants_of(&rows, &spans);
        let first = instants.iter().copied().min().unwrap_or_else(base_ms);
        let last = instants.iter().copied().max().unwrap_or(first);

        let mut tx = devtime_store::begin_chunk(pool).await?;
        devtime_store::upsert_session(
            &mut tx,
            &SessionRow {
                session_id: self.session.clone(),
                project_id: project.to_string(),
                started_at: Some(format_ms(first)),
                ended_at: Some(format_ms(last)),
                dirty: 1,
                parser_version: SCRIPT_PARSER_VERSION,
                ..Default::default()
            },
        )
        .await?;
        for turn in &rows.turns {
            let seq = devtime_store::open_turn(
                &mut tx,
                &self.session,
                &turn.started_at,
                turn.opens_with_correction.map(|flag| flag != 0),
                turn.parser_version,
            )
            .await?;
            devtime_store::touch_turn(&mut tx, &self.session, seq, &turn.ended_at).await?;
            if turn.interrupted != 0 {
                devtime_store::mark_turn_interrupted(&mut tx, &self.session, seq).await?;
            }
        }
        for message in &rows.messages {
            devtime_store::upsert_message(&mut tx, message).await?;
        }
        for attempt in &rows.attempts {
            devtime_store::upsert_attempt_launch(&mut tx, attempt).await?;
        }
        for marker in &rows.markers {
            devtime_store::insert_marker(&mut tx, marker).await?;
        }
        tx.commit().await?;
        devtime_store::replace_spans(pool, &self.session, &spans).await
    }
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devtime_rules::{Outcome, Role, passed};
    use crate::devtime_rules_cmd::CmdClass;

    /// The invariant of the lane builder: a lane's spans are sorted, touch end to start, begin and end
    /// where the lane does, and none is empty.
    fn assert_partition(spans: &[SpanRow], lane: &str, start: i64, end: i64) {
        let own: Vec<&SpanRow> = spans.iter().filter(|span| span.lane == lane).collect();
        assert!(!own.is_empty(), "lane {lane} has no spans");
        assert_eq!(own[0].started_at, ts(start), "lane {lane} starts late");
        assert_eq!(
            own[own.len() - 1].ended_at,
            ts(end),
            "lane {lane} ends early"
        );
        for span in &own {
            assert!(span.started_at < span.ended_at, "empty span {span:?}");
        }
        for pair in own.windows(2) {
            assert_eq!(
                pair[0].ended_at, pair[1].started_at,
                "gap or overlap in {lane}"
            );
        }
    }

    const SESSION: &str = "
        turn  main 0
        msg   main 1+1  id=m1 model=opus ctx=1000
        bash  main 2+3  msg=m1 prog=\"cargo test\" out=error exit=101 aid=red
        edit  main 6+1  path=core/src/a.rs aid=fix
        bash  main 8+3  prog=\"cargo test\" aid=green
        agent main 13+20 type=wf-executor id=ag1 model=sonnet aid=ag
        read  agent:ag1 14+1 path=core/src/a.rs aid=r1
    ";

    #[test]
    fn script_builds_rows_spans_and_facts() {
        let parsed = script(SESSION);
        let rows = parsed.rows();
        assert_eq!(rows.turns.len(), 1);
        assert_eq!(rows.attempts.len(), 5);
        assert_eq!(
            rows.messages.len(),
            5,
            "m1 plus one implicit message per other attempt"
        );

        let spans = parsed.spans_default();
        assert_partition(&spans, "main", 0, 33);
        assert_partition(&spans, "agent:ag1", 12, 15);
        assert!(
            spans
                .iter()
                .enumerate()
                .all(|(i, span)| span.id == i as i64 + 1),
            "span ids are 1..=n in order"
        );
        assert!(
            spans
                .iter()
                .any(|s| s.kind == "subagent" && s.lane == "main")
        );

        let facts = parsed.facts(&DevtimeRulesConfig::default());
        let order: Vec<&str> = facts
            .attempts
            .iter()
            .map(|a| a.attempt_id.as_str())
            .collect();
        assert_eq!(
            order,
            ["red", "fix", "green", "ag", "r1"],
            "sorted by start"
        );
        assert!(facts.attempts.iter().enumerate().all(|(i, a)| a.idx == i));

        let red = attempt(&facts, "red");
        assert!(red.is_shell && red.is_main);
        assert_eq!(red.cmd_class, Some(CmdClass::Test));
        assert_eq!(red.outcome, Outcome::Error);
        assert_eq!(red.exit_code, Some(101));
        assert_eq!(red.error_class.as_deref(), Some("exit_nonzero"));
        assert_eq!(red.model.as_deref(), Some("opus"), "from message m1");
        assert_eq!(red.message_id.as_deref(), Some("m1"));
        assert_eq!(red.turn_seq, Some(1));
        assert_eq!((red.started_ms, red.done_ms), (at(2), at(5)));

        let fix = attempt(&facts, "fix");
        assert!(fix.is_edit && !fix.is_shell);
        assert_eq!(fix.edits.len(), 1);
        assert_eq!(fix.edits[0].path, "core/src/a.rs");
        assert_eq!(fix.files, ["core/src/a.rs"]);
        assert_eq!(fix.message_id.as_deref(), Some("im-fix"));

        let green = attempt(&facts, "green");
        assert!(passed(green));
        assert_eq!(
            green.cmd_hash, red.cmd_hash,
            "the same program shares a hash"
        );
        assert_eq!(green.cmd_hash.as_deref(), Some("h-cargo_test"));

        let ag = attempt(&facts, "ag");
        assert_eq!(ag.kind, "agent");
        assert_eq!(ag.role, Some(Role::Implementer));
        assert_eq!(ag.agent_id.as_deref(), Some("ag1"));
        assert_eq!(ag.model.as_deref(), Some("sonnet"));

        let r1 = attempt(&facts, "r1");
        assert_eq!(r1.lane, "agent:ag1");
        assert!(r1.is_read && !r1.is_main);
        assert_eq!(r1.reads[0].offset, 0);

        assert_eq!(facts.turns.len(), 1);
        assert_eq!(facts.turns[0].opens_with_correction, None);
        assert_eq!(facts.messages.len(), 5);
        assert!(
            facts
                .messages
                .iter()
                .any(|m| m.message_id == "m1" && m.has_tool_use)
        );
        assert_eq!(facts.started_ms, at(0));
        assert_eq!(facts.ended_ms, at(33));
        assert_eq!(facts.project_id, "p1");
    }

    #[test]
    fn script_defaults_shorthands_and_background() {
        let facts = script(
            "
            turn   main 0 correction=1
            bash   main 1+1 class=test aid=a
            bash   main 3+1 class=lint aid=b
            ps     main 5+1 prog=Start-Sleep aid=c
            bash   main 7+0 bg=1 bg_end=20 bg_status=completed aid=d
            bash   main 8+1 out=error aid=e
            bash   main 9+1 out=error exit=75 err=exit_75 aid=f
            marker main 21 kind=bg_notification ref=@d
            ",
        )
        .facts(&DevtimeRulesConfig::default());

        let a = attempt(&facts, "a");
        assert_eq!(a.cmd_program.as_deref(), Some("cargo test"));
        assert_eq!(a.cmd_class, Some(CmdClass::Test));
        assert_eq!(attempt(&facts, "b").cmd_class, Some(CmdClass::Lint));

        let c = attempt(&facts, "c");
        assert_eq!(c.tool_name, "PowerShell");
        assert!(c.is_sleep && c.is_shell);

        let d = attempt(&facts, "d");
        assert!(d.background);
        assert_eq!(d.outcome, Outcome::Ok, "completed, no exit code");
        assert_eq!(d.done_ms, at(20));
        assert!(facts.spans.iter().any(|s| s.lane == "bg:toolu_d"));
        assert_eq!(facts.markers.len(), 1);
        assert_eq!(facts.markers[0].reference.as_deref(), Some("toolu_d"));

        assert_eq!(
            attempt(&facts, "e").error_class.as_deref(),
            Some("tool_error")
        );
        assert_eq!(attempt(&facts, "f").error_class.as_deref(), Some("exit_75"));
        assert_eq!(attempt(&facts, "f").exit_code, Some(75));
        assert_eq!(facts.turns[0].opens_with_correction, Some(true));
    }

    #[test]
    #[should_panic(expected = "script line 2")]
    fn an_unknown_key_is_refused_with_its_line() {
        let _ = script("turn main 0\nbash main 1+1 progg=ls");
    }
}
