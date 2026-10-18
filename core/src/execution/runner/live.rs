//! The live view of a turn read off its stream: tool calls, the plan, details and `live_from_stream`.

use super::*;

/// A turn as it stands PART WAY THROUGH: what has been written, and what is being done.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct LiveTurn {
    /// The answer so far. Empty means nothing has been said yet, which on a live turn is not the
    /// same claim as a turn that answered with nothing.
    pub text: String,
    /// The tool being run right now, or `None` when the model is writing rather than acting.
    pub doing: Option<String>,
    /// Roughly how many tokens the model spent thinking, or `None` when it did not think or the
    /// stream never said.
    ///
    /// The only thing about a thought that this machine can actually have. The CLI emits the
    /// signature and a running `thinking_tokens` estimate and WITHHOLDS the text: asked of a real
    /// interactive session, 610 thinking blocks, every one of them `thinking: ""`. So a window that
    /// offered to show the reasoning would be offering something nothing here holds — and this is
    /// what it says instead, which is true.
    ///
    /// An estimate, and named one. It is the CLI's own running count, and what it has to be right
    /// about is whether the model deliberated and roughly how hard.
    pub thought_tokens: Option<i64>,
    /// What the model thought before it answered, oldest first.
    ///
    /// Empty in practice on every stream this daemon has seen, for the reason above — the parse is
    /// here so that the day the CLI stops withholding the words, they arrive. Empty is therefore
    /// the ordinary case and not a failure, and nothing downstream may read it as one.
    ///
    /// Apart from `text` and never joined to it. Thinking is not the reply: it is working, often
    /// wrong on the way to being right, and a window that concatenated the two would record the
    /// model's private reasoning as the thing it said — which is then what a replay quotes back to
    /// it, and what a person reads as its answer.
    pub thought: Vec<String>,
    /// Every tool the turn ran, oldest first.
    ///
    /// Beside the text and not folded into it: a turn that read four files and ran the tests
    /// answered with more than its last paragraph, and the paragraph on its own reads as an opinion
    /// rather than as work. It is also the only place a turn's actions are visible at all — the
    /// window shows the reply, and nothing else ever said what produced it.
    pub did: Vec<ToolCall>,
}

/// One tool call, as much of it as is worth showing.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub name: String,
    /// The one argument that says what this call was about, or `None` when none of them does.
    ///
    /// A path, a command, a pattern, a URL — deliberately not the whole input. A `Write` carries
    /// the file it is writing, and a chat that printed that argument would print the file.
    pub detail: Option<String>,
    /// The plan this call wrote, when the call was one that writes plans. Empty for every other.
    ///
    /// The exception to "the one argument": a `TodoWrite` carries no path and no command, so
    /// `detail_of` finds nothing and the call used to arrive as a bare name with nothing beside it
    /// — while what it actually carried was the whole plan. A model working through a list is the
    /// shape of most real work, and none of it reached the page.
    ///
    /// Defaulted on the way in. `tools_used` is stored JSON and every turn already recorded is a
    /// row without this field; a row that predates the plan reads as a call that wrote none, which
    /// is exactly what it was.
    #[serde(default)]
    pub todos: Vec<Todo>,
    /// What the tool answered, cut to `RESULT_LIMIT` characters, or `None` when nothing came back.
    ///
    /// Until this existed a turn said what it REACHED FOR and never what it found: `Bash` beside
    /// `cargo test dates::` with no way to learn, from the conversation, whether the tests passed.
    /// The model's paragraph underneath is a summary of this, and a summary is exactly the thing
    /// somebody opening a tool call has decided not to take on trust.
    ///
    /// Cut, because a `Read` of a three-thousand-line file answers with the file. The full length
    /// is kept beside it in `result_chars`, so the window can say what it is NOT showing rather
    /// than present a truncation as the whole answer.
    ///
    /// **Not on the transcript.** `ToolCall::without_result` strips this before the turn list is
    /// serialised, and the answers are fetched per turn on request — the transcript route is
    /// polled at a live turn's cadence, and a hundred turns of tool output on a one-second poll
    /// is a cost paid forever for something almost nobody has open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// How long the whole answer was, in characters. `None` when nothing came back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_chars: Option<i64>,
    /// Whether the tool answered with an error rather than an answer.
    ///
    /// Its own field and not inferred from the text: "the command failed" and "the command printed
    /// something that mentions an error" are different facts, and only the stream knows which this
    /// was. False on every turn recorded before the field existed, which is the honest default —
    /// nothing about those rows says a tool failed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub result_failed: bool,
    // Everything below is what the agent map draws: who made the call, when, and what became of
    // it. Every field is defaulted and skipped when empty, so rows recorded before them still
    // parse and an ordinary call costs nothing extra on the wire.
    /// The CLI's `tool_use` id, which is what `parent` on a subagent's calls points at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The `parent_tool_use_id` of the message that made this call: the `Task`/`Agent` call whose
    /// subagent made it. `None` for the main agent's own calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// A `Task`/`Agent` call's `subagent_type`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
    /// A `Task`/`Agent` call's `model` input, when it named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Whether the call asked to run in the background (`run_in_background: true`), or the CLI
    /// reported its task as backgrounded.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub background: bool,
    /// RFC 3339: when the `tool_use` was seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// RFC 3339: when its answer was seen — for a background call, when its task ended, since the
    /// answer only acknowledges the launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// A `Task`/`Agent` call's total tokens, as its result or its task events report them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// A background call's task: `running`, `completed`, `failed` or `killed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// The CLI's id for this call's task (`task_id` on its task events, `backgroundTaskId` on a
    /// launch's answer). What `chat_tasks` matches an end event on when it names no tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

