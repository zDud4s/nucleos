//! The conversations already had in the IDE, found on disk.
//!
//! The only module that knows `~/.claude/projects/` exists. Nothing here is ingested: the CLI's
//! transcripts stay where they are — 583 MB of them at the time of writing — and each is read on
//! the request that needs it. Listing reads the HEAD of a file for four facts and stops; opening
//! one reads it THROUGH for the conversation, which on the four largest files on this machine is
//! 0.2-3.1% of the bytes. The other 97% is tool calls, their results and the model's reasoning,
//! and none of it is a line anybody said.
//!
//! A session found here is not yet anything. It becomes a conversation when `chats.rs` opens a row
//! pointing at it, and from that moment it is an ordinary NucleOS conversation whose next turn
//! happens to resume a session the daemon never started.

use std::path::{Path, PathBuf};

/// How far into a transcript to look for the two facts that are not in its name.
///
/// The first user message is normally within the first handful of lines, but "normally" is not a
/// bound and these files reach 14 MB. Two ceilings rather than one: a file of few enormous lines
/// and a file of many small ones fail differently, and only bounding lines would let the first one
/// through.
const SCAN_LINES: usize = 400;
const SCAN_BYTES: usize = 512 * 1024;

/// How much of a conversation is read back, and how much of that may be text.
///
/// Two ceilings for the same reason `SCAN_LINES` and `SCAN_BYTES` are two: a session of a thousand
/// short messages and a session of three pasted logs overrun in different directions, and a count
/// alone would let the second through. Both are applied from the RECENT END, exactly as
/// `get_assistant_chat` bounds the daemon's own transcripts — the beginning of a long session is
/// the part nobody came back for.
const SAID_SHOWN: usize = 200;
const SAID_BYTES: usize = 512 * 1024;

/// The longest title kept. The first message can be a pasted stack trace.
const TITLE_LIMIT: usize = 120;

/// A conversation found in the CLI's transcript store.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IdeSession {
    /// The id the CLI resumes by — the file's own name, which is where the CLI itself keeps it.
    pub session_id: String,
    /// Where it was had, and therefore where it must be resumed from.
    pub cwd: String,
    /// The first thing the owner said in it, or `None` when nothing quotable was said.
    pub title: Option<String>,
    /// When the file was last written, RFC 3339.
    pub last_activity: String,
}

/// A session had in `cwd`, for the tests of the modules that take one.
///
/// Beside the type rather than copied into each of them: `create` takes the whole session precisely
/// so its directory and its id cannot come from two different places, and a fixture written out
/// four times is four places for them to.
#[cfg(test)]
pub fn had_in(cwd: &str, session_id: &str) -> IdeSession {
    IdeSession {
        session_id: session_id.to_string(),
        cwd: cwd.to_string(),
        title: None,
        last_activity: "2026-08-11T10:00:00+00:00".to_string(),
    }
}

/// Where the CLI keeps its transcripts, or `None` when this machine has no home directory.
///
/// Not configurable: the path is the CLI's, not ours, and a setting for it would be a place for the
/// two to disagree.
pub fn default_root() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".claude").join("projects"))
}

/// The most recently touched sessions that can still be resumed, newest first.
///
/// Ordered by the filesystem BEFORE anything is opened, and parsing stops once `limit` have been
/// accepted. There are hundreds of these files; reading every one of them to show twenty would be
/// paying for the whole store on every list.
///
/// A session whose directory is gone is dropped rather than shown greyed out. Resuming is the only
/// thing this list is for, and the CLI finds a session by hashing the directory it was had in — so
/// a row whose directory no longer exists is a row that cannot do the one thing it offers.
pub fn discover(root: &Path, limit: usize) -> Vec<IdeSession> {
    let mut candidates = transcripts(root);
    // Newest first. `sort_by` on the pair rather than `sort_by_key` on the time: ties are broken by
    // path, so two files written in the same second do not swap places between two calls.
    candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut found = Vec::new();
    for (path, modified) in candidates {
        if found.len() >= limit {
            break;
        }
        if let Some(session) = read_head(&path, modified) {
            found.push(session);
        }
    }
    found
}

/// One session by its id, or `None` when this machine has no such transcript.
///
/// The caller gets the directory from the FILE rather than from the request, which is the whole
/// reason this lookup exists — a caller that could name its own working directory could name one
/// where the classifier hook is wired and take the tools that come with it. `path_of` says why the
/// id itself cannot reach out of the store.
pub fn find(root: &Path, session_id: &str) -> Option<IdeSession> {
    let (path, modified) = path_of(root, session_id)?;
    read_head(&path, modified)
}

/// What was said in one session, oldest first, or `None` when this machine has no such transcript.
///
/// The window draws this ABOVE the turns the daemon ran, so a conversation picked up from the
/// editor opens showing what it already was. Continuing it was always the point; opening one that
/// looked empty made the continuation impossible to believe.
///
/// Deliberately not bound to whether the session can still be resumed. `discover` drops a session
/// whose directory is gone because resuming is all a listed row offers; reading is not resuming,
/// and the file still says what was said in it.
pub fn conversation(root: &Path, session_id: &str) -> Option<Conversation> {
    let (path, _) = path_of(root, session_id)?;
    Some(read_said(&path))
}

/// What was said in a session, and whether that is all of it.
///
/// `cut` exists because the alternative is a lie the window cannot detect. This is read from the
/// recent end under two ceilings, and two hundred messages back looks exactly like a conversation
/// that had two hundred messages -- somebody scrolls up, finds the top, and reads it as the whole
/// thing. Nothing on screen could have told them otherwise, so the fact travels with the text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Conversation {
    /// Oldest first, as they were said.
    pub said: Vec<Said>,
    /// Whether older messages exist in the file and are not here.
    pub cut: bool,
}

/// One thing said in a conversation had in the IDE.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Said {
    /// Whether the owner typed it. The model answered everything else here.
    pub by_owner: bool,
    pub text: String,
    /// Whether this is a note ABOUT the conversation rather than a line OF it.
    ///
    /// One thing sets it: a subagent worked here. Those rows are dropped -- they are a different
    /// conversation, with a different model, that the owner never saw and never spoke in, and
    /// interleaving them would put words in the transcript nobody in it said. Dropping them
    /// silently was the mistake: what remained was the model saying it would look into something,
    /// a long nothing, and then a summary of work with no visible cause.
    ///
    /// A note is never attributed to anybody, which is why it carries `by_owner: false` and is
    /// still not the model speaking. The window draws it as a note and not as a bubble.
    pub aside: bool,
}

