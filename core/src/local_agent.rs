//! A tool-calling loop for a model running on this machine.
//!
//! The CLI runners get their loop for free: the agent CLI owns the conversation, decides when to
//! call a tool, and hands back a finished answer. Ollama's chat endpoint does not — it answers one
//! exchange at a time, and a turn that reads three things is three round trips somebody has to
//! drive. This module is that somebody.
//!
//! It exists because the alternative to a local turn is not "a slightly worse answer": it is the
//! question and everything it touches leaving the machine. A model with no tools cannot answer
//! "what is running right now", so without a loop the local runner can only be given questions that
//! need no facts, which is nearly none of them.
//!
//! What it is NOT is a general agent harness. There is a hard ceiling on rounds, no recursion, no
//! planning, and no retry of a call that already failed the same way. A small model asked to choose
//! among tools repeats itself, and every guard here exists because that is the failure to expect
//! rather than the exception to handle.

use serde_json::Value;

/// How many times the model may call tools before the turn ends with what it has.
///
/// Not defensive decoration. A 4B model that cannot answer a question tends to call the same tool
/// with the same arguments indefinitely rather than say so, and without a ceiling that is a turn
/// which never ends, holding its chat slot, burning CPU, with a person watching a typing indicator.
/// Eight is enough for "list the projects, then read the run" and several steps more.
pub const MAX_TOOL_ROUNDS: usize = 8;

/// The context window every request in a local turn states explicitly.
///
/// Stated on each request and never inherited from Ollama's factory default, which is the lesson
/// `voice.rs` records at length: Ollama silently truncates a prompt that does not fit, so a default
/// window turns a long conversation into a short one nobody was told about.
///
/// Larger than triage's 8192 because a turn ACCUMULATES. Triage sends one prompt; this sends the
/// system message, the question, and then every tool schema and every tool result again on each
/// round. The number is therefore coupled to `MAX_TOOL_ROUNDS` above: raising the ceiling without
/// raising this trades a turn that gives up for a turn that quietly forgets its first lookup.
pub const TURN_NUM_CTX: usize = 16_384;

/// What a local turn is told it is.
///
/// Explicit in a way a CLI agent's prompt need not be. A hosted agent arrives knowing it is an
/// assistant with tools; a 4B served a bare question answers it from whatever it half-remembers,
/// and the failure looks like confident nonsense about a project it has never read.
pub const SYSTEM_PROMPT: &str = "You are NucleOS, answering its owner in a chat window. \
You can inspect this machine's projects, runs, proposals, budget and version-control queue \
through the tools you have been given, and you can start work with create_run or create_job. \
Call a tool whenever a question is about what is actually happening — never guess a run's status, \
a project's name or a number. Answer in the language the question was asked in. Be brief: this is \
a chat, not a report.";

/// What the loop can reach. Implemented over the MCP tool set in production and faked in tests.
///
/// A trait rather than a concrete type because the loop's logic — rounds, repeats, malformed calls —
/// is what needs testing, and testing it against real tools would mean standing up a daemon to
/// assert on a counter.
#[async_trait::async_trait]
pub trait ToolBox: Send + Sync {
    /// Tool definitions in Ollama's schema, which is OpenAI's: `{type, function:{name,
    /// description, parameters}}`.
    fn schemas(&self) -> Vec<Value>;

    /// Runs one tool and returns its result as the text the model will read. Errors are values
    /// here, not `Err`: a tool that failed is something the model must be told about so it can try
    /// something else, not something that ends the turn.
    async fn call(&self, name: &str, arguments: &Value) -> String;
}

/// Why a turn stopped, for the caller to log. The answer itself is returned either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// The model produced a final answer.
    Answered,
    /// The round ceiling was reached first.
    RoundsExhausted,
    /// The model asked for the same thing that had already failed.
    RepeatedAFailedCall,
}

#[derive(Debug, Clone)]
pub struct Turn {
    pub answer: String,
    pub ending: Ending,
    /// How many tool calls were executed, for the log line that explains a slow turn.
    pub tool_calls: usize,
}