impl ToolCall {
    /// The same call with its answer removed, for the transcript. See `result`.
    pub fn without_result(self) -> Self {
        Self {
            result: None,
            result_chars: None,
            result_failed: false,
            ..self
        }
    }
}

/// One line of a plan.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Todo {
    pub text: String,
    /// As the CLI words it: `pending`, `in_progress`, `completed`. Kept as it arrives rather than
    /// mapped to something of this daemon's own — a fourth state invented upstream would otherwise
    /// silently become one of the three here.
    pub status: String,
}

/// The plan inside a `TodoWrite` input, or nothing at all for every other tool.
///
/// Anything shaped wrong is skipped rather than guessed at: a plan drawn from a half-understood
/// input is a list of work nobody planned.
pub(super) fn plan_of(name: &str, input: Option<&serde_json::Value>) -> Vec<Todo> {
    if name != "TodoWrite" {
        return Vec::new();
    }
    let Some(items) = input
        .and_then(|input| input.get("todos"))
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let text = item.get("content").and_then(|v| v.as_str())?.trim();
            if text.is_empty() {
                return None;
            }
            Some(Todo {
                text: cut_detail(text),
                status: item
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("pending")
                    .to_string(),
            })
        })
        .collect()
}

/// The longest detail kept. A command line can be a heredoc.
pub(super) const DETAIL_LIMIT: usize = 120;

/// The longest tool answer kept.
///
/// Two thousand characters is about thirty lines: enough for a test summary, a short diff or the
/// head of a compiler's complaint, which is what somebody opening a tool call is looking for. A
/// `Read` answers with a whole file and a `Grep` with every hit, and neither belongs in a row of
/// a database that is read back in full every time a conversation is opened.
pub(super) const RESULT_LIMIT: usize = 2000;

/// What a `tool_result` block actually said, flattened.
///
/// The CLI sends `content` two ways — a bare string, or an array of content blocks — and both are
/// ordinary. Anything else comes back as `None` rather than as a JSON dump: a window showing the
/// serialisation of a shape this daemon did not recognise is worse than one showing nothing.
pub(super) fn result_text(content: Option<&serde_json::Value>) -> Option<String> {
    match content? {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(blocks) => {
            let joined = blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            (!joined.is_empty()).then_some(joined)
        }
        _ => None,
    }
}