/// The transcript named by an id, matched against the store's own listing.
///
/// The matching is the security property, and is why this is one function rather than a lookup
/// written twice. The id arrives from a client: a path BUILT from `../../` reaches wherever it
/// likes, while a name COMPARED against a directory listing can only ever be one of the names that
/// were there.
fn path_of(root: &Path, session_id: &str) -> Option<(PathBuf, std::time::SystemTime)> {
    transcripts(root)
        .into_iter()
        .find(|(path, _)| path.file_stem().and_then(|s| s.to_str()) == Some(session_id))
}

/// Every `<project>/<session>.jsonl` under the root, with its modification time.
///
/// Two levels deep exactly, and not a recursive walk: the CLI's layout is one directory per project
/// containing flat transcripts, and a walk would follow whatever else ends up in there.
fn transcripts(root: &Path) -> Vec<(PathBuf, std::time::SystemTime)> {
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(root) else {
        return out;
    };
    for project in projects.flatten() {
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let modified = file
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            out.push((path, modified));
        }
    }
    out
}

/// Reads one transcript far enough to describe it, or `None` if it describes nothing resumable.
fn read_head(path: &Path, modified: std::time::SystemTime) -> Option<IdeSession> {
    use std::io::BufRead;

    let session_id = path.file_stem()?.to_str()?.to_string();
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);

    let mut cwd: Option<String> = None;
    let mut title: Option<String> = None;
    let mut line = String::new();
    let mut lines = 0usize;
    let mut bytes = 0usize;

    while lines < SCAN_LINES && bytes < SCAN_BYTES {
        line.clear();
        // A transcript can hold any byte a tool printed, so a line that is not UTF-8 is a line to
        // skip, not a file to give up on: `read_line` fails on the line, and the next one may well
        // be the one carrying the directory.
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(n) => {
                lines += 1;
                bytes += n;
            }
            Err(_) => break,
        }
        let Ok(row) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if cwd.is_none()
            && let Some(found) = row.get("cwd").and_then(|v| v.as_str())
            && !found.is_empty()
        {
            cwd = Some(found.to_string());
        }
        if title.is_none() {
            title = spoken_by_the_owner(&row).map(|said| cut_to(said, TITLE_LIMIT));
        }
        if cwd.is_some() && title.is_some() {
            break;
        }
    }

    // The directory is read from INSIDE the file and never from the directory name above it. That
    // name is the path with every separator and colon flattened to `-`, so `C--Projects-nucleos`
    // could have come from a `:` or from a `.`, and guessing wrong means launching the CLI
    // somewhere the session does not exist — where it silently starts a new one instead.
    let cwd = cwd?;
    if !Path::new(&cwd).is_dir() {
        return None;
    }

    Some(IdeSession {
        session_id,
        cwd,
        title,
        last_activity: chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339(),
    })
}

/// The text of a row, when that row is the owner typing.
///
/// Three things wear `"type": "user"` in these files and only one of them was typed: a real message,
/// a tool result being handed back, and the harness's own injections — caveats about local commands,
/// system reminders, pasted file contents, the body of a skill. Titling a conversation with any of
/// the others would name it after plumbing, and putting one in the body of a conversation would
/// attribute it to somebody.
///
/// Injections do not get a row to themselves. The editor writes `<ide_opened_file>...` into the
/// SAME row the person typed into, and it writes first — so a row is searched block by block for
/// the first thing a person could have said, rather than judged by whichever block came first.
/// Reading only block zero threw away the message beside it and named the conversation after
/// whatever it found later, which is how a session ends up titled after a skill.
///
/// The full text, uncut. What a title needs and what a transcript needs are different lengths, and
/// the one place that knows which is the caller.
fn spoken_by_the_owner(row: &serde_json::Value) -> Option<String> {
    if row.get("type").and_then(|v| v.as_str()) != Some("user") {
        return None;
    }
    // The harness marking its own writing. A skill body, a hook's context, a session-start
    // injection: thousands of words nobody typed, opening with ordinary prose that no guard on the
    // first character can see. This one is not a guess — the file says so.
    if row.get("isMeta").and_then(|v| v.as_bool()) == Some(true) {
        return None;
    }
    // The editor summarising itself. A compaction is written as a user row, with no `isMeta` and
    // no `isSidechain`, carrying thousands of words the machine wrote ABOUT the conversation --
    // and it passes every other guard here. `spoken` turns it into a note; nothing quotes it.
    if is_compaction(row) {
        return None;
    }
    match row.get("message")?.get("content")? {
        serde_json::Value::String(text) => typed(text),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(|v| v.as_str()) == Some("text"))
            .filter_map(|block| block.get("text").and_then(|v| v.as_str()))
            .find_map(typed),
        _ => None,
    }
}

/// One block of a user row, when a person could have typed it.
///
/// An injection opens with a tag; a person almost never does. Cheap, and it fails the safe way —
/// the worst case is a message that opens with markup being skipped, and the next block, or the
/// directory, standing in for it.
fn typed(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with('<') || trimmed.starts_with("Caveat:") {
        return None;
    }
    Some(trimmed.to_string())
}