/// What a turn says when the model spent every round on tools and never wrote an answer.
///
/// Said in the first person and without jargon because it is delivered to a person in a chat
/// window, not to an operator in a log. "Round ceiling exceeded" would be accurate and useless.
pub const NO_ANSWER: &str =
    "I could not finish working that out. Try asking for one thing at a time.";

/// Drives one local turn to an answer.
///
/// `system` is separated from `prompt` because a local model needs to be told what it is far more
/// explicitly than a CLI agent does, and folding the two together would put that instruction inside
/// the part a person wrote — where it reads as something they asked for.
pub async fn run_turn(
    chat: &dyn LocalChat,
    tools: &dyn ToolBox,
    system: &str,
    prompt: &str,
) -> std::io::Result<Turn> {
    let schemas = tools.schemas();
    let mut messages = vec![
        serde_json::json!({"role": "system", "content": system}),
        serde_json::json!({"role": "user", "content": prompt}),
    ];
    // Keyed on name AND arguments: asking for the same run twice is a loop, asking for two
    // different runs is work. Only calls that FAILED are remembered — a tool that succeeded and is
    // called again may well be the model checking whether something changed.
    let mut failed: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut executed = 0;

    for _ in 0..MAX_TOOL_ROUNDS {
        let message = chat.exchange(messages.clone(), Some(schemas.clone())).await?;
        let calls = tool_calls(&message);

        if calls.is_empty() {
            return Ok(Turn {
                answer: content_of(&message),
                ending: Ending::Answered,
                tool_calls: executed,
            });
        }

        messages.push(message);
        for (name, arguments) in calls {
            let signature = format!("{name}:{arguments}");
            if failed.contains(&signature) {
                return Ok(Turn {
                    answer: NO_ANSWER.to_string(),
                    ending: Ending::RepeatedAFailedCall,
                    tool_calls: executed,
                });
            }

            let result = tools.call(&name, &arguments).await;
            executed += 1;
            if looks_like_failure(&result) {
                failed.insert(signature);
            }
            messages.push(serde_json::json!({
                "role": "tool",
                "tool_name": name,
                "content": result,
            }));
        }
    }

    Ok(Turn {
        answer: NO_ANSWER.to_string(),
        ending: Ending::RoundsExhausted,
        tool_calls: executed,
    })
}

/// A model on this machine, its tools, and the loop that joins them — assembled once at startup.
///
/// Held in `AppState` as an `Option`: `None` is the ship-dark default and means chat turns are
/// answered the way they always were.
pub struct LocalAssistant {
    chat: Box<dyn LocalChat>,
    tools: Box<dyn ToolBox>,
}

impl LocalAssistant {
    pub fn new(chat: Box<dyn LocalChat>, tools: Box<dyn ToolBox>) -> Self {
        Self { chat, tools }
    }

    pub async fn answer(&self, prompt: &str) -> std::io::Result<Turn> {
        run_turn(&*self.chat, &*self.tools, SYSTEM_PROMPT, prompt).await
    }
}

/// The one exchange the loop needs, so the loop can be tested without an HTTP server.
#[async_trait::async_trait]
pub trait LocalChat: Send + Sync {
    async fn exchange(
        &self,
        messages: Vec<Value>,
        tools: Option<Vec<Value>>,
    ) -> std::io::Result<Value>;
}

/// PURE: the calls an assistant message asks for, as `(name, arguments)`.
///
/// Arguments arrive as an object from Ollama but as a JSON STRING from some models' templates, so
/// both are accepted and normalised. A call whose arguments parse as neither is passed on as an
/// empty object rather than dropped: the tool will reject it, and the model gets told why — which
/// is the behaviour that lets it correct itself.
fn tool_calls(message: &Value) -> Vec<(String, Value)> {
    let Some(calls) = message.get("tool_calls").and_then(Value::as_array) else {
        return Vec::new();
    };

    calls
        .iter()
        .filter_map(|call| {
            let function = call.get("function")?;
            let name = function.get("name")?.as_str()?.to_string();
            let arguments = match function.get("arguments") {
                Some(Value::String(raw)) => {
                    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::json!({}))
                }
                Some(value) => value.clone(),
                None => serde_json::json!({}),
            };
            Some((name, arguments))
        })
        .collect()
}