/// The argument of a tool call worth showing beside its name.
///
/// A fixed list of keys tried in order, rather than "the first string in the object": the input
/// keys belong to the tools, and an unknown tool would otherwise contribute whichever field
/// happened to be ordered first — a different answer between two runs of the same call.
pub fn detail_of(input: &serde_json::Value) -> Option<String> {
    // `description` last, and last on purpose: it is what a `Task` carries and nothing else does,
    // and a tool that also says where it acted must answer with that instead. A key ordered above
    // it would make the sentence a model wrote win over the file it opened.
    //
    // `notebook_path` is here because `NotebookEdit` names its target with it and nothing else in
    // this list matched, so every notebook write showed as a bare tool name with no file beside
    // it. That is cosmetic on an allow and it is not cosmetic on an approval: a person was being
    // asked to permit a write without being told what it writes, which is not a question anyone
    // can answer. It sits beside `file_path` because it IS the file path, under the one tool that
    // spells it differently.
    const KEYS: [&str; 8] = [
        "file_path",
        "notebook_path",
        "path",
        "command",
        "pattern",
        "url",
        "query",
        "description",
    ];
    // `AskUserQuestion` names no path or command; what it is about is the question it asks.
    if let Some(q) = input
        .pointer("/questions/0/question")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|q| !q.is_empty())
    {
        return Some(cut_detail(q));
    }
    let found = KEYS
        .iter()
        .find_map(|key| input.get(key).and_then(|value| value.as_str()))?;
    let trimmed = found.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(cut_detail(trimmed))
}

/// The same ceiling for a detail and for a line of a plan: both are one line beside a tool's name,
/// and a plan item can be a paragraph somebody pasted.
pub(super) fn cut_detail(text: &str) -> String {
    let mut out: String = text.chars().take(DETAIL_LIMIT).collect();
    if text.chars().count() > DETAIL_LIMIT {
        out.push('…');
    }
    out
}

/// Distils a stream still being written into the two things worth showing while it is.
///
/// Beside `extract_reply`, and for the same stated reason: knowing the CLI's output format is this
/// module's job. The alternative was to teach the window these shapes, which would put a format the
/// app does not own — and which changes without asking — into the one place that cannot be tested
/// against the real thing.
///
/// The two sources of the same words are the whole difficulty. With `--include-partial-messages`
/// the text arrives twice: once as `text_delta`s while it is typed, and again in the completed
/// `assistant` message. So the deltas fill a buffer, and a completed message REPLACES that buffer
/// with what it says — which is also what makes this correct when no partials arrive at all.
///
/// Completed messages accumulate rather than replace each other: text, a tool call, then more text
/// is one answer with a gap in it, and keeping only the newest message would silently drop
/// everything the model said before it reached for anything.
pub fn live_from_stream(stream: &str) -> LiveTurn {
    stream
        .lines()
        .fold(LiveStream::default(), |mut live, line| {
            live.push_line(line);
            live
        })
        .snapshot()
}

/// `live_from_stream`'s state, kept between lines so a reader that is handed a stream a piece at a
/// time (the SSE tail, one per subscriber) distils only what is new instead of re-reading all of it
/// on every tick. Fed every line in order, `snapshot` is exactly what `live_from_stream` answers for
/// the whole text — which is the one definition, since `live_from_stream` is a fold over this.
#[derive(Debug, Default, Clone)]
pub struct LiveStream {
    finished: Vec<String>,
    writing: String,
    thought: Vec<String>,
    thought_tokens: Option<i64>,
    pondering: String,
    doing: Option<String>,
    did: Vec<ToolCall>,
    // The `tool_use` id of each call in `did`, by the same index: the pairing key for answers and
    // task events, kept beside `ToolCall::id` so a call sent without an id still holds its place.
    called: Vec<Option<String>>,
    // Background task ids to the index of the call that launched them, learned from the task
    // events and from the launch's own answer.
    tasks: std::collections::HashMap<String, usize>,
}

