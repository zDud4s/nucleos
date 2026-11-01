//! `nucleos-core --restart [--force] [--timeout-secs N]`: asks the daemon to restart once running
//! work has settled, and follows the wait.
//!
//! Exit 0 means the restart is under way (the daemon stopped answering or another pid answers; with
//! `--force` that is watched for up to 15s), exit 4 that the drain ended, or the forced grace
//! passed, with the same daemon still up (the runs it was waiting
//! on are listed on stderr; a reading without a pid is exit 1), exit 1 that the
//! daemon could not be asked, exit 2 a bad command line. It never starts a daemon. `requester_run`
//! is `NUCLEOS_RUN_ID`, so the run that asks for the restart is not waited on by itself.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::shutdown::{DEFAULT_DRAIN_TIMEOUT, MAX_DRAIN_TIMEOUT};

pub const USAGE: &str = "usage: nucleos-core --restart [--force] [--timeout-secs N]\n\
  restarts the daemon once no run is working (a run parked on a question does not count).\n\
  --force           restart now, without waiting for running work\n\
  --timeout-secs N  how long to wait for running work (default 1800, at most 86400)\n\
  exit 0: the restart is under way   exit 4: the limit passed with work still running,\n\
  or a forced restart left the same daemon up";

/// The limit passed with work still running.
pub const EXIT_LIMIT_PASSED: i32 = 4;
/// A bad command line.
pub const EXIT_USAGE: i32 = 2;
/// How often the follow looks at the readiness.
pub const POLL: Duration = Duration::from_secs(2);
/// How long a forced restart is watched for the daemon to leave (the daemon's exit watchdog is 10s).
pub const FORCE_GRACE: Duration = Duration::from_secs(15);
/// Slack past the daemon's own limit before the client gives up on seeing it end.
const GRACE: Duration = Duration::from_secs(60);

/// What `--restart` asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub force: bool,
    pub timeout: Duration,
    pub requester_run: Option<i64>,
}

impl Call {
    /// The query of `POST /system/restart`.
    pub fn restart_query(&self) -> String {
        let mut query = format!("timeout_secs={}", self.timeout.as_secs());
        if self.force {
            query.push_str("&force=true");
        }
        if let Some(run) = self.requester_run {
            query.push_str(&format!("&requester_run={run}"));
        }
        query
    }

    /// The query of `GET /system/update-readiness`: only the requester matters to it.
    pub fn readiness_query(&self) -> String {
        self.requester_run
            .map(|run| format!("?requester_run={run}"))
            .unwrap_or_default()
    }
}

/// PURE: `None` when `--restart` is not among the arguments (`args[0]` is the program), otherwise
/// the call or why it is refused. `env_run_id` is `NUCLEOS_RUN_ID`; one that is not a number is
/// ignored.
pub fn invocation(args: &[String], env_run_id: Option<&str>) -> Option<Result<Call, String>> {
    if !args.iter().skip(1).any(|arg| arg == "--restart") {
        return None;
    }
    let mut call = Call {
        force: false,
        timeout: DEFAULT_DRAIN_TIMEOUT,
        requester_run: env_run_id.and_then(|id| id.trim().parse().ok()),
    };
    let mut rest = args.iter().skip(1);
    while let Some(arg) = rest.next() {
        let seconds = match arg.as_str() {
            "--restart" => continue,
            "--force" => {
                call.force = true;
                continue;
            }
            "--timeout-secs" => match rest.next() {
                Some(value) => value.as_str(),
                None => return Some(Err("--timeout-secs needs a number of seconds".to_owned())),
            },
            other => match other.strip_prefix("--timeout-secs=") {
                Some(value) => value,
                None => return Some(Err(format!("unknown argument with --restart: {other}"))),
            },
        };
        match seconds.parse::<u64>() {
            Ok(seconds) => call.timeout = Duration::from_secs(seconds).min(MAX_DRAIN_TIMEOUT),
            Err(_) => return Some(Err(format!("--timeout-secs is not a number: {seconds}"))),
        }
    }
    Some(Ok(call))
}

/// What to print and how to exit.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// One reading of the daemon's readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub draining: bool,
    /// The answering daemon's process id.
    pub pid: u32,
    /// `(run_id, chat_id)` of each run the restart is waiting on.
    pub waiting_on: Vec<(i64, Option<String>)>,
}