fn content_of(message: &Value) -> String {
    message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Whether a tool result is one the model should not be allowed to ask for again unchanged.
///
/// The daemon's tools answer errors as `{"error": ...}` (`mcp_tools::error_json`), so this is a
/// shape check and not a search for the word "error" in prose — a run whose output mentions an
/// error is a successful read of a failed run, and re-reading it is legitimate.
fn looks_like_failure(result: &str) -> bool {
    serde_json::from_str::<Value>(result)
        .ok()
        .and_then(|value| value.get("error").cloned())
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Answers a scripted sequence of assistant messages, and records what it was asked.
    struct ScriptedChat {
        replies: Mutex<std::collections::VecDeque<Value>>,
        seen: Mutex<Vec<Vec<Value>>>,
        tools_offered: Mutex<Vec<bool>>,
    }

    impl ScriptedChat {
        fn new(replies: Vec<Value>) -> Self {
            Self {
                replies: Mutex::new(replies.into()),
                seen: Mutex::new(Vec::new()),
                tools_offered: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl LocalChat for ScriptedChat {
        async fn exchange(
            &self,
            messages: Vec<Value>,
            tools: Option<Vec<Value>>,
        ) -> std::io::Result<Value> {
            self.seen.lock().unwrap().push(messages);
            self.tools_offered.lock().unwrap().push(tools.is_some());
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| std::io::Error::other("the script ran out of replies"))
        }
    }

    struct FakeTools {
        answer: &'static str,
        calls: Mutex<Vec<(String, Value)>>,
    }

    impl FakeTools {
        fn answering(answer: &'static str) -> Self {
            Self {
                answer,
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl ToolBox for FakeTools {
        fn schemas(&self) -> Vec<Value> {
            vec![serde_json::json!({
                "type": "function",
                "function": {"name": "get_run", "description": "read a run", "parameters": {}}
            })]
        }

        async fn call(&self, name: &str, arguments: &Value) -> String {
            self.calls
                .lock()
                .unwrap()
                .push((name.to_string(), arguments.clone()));
            self.answer.to_string()
        }
    }

    fn says(text: &str) -> Value {
        serde_json::json!({"role": "assistant", "content": text})
    }

    fn calls(name: &str, arguments: Value) -> Value {
        serde_json::json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{"function": {"name": name, "arguments": arguments}}]
        })
    }

    #[tokio::test]
    async fn an_answer_without_tools_ends_the_turn_in_one_exchange() {
        let chat = ScriptedChat::new(vec![says("three runs are going")]);
        let tools = FakeTools::answering("{}");

        let turn = run_turn(&chat, &tools, "you are nucleos", "what is running?")
            .await
            .unwrap();

        assert_eq!(turn.answer, "three runs are going");
        assert_eq!(turn.ending, Ending::Answered);
        assert_eq!(turn.tool_calls, 0);
        assert_eq!(chat.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_tool_result_is_fed_back_and_the_next_answer_is_returned() {
        let chat = ScriptedChat::new(vec![
            calls("get_run", serde_json::json!({"id": 7})),
            says("run 7 finished"),
        ]);
        let tools = FakeTools::answering(r#"{"status":"completed"}"#);

        let turn = run_turn(&chat, &tools, "system", "how did run 7 go?")
            .await
            .unwrap();

        assert_eq!(turn.answer, "run 7 finished");
        assert_eq!(turn.tool_calls, 1);
        assert_eq!(
            tools.calls.lock().unwrap().as_slice(),
            [("get_run".to_string(), serde_json::json!({"id": 7}))]
        );

        // The second exchange must carry the first assistant message AND the tool result, or the
        // model is answering with no idea what came back.
        let second = &chat.seen.lock().unwrap()[1];
        assert_eq!(second.len(), 4, "expected system, user, assistant, tool");
        assert_eq!(second[3]["role"], "tool");
        assert_eq!(second[3]["content"], r#"{"status":"completed"}"#);
    }

    /// Some templates serialise the arguments object as a JSON string. Both spellings have to reach
    /// the tool as the same thing, or half the models silently pass `{}` to everything.
    #[tokio::test]
    async fn arguments_are_accepted_as_an_object_or_as_a_json_string() {
        for arguments in [
            serde_json::json!({"id": 7}),
            serde_json::json!(r#"{"id": 7}"#),
        ] {
            let chat = ScriptedChat::new(vec![calls("get_run", arguments), says("done")]);
            let tools = FakeTools::answering("{}");

            run_turn(&chat, &tools, "system", "go").await.unwrap();

            assert_eq!(
                tools.calls.lock().unwrap()[0].1,
                serde_json::json!({"id": 7})
            );
        }
    }

    /// The failure this loop is built around: a small model asking for the same broken thing for
    /// ever. One error goes back to it; the same call again ends the turn.
    #[tokio::test]
    async fn repeating_a_call_that_already_failed_ends_the_turn() {
        let chat = ScriptedChat::new(vec![
            calls("get_run", serde_json::json!({"id": 999})),
            calls("get_run", serde_json::json!({"id": 999})),
            says("never reached"),
        ]);
        let tools = FakeTools::answering(r#"{"error":"unknown run"}"#);

        let turn = run_turn(&chat, &tools, "system", "go").await.unwrap();

        assert_eq!(turn.ending, Ending::RepeatedAFailedCall);
        assert_eq!(turn.answer, NO_ANSWER);
        assert_eq!(
            turn.tool_calls, 1,
            "the repeat must not be executed a second time"
        );
    }

    /// A call that failed is remembered; a call that SUCCEEDED is not, because asking again is how
    /// anybody checks whether something changed.
    #[tokio::test]
    async fn repeating_a_call_that_worked_is_allowed() {
        let chat = ScriptedChat::new(vec![
            calls("get_run", serde_json::json!({"id": 7})),
            calls("get_run", serde_json::json!({"id": 7})),
            says("still running"),
        ]);
        let tools = FakeTools::answering(r#"{"status":"running"}"#);

        let turn = run_turn(&chat, &tools, "system", "go").await.unwrap();

        assert_eq!(turn.ending, Ending::Answered);
        assert_eq!(turn.tool_calls, 2);
    }

    #[tokio::test]
    async fn the_round_ceiling_ends_a_turn_that_never_answers() {
        let replies = (0..MAX_TOOL_ROUNDS + 2)
            .map(|index| calls("get_run", serde_json::json!({"id": index})))
            .collect();
        let chat = ScriptedChat::new(replies);
        let tools = FakeTools::answering(r#"{"status":"running"}"#);

        let turn = run_turn(&chat, &tools, "system", "go").await.unwrap();

        assert_eq!(turn.ending, Ending::RoundsExhausted);
        assert_eq!(turn.answer, NO_ANSWER);
        assert_eq!(
            turn.tool_calls, MAX_TOOL_ROUNDS,
            "the ceiling counts rounds, and each of these rounds ran one tool"
        );
    }

    /// A run whose output mentions an error is a SUCCESSFUL read of a failed run. Treating it as a
    /// failed call would stop the model looking at the same broken run twice, which is exactly what
    /// somebody debugging asks it to do.
    #[test]
    fn only_the_error_shape_counts_as_a_failed_call() {
        assert!(looks_like_failure(r#"{"error":"unknown run"}"#));
        assert!(!looks_like_failure(
            r#"{"status":"failed","stderr":"error: boom"}"#
        ));
        assert!(!looks_like_failure("an error happened"));
        assert!(!looks_like_failure("[]"));
    }

    #[test]
    fn a_message_with_no_calls_is_a_final_answer() {
        assert!(tool_calls(&says("done")).is_empty());
        assert!(tool_calls(&serde_json::json!({"tool_calls": []})).is_empty());
    }

    #[tokio::test]
    async fn tools_are_offered_on_every_exchange() {
        let chat = ScriptedChat::new(vec![
            calls("get_run", serde_json::json!({"id": 7})),
            says("done"),
        ]);
        let tools = FakeTools::answering("{}");

        run_turn(&chat, &tools, "system", "go").await.unwrap();

        // Withdrawing the tools after the first round would leave the model unable to follow up,
        // and the symptom — a confident answer built on one lookup — looks like a smarter model
        // rather than a broken loop.
        assert_eq!(*chat.tools_offered.lock().unwrap(), vec![true, true]);
    }
}