impl LiveStream {
    /// Reads one line of the stream. A line that is not JSON changes nothing.
    pub fn push_line(&mut self, line: &str) {
        let Self {
            finished,
            writing,
            thought,
            thought_tokens,
            pondering,
            doing,
            did,
            called,
            tasks,
        } = self;
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            return;
        };
        // Set on every line a subagent wrote: the `Task`/`Agent` call that spawned it.
        let parent = value
            .get("parent_tool_use_id")
            .and_then(|p| p.as_str())
            .map(str::to_string);
        // The stream's own clock, which the CLI puts on `user` lines and not on `assistant` ones.
        let stamp = value.get("timestamp").and_then(|t| t.as_str());
        match value.get("type").and_then(|t| t.as_str()) {
            // Only `text_delta` carries a `text`. A tool call's arguments stream as
            // `input_json_delta` under `partial_json`, and reading that as speech would put a
            // half-written JSON object in the middle of a sentence.
            // A subagent's own words stream on the same channel, marked with the call that spawned
            // it. They are not this turn's answer, and reading them would splice them into it.
            Some("stream_event") if parent.is_some() => {}
            Some("stream_event") => {
                if let Some(text) = value.pointer("/event/delta/text").and_then(|t| t.as_str()) {
                    writing.push_str(text);
                }
                // Thinking streams on the same channel under its own key — and arrives EMPTY.
                // The CLI sends `thinking_delta`s carrying `"thinking": ""` and a token estimate,
                // never the words. Read anyway, so the day it stops withholding them they appear;
                // measured below, because the measurement is the part that exists.
                if let Some(text) = value
                    .pointer("/event/delta/thinking")
                    .and_then(|t| t.as_str())
                {
                    pondering.push_str(text);
                }
                *thought_tokens = larger(
                    *thought_tokens,
                    value.pointer("/event/delta/estimated_tokens"),
                );
            }
            // The one line that says anything real about a thought. A running total, and it arrives
            // even on the turns whose thinking block came through with its text stripped out —
            // which, so far, is all of them.
            Some("system")
                if value.get("subtype").and_then(|s| s.as_str()) == Some("thinking_tokens") =>
            {
                *thought_tokens = larger(*thought_tokens, value.get("estimated_tokens"));
            }
            Some("system") => task_event(&value, stamp, did, called, tasks),
            Some("assistant") => {
                let blocks = value
                    .pointer("/message/content")
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default();
                let text = blocks
                    .iter()
                    .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(
                        "

",
                    );
                // A subagent's message contributes its tool calls and nothing else: its text and
                // its thinking belong to the subagent, and the deltas being buffered are the main
                // agent's, so they must not be cleared by it either.
                let own = parent.is_none();
                if own && !text.trim().is_empty() {
                    finished.push(text);
                }
                thought.extend(
                    blocks
                        .iter()
                        .filter(|_| own)
                        .filter(|block| {
                            block.get("type").and_then(|t| t.as_str()) == Some("thinking")
                        })
                        .filter_map(|block| block.get("thinking").and_then(|t| t.as_str()))
                        .filter(|text| !text.trim().is_empty())
                        .map(str::to_string),
                );
                for block in blocks
                    .iter()
                    .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
                {
                    let Some(name) = block.get("name").and_then(|n| n.as_str()) else {
                        continue;
                    };
                    let input = block.get("input");
                    let id = block
                        .get("id")
                        .and_then(|id| id.as_str())
                        .map(str::to_string);
                    let spawns = matches!(name, "Task" | "Agent");
                    let spawn_field = |key: &str| {
                        input
                            .filter(|_| spawns)
                            .and_then(|input| input.get(key))
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    };
                    let background = input
                        .and_then(|input| input.get("run_in_background"))
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    did.push(ToolCall {
                        name: name.to_string(),
                        detail: input.and_then(detail_of),
                        todos: plan_of(name, input),
                        subagent_type: spawn_field("subagent_type"),
                        model: spawn_field("model"),
                        background,
                        status: background.then(|| "running".to_string()),
                        started_at: Some(seen_at(
                            stamp,
                            id.as_deref().map(|id| format!("use:{id}")),
                        )),
                        parent: parent.clone(),
                        id: id.clone(),
                        ..ToolCall::default()
                    });
                    called.push(id);
                    if own {
                        *doing = Some(name.to_string());
                    }
                }
                // The message that just completed is the one those deltas were writing — both
                // kinds of them. The CLI sends one `assistant` event per API message carrying every
                // block of it, so a message that ended a thought carries that thought, and keeping
                // the buffer as well would show it twice.
                if own {
                    writing.clear();
                    pondering.clear();
                }
            }
            // A tool answering is the only thing that ends a tool call. Clearing this anywhere else
            // would show the model as writing while a command is still running.
            Some("user") => {
                let mut returned = false;
                for block in value
                    .pointer("/message/content")
                    .and_then(|c| c.as_array())
                    .into_iter()
                    .flatten()
                    .filter(|block| {
                        block.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                    })
                {
                    returned = true;
                    // Paired by id and never by position. A turn can have two tool calls in flight
                    // at once — the CLI runs them concurrently — and answers arrive in whatever
                    // order the tools finish, so "the most recent call" is wrong exactly when it
                    // matters. An answer whose id names no call this stream made is dropped: it
                    // belongs to something that is not in this list.
                    let Some(index) = block
                        .get("tool_use_id")
                        .and_then(|id| id.as_str())
                        .and_then(|id| called.iter().position(|made| made.as_deref() == Some(id)))
                    else {
                        continue;
                    };
                    let call = &mut did[index];
                    // A background call's answer only acknowledges the launch; it finishes when
                    // its task does (`task_event`).
                    if !call.background {
                        call.finished_at = Some(seen_at(
                            stamp,
                            call.id.as_deref().map(|id| format!("result:{id}")),
                        ));
                    }
                    let reported = value.get("tool_use_result");
                    if matches!(call.name.as_str(), "Task" | "Agent")
                        && let Some(total) = reported
                            .and_then(|r| r.get("totalTokens"))
                            .and_then(serde_json::Value::as_u64)
                    {
                        call.tokens = Some(total);
                    }
                    if let Some(task) = reported
                        .and_then(|r| r.get("backgroundTaskId"))
                        .and_then(|t| t.as_str())
                    {
                        call.task_id.get_or_insert_with(|| task.to_string());
                        tasks.insert(task.to_string(), index);
                    }
                    let Some(text) = result_text(block.get("content")) else {
                        continue;
                    };
                    call.result_chars = Some(text.chars().count() as i64);
                    call.result = Some(text.chars().take(RESULT_LIMIT).collect());
                    call.result_failed = block
                        .get("is_error")
                        .and_then(|flag| flag.as_bool())
                        .unwrap_or(false);
                }
                if returned {
                    *doing = None;
                }
            }
            _ => {}
        }
    }

    /// What the stream read so far says: the answer, the thought and the tool calls.
    ///
    /// A thought or an answer still being typed is worth showing — that is most of what a live turn
    /// IS while it is hard — so the open buffers count here without being closed.
    pub fn snapshot(&self) -> LiveTurn {
        let mut finished = self.finished.clone();
        if !self.writing.trim().is_empty() {
            finished.push(self.writing.clone());
        }
        let mut thought = self.thought.clone();
        if !self.pondering.trim().is_empty() {
            thought.push(self.pondering.clone());
        }

        LiveTurn {
            text: finished.join("\n\n"),
            thought_tokens: self.thought_tokens,
            thought,
            doing: self.doing.clone(),
            did: self.did.clone(),
        }
    }
}

