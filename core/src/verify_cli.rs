//! `nucleos-core --verify`: the command-line client of covered verification (F3-14).
//!
//! Run from inside a worktree, it asks the RUNNING daemon to verify that worktree
//! (`POST /verify` with `kind: test, scope: scope, cover: true, wait: false`, so the ticket id comes
//! back at once), follows the ticket through `/verify/status` up to a wait limit, prints one line per unit (plus the tail of every
//! red unit, the ticket's note and the files no unit covers) and turns the verdict into an exit code. The daemon does the work; this is the thin
//! client that makes it reachable from a shell, a hook or a script, so the logic of "ask, follow,
//! render, map" lives here, testable against an in-process server, and `main.rs` only gathers the
//! token, the cwd and the arguments.
//!
//! It NEVER starts a daemon: with none running there is nothing to ask, and starting one from a
//! verification command would turn a question into a side effect. The exit codes are in [`USAGE`].
//! Nothing calls this command automatically.

use crate::daemon_client::DaemonClient;
use serde_json::Value;
use std::time::{Duration, Instant};

/// The verdict was `passed`.
pub const EXIT_PASSED: i32 = 0;
/// The verdict was `failed` or `errored`.
pub const EXIT_FAILED: i32 = 1;
/// A ticket id is known but no verdict arrived (the wait limit, a lost status call, a ticket done
/// without a verdict this client knows). Re-running joins the same units.
pub const EXIT_NO_VERDICT: i32 = 3;
/// Nothing was verified: no token, a refusal, an unreachable daemon, or a verdict that covered
/// nothing. 2 is skipped on purpose: it is the conventional usage-error code.
pub const EXIT_NOT_VERIFIED: i32 = 4;

/// How long the whole run (submit plus every poll) may take unless told otherwise.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(3600);

/// The least a poll round may take. The daemon holds `/verify/status` open while a ticket is
/// unfinished, but a daemon that answers at once (an old build, a ticket that just changed hands)
/// would otherwise be asked in a tight loop.
const POLL_FLOOR: Duration = Duration::from_millis(500);

/// What `nucleos-core --verify --help` prints, and what a refused `--verify-wait` repeats.
pub const USAGE: &str = "\
usage: nucleos-core --verify [--verify-wait <secs>]

Asks the running daemon to verify the worktree this command is run from, with covered
verification (the daemon picks the base), and waits for the verdict. It never starts a daemon.
Prints one line per unit, the tail of every unit that failed, the ticket's note and the files
no unit covers.

  --verify-wait <secs>   longest the whole run may take, in whole seconds (also --verify-wait=<secs>).
                         Else the NUCLEOS_VERIFY_WAIT environment variable, else 3600.

exit codes:
  0  passed
  1  failed or errored
  3  the ticket has no verdict yet (wait limit reached, or its status could not be read):
     it prints `ticket <id>`; re-run `nucleos-core --verify` to join its units
  4  not verified: no daemon token, a refusal from the daemon (including a covered verification
     it cannot do), an unreachable daemon, a bad --verify-wait, or a verdict that ran nothing
";

/// What a run prints and how it ends. `main` prints the two streams and exits with `code`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The wait limit: `--verify-wait <secs>` or `--verify-wait=<secs>` in `args`, else `env` (the
/// `NUCLEOS_VERIFY_WAIT` value), else [`DEFAULT_WAIT`]. The flag beats the environment. Whole
/// positive seconds only: `0`, a non-integer and a flag with no value are refused, never read as
/// the default.
pub fn wait_limit(args: &[String], env: Option<&str>) -> Result<Duration, String> {
    for (at, arg) in args.iter().enumerate() {
        if arg == "--verify-wait" {
            return match args.get(at + 1) {
                Some(value) => parse_seconds(value, "--verify-wait"),
                None => Err("--verify-wait needs a number of seconds".to_string()),
            };
        }
        if let Some(value) = arg.strip_prefix("--verify-wait=") {
            return parse_seconds(value, "--verify-wait");
        }
    }
    match env {
        Some(value) => parse_seconds(value, "NUCLEOS_VERIFY_WAIT"),
        None => Ok(DEFAULT_WAIT),
    }
}

