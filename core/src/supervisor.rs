//! `nucleos-core --supervise`: the loop the Windows logon task runs.
//!
//! The decisions (which exit is a failure, how long to pause, when to give up) are
//! `nucleos_core::supervise`'s and are tested there. This file is only the effects around them:
//! spawn the same executable without the flag, wait, ask the policy, sleep, repeat. It is
//! deliberately thin and has no test of its own; what only a real Windows session proves is listed
//! in the handoff of the change that added it.

use nucleos_core::process_tree::KillOnCloseJob;
use nucleos_core::relaunch::{self, SUPERVISED_VAR, SwapOutcome};
use nucleos_core::supervise::{
    Decision, EXIT_PORT_BUSY, EXIT_RESTART_REQUESTED, Policy, SUPERVISE_FLAG, StopReason, Tracker,
};
use nucleos_core::{daemon_client, data_dir, settings_cli};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

/// Where the supervisor writes. A file of its own beside the daemon's logs, because the daemon's
/// rolling appender belongs to the child process and two writers on one rotation would race on the
/// cleanup; stderr too, for a supervisor started from a terminal.
struct Journal {
    file: Option<PathBuf>,
}

impl Journal {
    fn open() -> Self {
        let file = data_dir::default_data_dir().ok().map(|default_dir| {
            let dir = data_dir::data_dir_from(
                std::env::var(daemon_client::DATA_DIR_VAR).ok().as_deref(),
                &default_dir,
            )
            .join("logs");
            let _ = std::fs::create_dir_all(&dir);
            dir.join("nucleos-supervisor.log")
        });
        Self { file }
    }

    fn say(&self, line: &str) {
        let stamped = format!(
            "{} nucleos-supervisor: {line}",
            chrono::Local::now().to_rfc3339()
        );
        if let Some(path) = &self.file
            && let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        {
            let _ = writeln!(file, "{stamped}");
        }
        // Never `eprintln!`: it panics when the console is gone, and the file line is already written.
        let _ = writeln!(std::io::stderr(), "{stamped}");
    }
}

/// Runs until the daemon is stopped on purpose, another daemon owns the port, or the restart budget
/// is spent. The return value is this process's exit code.
pub(super) async fn run() -> i32 {
    let journal = Journal::open();
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            journal.say(&format!("cannot resolve the executable to launch: {e}"));
            return 1;
        }
    };
    // The same command line without the flag; the working directory and environment are inherited.
    let child_args: Vec<std::ffi::OsString> = std::env::args_os()
        .skip(1)
        .filter(|arg| arg != SUPERVISE_FLAG)
        .collect();
    let port = daemon_client::port();
    let policy = Policy::default();
    let mut tracker = Tracker::new(policy);
    let origin = Instant::now();
    loop {
        if settings_cli::daemon_answers_on(port, settings_cli::PROBE_WAIT).await {
            journal.say(&format!(
                "something already answers on 127.0.0.1:{port}; not launching another daemon"
            ));
            return 0;
        }

        // One job per launch, holding the daemon's whole process tree (agents, cargo, gates). The OS
        // closes the handle when this process dies in any way, so a killed supervisor never leaves a
        // daemon nobody watches. It is also dropped below, once the daemon has exited and before
        // the pause: the orphans of a failed daemon must be dead before the restart reconciles.
        let job = KillOnCloseJob::new();
        if job.is_none() {
            journal
                .say("no job object: killing this supervisor alone will leave the daemon running");
        }
        let started = origin.elapsed();
        journal.say(&format!("launching {}", exe.display()));
        let code = match tokio::process::Command::new(&exe)
            .args(&child_args)
            // The task runs under `conhost --headless`, whose stdin handle may be dead, and the
            // daemon would hand that to everything it starts.
            .stdin(std::process::Stdio::null())
            // Tells the daemon that this process relaunches it, so a restart is an exit and not a
            // relauncher of its own (which this job would kill).
            .env(SUPERVISED_VAR, "1")
            .kill_on_drop(true)
            .spawn()
        {
            Ok(mut child) => {
                if let (Some(job), Some(pid)) = (&job, child.id())
                    && !job.adopt(pid)
                {
                    journal.say("could not put the daemon in the job object");
                }
                match child.wait().await {
                    Ok(status) => status.code(),
                    Err(e) => {
                        journal.say(&format!("waiting for the daemon failed: {e}"));
                        None
                    }
                }
            }
            Err(e) => {
                journal.say(&format!("could not launch the daemon: {e}"));
                None
            }
        };
        drop(job);
        let ended = origin.elapsed();
        let ran = ended.saturating_sub(started);
        let shown = code.map_or("none".to_string(), |code| code.to_string());

        match tracker.on_exit(code, started, ended) {
            Decision::Stop(StopReason::Stopped) => {
                journal.say(&format!(
                    "the daemon exited with code {shown} after {}s: a stop, not restarting",
                    ran.as_secs()
                ));
                return 0;
            }
            Decision::Stop(StopReason::PortBusy) => {
                journal.say(&format!(
                    "the daemon exited with code {shown}: something already answers on 127.0.0.1:{port}, not restarting"
                ));
                return EXIT_PORT_BUSY;
            }
            Decision::Restart { after } if code == Some(EXIT_RESTART_REQUESTED) => {
                journal.say(&format!(
                    "restart requested after {}s; relaunching",
                    ran.as_secs()
                ));
                // The staged build, if there is one, goes in before the next launch.
                match relaunch::swap_staged(&exe) {
                    SwapOutcome::NothingStaged => {}
                    SwapOutcome::Swapped => journal.say("swapped in the staged executable"),
                    SwapOutcome::Failed(reason) => journal.say(&format!(
                        "could not swap in the staged executable, relaunching the current one: {reason}"
                    )),
                }
                tokio::time::sleep(after).await;
            }
            Decision::Restart { after } => {
                journal.say(&format!(
                    "the daemon failed with code {shown} after {}s; restart {} of {} in {}s",
                    ran.as_secs(),
                    tracker.restarts_counted(),
                    policy.max_restarts,
                    after.as_secs()
                ));
                tokio::time::sleep(after).await;
            }
            Decision::GiveUp => {
                journal.say(&format!(
                    "the daemon failed with code {shown} after {}s and the budget of {} restarts in {}s is spent; giving up",
                    ran.as_secs(),
                    policy.max_restarts,
                    policy.window.as_secs()
                ));
                return 1;
            }
        }
    }
}