/// The larger of what is known and what a line claims, ignoring a line that claims nothing.
///
/// `estimated_tokens` is a RUNNING total and arrives on two different kinds of line, one of which
/// sometimes sends it null. Taking the largest is what makes a stream read at any point report the
/// whole thought so far rather than whichever line happened to come last.
pub(super) fn larger(known: Option<i64>, claimed: Option<&serde_json::Value>) -> Option<i64> {
    match claimed.and_then(serde_json::Value::as_i64) {
        Some(seen) => Some(known.map_or(seen, |known| known.max(seen))),
        None => known,
    }
}

/// When this daemon first saw each keyed event, for lines that carry no clock of their own.
///
/// `live_from_stream` re-reads the whole stream on every poll and once more at turn end, so "now"
/// taken during a parse would move each time. Remembered by key (a `tool_use` id) instead, so the
/// first parse fixes the time and every later one repeats it.
pub(super) static FIRST_SEEN: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, String>>,
> = std::sync::LazyLock::new(Default::default);

/// Above this many remembered keys the memory starts over. Keys are only needed while their turn
/// is being read, so forgetting old ones costs nothing that is still shown.
pub(super) const FIRST_SEEN_LIMIT: usize = 4096;

/// The stream's own timestamp when the line has one, else when this daemon first saw `key`.
pub(super) fn seen_at(stamp: Option<&str>, key: Option<String>) -> String {
    if let Some(stamp) = stamp.filter(|stamp| !stamp.is_empty()) {
        return stamp.to_string();
    }
    let now = chrono::Utc::now().to_rfc3339();
    let Some(key) = key else {
        return now;
    };
    let Ok(mut seen) = FIRST_SEEN.lock() else {
        return now;
    };
    if seen.len() >= FIRST_SEEN_LIMIT && !seen.contains_key(&key) {
        seen.clear();
    }
    seen.entry(key).or_insert(now).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recorded stream: partial text, a thought estimate, a tool call and its answer, more text.
    /// Every line carries a timestamp and every call an id, so that the two readers cannot differ
    /// by the clock of the moment they read.
    fn recorded() -> Vec<String> {
        [
            r#"{"type":"system","subtype":"init","timestamp":"2026-10-17T10:00:00Z"}"#,
            r#"{"type":"stream_event","timestamp":"2026-10-17T10:00:01Z","event":{"delta":{"thinking":"","estimated_tokens":12}}}"#,
            r#"{"type":"stream_event","timestamp":"2026-10-17T10:00:02Z","event":{"delta":{"text":"Looking at "}}}"#,
            r#"{"type":"stream_event","timestamp":"2026-10-17T10:00:03Z","event":{"delta":{"text":"the file."}}}"#,
            r#"{"type":"assistant","timestamp":"2026-10-17T10:00:04Z","message":{"content":[{"type":"text","text":"Looking at the file."},{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"src/main.rs"}}]}}"#,
            r#"this line is not json at all"#,
            r#"{"type":"user","timestamp":"2026-10-17T10:00:05Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"fn main() {}"}]}}"#,
            r#"{"type":"stream_event","timestamp":"2026-10-17T10:00:06Z","event":{"delta":{"text":"It is empty."}}}"#,
            r#"{"type":"assistant","timestamp":"2026-10-17T10:00:07Z","message":{"content":[{"type":"text","text":"It is empty."}]}}"#,
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    /// The incremental reader that the live tail keeps per subscriber must read a stream exactly
    /// as the one-shot reader reads it whole — at every prefix, not only at the end, since each
    /// tick shows whatever has arrived so far.
    #[test]
    fn a_stream_fed_line_by_line_reads_as_the_whole_stream() {
        let lines = recorded();
        let mut stream = LiveStream::default();

        for (n, line) in lines.iter().enumerate() {
            stream.push_line(line);
            let whole = lines[..=n].join("\n");
            assert_eq!(
                stream.snapshot(),
                live_from_stream(&whole),
                "after {} line(s)",
                n + 1
            );
        }

        let end = stream.snapshot();
        assert!(!end.text.is_empty(), "the recorded stream says something");
        assert_eq!(end, live_from_stream(&lines.join("\n")));
    }
}