/// What the command line asks of `--verify`, decided from the arguments alone so `main` only maps
/// it to a print or an exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// No `--verify` or `--verify-*` argument: not this command's call, `main` moves on.
    NotVerify,
    /// `--help` beside a `--verify` flag: print [`USAGE`], exit 0.
    Help,
    /// A `--verify-*` flag without `--verify` itself: a usage error, answered with the usage and
    /// [`EXIT_NOT_VERIFIED`], contacting nothing.
    NeedsVerify,
    /// The wait limit was refused (the reason): usage and [`EXIT_NOT_VERIFIED`].
    BadWait(String),
    /// Verify the current worktree, waiting up to this long.
    Run(Duration),
}

/// Decide what the arguments ask of `--verify`, in this order: no `--verify` and no `--verify-*`
/// spelling is not this command's call; `--help` anywhere wins over everything else; a `--verify-*`
/// flag without `--verify` is a usage error (it used to fall past `main`'s block and every other
/// one, and start a daemon, the same bug class as the `--land=` note in `main.rs`); then the wait
/// limit ([`wait_limit`]) is read, and a refusal is [`Invocation::BadWait`].
pub fn invocation(args: &[String], env_wait: Option<&str>) -> Invocation {
    if !args
        .iter()
        .any(|a| a == "--verify" || a.starts_with("--verify-"))
    {
        return Invocation::NotVerify;
    }
    if args.iter().any(|a| a == "--help") {
        return Invocation::Help;
    }
    if !args.iter().any(|a| a == "--verify") {
        return Invocation::NeedsVerify;
    }
    match wait_limit(args, env_wait) {
        Ok(limit) => Invocation::Run(limit),
        Err(reason) => Invocation::BadWait(reason),
    }
}

fn parse_seconds(raw: &str, source: &str) -> Result<Duration, String> {
    match raw.trim().parse::<u64>() {
        Ok(secs) if secs > 0 => Ok(Duration::from_secs(secs)),
        _ => Err(format!(
            "{source} must be a whole number of seconds above zero, got {raw:?}"
        )),
    }
}

/// The exit code a finished ticket's verdict maps to; `None` for a verdict this client does not
/// know (or none at all), which the caller treats as "no verdict".
fn verdict_code(ticket: &Value) -> Option<i32> {
    match ticket["verdict"].as_str()? {
        "passed" => Some(EXIT_PASSED),
        "failed" | "errored" => Some(EXIT_FAILED),
        // Covers nothing (verify.rs): not a pass, and not a failure either.
        "nothing_ran" => Some(EXIT_NOT_VERIFIED),
        _ => None,
    }
}

