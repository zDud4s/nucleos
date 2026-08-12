//! The conversations already had in the IDE, found on disk.
//!
//! The only module that knows `~/.claude/projects/` exists. Nothing here is ingested: the CLI's
//! transcripts stay where they are — 583 MB of them at the time of writing — and this reads the
//! head of a file to answer four questions and then stops.
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
/// Matches against names FOUND on disk rather than building a path out of the id. The id arrives
/// from a client, and the difference matters: a path built from `../../` reaches wherever it likes,
/// while a name compared against a directory listing can only ever be one of the names that were
/// there. The caller then gets the directory from the file rather than from the request, which is
/// the whole reason this lookup exists — a caller that could name its own working directory could
/// name one where the classifier hook is wired and take the tools that come with it.
pub fn find(root: &Path, session_id: &str) -> Option<IdeSession> {
    transcripts(root)
        .into_iter()
        .find(|(path, _)| path.file_stem().and_then(|s| s.to_str()) == Some(session_id))
        .and_then(|(path, modified)| read_head(&path, modified))
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
            title = spoken_by_the_owner(&row);
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
/// system reminders, pasted file contents. Titling a conversation with any of the others would name
/// it after plumbing.
fn spoken_by_the_owner(row: &serde_json::Value) -> Option<String> {
    if row.get("type").and_then(|v| v.as_str()) != Some("user") {
        return None;
    }
    let content = row.get("message")?.get("content")?;
    let text = match content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .find(|block| block.get("type").and_then(|v| v.as_str()) == Some("text"))
            .and_then(|block| block.get("text").and_then(|v| v.as_str()))
            .map(str::to_string)?,
        _ => return None,
    };

    let trimmed = text.trim();
    // An injection opens with a tag; a person almost never does. Cheap, and it fails the safe way —
    // the worst case is a conversation that opens with markup losing its title and falling back to
    // its directory.
    if trimmed.is_empty() || trimmed.starts_with('<') || trimmed.starts_with("Caveat:") {
        return None;
    }

    let mut title: String = trimmed.chars().take(TITLE_LIMIT).collect();
    if trimmed.chars().count() > TITLE_LIMIT {
        title.push('…');
    }
    Some(title)
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
}
