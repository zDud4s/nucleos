//! `nucleos-core --restart [--force] [--timeout-secs N]`: asks the daemon to restart once running
//! work has settled, and follows the wait.
//!
//! Exit 0 means the restart is under way (the daemon stopped answering, or `--force`), exit 4 that
//! the limit passed with work still running (the runs are listed on stderr), exit 1 that the
//! daemon could not be asked, exit 2 a bad command line. It never starts a daemon. `requester_run`
//! is `NUCLEOS_RUN_ID`, so the run that asks for the restart is not waited on by itself.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::shutdown::{DEFAULT_DRAIN_TIMEOUT, MAX_DRAIN_TIMEOUT};

pub const USAGE: &str = "usage: nucleos-core --restart [--force] [--timeout-secs N]\n\
  restarts the daemon once no run is working (a run parked on a question does not count).\n\
  --force           restart now, without waiting for running work\n\
  --timeout-secs N  how long to wait for running work (default 1800, at most 86400)\n\
  exit 0: the restart is under way   exit 4: the limit passed with work still running";

/// The limit passed with work still running.
pub const EXIT_LIMIT_PASSED: i32 = 4;
/// A bad command line.
pub const EXIT_USAGE: i32 = 2;
/// How often the follow looks at the readiness.
pub const POLL: Duration = Duration::from_secs(2);
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
    /// `(run_id, chat_id)` of each run the restart is waiting on.
    pub waiting_on: Vec<(i64, Option<String>)>,
}

impl Reading {
    pub fn from_json(value: &Value) -> Self {
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
        Self {
            draining: value["draining"].as_bool().unwrap_or(false),
            waiting_on,
        }
    }
}

/// What a reading means for the follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Wait,
    /// A daemon that is no longer draining and had nothing left to wait on: the restart went through
    /// and this is the new daemon.
    Restarting,
    /// No longer draining while runs were still blocking: the daemon's wait timed out.
    LimitPassed,
}

/// PURE: `was_waiting` is whether the last reading that was still draining had runs blocking.
pub fn judge(reading: &Reading, was_waiting: bool) -> Step {
    match (reading.draining, was_waiting) {
        (true, _) => Step::Wait,
        (false, true) => Step::LimitPassed,
        (false, false) => Step::Restarting,
    }
}

fn limit_passed(last: &Reading, call: &Call) -> Outcome {
    let mut stderr = format!(
        "not restarted: {} run(s) still working after {}s; the daemon carries on\n",
        last.waiting_on.len(),
        call.timeout.as_secs()
    );
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

/// Asks the daemon at `base_url` to restart and, unless forced, follows it every `poll` until it
/// stops answering (under way), stops draining (see [`judge`]) or the limit plus a minute passes.
pub async fn run(base_url: &str, token: &str, call: &Call, poll: Duration) -> Outcome {
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
        Ok(response) => Reading::from_json(&response.json().await.unwrap_or(Value::Null)),
    };
    if call.force {
        return restarting();
    }
    let deadline = Instant::now() + call.timeout + GRACE;
    let mut last = first;
    let mut was_waiting = !last.waiting_on.is_empty();
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
            Ok(response) => Reading::from_json(&response.json().await.unwrap_or(Value::Null)),
        };
        match judge(&reading, was_waiting) {
            Step::Wait => {
                was_waiting = !reading.waiting_on.is_empty();
                last = reading;
            }
            Step::Restarting => return restarting(),
            Step::LimitPassed => return limit_passed(&last, call),
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
        let draining = |blocked: bool| Reading {
            draining: true,
            waiting_on: if blocked { vec![(7, None)] } else { vec![] },
        };
        let door_open = Reading {
            draining: false,
            waiting_on: vec![],
        };
        assert_eq!(judge(&draining(true), true), Step::Wait);
        assert_eq!(judge(&draining(false), true), Step::Wait);
        assert_eq!(judge(&door_open, true), Step::LimitPassed);
        assert_eq!(judge(&door_open, false), Step::Restarting);
    }

    /// A daemon whose drain timed out answers `draining: false` with the runs still live: exit 4,
    /// and the runs are named on stderr.
    #[tokio::test]
    async fn restart_exits_4_with_waiting_on_when_the_limit_passes() {
        use axum::routing::{get, post};
        let waiting = serde_json::json!({
            "draining": true,
            "waiting_on": [{ "run_id": 5, "chat_id": "chat-a" }]
        });
        let timed_out = serde_json::json!({
            "draining": false,
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
        let outcome = run(&base, "token", &call, Duration::from_millis(5)).await;
        assert_eq!(outcome.code, EXIT_LIMIT_PASSED);
        assert!(
            outcome.stderr.contains("run 5 (chat chat-a)"),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn restart_without_a_daemon_exits_1() {
        let call = Call {
            force: true,
            timeout: Duration::from_secs(1),
            requester_run: None,
        };
        // Port 1 is never a daemon.
        let outcome = run("http://127.0.0.1:1", "token", &call, POLL).await;
        assert_eq!(outcome.code, 1);
    }
}
