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

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"{"pid":19996,"sessionId":"cd303e3a-0","cwd":"c:\Projects\nucleos",
        "startedAt":1792523514511,"procStart":"134369971128448809","version":"2.1.304",
        "kind":"interactive","entrypoint":"claude-vscode","name":"nucleos-12","status":"idle",
        "updatedAt":1792524140337,"waitingFor":"input needed"}"#;

    #[test]
    fn a_full_session_file_parses() {
        let s = parse_session(FULL).unwrap();
        assert_eq!(s.pid, 19996);
        assert_eq!(s.session_id, "cd303e3a-0");
        assert_eq!(s.cwd.as_deref(), Some("c:\Projects\nucleos"));
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
}
