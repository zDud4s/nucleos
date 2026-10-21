//! Test fixtures: jsonl lines shaped like Claude Code's, and (below, added later) a tempdir home.

use serde_json::{json, Value};

use std::collections::HashMap;
use std::sync::Mutex;

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
