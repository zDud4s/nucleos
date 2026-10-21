//! The small files Claude Code keeps beside the transcripts, and "is that process still there".
//! Third-party, undocumented formats: every field but the identity ones is optional.

// This is the file's FINAL production `use` block (Tasks 7-8 add none: the Windows and unix probes
// use function-local `use` / fully qualified paths).
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::fleet::model::Timestamp;

/// `~/.claude/sessions/<pid>.json`.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionFile {
    pub pid: u32,
    pub session_id: String,
    pub cwd: Option<String>,
    pub started_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    /// The process creation time as Claude Code wrote it (a Windows FILETIME, decimal).
    pub proc_start: Option<String>,
    pub name: Option<String>,
    /// `busy | idle | waiting` as seen; kept as written.
    pub status: Option<String>,
    pub kind: Option<String>,
    pub version: Option<String>,
}

/// `Err` carries a STATIC cause, so the caller can log "once per cause".
pub fn parse_session(text: &str) -> Result<SessionFile, &'static str> {
    let v: Value = serde_json::from_str(text).map_err(|_| "session file is not valid JSON")?;
    let pid = v
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|p| u32::try_from(p).ok())
        .ok_or("session file has no pid")?;
    let session_id = v
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("session file has no sessionId")?
        .to_owned();
    let string = |key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
    let millis = |key: &str| {
        v.get(key)
            .and_then(Value::as_i64)
            .and_then(DateTime::<Utc>::from_timestamp_millis)
    };
    let proc_start = match v.get("procStart") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    Ok(SessionFile {
        pid,
        session_id,
        cwd: string("cwd"),
        started_at: millis("startedAt"),
        updated_at: millis("updatedAt"),
        proc_start,
        name: string("name"),
        status: string("status"),
        kind: string("kind"),
        version: string("version"),
    })
}

/// `agent-<id>.meta.json` beside a subagent transcript.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentMeta {
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub model: Option<String>,
    pub parent_agent_id: Option<String>,
    /// Written in place when the agent ends.
    pub finished: bool,
    /// Written INSTEAD of `finished` when the owner stopped the agent.
    pub stopped_by_user: bool,
}

impl AgentMeta {
    /// The harness's own "ended cleanly" marker (spec §3, children).
    pub fn is_closed(&self) -> bool {
        self.finished || self.stopped_by_user
    }
}

pub fn parse_meta(text: &str) -> Result<AgentMeta, &'static str> {
    let v: Value = serde_json::from_str(text).map_err(|_| "agent meta file is not valid JSON")?;
    let string = |key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
    let flag = |key: &str| v.get(key).and_then(Value::as_bool) == Some(true);
    Ok(AgentMeta {
        agent_type: string("agentType"),
        description: string("description"),
        model: string("model"),
        parent_agent_id: string("parentAgentId"),
        finished: flag("finished"),
        stopped_by_user: flag("stoppedByUser"),
    })
}

/// What the OS says about a pid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Proc {
    Dead,
    /// `start_token` is the process creation time in the same encoding as
    /// `SessionFile::proc_start`, or `None` where it cannot be read or is not compared.
    Alive { start_token: Option<String> },
}

pub trait ProcessProbe: Send + Sync {
    fn probe(&self, pid: u32) -> Proc;
}

/// Spec §4: a registry entry is live only if its PID is alive AND the process is the one that
/// wrote it (`procStart` equal), so a recycled pid does not resurrect a dead session.
pub fn is_same_process(session: &SessionFile, probe: &dyn ProcessProbe) -> bool {
    match probe.probe(session.pid) {
        Proc::Dead => false,
        Proc::Alive { start_token } => match (&session.proc_start, start_token) {
            (Some(wrote), Some(now)) => *wrote == now,
            // Nothing to compare: the pid alone decides.
            _ => true,
        },
    }
}

pub struct OsProbe;

impl ProcessProbe for OsProbe {
    fn probe(&self, pid: u32) -> Proc {
        os_probe(pid)
    }
}

/// Windows: `procStart` is the creation FILETIME (verified against `Get-Process ... StartTime`).
#[cfg(windows)]
fn os_probe(pid: u32) -> Proc {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const STILL_ACTIVE: u32 = 259;
    const ERROR_ACCESS_DENIED: i32 = 5;
    // SAFETY: plain Win32 calls on a handle opened and closed in this block; the out-pointers
    // are valid locals.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            // Access denied means the process exists but is not ours to inspect.
            return if std::io::Error::last_os_error().raw_os_error() == Some(ERROR_ACCESS_DENIED) {
                Proc::Alive { start_token: None }
            } else {
                Proc::Dead
            };
        }
        let mut code = 0u32;
        let running = GetExitCodeProcess(handle, &mut code) != 0 && code == STILL_ACTIVE;
        let (mut created, mut exited, mut kernel, mut user) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let timed = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0;
        CloseHandle(handle);
        if !running {
            return Proc::Dead;
        }
        let start_token = timed.then(|| {
            ((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime)).to_string()
        });
        Proc::Alive { start_token }
    }
}

