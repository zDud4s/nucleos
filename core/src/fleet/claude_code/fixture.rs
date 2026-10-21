//! Test fixtures: jsonl lines shaped like Claude Code's, and (below, added later) a tempdir home.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use super::session::{Proc, ProcessProbe};

/// A probe whose answers the test sets; an unknown pid is dead.
#[derive(Default)]
pub struct FakeProbe(Mutex<HashMap<u32, Proc>>);

impl FakeProbe {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, pid: u32, proc: Proc) {
        self.0.lock().unwrap().insert(pid, proc);
    }

    /// Alive, with the start token the session file will carry.
    pub fn alive(&self, pid: u32, token: &str) {
        self.set(pid, Proc::Alive { start_token: Some(token.to_owned()) });
    }
}

impl ProcessProbe for FakeProbe {
    fn probe(&self, pid: u32) -> Proc {
        self.0.lock().unwrap().get(&pid).cloned().unwrap_or(Proc::Dead)
    }
}

pub const SESSION: &str = "11111111-1111-4111-8111-111111111111";
/// The project directory name Claude Code derives from `c:\work\proj`.
pub const PROJ: &str = "c--work-proj";
pub const TS: &str = "2026-10-20T10:00:00.000Z";

pub fn user_prompt(ts: &str, text: &str) -> String {
    json!({"type":"user","timestamp":ts,"message":{"role":"user","content":text}}).to_string()
}

/// An injected / companion user record that is not something the owner typed.
pub fn meta_user(ts: &str, text: &str) -> String {
    json!({"type":"user","isMeta":true,"timestamp":ts,"message":{"role":"user","content":text}})
        .to_string()
}

/// One assistant record carrying ONE content block, as Claude Code writes them.
/// `usage` is `(input, cache_creation, cache_read)`.
pub fn assistant(ts: &str, stop: Option<&str>, block: Value, usage: Option<(i64, i64, i64)>) -> String {
    let mut message = json!({
        "model": "claude-test-1", "role": "assistant", "content": [block], "stop_reason": stop
    });
    if let Some((input, created, read)) = usage {
        message["usage"] = json!({
            "input_tokens": input, "cache_creation_input_tokens": created,
            "cache_read_input_tokens": read, "output_tokens": 5
        });
    }
    json!({"type":"assistant","timestamp":ts,"message":message}).to_string()
}

pub fn text_block(text: &str) -> Value {
    json!({"type":"text","text":text})
}

pub fn thinking_block() -> Value {
    json!({"type":"thinking","thinking":"","signature":"sig"})
}

pub fn tool_block(id: &str, name: &str, input: Value) -> Value {
    json!({"type":"tool_use","id":id,"name":name,"input":input})
}

pub fn tool_result(ts: &str, id: &str, body: &str, tool_use_result: Value) -> String {
    json!({
        "type":"user","timestamp":ts,
        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":body}]},
        "toolUseResult": tool_use_result
    })
    .to_string()
}

pub fn tool_error(ts: &str, id: &str, body: &str) -> String {
    json!({
        "type":"user","timestamp":ts,
        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":body,"is_error":true}]}
    })
    .to_string()
}

pub fn cost_state(usd: f64) -> String {
    json!({"type":"cost-state","totalCostUSD":usd}).to_string()
}

/// A finished turn: prompt, then a final answer (usage 10 + 20 + 300 = 330 context tokens).
pub fn closed_turn() -> Vec<String> {
    vec![
        user_prompt(TS, "do the thing"),
        assistant(TS, Some("end_turn"), text_block("all done"), Some((10, 20, 300))),
    ]
}

/// A turn cut off with a tool call that never got its result.
pub fn open_turn() -> Vec<String> {
    vec![
        user_prompt(TS, "do the thing"),
        assistant(
            TS,
            Some("tool_use"),
            tool_block("toolu_1", "Bash", json!({"command":"cargo test"})),
            Some((10, 20, 300)),
        ),
    ]
}

/// A fake `~` holding a `.claude` tree.
pub struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    pub fn new() -> Self {
        Self { dir: tempfile::tempdir().expect("a tempdir") }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn claude(&self) -> PathBuf {
        self.dir.path().join(".claude")
    }

    pub fn project_dir(&self) -> PathBuf {
        self.claude().join("projects").join(PROJ)
    }

    /// `sessions/<pid>.json`, shaped like the real one (cwd `c:\work\proj`). `startedAt` is its own
    /// constant (Task 10 asserts it); `updatedAt` is a clearly OLD one (Sept 2020), so the retention
    /// tests depend on the transcript's mtime only and pass whatever the wall clock says.
    pub fn session_file(&self, pid: u32, session_id: &str, status: &str, proc_start: &str) -> PathBuf {
        let dir = self.claude().join("sessions");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{pid}.json"));
        let body = json!({
            "pid": pid, "sessionId": session_id, "cwd": "c:\\work\\proj",
            "startedAt": 1_792_523_514_511_i64, "procStart": proc_start, "version": "2.1.304",
            "kind": "interactive", "entrypoint": "cli", "name": "proj-1", "status": status,
            "updatedAt": 1_600_000_000_000_i64
        });
        fs::write(&path, body.to_string()).unwrap();
        path
    }

    fn write_lines(path: &Path, lines: &[String]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = lines.join("\n");
        if !lines.is_empty() {
            text.push('\n');
        }
        fs::write(path, text).unwrap();
    }

    /// `projects/<proj>/<session>.jsonl`.
    pub fn transcript(&self, session_id: &str, lines: &[String]) -> PathBuf {
        let path = self.project_dir().join(format!("{session_id}.jsonl"));
        Self::write_lines(&path, lines);
        path
    }

    pub fn subagents_dir(&self, session_id: &str) -> PathBuf {
        self.project_dir().join(session_id).join("subagents")
    }

    /// `agent-<id>.meta.json` + `agent-<id>.jsonl`; returns the jsonl path.
    pub fn subagent(&self, session_id: &str, agent: &str, meta: Value, lines: &[String]) -> PathBuf {
        let dir = self.subagents_dir(session_id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("agent-{agent}.meta.json")), meta.to_string()).unwrap();
        let path = dir.join(format!("agent-{agent}.jsonl"));
        Self::write_lines(&path, lines);
        path
    }

    pub fn meta_path(&self, session_id: &str, agent: &str) -> PathBuf {
        self.subagents_dir(session_id).join(format!("agent-{agent}.meta.json"))
    }
}

/// Appends raw text (no newline added) to an existing file.
pub fn append(path: &Path, text: &str) {
    use std::io::Write;
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

/// Moves a file's mtime into the past.
pub fn age(path: &Path, minutes: u64) {
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(minutes * 60)).unwrap();
}