/// One line per unit: its status, then its group, or its argv when it has none. A unit that was
/// red before the change says so, and the tail of a failed or errored unit is indented below it.
fn render_units(ticket: &Value) -> String {
    let mut out = String::new();
    let Some(units) = ticket["units"].as_array() else {
        return out;
    };
    for unit in units {
        let status = unit["status"].as_str().unwrap_or("unknown");
        let label = match unit["group"].as_str() {
            Some(group) if !group.is_empty() => group.to_string(),
            _ => unit["argv"]
                .as_array()
                .map(|argv| {
                    argv.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default(),
        };
        out.push_str(&format!("{status:<14} {label}"));
        if let Some(already) = unit["already_failing"].as_str() {
            out.push_str(&format!(" (already failing on the target: {already})"));
        }
        out.push('\n');
        if matches!(status, "failed" | "errored")
            && let Some(tail) = unit["output_tail"].as_str()
        {
            for line in tail.lines() {
                out.push_str(&format!("    {line}\n"));
            }
        }
    }
    out
}

/// The listing of a finished ticket: its units, the ticket's note and the files no unit covers
/// (each only when there is one), then the verdict and the ticket it belongs to.
pub fn render(ticket: &Value) -> String {
    let id = ticket["ticket"].as_i64().unwrap_or_default();
    let verdict = ticket["verdict"].as_str().unwrap_or("none");
    let mut out = render_units(ticket);
    if let Some(note) = ticket["note"].as_str().filter(|note| !note.is_empty()) {
        out.push_str(&format!("note: {note}\n"));
    }
    if let Some(unclaimed) = ticket["unclaimed"]
        .as_array()
        .filter(|list| !list.is_empty())
    {
        out.push_str("unclaimed (no unit covers these files):\n");
        for path in unclaimed.iter().filter_map(Value::as_str) {
            out.push_str(&format!("    {path}\n"));
        }
    }
    out.push_str(&format!("verdict: {verdict} (ticket {id})\n"));
    out
}

fn not_verified(reason: &str) -> Outcome {
    Outcome {
        code: EXIT_NOT_VERIFIED,
        stdout: String::new(),
        stderr: format!("not verified: {reason}\n"),
    }
}

/// Exit 3: the ticket exists, its verdict does not. The last units seen go to stdout so the
/// asker still sees where it stood, and the way back to the same ticket is spelt out.
fn no_verdict(ticket: &Value, id: i64, reason: &str) -> Outcome {
    Outcome {
        code: EXIT_NO_VERDICT,
        stdout: render_units(ticket),
        stderr: format!(
            "ticket {id}: no verdict arrived ({reason}). \
             re-run `nucleos-core --verify` to join its units.\n"
        ),
    }
}

/// A done ticket: its listing and the exit code its verdict maps to.
fn finished(ticket: &Value, id: i64) -> Outcome {
    match verdict_code(ticket) {
        Some(code) => Outcome {
            code,
            stdout: render(ticket),
            stderr: if code == EXIT_NOT_VERIFIED {
                "not verified: the daemon ran nothing for this change\n".to_string()
            } else {
                String::new()
            },
        },
        None => no_verdict(
            ticket,
            id,
            "the ticket is done without a verdict this client knows",
        ),
    }
}

/// Asks the daemon to verify `worktree` with cover, follows the ticket for at most `limit` and
/// returns what to print and how to exit. The limit bounds the whole run, submit included.
pub async fn run(client: &DaemonClient, worktree: &str, limit: Duration) -> Outcome {
    let started = Instant::now();
    let mut ticket = match tokio::time::timeout(limit, client.verify_cover(worktree)).await {
        Ok(Ok(ticket)) => ticket,
        Ok(Err(reason)) => return not_verified(&reason),
        // The id is unknown, so a ticket the daemon did queue cannot be joined by number.
        Err(_) => {
            return not_verified(&format!(
                "the daemon did not answer within {}s; a ticket may have been queued",
                limit.as_secs()
            ));
        }
    };
    let Some(id) = ticket["ticket"].as_i64() else {
        return not_verified("the daemon's reply names no ticket");
    };
    loop {
        if ticket["done"].as_bool() == Some(true) {
            return finished(&ticket, id);
        }
        let remaining = limit.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return no_verdict(&ticket, id, "the wait limit was reached");
        }
        let asked = Instant::now();
        match tokio::time::timeout(remaining, client.verify_status(id, true)).await {
            Ok(Ok(next)) => ticket = next,
            Ok(Err(reason)) => {
                return no_verdict(
                    &ticket,
                    id,
                    &format!("its status could not be read: {reason}"),
                );
            }
            Err(_) => return no_verdict(&ticket, id, "the wait limit was reached"),
        }
        if ticket["done"].as_bool() != Some(true) && asked.elapsed() < POLL_FLOOR {
            let rest = POLL_FLOOR.saturating_sub(asked.elapsed());
            tokio::time::sleep(rest.min(limit.saturating_sub(started.elapsed()))).await;
        }
    }
}

#[cfg(test)]
mod tests {
    //! `nucleos-core --verify`, the client half of covered verification (F3-14), tested against
    //! in-process axum servers that answer in the shape of `verify_plan::Ticket`.
    use super::*;
    use crate::daemon_client::DaemonClient;
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const TOKEN: &str = "test-token";
    const WORKTREE: &str = "C:/projects/live/.nucleos/worktrees/job-1";

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    /// Serves `app` on a free loopback port and returns the daemon URL.
    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}")
    }

    /// A daemon that answers every request with one canned status and body.
    async fn refusing_daemon(status: axum::http::StatusCode, body: &'static str) -> String {
        serve(axum::Router::new().fallback(move || async move { (status, body) })).await
    }

    /// A daemon whose `/verify` and `/verify/status` each answer one canned ticket, recording
    /// every body it was sent: `(verify bodies, status bodies)`.
    async fn ticket_daemon(
        submit: Value,
        status: Value,
    ) -> (String, Arc<Mutex<Vec<Value>>>, Arc<Mutex<Vec<Value>>>) {
        let submitted: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let polled: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let (seen_submit, seen_status) = (submitted.clone(), polled.clone());
        let app = axum::Router::new()
            .route(
                "/verify",
                axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                    let (submit, seen) = (submit.clone(), seen_submit.clone());
                    async move {
                        seen.lock().unwrap().push(body);
                        axum::Json(submit)
                    }
                }),
            )
            .route(
                "/verify/status",
                axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                    let (status, seen) = (status.clone(), seen_status.clone());
                    async move {
                        seen.lock().unwrap().push(body);
                        axum::Json(status)
                    }
                }),
            );
        (serve(app).await, submitted, polled)
    }

    /// A daemon that finishes the ticket in the first reply.
    async fn answering(ticket: Value) -> String {
        let (url, _, _) = ticket_daemon(ticket.clone(), ticket).await;
        url
    }

    fn unit(group: Option<&str>, argv: &[&str], status: &str, tail: Option<&str>) -> Value {
        let mut unit = json!({
            "group": group,
            "argv": argv,
            "why": "touched",
            "status": status,
            "duration_ms": 12,
            "exit_code": if status == "passed" { json!(0) } else { json!(1) },
            "skipped_reason": null,
            "run_id": null,
            "cached_from": null,
        });
        if let Some(tail) = tail {
            unit["output_tail"] = json!(tail);
        }
        unit
    }

    fn ticket(id: i64, done: bool, verdict: Option<&str>, units: Vec<Value>) -> Value {
        json!({
            "ticket": id,
            "done": done,
            "verdict": verdict,
            "project_id": "live-project",
            "worktree": WORKTREE,
            "kind": "test",
            "scope": "scope",
            "base": "abc1234",
            "note": null,
            "unclaimed": [],
            "progress": { "total": units.len(), "finished": 0, "queued": 0, "running": [] },
            "units": units,
        })
    }

    fn client(url: String) -> DaemonClient {
        DaemonClient::new(url, TOKEN.to_string())
    }

    /// **One hour unless told otherwise, and the flag outranks the environment.**
    #[test]
    fn the_wait_limit_defaults_to_an_hour_and_the_flag_beats_the_env() {
        assert_eq!(DEFAULT_WAIT, Duration::from_secs(3600));
        assert_eq!(
            wait_limit(&args(&["--verify"]), None),
            Ok(Duration::from_secs(3600))
        );
        assert_eq!(
            wait_limit(&args(&["--verify"]), Some("120")),
            Ok(Duration::from_secs(120)),
            "the env alone sets the limit"
        );
        assert_eq!(
            wait_limit(&args(&["--verify", "--verify-wait", "30"]), Some("120")),
            Ok(Duration::from_secs(30)),
            "the flag, spelt with a space, beats the env"
        );
        assert_eq!(
            wait_limit(&args(&["--verify", "--verify-wait=45"]), Some("120")),
            Ok(Duration::from_secs(45)),
            "the flag, spelt with an equals sign, beats the env"
        );
    }

    /// **Whole positive seconds or nothing.**
    #[test]
    fn the_wait_limit_refuses_what_is_not_whole_positive_seconds() {
        for bad in ["0", "abc", "-5", "1.5", ""] {
            assert!(
                wait_limit(&args(&["--verify", "--verify-wait", bad]), None).is_err(),
                "the flag value {bad:?} must be refused"
            );
        }
        assert!(
            wait_limit(&args(&["--verify", "--verify-wait=0"]), None).is_err(),
            "the equals spelling is held to the same rule"
        );
        assert!(
            wait_limit(&args(&["--verify", "--verify-wait"]), None).is_err(),
            "a flag with no value must be refused, not read as the default"
        );
        assert!(
            wait_limit(&args(&["--verify"]), Some("0")).is_err(),
            "the env is held to the same rule"
        );
        assert!(
            wait_limit(&args(&["--verify"]), Some("soon")).is_err(),
            "a non-integer env must be refused"
        );
    }

    /// **A passed ticket is exit 0, and the screen names every unit.**
    #[tokio::test]
    async fn a_passed_ticket_exits_zero_and_lists_every_unit() {
        let url = answering(ticket(
            41,
            true,
            Some("passed"),
            vec![
                unit(Some("core"), &["cargo", "test"], "passed", None),
                unit(None, &["go", "test", "./..."], "skipped_cached", None),
            ],
        ))
        .await;

        let outcome = run(&client(url), WORKTREE, Duration::from_secs(30)).await;

        assert_eq!(
            outcome.code, EXIT_PASSED,
            "stderr said {:?}",
            outcome.stderr
        );
        assert_eq!(EXIT_PASSED, 0);
        let lines: Vec<&str> = outcome.stdout.lines().collect();
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("passed") && l.contains("core")),
            "a unit with a group is listed by its group: {:?}",
            outcome.stdout
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("skipped_cached") && l.contains("go test ./...")),
            "a unit without a group is listed by its argv: {:?}",
            outcome.stdout
        );
        assert!(
            outcome.stdout.contains("verdict: passed") && outcome.stdout.contains("41"),
            "the verdict and the ticket close the listing: {:?}",
            outcome.stdout
        );
    }

    /// **A finished ticket's listing carries its note and the files no unit covers, before the
    /// verdict line.**
    #[tokio::test]
    async fn a_finished_ticket_prints_the_note_and_the_unclaimed_files() {
        let mut finished = ticket(
            43,
            true,
            Some("passed"),
            vec![unit(Some("core"), &["cargo", "test"], "passed", None)],
        );
        finished["note"] = json!("every group runs: paths no group claims");
        finished["unclaimed"] = json!(["README.md", "docs/notes.md"]);
        let url = answering(finished).await;

        let outcome = run(&client(url), WORKTREE, Duration::from_secs(30)).await;

        assert_eq!(
            outcome.code, EXIT_PASSED,
            "stderr said {:?}",
            outcome.stderr
        );
        let stdout = &outcome.stdout;
        let verdict = stdout
            .find("verdict: passed")
            .unwrap_or_else(|| panic!("no verdict line in {stdout:?}"));
        for needle in [
            "every group runs: paths no group claims",
            "README.md",
            "docs/notes.md",
        ] {
            let at = stdout
                .find(needle)
                .unwrap_or_else(|| panic!("{needle:?} missing from {stdout:?}"));
            assert!(
                at < verdict,
                "{needle:?} must come before the verdict line: {stdout:?}"
            );
        }
    }

    /// **The argument decision is pure: what `main` does is read off [`invocation`] alone.**
    #[test]
    fn the_invocation_is_decided_from_the_arguments_alone() {
        assert_eq!(
            invocation(&args(&["nucleos-core", "--land"]), None),
            Invocation::NotVerify,
            "a call that names no --verify flag is not this command's"
        );
        assert_eq!(
            invocation(&args(&["nucleos-core", "--verify", "--help"]), None),
            Invocation::Help
        );
        assert_eq!(
            invocation(
                &args(&["nucleos-core", "--verify-wait", "30", "--help"]),
                None
            ),
            Invocation::Help,
            "--help wins even beside a bare --verify-* flag"
        );
        assert_eq!(
            invocation(&args(&["nucleos-core", "--verify-wait", "30"]), None),
            Invocation::NeedsVerify
        );
        assert_eq!(
            invocation(&args(&["nucleos-core", "--verify-wait=30"]), None),
            Invocation::NeedsVerify
        );
        assert!(
            matches!(
                invocation(
                    &args(&["nucleos-core", "--verify", "--verify-wait", "0"]),
                    None
                ),
                Invocation::BadWait(_)
            ),
            "a zero wait is refused"
        );
        assert_eq!(
            invocation(&args(&["nucleos-core", "--verify"]), Some("120")),
            Invocation::Run(Duration::from_secs(120))
        );
        assert_eq!(
            invocation(&args(&["nucleos-core", "--verify"]), None),
            Invocation::Run(DEFAULT_WAIT)
        );
    }

    /// **A failed ticket is exit 1, and the red tail is on screen.**
    #[tokio::test]
    async fn a_failed_ticket_exits_one_and_prints_the_red_tail() {
        let url = answering(ticket(
            42,
            true,
            Some("failed"),
            vec![
                unit(Some("shell"), &["npm", "test"], "passed", None),
                unit(
                    Some("core"),
                    &["cargo", "test"],
                    "failed",
                    Some("assertion failed: left == right\nthread 'x' panicked"),
                ),
            ],
        ))
        .await;

        let outcome = run(&client(url), WORKTREE, Duration::from_secs(30)).await;

        assert_eq!(
            outcome.code, EXIT_FAILED,
            "stderr said {:?}",
            outcome.stderr
        );
        assert_eq!(EXIT_FAILED, 1);
        assert!(
            outcome.stdout.contains("assertion failed: left == right")
                && outcome.stdout.contains("thread 'x' panicked"),
            "every line of the red unit's tail is printed: {:?}",
            outcome.stdout
        );
        assert!(
            outcome.stdout.contains("verdict: failed"),
            "the verdict is printed: {:?}",
            outcome.stdout
        );
    }

    /// **A refusal is "not verified", and it says what the daemon said.**
    #[tokio::test]
    async fn a_refused_request_is_not_verified_and_says_why() {
        let url = refusing_daemon(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "cover needs a base: the target has no green commit",
        )
        .await;

        let outcome = run(&client(url), WORKTREE, Duration::from_secs(30)).await;

        assert_eq!(outcome.code, EXIT_NOT_VERIFIED);
        assert_eq!(EXIT_NOT_VERIFIED, 4);
        assert!(
            outcome
                .stderr
                .contains("cover needs a base: the target has no green commit"),
            "stderr carries the daemon's own words: {:?}",
            outcome.stderr
        );
    }

    /// **No daemon is "not verified" too, and nothing starts one.**
    #[tokio::test]
    async fn an_unreachable_daemon_is_not_verified() {
        // Bind, note the address, drop: the port is closed by the time the client connects.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);

        let outcome = run(
            &client(format!("http://{address}")),
            WORKTREE,
            Duration::from_secs(30),
        )
        .await;

        assert_eq!(
            outcome.code, EXIT_NOT_VERIFIED,
            "stderr: {:?}",
            outcome.stderr
        );
        assert!(
            outcome.stderr.contains("not verified"),
            "stderr says so: {:?}",
            outcome.stderr
        );
    }

    /// **The first reply is not the verdict: the ticket is followed, and cover travels.**
    ///
    /// The assertion is on the BODIES that travelled. The exit code would be 0 with `cover`
    /// dropped on the floor, and a client that stops asking for covered verification is the
    /// failure this command exists to end.
    #[tokio::test]
    async fn an_unfinished_ticket_is_followed_to_its_verdict_with_cover_on_the_wire() {
        let running = unit(Some("core"), &["cargo", "test"], "running", None);
        let finished = unit(Some("core"), &["cargo", "test"], "passed", None);
        let (url, submitted, polled) = ticket_daemon(
            ticket(77, false, None, vec![running]),
            ticket(77, true, Some("passed"), vec![finished]),
        )
        .await;

        let outcome = run(&client(url), WORKTREE, Duration::from_secs(30)).await;

        assert_eq!(
            outcome.code, EXIT_PASSED,
            "stderr said {:?}",
            outcome.stderr
        );
        assert_eq!(
            *submitted.lock().unwrap(),
            vec![json!({
                "kind": "test",
                "scope": "scope",
                "worktree": WORKTREE,
                "cover": true,
                "wait": false,
            })],
            "the one submission is a covered scope test of the given worktree, and the client \
             sends no files and no base"
        );
        assert_eq!(
            *polled.lock().unwrap(),
            vec![json!({ "ticket": 77, "wait": true })],
            "the ticket is followed through /verify/status, holding the line"
        );
        assert!(
            outcome.stdout.contains("verdict: passed"),
            "the verdict comes from the status reply: {:?}",
            outcome.stdout
        );
    }

    /// **Out of time with the ticket still running is exit 3, and the ticket is named.**
    #[tokio::test]
    async fn no_verdict_by_the_limit_exits_three_and_names_the_ticket() {
        let pending = ticket(
            91,
            false,
            None,
            vec![unit(Some("core"), &["cargo", "test"], "running", None)],
        );
        let (url, _, _) = ticket_daemon(pending.clone(), pending).await;

        let outcome = run(&client(url), WORKTREE, Duration::from_secs(1)).await;

        assert_eq!(
            outcome.code, EXIT_NO_VERDICT,
            "stdout said {:?}",
            outcome.stdout
        );
        assert_eq!(EXIT_NO_VERDICT, 3);
        let said = format!("{}{}", outcome.stdout, outcome.stderr);
        assert!(said.contains("91"), "the ticket id is named: {said:?}");
        assert!(
            said.contains("re-run"),
            "the way to rejoin it is named: {said:?}"
        );
    }

    /// **A status call that fails does not lose the ticket.**
    #[tokio::test]
    async fn a_lost_status_reply_exits_three_and_names_the_ticket() {
        let app = axum::Router::new()
            .route(
                "/verify",
                axum::routing::post(|| async {
                    axum::Json(ticket(
                        57,
                        false,
                        None,
                        vec![unit(Some("core"), &["cargo", "test"], "running", None)],
                    ))
                }),
            )
            .route(
                "/verify/status",
                axum::routing::post(|| async {
                    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom")
                }),
            );
        let url = serve(app).await;

        let outcome = run(&client(url), WORKTREE, Duration::from_secs(30)).await;

        assert_eq!(
            outcome.code, EXIT_NO_VERDICT,
            "stdout said {:?}",
            outcome.stdout
        );
        let said = format!("{}{}", outcome.stdout, outcome.stderr);
        assert!(said.contains("57"), "the ticket id is named: {said:?}");
    }

    /// **A verdict that covered nothing is not a pass.**
    #[tokio::test]
    async fn nothing_ran_is_not_verified() {
        let url = answering(ticket(63, true, Some("nothing_ran"), vec![])).await;

        let outcome = run(&client(url), WORKTREE, Duration::from_secs(30)).await;

        assert_eq!(
            outcome.code, EXIT_NOT_VERIFIED,
            "stdout said {:?}",
            outcome.stdout
        );
        assert!(
            outcome.stderr.contains("not verified"),
            "stderr says so: {:?}",
            outcome.stderr
        );
    }
}