/// Unix: existence only. How `procStart` is encoded there is unverified (VERIFY on a Linux or
/// macOS machine before adding a start-time comparison), and a false "dead" is worse than the
/// rare pid reuse.
#[cfg(unix)]
fn os_probe(pid: u32) -> Proc {
    let Ok(raw) = libc::pid_t::try_from(pid) else {
        return Proc::Dead;
    };
    if raw <= 0 {
        return Proc::Dead;
    }
    // SAFETY: signal 0 performs the existence and permission checks and delivers nothing.
    let rc = unsafe { libc::kill(raw, 0) };
    if rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) {
        Proc::Alive { start_token: None }
    } else {
        Proc::Dead
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::claude_code::fixture::FakeProbe;

    const FULL: &str = r#"{"pid":19996,"sessionId":"cd303e3a-0","cwd":"c:\\Projects\\nucleos",
        "startedAt":1792523514511,"procStart":"134369971128448809","version":"2.1.304",
        "kind":"interactive","entrypoint":"claude-vscode","name":"nucleos-12","status":"idle",
        "updatedAt":1792524140337,"waitingFor":"input needed"}"#;

    #[test]
    fn a_full_session_file_parses() {
        let s = parse_session(FULL).unwrap();
        assert_eq!(s.pid, 19996);
        assert_eq!(s.session_id, "cd303e3a-0");
        assert_eq!(s.cwd.as_deref(), Some("c:\\Projects\\nucleos"));
        assert_eq!(s.proc_start.as_deref(), Some("134369971128448809"));
        assert_eq!(s.name.as_deref(), Some("nucleos-12"));
        assert_eq!(s.status.as_deref(), Some("idle"));
        assert_eq!(s.kind.as_deref(), Some("interactive"));
        assert_eq!(s.version.as_deref(), Some("2.1.304"));
        assert_eq!(s.started_at.unwrap().timestamp_millis(), 1_792_523_514_511);
        assert_eq!(s.updated_at.unwrap().timestamp_millis(), 1_792_524_140_337);
    }

    #[test]
    fn only_pid_and_session_id_are_required() {
        let s = parse_session(r#"{"pid":7,"sessionId":"abc"}"#).unwrap();
        assert_eq!((s.pid, s.session_id.as_str()), (7, "abc"));
        assert!(s.cwd.is_none() && s.proc_start.is_none() && s.name.is_none() && s.status.is_none());
        assert_eq!(parse_session(r#"{"sessionId":"abc"}"#), Err("session file has no pid"));
        assert_eq!(parse_session(r#"{"pid":7}"#), Err("session file has no sessionId"));
        assert_eq!(parse_session("{\"pid\":7,\"sess"), Err("session file is not valid JSON"));
    }

    #[test]
    fn proc_start_may_be_a_number() {
        let s = parse_session(r#"{"pid":7,"sessionId":"abc","procStart":12345}"#).unwrap();
        assert_eq!(s.proc_start.as_deref(), Some("12345"));
    }

    #[test]
    fn a_meta_file_parses_and_an_empty_one_degrades() {
        let m = parse_meta(
            r#"{"agentType":"Explore","description":"Read specs","model":"sonnet",
                "parentAgentId":"ae69","spawnDepth":2,"finished":true}"#,
        )
        .unwrap();
        assert_eq!(m.agent_type.as_deref(), Some("Explore"));
        assert_eq!(m.description.as_deref(), Some("Read specs"));
        assert_eq!(m.model.as_deref(), Some("sonnet"));
        assert_eq!(m.parent_agent_id.as_deref(), Some("ae69"));
        assert!(m.finished && m.is_closed());

        let empty = parse_meta("{}").unwrap();
        assert_eq!(empty, AgentMeta::default());
        assert!(!empty.is_closed());
        assert_eq!(parse_meta("{\"agentTy"), Err("agent meta file is not valid JSON"));
    }

    #[test]
    fn a_user_stopped_agent_counts_as_closed_without_a_finished_marker() {
        let m = parse_meta(r#"{"agentType":"general-purpose","stoppedByUser":true}"#).unwrap();
        assert!(!m.finished && m.stopped_by_user && m.is_closed());
    }

    fn session(pid: u32, proc_start: Option<&str>) -> SessionFile {
        let mut s = parse_session(&format!(r#"{{"pid":{pid},"sessionId":"abc"}}"#)).unwrap();
        s.proc_start = proc_start.map(str::to_owned);
        s
    }

    #[test]
    fn the_session_file_is_live_only_for_the_process_that_wrote_it() {
        let probe = FakeProbe::new();
        // Dead pid.
        assert!(!is_same_process(&session(1, Some("777")), &probe));
        // Alive, same start time.
        probe.alive(1, "777");
        assert!(is_same_process(&session(1, Some("777")), &probe));
        // Alive, but the pid now belongs to a different process (reuse).
        assert!(!is_same_process(&session(1, Some("999")), &probe));
        // Alive and nothing to compare (unix probe, or a file without procStart): the pid decides.
        probe.set(2, Proc::Alive { start_token: None });
        assert!(is_same_process(&session(2, Some("777")), &probe));
        assert!(is_same_process(&session(1, None), &probe));
    }

    #[test]
    fn the_os_probe_sees_this_process_and_not_an_absurd_pid() {
        let me = OsProbe.probe(std::process::id());
        assert!(matches!(me, Proc::Alive { .. }), "got {me:?}");
        assert_eq!(OsProbe.probe(u32::MAX - 15), Proc::Dead);
    }

    #[cfg(windows)]
    #[test]
    fn on_windows_the_start_token_is_the_creation_filetime() {
        let Proc::Alive { start_token: Some(token) } = OsProbe.probe(std::process::id()) else {
            panic!("this process should be alive with a start token");
        };
        let ticks: u64 = token.parse().expect("a decimal FILETIME");
        // 1970-01-01 in FILETIME ticks: any real process started after it.
        assert!(ticks > 116_444_736_000_000_000);
    }
}