/// The text of a row, when that row is the model answering.
///
/// Only the `text` blocks. An assistant row also carries `thinking` and `tool_use`, and neither was
/// said to anybody — the first is the model talking to itself and the second is it acting. Several
/// text blocks in one row are joined rather than kept apart: they were one answer, and splitting
/// them into separate bubbles would invent a pause that never happened.
fn answered(row: &serde_json::Value) -> Option<String> {
    if row.get("type").and_then(|v| v.as_str()) != Some("assistant") {
        return None;
    }
    let blocks = row.get("message")?.get("content")?.as_array()?;
    let text = blocks
        .iter()
        .filter(|block| block.get("type").and_then(|v| v.as_str()) == Some("text"))
        .filter_map(|block| block.get("text").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join(
            "

",
        );
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// One row of a transcript as a line of the conversation, or `None` when it is not one.
///
/// A subagent's rows are dropped here. `isSidechain` marks the exchange a `Task` tool ran inside
/// this session — a different conversation, with a different model, that the owner never saw and
/// never spoke in. Interleaving it would put words in the transcript that nobody in it said.
///
/// `read_said` counts them as they go past and leaves one note per run. Dropping is right; dropping
/// without a trace is what left an unexplained silence in the middle of the conversation.
fn spoken(row: &serde_json::Value) -> Option<Said> {
    if row.get("isSidechain").and_then(|v| v.as_bool()) == Some(true) {
        return None;
    }
    // A note rather than a deletion. The compaction is the most load-bearing event in a long
    // session: everything above it is what the model no longer remembers, and a reader who does not
    // know it happened cannot tell why the conversation seems to restart mid-thought.
    if is_compaction(row) {
        return Some(Said {
            by_owner: false,
            text: "the editor ran out of context here and summarised what came before".to_string(),
            aside: true,
        });
    }
    match row.get("type").and_then(|v| v.as_str())? {
        "user" => spoken_by_the_owner(row).map(|text| Said {
            by_owner: true,
            text,
            aside: false,
        }),
        "assistant" => answered(row).map(|text| Said {
            by_owner: false,
            text,
            aside: false,
        }),
        _ => None,
    }
}

/// A file the conversation moved, and by how much.
struct Change {
    name: String,
    added: usize,
    removed: usize,
}

/// The longest list of files a single note names. Past this it says how many more there were: a
/// note that is longer than the messages around it stops being a note.
const CHANGED_NAMED: usize = 6;

/// Adds a row's edit to the run being collected, if the row is one.
///
/// By file NAME and not by path. The conversation happened inside one project and the note is a
/// summary, not a record — two files with the same name in different folders is a cost worth
/// paying to avoid three absolute Windows paths in a line meant to be glanced at.
///
/// A patch that moved nothing is not an edit. A `Read` carries a result too, and a note saying a
/// file moved by zero lines claims something happened when nothing did.
fn record_change(changed: &mut Vec<Change>, row: &serde_json::Value) {
    let Some(result) = row.get("toolUseResult") else {
        return;
    };
    let Some(path) = result.get("filePath").and_then(|v| v.as_str()) else {
        return;
    };
    let Some(hunks) = result.get("structuredPatch").and_then(|v| v.as_array()) else {
        return;
    };
    let (mut added, mut removed) = (0usize, 0usize);
    for line in hunks
        .iter()
        .filter_map(|hunk| hunk.get("lines").and_then(|v| v.as_array()))
        .flatten()
        .filter_map(|line| line.as_str())
    {
        match line.chars().next() {
            Some('+') => added += 1,
            Some('-') => removed += 1,
            _ => {}
        }
    }
    if added == 0 && removed == 0 {
        return;
    }
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_string();
    // The same file worked in several passes is one file that moved, not one note per pass.
    if let Some(seen) = changed.iter_mut().find(|change| change.name == name) {
        seen.added += added;
        seen.removed += removed;
        return;
    }
    changed.push(Change {
        name,
        added,
        removed,
    });
}

/// The commands an assistant row ran, added to the run being collected.
///
/// `Bash` only, and its command. A conversation runs dozens of tools and almost all of them are
/// reads: naming every one would bury the two that matter under a list of files opened. What a
/// person wants to know is whether the tests were run, and reading a file is not that.
fn record_commands(ran: &mut Vec<String>, row: &serde_json::Value) {
    if row.get("type").and_then(|v| v.as_str()) != Some("assistant") {
        return;
    }
    let Some(blocks) = row
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    else {
        return;
    };
    for block in blocks {
        if block.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
            continue;
        }
        if block.get("name").and_then(|v| v.as_str()) != Some("Bash") {
            continue;
        }
        let Some(command) = block
            .get("input")
            .and_then(|i| i.get("command"))
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        // The FIRST line, and short. Measured against real transcripts rather than imagined: a
        // coding session runs heredocs, and eighty characters of one is four lines of shell with a
        // `<<PY` in the middle. A margin has to be glanceable or it is worse than nothing.
        let command = command.lines().next().unwrap_or("").trim();
        if command.is_empty() {
            continue;
        }
        let command = cut_to(command.to_string(), COMMAND_LIMIT);
        // The same command twice in a stretch is one thing that was done, not two.
        if !ran.contains(&command) {
            ran.push(command);
        }
    }
}

/// The longest command kept whole, on one line.
const COMMAND_LIMIT: usize = 60;

/// The most commands one note names, for the reason `CHANGED_NAMED` exists.
const RAN_NAMED: usize = 3;

/// The note left where the conversation did something rather than said something.
///
/// One note for both halves, not two: what was run and what changed are the same answer to the same
/// question, and splitting them would put two lines of margin between every pair of sentences.
fn did(ran: &[String], changed: &[Change]) -> Said {
    let mut parts: Vec<String> = Vec::new();
    if !ran.is_empty() {
        let mut text = format!(
            "ran {}",
            ran.iter()
                .take(RAN_NAMED)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
        if ran.len() > RAN_NAMED {
            text.push_str(&format!(" and {} more", ran.len() - RAN_NAMED));
        }
        parts.push(text);
    }
    if !changed.is_empty() {
        let named: Vec<String> = changed
            .iter()
            .take(CHANGED_NAMED)
            .map(|change| format!("{} +{} −{}", change.name, change.added, change.removed))
            .collect();
        let mut text = format!("changed {}", named.join(", "));
        if changed.len() > CHANGED_NAMED {
            text.push_str(&format!(" and {} more", changed.len() - CHANGED_NAMED));
        }
        parts.push(text);
    }
    Said {
        by_owner: false,
        text: parts.join(" · "),
        aside: true,
    }
}

/// Keeps one line, under the ceiling that governs how much CONVERSATION is read back.
///
/// Notes ride along and are not counted. The ceiling exists to bound how much of a conversation
/// comes back, and a note is not conversation — counting them against the same limit cost one real
/// message for every note added, which on a real session was eighty-nine of them out of two
/// hundred lines. The margin is not allowed to eat the thing it is a margin to.
///
/// A note left at the front once its neighbours are gone goes with them: it describes work that
/// happened between two messages nobody can see any more. Only when something was actually dropped,
/// though — a conversation that legitimately OPENS on a note (a compaction, most often) keeps it.
fn keep(
    kept: &mut std::collections::VecDeque<Said>,
    cut: &mut bool,
    spoken_kept: &mut usize,
    item: Said,
) {
    if !item.aside {
        *spoken_kept += 1;
    }
    kept.push_back(item);

    let mut dropped_any = false;
    while *spoken_kept > SAID_SHOWN {
        match kept.pop_front() {
            Some(dropped) => {
                dropped_any = true;
                if !dropped.aside {
                    *spoken_kept -= 1;
                    *cut = true;
                }
            }
            None => break,
        }
    }
    if dropped_any {
        trim_leading_notes(kept);
    }
}

/// Drops notes left stranded at the front by a trim.
fn trim_leading_notes(kept: &mut std::collections::VecDeque<Said>) {
    while kept.front().is_some_and(|line| line.aside) {
        kept.pop_front();
    }
}

/// Whether a row is the editor's own summary of a conversation that ran out of context.
fn is_compaction(row: &serde_json::Value) -> bool {
    row.get("isCompactSummary").and_then(|v| v.as_bool()) == Some(true)
}

/// The note left where a subagent worked.
///
/// Worded here rather than in the window because the count is the fact and the sentence is the
/// smallest honest way to carry it in a field that is text. It is not attributed to anybody: the
/// window draws an aside as a note, never as something said.
fn note(rows: usize) -> Said {
    Said {
        by_owner: false,
        text: format!("a subagent worked here — {rows} messages, not shown"),
        aside: true,
    }
}

/// `text` if it fits in `limit` characters, and its first `limit` with an ellipsis if it does not.
fn cut_to(text: String, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text;
    }
    let mut cut: String = text.chars().take(limit).collect();
    cut.push('…');
    cut
}

/// Reads one transcript through for the conversation in it, newest end kept.
///
/// The whole file, and no index kept of it. An index would be a second copy of somebody else's
/// truth: the CLI owns these files and writes to them whenever a session is typed into, so a cache
/// would be right until the moment it mattered. The cost is one pass over a file on the request
/// that opens the conversation, which is a click and not a poll.
///
/// A file that cannot be opened reads back as an empty conversation rather than as a failure. The
/// session is still resumable — the CLI reads its own store — and refusing to draw the chat because
/// its history could not be read would take away the working half along with the broken one.
fn read_said(path: &Path) -> Conversation {
    use std::io::BufRead;

    let Ok(file) = std::fs::File::open(path) else {
        return Conversation {
            said: Vec::new(),
            cut: false,
        };
    };
    let mut reader = std::io::BufReader::new(file);
    let mut kept: std::collections::VecDeque<Said> = std::collections::VecDeque::new();
    let mut line = String::new();
    // Set where a message is DROPPED, not where one is shortened. A long message that was cut
    // short is still on screen and still says who said it; a dropped one is a gap, and the gap is
    // what a reader has no way of noticing.
    let mut cut = false;
    // How many of the kept lines are conversation rather than margin. The ceiling is on these.
    let mut spoken_kept: usize = 0;
    // How many rows of the subagent excursion currently open have gone past.
    let mut aside: usize = 0;
    // The files changed since the last thing anybody said, in the order they were first touched.
    let mut changed: Vec<Change> = Vec::new();
    // And the commands run in the same stretch, in the order they were run.
    let mut ran: Vec<String> = Vec::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let Ok(row) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        // A run of them, not each one: a `Task` is a whole conversation, and one note per row of
        // it would bury the conversation it happened inside. The run is closed by the next thing
        // actually said, below.
        if row.get("isSidechain").and_then(|v| v.as_bool()) == Some(true) {
            aside += 1;
            continue;
        }
        // Counted before the row is judged as speech: a tool result is not a line of the
        // conversation and never becomes one, but what it did is the only record of what the
        // conversation actually changed.
        record_change(&mut changed, &row);
        // A row that ran something and said nothing never reaches the line above, so its commands
        // are taken here. A row that did both is handled after its sentence is kept.
        if spoken(&row).is_none() {
            record_commands(&mut ran, &row);
        }
        let Some(mut item) = spoken(&row) else {
            continue;
        };
        if aside > 0 {
            keep(&mut kept, &mut cut, &mut spoken_kept, note(aside));
            aside = 0;
        }
        if !changed.is_empty() || !ran.is_empty() {
            keep(&mut kept, &mut cut, &mut spoken_kept, did(&ran, &changed));
            changed.clear();
            ran.clear();
        }
        // Cut before it is kept, so one pasted log cannot be carried around whole only to be
        // thrown away by the byte ceiling below — and so that a message bigger than that ceiling
        // is shown cut rather than dropped, which would leave a gap nothing on screen explains.
        item.text = cut_to(item.text, SAID_BYTES);
        keep(&mut kept, &mut cut, &mut spoken_kept, item);
        // AFTER the line, not before it: a model says what it is about to do and then does it, and
        // a margin written the other way round describes work that had not happened yet.
        record_commands(&mut ran, &row);
    }

    // An excursion, or an edit, that the file ends inside. Both happened, and a note is the whole
    // point of noticing them.
    if aside > 0 {
        keep(&mut kept, &mut cut, &mut spoken_kept, note(aside));
    }
    if !changed.is_empty() || !ran.is_empty() {
        keep(&mut kept, &mut cut, &mut spoken_kept, did(&ran, &changed));
    }

    // The byte ceiling, taken off the oldest end. The last message is never dropped: a conversation
    // that came back empty because its final message was enormous would look like one nobody spoke
    // in.
    let mut total: usize = kept.iter().map(|said| said.text.len()).sum();
    let mut trimmed = false;
    while kept.len() > 1 && total > SAID_BYTES {
        if let Some(dropped) = kept.pop_front() {
            total -= dropped.text.len();
            trimmed = true;
            if !dropped.aside {
                cut = true;
            }
        }
    }
    // Only where something was actually dropped. A conversation legitimately OPENS on a note when
    // the editor compacted it before the first thing anybody said, and trimming that unconditionally
    // threw away the one line explaining why the conversation starts mid-thought.
    if trimmed {
        trim_leading_notes(&mut kept);
    }

    Conversation {
        said: kept.into(),
        cut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch transcript store that takes itself away.
    struct Store(PathBuf);

    impl Store {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("nucleos-ide-{}", crate::auth::generate_uuid_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        /// Writes a transcript for a session had in `cwd`, which is created so it exists.
        fn session(&self, project: &str, session_id: &str, lines: &[serde_json::Value]) -> PathBuf {
            let cwd = self.0.join("work").join(project);
            std::fs::create_dir_all(&cwd).unwrap();
            let dir = self.0.join("projects").join(project);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("{session_id}.jsonl"));
            let body: String = lines
                .iter()
                .map(|line| {
                    let mut line = line.clone();
                    if line.get("cwd").is_some() {
                        line["cwd"] = serde_json::json!(cwd.to_str().unwrap());
                    }
                    format!("{line}\n")
                })
                .collect();
            std::fs::write(&path, body).unwrap();
            path
        }

        fn root(&self) -> PathBuf {
            self.0.join("projects")
        }
    }

    impl Drop for Store {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn set_modified(path: &Path, when: std::time::SystemTime) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(when))
            .unwrap();
    }

    fn said(text: &str) -> serde_json::Value {
        serde_json::json!({"type": "user", "cwd": "", "message": {"content": text}})
    }

    #[test]
    fn a_session_is_named_by_the_first_thing_its_owner_said() {
        let store = Store::new();
        store.session(
            "one",
            "aaaa-1111",
            &[said("arranja o parser de datas"), said("e agora o resto")],
        );

        let found = discover(&store.root(), 10);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].session_id, "aaaa-1111");
        assert_eq!(found[0].title.as_deref(), Some("arranja o parser de datas"));
    }

    /// The harness types into these files too, and none of it was said by anyone.
    #[test]
    fn the_harnesss_own_injections_do_not_name_a_conversation() {
        let store = Store::new();
        store.session(
            "one",
            "aaaa-1111",
            &[
                said("<local-command-caveat>corri um comando</local-command-caveat>"),
                said("Caveat: The messages below were generated by the user"),
                serde_json::json!({
                    "type": "user", "cwd": "",
                    "message": {"content": [{"type": "tool_result", "content": "ok"}]}
                }),
                said("o que falta fazer"),
            ],
        );

        let found = discover(&store.root(), 10);

        assert_eq!(found[0].title.as_deref(), Some("o que falta fazer"));
    }

    /// The editor writes into the SAME row the person typed into, and it writes first.
    ///
    /// A message sent with a file open arrives as two text blocks: `<ide_opened_file>...` and then
    /// what was actually typed. Reading only the first block and rejecting it throws away the row
    /// that held the real message -- and the conversation then gets named after whatever came next,
    /// which is plumbing. The injection is skipped; the sentence beside it is not.
    #[test]
    fn a_message_the_editor_wrote_a_prefix_onto_is_still_the_first_thing_said() {
        let store = Store::new();
        store.session(
            "one",
            "aaaa-1111",
            &[serde_json::json!({
                "type": "user", "cwd": "",
                "message": {"content": [
                    {"type": "text", "text": "<ide_opened_file>abriu layer.py</ide_opened_file>"},
                    {"type": "text", "text": "arranja o parser de datas"},
                ]}
            })],
        );

        let found = discover(&store.root(), 10);

        assert_eq!(found[0].title.as_deref(), Some("arranja o parser de datas"));
    }

    /// `isMeta` is the harness saying so itself.
    ///
    /// A skill body, a hook's context, a session-start injection -- these are `"type": "user"` rows
    /// carrying thousands of words nobody typed, and they open with ordinary prose, so no guard on
    /// the first character can see them. The file already marks them, and the mark is exact.
    #[test]
    fn a_row_the_harness_marked_as_its_own_is_not_something_anybody_said() {
        let store = Store::new();
        store.session(
            "one",
            "aaaa-1111",
            &[
                serde_json::json!({
                    "type": "user", "cwd": "", "isMeta": true,
                    "message": {"content": [{"type": "text", "text": "Base directory for this skill: C:/skills/debugging"}]}
                }),
                said("arranja o parser de datas"),
            ],
        );

        let found = discover(&store.root(), 10);
        assert_eq!(found[0].title.as_deref(), Some("arranja o parser de datas"));

        let read = conversation(&store.root(), "aaaa-1111").unwrap().said;
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].text, "arranja o parser de datas");
    }

    /// The directory comes from inside the file, and this is the case that proves it has to: the
    /// folder above is named `one`, which is not a path at all.
    #[test]
    fn the_directory_is_read_from_the_file_and_not_from_the_folder_above_it() {
        let store = Store::new();
        store.session("one", "aaaa-1111", &[said("olá")]);

        let found = discover(&store.root(), 10);

        assert!(Path::new(&found[0].cwd).is_dir(), "{}", found[0].cwd);
        assert!(found[0].cwd.ends_with("one"), "{}", found[0].cwd);
    }

    /// A worktree that was deleted leaves its transcripts behind. Offering one is offering something
    /// that cannot be done: the CLI finds a session by the directory it was had in.
    #[test]
    fn a_session_whose_directory_is_gone_is_not_offered() {
        let store = Store::new();
        store.session("gone", "aaaa-1111", &[said("olá")]);
        std::fs::remove_dir_all(store.0.join("work").join("gone")).unwrap();

        assert!(discover(&store.root(), 10).is_empty());
    }

    /// A transcript with no `cwd` anywhere in its head cannot be resumed from anywhere in
    /// particular, so it is not offered rather than guessed at.
    #[test]
    fn a_transcript_that_never_says_where_it_was_had_is_not_offered() {
        let store = Store::new();
        let dir = store.0.join("projects").join("nowhere");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("aaaa-1111.jsonl"), "{\"type\":\"user\"}\n").unwrap();

        assert!(discover(&store.root(), 10).is_empty());
    }

    /// Newest first, and the limit is applied AFTER the ordering — otherwise a busy day's sessions
    /// would be crowded out by whichever files the directory happened to list first.
    #[test]
    fn the_most_recent_come_first_and_the_limit_cuts_the_oldest() {
        let store = Store::new();
        let older = store.session("one", "older", &[said("primeiro")]);
        let newer = store.session("two", "newer", &[said("segundo")]);
        // Written second is not enough to be newer — the two writes can land inside one tick of
        // whatever resolution the filesystem keeps — so the times are stated rather than raced for.
        let now = std::time::SystemTime::now();
        set_modified(&older, now - std::time::Duration::from_secs(600));
        set_modified(&newer, now);

        let found = discover(&store.root(), 10);
        assert_eq!(
            found
                .iter()
                .map(|s| s.session_id.as_str())
                .collect::<Vec<_>>(),
            ["newer", "older"]
        );

        let one = discover(&store.root(), 1);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].session_id, "newer");
    }

    /// A store that is not there is an empty list, not a failure. The CLI may simply never have run
    /// on this machine, and that is not a daemon error.
    #[test]
    fn a_missing_store_is_no_sessions_rather_than_an_error() {
        assert!(discover(Path::new("C:/nucleos-no-such-store-anywhere"), 10).is_empty());
    }

    #[test]
    fn a_session_is_found_by_its_id_and_brings_its_own_directory() {
        let store = Store::new();
        store.session("one", "aaaa-1111", &[said("olá")]);
        store.session("two", "bbbb-2222", &[said("adeus")]);

        let found = find(&store.root(), "bbbb-2222").expect("the session was not found");
        assert_eq!(found.title.as_deref(), Some("adeus"));
        assert!(found.cwd.ends_with("two"), "{}", found.cwd);

        assert!(find(&store.root(), "never-existed").is_none());
    }

    /// The id comes from a client, so the lookup must not be a path built out of it. Matching
    /// against names found on disk makes traversal unrepresentable rather than merely refused.
    #[test]
    fn an_id_that_tries_to_climb_out_of_the_store_finds_nothing() {
        let store = Store::new();
        store.session("one", "aaaa-1111", &[said("olá")]);
        // A real transcript, reachable by traversal from inside the store, and named so that a
        // lookup that joined paths would land on it.
        let outside = store.0.join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.jsonl"), "{}\n").unwrap();

        assert!(find(&store.root(), "../elsewhere/secret").is_none());
        assert!(find(&store.root(), "..\\elsewhere\\secret").is_none());
    }

    /// An assistant row that ran one command and said nothing.
    fn ran(command: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "assistant",
            "message": {"content": [
                {"type": "tool_use", "name": "Bash", "input": {"command": command}}
            ]}
        })
    }

    /// A tool result as the CLI records one for an edit: the file, and the hunk that moved it.
    fn edited(path: &str, lines: &[&str]) -> serde_json::Value {
        let patch = if lines.is_empty() {
            serde_json::json!([])
        } else {
            serde_json::json!([{"oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1,
                                "lines": lines}])
        };
        serde_json::json!({
            "type": "user", "cwd": "",
            "message": {"content": [{"type": "tool_result", "content": "ok"}]},
            "toolUseResult": {"filePath": path, "structuredPatch": patch}
        })
    }

    fn replied(text: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": text}]}
        })
    }

    /// What a person came back to read: the two of them talking, in the order they talked.
    ///
    /// Everything else stays in the file. On the four largest transcripts on this machine the tool
    /// calls, their results and the model's reasoning are 97-99.8% of the bytes, and not one of
    /// them is a line of the conversation.
    #[test]
    fn a_session_reads_back_as_what_the_two_of_them_said() {
        let store = Store::new();
        store.session(
            "one",
            "aaaa-1111",
            &[
                said("arranja o parser de datas"),
                serde_json::json!({
                    "type": "assistant",
                    "message": {"content": [{"type": "thinking", "thinking": "deixa ver"}]}
                }),
                serde_json::json!({
                    "type": "assistant",
                    "message": {"content": [{"type": "tool_use", "name": "Read", "input": {}}]}
                }),
                serde_json::json!({
                    "type": "user",
                    "message": {"content": [{"type": "tool_result", "content": "ok"}]}
                }),
                replied("está arranjado"),
            ],
        );

        let read = conversation(&store.root(), "aaaa-1111")
            .expect("the session was not found")
            .said;

        assert_eq!(
            read,
            vec![
                Said {
                    by_owner: true,
                    text: "arranja o parser de datas".into(),
                    aside: false,
                },
                Said {
                    by_owner: false,
                    text: "está arranjado".into(),
                    aside: false,
                },
            ]
        );
    }

    /// The same rule that keeps the harness from NAMING a conversation keeps it out of the body of
    /// one. A caveat the CLI typed is not a thing anybody said.
    #[test]
    fn the_harnesss_own_injections_are_not_part_of_the_conversation() {
        let store = Store::new();
        store.session(
            "one",
            "aaaa-1111",
            &[
                said("<system-reminder>o ficheiro mudou</system-reminder>"),
                said("Caveat: The messages below were generated by the user"),
                said("o que falta fazer"),
            ],
        );

        let read = conversation(&store.root(), "aaaa-1111").unwrap().said;

        assert_eq!(read.len(), 1);
        assert_eq!(read[0].text, "o que falta fazer");
    }

    /// A command is one line, whatever it was.
    ///
    /// Measured on real transcripts and not imagined: a coding session runs heredocs, and eighty
    /// characters of one is four lines of shell with a `<<PY` in the middle. The margin has to be
    /// glanceable or it is worse than nothing -- so what is kept is the first line, short.
    #[test]
    fn a_command_is_shown_as_one_short_line() {
        let store = Store::new();
        store.session(
            "one",
            "aaaa-1111",
            &[
                said("corre"),
                ran("python - <<'PY'\nimport json\nprint(json.dumps({}))\nPY"),
                replied("corri"),
            ],
        );

        let read = conversation(&store.root(), "aaaa-1111").unwrap().said;

        let note = &read[1];
        assert!(
            !note.text.contains('\n'),
            "the note is several lines: {}",
            note.text
        );
        assert!(note.text.contains("python"), "{}", note.text);
        assert!(!note.text.contains("import json"), "{}", note.text);
    }

    /// The margin does not cost the conversation its messages.
    ///
    /// The ceiling is on how much CONVERSATION is read back, and a note is not conversation. On a
    /// real session the notes outnumbered the sentences: counting them against the same limit
    /// pushed out one real message for every note added, so the fix made the page emptier.
    #[test]
    fn a_note_never_pushes_a_message_off_the_end() {
        let store = Store::new();
        let mut rows: Vec<serde_json::Value> = Vec::new();
        for n in 0..SAID_SHOWN {
            rows.push(said(&format!("mensagem {n}")));
            rows.push(ran(&format!("echo {n}")));
        }
        store.session("one", "aaaa-1111", &rows);

        let read = conversation(&store.root(), "aaaa-1111").unwrap();

        let messages = read.said.iter().filter(|line| !line.aside).count();
        assert_eq!(messages, SAID_SHOWN, "notes ate the conversation");
        assert!(
            !read.cut,
            "nothing was dropped, so nothing should claim it was"
        );
    }

    /// What the conversation RAN, which is the other half of what it did.
    ///
    /// A model that ran the tests and one that did not write the same shape of sentence afterwards.
    /// The commands are in the file, in the `tool_use` blocks, and the note that already names the
    /// files carries them too rather than starting a second column of margin.
    #[test]
    fn a_conversation_says_what_it_ran_beside_what_it_changed() {
        let store = Store::new();
        store.session(
            "one",
            "aaaa-1111",
            &[
                said("corre os testes"),
                ran("cargo test"),
                edited("C:/proj/a.rs", &["+um"]),
                replied("passaram"),
            ],
        );

        let read = conversation(&store.root(), "aaaa-1111").unwrap().said;

        assert_eq!(read.len(), 3, "{read:?}");
        let note = &read[1];
        assert!(note.aside);
        assert!(note.text.contains("cargo test"), "{}", note.text);
        assert!(note.text.contains("a.rs"), "{}", note.text);
    }

    /// A command runs AFTER the sentence in the row that launched it, and the note lands after that
    /// sentence rather than before it. A model says what it is about to do and then does it; the
    /// margin has to read in that order or it describes work that had not happened yet.
    #[test]
    fn what_a_message_ran_is_noted_under_it_and_not_above_it() {
        let store = Store::new();
        store.session(
            "one",
            "aaaa-1111",
            &[
                said("corre os testes"),
                serde_json::json!({
                    "type": "assistant",
                    "message": {"content": [
                        {"type": "text", "text": "vou correr"},
                        {"type": "tool_use", "name": "Bash", "input": {"command": "cargo test"}},
                    ]}
                }),
                replied("passaram"),
            ],
        );

        let read = conversation(&store.root(), "aaaa-1111").unwrap().said;

        assert_eq!(read.len(), 4, "{read:?}");
        assert_eq!(read[1].text, "vou correr");
        assert!(
            read[2].aside && read[2].text.contains("cargo test"),
            "{}",
            read[2].text
        );
        assert_eq!(read[3].text, "passaram");
    }

    /// What the conversation CHANGED, which was the one thing it never said.
    ///
    /// A transcript reads as talk: the model says it will fix something, and the next line is it
    /// saying the thing is fixed. The edit in between is on disk -- the CLI writes the whole diff
    /// into `toolUseResult` as a `structuredPatch`, 1721 of them in eight real files on this
    /// machine -- and none of it reached the page.
    ///
    /// A summary and NOT the diff: a note says which files moved and by how much. The hunks
    /// themselves are megabytes across a long session, and this is read on every open.
    #[test]
    fn the_files_a_conversation_changed_are_named_where_it_changed_them() {
        let store = Store::new();
        store.session(
            "one",
            "aaaa-1111",
            &[
                said("arranja o parser"),
                edited("C:/proj/core/src/parser.rs", &[" a", "-b", "-c", "+d"]),
                edited("C:/proj/core/src/http.rs", &["+x", " y"]),
                replied("arranjado"),
            ],
        );

        let read = conversation(&store.root(), "aaaa-1111").unwrap().said;

        assert_eq!(read.len(), 3, "{read:?}");
        assert!(read[1].aside, "the edits left no note");
        assert!(read[1].text.contains("parser.rs"), "{}", read[1].text);
        assert!(read[1].text.contains("http.rs"), "{}", read[1].text);
        // Two removed and one added in the first, one added in the second.
        assert!(read[1].text.contains("+1"), "{}", read[1].text);
        assert!(read[1].text.contains("2"), "{}", read[1].text);
    }

    /// The same file edited four times in a row is one file that moved, not four notes. A model
    /// works a file in passes, and a note per pass would bury the conversation it happened inside.
    #[test]
    fn the_same_file_touched_twice_is_counted_once_and_added_up() {
        let store = Store::new();
        store.session(
            "one",
            "aaaa-1111",
            &[
                said("arranja"),
                edited("C:/proj/a.rs", &["+um"]),
                edited("C:/proj/a.rs", &["+dois", "-tres"]),
                replied("feito"),
            ],
        );

        let read = conversation(&store.root(), "aaaa-1111").unwrap().said;

        let note = &read[1];
        assert!(note.aside);
        assert_eq!(note.text.matches("a.rs").count(), 1, "{}", note.text);
        assert!(note.text.contains("+2"), "{}", note.text);
    }

    /// A tool that changed nothing leaves nothing. A `Read` carries a result too, and a note saying
    /// a file moved by zero lines is a claim that something happened when nothing did.
    #[test]
    fn a_result_that_changed_no_file_leaves_no_note() {
        let store = Store::new();
        store.session(
            "one",
            "aaaa-1111",
            &[said("olha"), edited("C:/proj/a.rs", &[]), replied("olhei")],
        );

        let read = conversation(&store.root(), "aaaa-1111").unwrap().said;

        assert_eq!(read.len(), 2, "{read:?}");
        assert!(read.iter().all(|line| !line.aside));
    }

    /// The editor's own context ran out, and the summary of it is not something anybody said.
    ///
    /// A compaction is written as `"type": "user"`, with no `isMeta` and no `isSidechain` -- it
    /// passes every other guard here, and what it carries is thousands of words the machine wrote
    /// about the conversation, attributed to the person reading it back. Twenty-five of them sat in
    /// six real transcripts on this machine.
    ///
    /// It is a note and not a deletion, because the compaction is the most load-bearing event in a
    /// long session: everything above it is what the model no longer remembers, and a reader with
    /// no idea it happened cannot tell why the conversation seems to restart mid-thought.
    #[test]
    fn a_compaction_is_a_note_and_never_something_the_person_said() {
        let store = Store::new();
        store.session(
            "one",
            "aaaa-1111",
            &[
                said("arranja o parser"),
                serde_json::json!({
                    "type": "user", "cwd": "", "isCompactSummary": true,
                    "message": {"content": "This session is being continued from a previous conversation"}
                }),
                replied("arranjado"),
            ],
        );

        let read = conversation(&store.root(), "aaaa-1111").unwrap().said;

        assert_eq!(read.len(), 3, "{read:?}");
        assert!(read[1].aside, "the compaction was attributed to somebody");
        assert!(!read[1].by_owner);
        assert!(
            !read[1].text.contains("This session is being continued"),
            "the machine's summary was printed as a message: {}",
            read[1].text
        );
    }

    /// And it never names the conversation either. A session that begins where a compaction left
    /// off would otherwise be titled with the first line of a summary nobody wrote.
    #[test]
    fn a_compaction_does_not_name_a_conversation() {
        let store = Store::new();
        store.session(
            "one",
            "aaaa-1111",
            &[
                serde_json::json!({
                    "type": "user", "cwd": "", "isCompactSummary": true,
                    "message": {"content": "This session is being continued from a previous conversation"}
                }),
                said("e agora o resto"),
            ],
        );

        let found = discover(&store.root(), 10);

        assert_eq!(found[0].title.as_deref(), Some("e agora o resto"));
    }

    /// A subagent leaves a gap, and the gap is now named.
    ///
    /// `isSidechain` marks the exchange a `Task` ran inside this session -- a different
    /// conversation, with a different model, that the owner never saw and never spoke in. Dropping
    /// those rows is right, and dropping them SILENTLY was not: what remained was the model saying
    /// it would look into something, a long nothing, and then a summary of work with no visible
    /// cause. A subagent doing ten minutes of work is the most interesting thing on the page.
    #[test]
    fn a_subagent_leaves_a_note_where_it_worked_rather_than_a_gap() {
        let store = Store::new();
        let aside = |text: &str| {
            serde_json::json!({
                "type": "assistant", "cwd": "", "isSidechain": true,
                "message": {"content": [{"type": "text", "text": text}]}
            })
        };
        store.session(
            "one",
            "aaaa-1111",
            &[
                said("arranja o parser"),
                aside("vou ler os ficheiros"),
                aside("li tres"),
                aside("encontrei"),
                replied("arranjado"),
            ],
        );

        let read = conversation(&store.root(), "aaaa-1111").unwrap().said;

        assert_eq!(read.len(), 3, "{read:?}");
        assert!(read[0].by_owner && !read[0].aside);
        // One note for the whole run of them, carrying how much happened inside it.
        assert!(read[1].aside, "the subagent left no note");
        assert!(read[1].text.contains('3'), "{}", read[1].text);
        assert_eq!(read[2].text, "arranjado");
        assert!(!read[2].aside);
    }

    /// Two separate excursions are two notes, not one. They happened at different points in the
    /// conversation and the second one is not a continuation of the first.
    #[test]
    fn two_subagent_excursions_leave_two_notes() {
        let store = Store::new();
        let aside = || {
            serde_json::json!({
                "type": "assistant", "cwd": "", "isSidechain": true,
                "message": {"content": [{"type": "text", "text": "a trabalhar"}]}
            })
        };
        store.session(
            "one",
            "aaaa-1111",
            &[aside(), replied("primeiro"), aside(), replied("segundo")],
        );

        let read = conversation(&store.root(), "aaaa-1111").unwrap().said;

        assert_eq!(read.iter().filter(|s| s.aside).count(), 2, "{read:?}");
    }

    /// Cutting silently is the failure this reports.
    ///
    /// A conversation read from its recent end has a beginning that is not on screen, and until now
    /// nothing said so: a person scrolled up, found the top, and read it as the whole thing. The
    /// window cannot work it out for itself -- 200 messages back looks exactly like a conversation
    /// that had 200 messages.
    #[test]
    fn a_conversation_that_was_cut_says_so_and_one_that_was_not_does_not() {
        let store = Store::new();
        let many: Vec<serde_json::Value> = (0..SAID_SHOWN + 5)
            .map(|n| said(&format!("mensagem {n}")))
            .collect();
        store.session("one", "aaaa-1111", &many);
        store.session("one", "bbbb-2222", &[said("uma so")]);

        let long = conversation(&store.root(), "aaaa-1111").unwrap();
        let short = conversation(&store.root(), "bbbb-2222").unwrap();

        assert!(long.cut, "the beginning was dropped and nothing said so");
        assert!(
            !short.cut,
            "nothing was dropped, so nothing should claim it was"
        );
        assert_eq!(long.said.len(), SAID_SHOWN);
    }

    /// A conversation is read from its recent end, exactly as `get_assistant_chat` reads the
    /// daemon's own. The beginning of a long session is the part nobody is coming back for.
    #[test]
    fn only_the_recent_end_of_a_long_conversation_is_read_back() {
        let store = Store::new();
        let lines: Vec<serde_json::Value> = (0..SAID_SHOWN + 20)
            .map(|n| said(&format!("mensagem {n}")))
            .collect();
        store.session("one", "aaaa-1111", &lines);

        let read = conversation(&store.root(), "aaaa-1111").unwrap().said;

        assert_eq!(read.len(), SAID_SHOWN);
        assert_eq!(read[0].text, format!("mensagem {}", 20));
        assert_eq!(
            read[SAID_SHOWN - 1].text,
            format!("mensagem {}", SAID_SHOWN + 19)
        );
    }

    /// The count is not a bound on its own: one pasted log is worth a thousand short messages, and
    /// the window has to draw whatever comes back.
    #[test]
    fn a_few_enormous_messages_are_cut_down_to_the_last_of_them() {
        let store = Store::new();
        let huge = "x".repeat(SAID_BYTES / 2 + 1);
        store.session(
            "one",
            "aaaa-1111",
            &[said(&huge), said(&huge), said("a última")],
        );

        let read = conversation(&store.root(), "aaaa-1111").unwrap().said;

        assert_eq!(read.len(), 2);
        assert_eq!(read[1].text, "a última");
    }

    /// One message bigger than the whole ceiling is shown cut rather than dropped. Dropping it
    /// would leave a gap nothing on screen could explain.
    #[test]
    fn a_single_message_past_the_ceiling_is_cut_rather_than_lost() {
        let store = Store::new();
        store.session("one", "aaaa-1111", &[said(&"x".repeat(SAID_BYTES * 2))]);

        let read = conversation(&store.root(), "aaaa-1111").unwrap().said;

        assert_eq!(read.len(), 1);
        assert!(
            read[0].text.len() <= SAID_BYTES + 4,
            "{}",
            read[0].text.len()
        );
        assert!(read[0].text.ends_with('…'));
    }

    /// A subagent's conversation is a different conversation. It runs inside this session and was
    /// never said to the person reading it back.
    #[test]
    fn what_a_subagent_said_is_not_part_of_this_conversation() {
        let store = Store::new();
        store.session(
            "one",
            "aaaa-1111",
            &[
                said("procura o bug"),
                serde_json::json!({
                    "type": "user", "isSidechain": true,
                    "message": {"content": "procura o bug"}
                }),
                serde_json::json!({
                    "type": "assistant", "isSidechain": true,
                    "message": {"content": [{"type": "text", "text": "está no parser"}]}
                }),
                replied("está no parser"),
            ],
        );

        let read = conversation(&store.root(), "aaaa-1111").unwrap().said;

        // Three lines: what was asked, a note that a subagent worked, and the answer. The note is
        // not the subagent talking — nothing it said is here, which is the point of the test.
        assert_eq!(read.len(), 3);
        assert_eq!(read[0].text, "procura o bug");
        assert!(read[1].aside);
        assert!(!read[2].by_owner);
        assert!(
            !read
                .iter()
                .any(|line| line.text == "está no parser" && line.aside),
            "the subagent's own words reached the conversation"
        );
    }

    /// The id arrives from a client here too, so the lookup is the same one `find` uses: a name
    /// matched against the store's own listing, never a path built out of what was sent.
    #[test]
    fn a_conversation_cannot_be_read_from_outside_the_store() {
        let store = Store::new();
        store.session("one", "aaaa-1111", &[said("olá")]);
        let outside = store.0.join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("secret.jsonl"),
            "{}
",
        )
        .unwrap();

        assert!(conversation(&store.root(), "../elsewhere/secret").is_none());
        assert!(conversation(&store.root(), "never-existed").is_none());
    }

    /// A conversation is readable after its directory is gone, and it is deliberately not
    /// CONTINUABLE then — `discover` drops it. Reading is not resuming: the file is still there and
    /// still says what was said in it.
    #[test]
    fn a_conversation_whose_directory_is_gone_can_still_be_read() {
        let store = Store::new();
        store.session("gone", "aaaa-1111", &[said("olá")]);
        std::fs::remove_dir_all(store.0.join("work").join("gone")).unwrap();

        assert!(discover(&store.root(), 10).is_empty());
        assert_eq!(
            conversation(&store.root(), "aaaa-1111").unwrap().said.len(),
            1
        );
    }
}