impl Reading {
    /// `Err` names what is missing: a body that is not the readiness, or one without a pid.
    pub fn from_json(value: &Value) -> Result<Self, String> {
        if !value.is_object() {
            return Err("the readiness body could not be parsed as JSON".to_owned());
        }
        let pid = value["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| "the readiness has no pid".to_owned())?;
        let waiting_on = value["waiting_on"]
            .as_array()
            .map(|runs| {
                runs.iter()
                    .filter_map(|run| {
                        Some((
                            run["run_id"].as_i64()?,
                            run["chat_id"].as_str().map(str::to_owned),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            draining: value["draining"].as_bool().unwrap_or(false),
            pid,
            waiting_on,
        })
    }
}

/// What a reading means for the follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Wait,
    /// A different daemon answers: the restart went through and this is the new one.
    Restarting,
    /// The same daemon, no longer draining: the drain ended without a restart (its wait timed out).
    DrainEnded,
}

/// PURE: `started_pid` is the pid of the daemon that was asked. A daemon that stopped answering is
/// judged by the caller as under way.
pub fn judge(reading: &Reading, started_pid: u32) -> Step {
    if reading.pid != started_pid {
        Step::Restarting
    } else if reading.draining {
        Step::Wait
    } else {
        Step::DrainEnded
    }
}

fn limit_passed(last: &Reading, call: &Call) -> Outcome {
    let mut stderr = if last.waiting_on.is_empty() {
        format!(
            "not restarted: the drain ended without a restart after {}s; the daemon carries on\n",
            call.timeout.as_secs()
        )
    } else {
        format!(
            "not restarted: {} run(s) still working after {}s; the daemon carries on\n",
            last.waiting_on.len(),
            call.timeout.as_secs()
        )
    };
    for (run, chat) in &last.waiting_on {
        match chat {
            Some(chat) => stderr.push_str(&format!("  waiting_on: run {run} (chat {chat})\n")),
            None => stderr.push_str(&format!("  waiting_on: run {run}\n")),
        }
    }
    Outcome {
        code: EXIT_LIMIT_PASSED,
        stdout: String::new(),
        stderr,
    }
}

fn restarting() -> Outcome {
    Outcome {
        code: 0,
        stdout: "restart under way\n".to_owned(),
        stderr: String::new(),
    }
}

fn failed(reason: String) -> Outcome {
    Outcome {
        code: 1,
        stdout: String::new(),
        stderr: format!("not restarted: {reason}\n"),
    }
}

/// A forced restart answers 202 before the relauncher is spawned, so the answer proves nothing:
/// watch for up to `grace` until the daemon stops answering or another pid does. The same pid still
/// answering at the end means the restart did not happen (exit 4).
async fn follow_forced(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
    call: &Call,
    poll: Duration,
    grace: Duration,
    started_pid: u32,
) -> Outcome {
    let deadline = Instant::now() + grace;
    loop {
        tokio::time::sleep(poll).await;
        let response = http
            .get(format!(
                "{base_url}/system/update-readiness{}",
                call.readiness_query()
            ))
            .bearer_auth(token)
            .send()
            .await;
        match response {
            Err(_) => return restarting(),
            Ok(response) if response.status().is_success() => {
                let body = response.json().await.unwrap_or(Value::Null);
                if let Ok(reading) = Reading::from_json(&body)
                    && judge(&reading, started_pid) == Step::Restarting
                {
                    return restarting();
                }
            }
            Ok(_) => {}
        }
        if Instant::now() >= deadline {
            return Outcome {
                code: EXIT_LIMIT_PASSED,
                stdout: String::new(),
                stderr: format!(
                    "not restarted: the daemon is still answering {}s after the forced request; it carries on\n",
                    grace.as_secs()
                ),
            };
        }
    }
}

/// Asks the daemon at `base_url` to restart and follows it every `poll` until it stops answering
/// (under way), stops draining (see [`judge`]) or the limit plus a minute passes. A forced request
/// is watched for `force_grace` instead (see [`follow_forced`]).
pub async fn run(
    base_url: &str,
    token: &str,
    call: &Call,
    poll: Duration,
    force_grace: Duration,
) -> Outcome {
    let http = reqwest::Client::new();
    let asked = http
        .post(format!(
            "{base_url}/system/restart?{}",
            call.restart_query()
        ))
        .bearer_auth(token)
        .send()
        .await;
    let first = match asked {
        Err(error) => return failed(format!("the daemon is not reachable: {error}")),
        Ok(response) if !response.status().is_success() => {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return failed(format!("the daemon refused ({status}): {text}"));
        }
        Ok(response) => match Reading::from_json(&response.json().await.unwrap_or(Value::Null)) {
            Ok(reading) => reading,
            Err(reason) => return failed(format!("the restart answer: {reason}")),
        },
    };
    let started_pid = first.pid;
    if call.force {
        return follow_forced(&http, base_url, token, call, poll, force_grace, started_pid).await;
    }
    let deadline = Instant::now() + call.timeout + GRACE;
    let mut last = first;
    loop {
        if Instant::now() >= deadline {
            return limit_passed(&last, call);
        }
        tokio::time::sleep(poll).await;
        let response = http
            .get(format!(
                "{base_url}/system/update-readiness{}",
                call.readiness_query()
            ))
            .bearer_auth(token)
            .send()
            .await;
        let reading = match response {
            // The daemon went away: the restart is under way.
            Err(_) => return restarting(),
            Ok(response) if !response.status().is_success() => {
                return failed(format!("the readiness was refused ({})", response.status()));
            }
            Ok(response) => {
                match Reading::from_json(&response.json().await.unwrap_or(Value::Null)) {
                    Ok(reading) => reading,
                    Err(reason) => return failed(format!("the readiness answer: {reason}")),
                }
            }
        };
        match judge(&reading, started_pid) {
            Step::Wait => last = reading,
            Step::Restarting => return restarting(),
            Step::DrainEnded => return limit_passed(&reading, call),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn restart_parses_its_flags() {
        assert!(invocation(&words("nucleos-core"), None).is_none());
        assert!(invocation(&words("nucleos-core --restart"), None).is_some());
        // The program name is not an argument.
        assert!(invocation(&words("--restart"), None).is_none());

        let plain = invocation(&words("nucleos-core --restart"), None)
            .unwrap()
            .unwrap();
        assert_eq!(
            plain,
            Call {
                force: false,
                timeout: DEFAULT_DRAIN_TIMEOUT,
                requester_run: None
            }
        );

        let forced = invocation(&words("nucleos-core --restart --force"), None)
            .unwrap()
            .unwrap();
        assert!(forced.force);

        let spaced = invocation(&words("nucleos-core --restart --timeout-secs 90"), None)
            .unwrap()
            .unwrap();
        assert_eq!(spaced.timeout, Duration::from_secs(90));
        let joined = invocation(&words("nucleos-core --timeout-secs=30 --restart"), None)
            .unwrap()
            .unwrap();
        assert_eq!(joined.timeout, Duration::from_secs(30));

        let capped = invocation(&words("nucleos-core --restart --timeout-secs 999999"), None)
            .unwrap()
            .unwrap();
        assert_eq!(capped.timeout, MAX_DRAIN_TIMEOUT);

        for bad in [
            "nucleos-core --restart --timeout-secs",
            "nucleos-core --restart --timeout-secs soon",
            "nucleos-core --restart --frobnicate",
        ] {
            assert!(invocation(&words(bad), None).unwrap().is_err(), "{bad}");
        }
    }

    #[test]
    fn restart_sends_the_run_id_from_its_env() {
        let call = invocation(&words("nucleos-core --restart --force"), Some("42"))
            .unwrap()
            .unwrap();
        assert_eq!(call.requester_run, Some(42));
        let query = call.restart_query();
        assert!(query.contains("requester_run=42"), "{query}");
        assert!(query.contains("force=true"), "{query}");
        assert!(query.contains("timeout_secs=1800"), "{query}");
        assert_eq!(call.readiness_query(), "?requester_run=42");

        for env in [None, Some(""), Some("not-a-run")] {
            let call = invocation(&words("nucleos-core --restart"), env)
                .unwrap()
                .unwrap();
            assert_eq!(call.requester_run, None, "{env:?}");
            assert!(!call.restart_query().contains("requester_run"));
            assert_eq!(call.readiness_query(), "");
        }
    }

    #[test]
    fn restart_follows_a_wait_to_its_end() {
        let reading = |pid: u32, draining: bool, blocked: bool| Reading {
            draining,
            pid,
            waiting_on: if blocked { vec![(7, None)] } else { vec![] },
        };
        assert_eq!(judge(&reading(10, true, true), 10), Step::Wait);
        assert_eq!(judge(&reading(10, true, false), 10), Step::Wait);
        assert_eq!(judge(&reading(10, false, true), 10), Step::DrainEnded);
    }

    #[test]
    fn restart_is_not_reported_when_the_drain_ended_with_the_daemon_up() {
        let up = Reading {
            draining: false,
            pid: 10,
            waiting_on: vec![],
        };
        assert_eq!(judge(&up, 10), Step::DrainEnded);
        let call = Call {
            force: false,
            timeout: Duration::from_secs(5),
            requester_run: None,
        };
        let outcome = limit_passed(&up, &call);
        assert_eq!(outcome.code, EXIT_LIMIT_PASSED);
        assert!(outcome.stderr.contains("drain ended"), "{outcome:?}");
    }

    #[test]
    fn restart_seen_through_a_new_pid_within_one_poll() {
        // The new daemon is not draining and has no runs: only the pid tells it from the old one.
        let new_daemon = Reading {
            draining: false,
            pid: 11,
            waiting_on: vec![],
        };
        assert_eq!(judge(&new_daemon, 10), Step::Restarting);
    }

    #[test]
    fn a_reading_without_a_pid_or_a_body_is_refused_by_name() {
        let no_pid = Reading::from_json(&serde_json::json!({ "draining": true }));
        assert!(no_pid.unwrap_err().contains("pid"));
        let no_body = Reading::from_json(&Value::Null);
        assert!(no_body.unwrap_err().contains("parsed"));
        assert!(Reading::from_json(&serde_json::json!({ "pid": 3 })).is_ok());
    }

    /// A daemon whose drain timed out answers `draining: false` with the runs still live: exit 4,
    /// and the runs are named on stderr.
    #[tokio::test]
    async fn restart_exits_4_with_waiting_on_when_the_limit_passes() {
        use axum::routing::{get, post};
        let waiting = serde_json::json!({
            "draining": true,
            "pid": 100,
            "waiting_on": [{ "run_id": 5, "chat_id": "chat-a" }]
        });
        let timed_out = serde_json::json!({
            "draining": false,
            "pid": 100,
            "waiting_on": [{ "run_id": 5, "chat_id": "chat-a" }]
        });
        let app = axum::Router::new()
            .route(
                "/system/restart",
                post(move || {
                    let waiting = waiting.clone();
                    async move { (axum::http::StatusCode::ACCEPTED, axum::Json(waiting)) }
                }),
            )
            .route(
                "/system/update-readiness",
                get(move || {
                    let timed_out = timed_out.clone();
                    async move { axum::Json(timed_out) }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let call = Call {
            force: false,
            timeout: Duration::from_secs(5),
            requester_run: None,
        };
        let outcome = run(
            &base,
            "token",
            &call,
            Duration::from_millis(5),
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(outcome.code, EXIT_LIMIT_PASSED);
        assert!(
            outcome.stderr.contains("run 5 (chat chat-a)"),
            "{outcome:?}"
        );
    }

    /// Serves a restart answer from pid 100 and a readiness from `readiness_pid`.
    async fn serve_forced(readiness_pid: u32) -> String {
        use axum::routing::{get, post};
        let app = axum::Router::new()
            .route(
                "/system/restart",
                post(|| async {
                    (
                        axum::http::StatusCode::ACCEPTED,
                        axum::Json(serde_json::json!({ "draining": true, "pid": 100 })),
                    )
                }),
            )
            .route(
                "/system/update-readiness",
                get(move || async move {
                    axum::Json(serde_json::json!({ "draining": false, "pid": readiness_pid }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    fn forced() -> Call {
        Call {
            force: true,
            timeout: Duration::from_secs(5),
            requester_run: None,
        }
    }

    /// The 202 comes before the relauncher is spawned: if the same daemon is still answering at the
    /// end of the grace, the restart did not happen.
    #[tokio::test]
    async fn a_forced_restart_that_leaves_the_same_daemon_up_is_not_reported_as_done() {
        let base = serve_forced(100).await;
        let outcome = run(
            &base,
            "token",
            &forced(),
            Duration::from_millis(5),
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(outcome.code, EXIT_LIMIT_PASSED, "{outcome:?}");
        assert!(outcome.stderr.contains("not restarted"), "{outcome:?}");
    }

    #[tokio::test]
    async fn a_forced_restart_seen_through_a_new_pid_exits_0() {
        let base = serve_forced(101).await;
        let outcome = run(
            &base,
            "token",
            &forced(),
            Duration::from_millis(5),
            Duration::from_millis(500),
        )
        .await;
        assert_eq!(outcome.code, 0, "{outcome:?}");
    }

    #[tokio::test]
    async fn restart_without_a_daemon_exits_1() {
        let call = Call {
            force: true,
            timeout: Duration::from_secs(1),
            requester_run: None,
        };
        // Port 1 is never a daemon.
        let outcome = run("http://127.0.0.1:1", "token", &call, POLL, FORCE_GRACE).await;
        assert_eq!(outcome.code, 1);
    }
}
