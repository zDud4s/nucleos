//! Errands: standing work that is not a code project — a Telegram topic with a folder, a notebook
//! and a toolbox of its own.
//!
//! This module owns the whole domain and nothing beyond it: the SQL of `errands` and
//! `errand_artifacts`, resolving a topic to an errand, the folder, the notebook, and the mark that
//! says what the model had read when it wrote a file.
//!
//! It does not know Telegram exists — a `chat_key` arrives here as an opaque string, the way
//! `runs.rs` receives a chat id. It starts no runs and chooses no model. Those belong to
//! `assistant.rs`, which asks this module which errand a message landed in and decides from there.
//! If this file ever learns any of them, two concerns will have met in one place, which is the drift
//! the module map in `core/AGENTS.md` exists to prevent.

// This is a bin-only crate, so dead-code reachability starts at `main`, and the module is now
// nearly reached: the `/errands` routes (`http.rs`) call `create`, `get`, `list`, `set_status`,
// `set_brain` and `close`, and the file and notebook routes beside them reach the folder, the files
// and the marks that go with them. Three items still wait for the turn (`assistant.rs`) and the MCP
// toolbox (`mcp_tools.rs`): `resolve`, which is how a message finds the errand it landed in,
// `append_notebook`, which the núcleo writes after answering, and `artifact_tainted`, which is asked
// before a file's text enters a prompt. Measured rather than assumed — with the line below removed
// the compiler names those three and nothing else, and three `#[allow]` attributes scattered over
// them would say less than one line here does. The instruction, not a description: DELETE THIS LINE
// with the change that gives the last of the three a caller.
//
// Scoped to the non-test build, the way `contacts.rs` scopes its own suppression, so it silences
// only the absence of a production caller. Under `cfg(test)` the lint stays live — every item below
// is exercised by this module's tests, and one that stops being exercised has to say so.
#![cfg_attr(not(test), allow(dead_code))]

use std::path::{Path, PathBuf};

/// Which model answers this errand.
///
/// Mirrors `chats::Brain` on purpose: it is the same question asked about a different scope, and two
/// different answers to one question is the divergence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brain {
    Cloud,
    Local,
}

impl Brain {
    /// The wire spelling, and the only one written down.
    ///
    /// The `brain` column carries a CHECK constraint naming these two words (migration 0069), and the
    /// same two words travel in every JSON body that moves a brain. Routing the column, the wire and
    /// `from_wire` through one function is what keeps those three answers identical — a spelling
    /// invented anywhere else is refused by the database, one layer away from whoever wrote it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cloud => "cloud",
            Self::Local => "local",
        }
    }

    /// An unreadable value reads as `Local`, matching this table's column default — and the default
    /// is the opposite of `chats::Brain`'s. An errand is born from a Telegram message, which goes to
    /// the local model today; a fallback that moved the question off this machine would change the
    /// bill without anyone having asked.
    ///
    /// Not `std::str::FromStr`, for the reason `chats::Brain::from_wire` gives: that trait is for
    /// parsing that can fail, and this deliberately cannot.
    pub fn from_wire(value: &str) -> Self {
        if value == "cloud" {
            Self::Cloud
        } else {
            Self::Local
        }
    }
}

/// Whether the errand is answering, on hold, or finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Active,
    Paused,
    Done,
}

impl Status {
    /// The wire spelling, for the reason [`Brain::as_str`] gives: `status` carries its own CHECK
    /// constraint over these three words, and a fourth spelling is a row the database refuses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Done => "done",
        }
    }

    /// Closes to the safe side: a value nobody can read becomes `Paused`, and a paused errand does
    /// not act. `Active` is the state that spends money, so it is never what a failed read produces.
    pub fn from_wire(value: &str) -> Self {
        match value {
            "active" => Self::Active,
            "done" => Self::Done,
            _ => Self::Paused,
        }
    }
}

/// Serialized as the wire spelling, never as the variant name.
///
/// Hand-written rather than derived: `#[derive(serde::Serialize)]` would put `Cloud` on the wire, a
/// word neither the column nor any client knows, and `rename_all = "lowercase"` would be a second
/// copy of the two words [`Brain::as_str`] already owns. Through `as_str` there is one source, so
/// the brain a client reads out of the list is the brain it can send back in a PATCH.
impl serde::Serialize for Brain {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// The wire spelling, for the reason [`Brain`]'s own implementation gives.
impl serde::Serialize for Status {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// One errand, as everything downstream needs it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Errand {
    pub id: i64,
    pub name: String,
    /// The topic this errand sits on, as the sidecar composed it.
    ///
    /// Carried out of the table rather than kept private to `resolve`, because the sidecar arrives
    /// knowing its chat key and nothing else: `/pausa` in a topic has to find the errand of THAT
    /// topic, and a list that does not say which topic each errand is on cannot answer it.
    pub chat_key: String,
    pub brain: Brain,
    /// Relative to the files root, and only ever resolved through [`folder_path`].
    pub folder: String,
    pub status: Status,
    /// When this errand is finished, in the owner's words — the gate of an investigation.
    ///
    /// `None` for every errand that answers when spoken to and does nothing else, which is the
    /// default and stays the default. See migration 0078 for why the gate is a sentence.
    pub done_when: Option<String>,
    /// How many more turns it may take on its own initiative. Zero means none, which is what makes
    /// this dark until somebody turns it on.
    pub windows_left: i64,
}

/// The errand of this topic, if there is one.
///
/// The absence of a row IS the answer, and it is the common one: every message from every group runs
/// through here, and almost none of them are errands. Nothing guesses an errand from the SHAPE of a
/// chat key, for the same reason `Origin` exists in `assistant.rs` — a fact we wrote down beats a
/// guess about a numbering scheme somebody else owns.
pub async fn resolve(pool: &sqlx::SqlitePool, chat_key: &str) -> sqlx::Result<Option<Errand>> {
    let row = sqlx::query_as::<_, ErrandRow>(
        "SELECT id, name, chat_key, brain, folder, status, done_when, windows_left FROM errands WHERE chat_key = ?",
    )
    .bind(chat_key)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(from_row))
}

/// The columns every read of this table selects.
type ErrandRow = (
    i64,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    i64,
);

/// The single place a row becomes an [`Errand`].
///
/// Written once because five of the six columns are `TEXT` in one tuple: a second mapping that read
/// any of them into another would compile, and the error would surface as an errand that is somehow
/// paused because it runs on a local model, or one whose folder is a chat key.
fn from_row(
    (id, name, chat_key, brain, folder, status, done_when, windows_left): ErrandRow,
) -> Errand {
    Errand {
        id,
        name,
        chat_key,
        brain: Brain::from_wire(&brain),
        folder,
        status: Status::from_wire(&status),
        done_when,
        windows_left,
    }
}

/// Every errand, newest first — the closed ones too.
///
/// Closed is not archived: `close` ends the asking and keeps the answer, and this list is read to
/// find work already done as much as work still moving. Hiding `done` here would make the folder on
/// disk the only surviving trace of it. A caller wanting only the live ones filters on `status`,
/// which it can only do if they are here.
///
/// `id DESC` after `created_at DESC` is not decoration: `create` stamps `Utc::now()`, and three
/// errands opened in one instant share a timestamp to the second. With the timestamp alone SQLite is
/// free to return them in any order, and the order it happens to pick is insertion order — oldest
/// first, exactly when the list is busiest, which is the one case the ordering exists for.
pub async fn list(pool: &sqlx::SqlitePool) -> sqlx::Result<Vec<Errand>> {
    let rows = sqlx::query_as::<_, ErrandRow>(
        "SELECT id, name, chat_key, brain, folder, status, done_when, windows_left FROM errands
          ORDER BY created_at DESC, id DESC",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(from_row).collect())
}

/// One errand by its id, if there is one.
///
/// [`resolve`] asked from the other side. A Telegram message arrives knowing its topic; an HTTP
/// route arrives knowing the id it handed out, and every function below this line takes an `Errand`
/// rather than an id — so this is the step between the two. The absence of a row is again the
/// answer and not an error, because a route that cannot tell "no such errand" from "the database is
/// down" answers the first with the status of the second.
pub async fn get(pool: &sqlx::SqlitePool, id: i64) -> sqlx::Result<Option<Errand>> {
    let row = sqlx::query_as::<_, ErrandRow>(
        "SELECT id, name, chat_key, brain, folder, status, done_when, windows_left FROM errands WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(from_row))
}

/// Opens an errand on a topic, answering with its id.
///
/// Two statements, and the folder is why: the folder name carries the row's id so that two errands
/// named the same thing cannot land in one directory, and the id does not exist until the row does.
/// So the row is inserted with an empty folder and the name is written in a second step, with
/// `RETURNING id` supplying the missing half.
///
/// The two statements are one transaction because that empty folder is a value every insert passes
/// through, and `folder` is UNIQUE. Left committed between the statements it is visible to every
/// other connection, so two errands opened at the same time on different topics would meet on the
/// one name neither of them keeps, and the second would be refused over a placeholder. Inside the
/// transaction no one else ever sees it: the row arrives already carrying its folder.
///
/// The folder is minted HERE and never accepted from a caller — the same argument `chats::create`
/// makes about the chat id. A second `create` on a topic that already has an errand fails on
/// `chat_key`'s UNIQUE constraint rather than being checked for first, so two callers racing lose on
/// the key instead of on a read that was true a moment ago.
pub async fn create(pool: &sqlx::SqlitePool, name: &str, chat_key: &str) -> sqlx::Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await?;

    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO errands (name, chat_key, brain, folder, status, created_at)
         VALUES (?, ?, 'local', '', 'active', ?) RETURNING id",
    )
    .bind(name)
    .bind(chat_key)
    .bind(&now)
    .fetch_one(&mut *transaction)
    .await?;

    sqlx::query("UPDATE errands SET folder = ? WHERE id = ?")
        .bind(folder_name(name, id))
        .bind(id)
        .execute(&mut *transaction)
        .await?;

    transaction.commit().await?;
    Ok(id)
}

/// Pauses or resumes an errand — one switch, turned in both directions.
///
/// `/pausa` and `/retomar` are the same statement with a different argument, and that is deliberate:
/// a pause nothing could undo would leave closing as the only way out of a topic gone noisy, and
/// closing is the move that ends the errand. Takes a [`Status`] and never a string, so the CHECK
/// constraint on the column can only be met.
pub async fn set_status(pool: &sqlx::SqlitePool, id: i64, status: Status) -> sqlx::Result<()> {
    sqlx::query("UPDATE errands SET status = ? WHERE id = ?")
        .bind(status.as_str())
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// Moves an errand between the local model and the cloud.
///
/// The whole mitigation for the local model not being good enough: an errand that needs judgement is
/// moved out and pays for it, then moved back, and nothing else about it changes. No session is
/// dropped on the way, unlike `chats::set_brain`'s caller — an errand's memory is its notebook, a
/// file both models read, and not a conversation one of them is halfway through.
pub async fn set_brain(pool: &sqlx::SqlitePool, id: i64, brain: Brain) -> sqlx::Result<()> {
    sqlx::query("UPDATE errands SET brain = ? WHERE id = ?")
        .bind(brain.as_str())
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// Ends the asking, and keeps everything that was found.
///
/// Deletes nothing, by design: the row stays, the folder stays, and the notebook in it is the record
/// of the work. The topic simply goes back to being loose conversation. A close that removed the row
/// would throw the answer away along with the question — and would also free `chat_key`, so the next
/// message in that topic could silently open a second errand over the first one's folder.
///
/// `closed_at` is what says WHEN the asking stopped. Without it a finished errand and one that was
/// never opened are the same row wearing different statuses.
pub async fn close(pool: &sqlx::SqlitePool, id: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE errands SET status = ?, closed_at = ? WHERE id = ?")
        .bind(Status::Done.as_str())
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// How long a folder name may be, id included.
///
/// This is about the path FITTING, not about it escaping — `files::resolve_within` already owns
/// escaping, and there is deliberately only one function in the daemon that decides it.
const FOLDER_MAX: usize = 64;

/// The folder name for an errand, derived from what a person typed into a chat message.
///
/// Which is to say: from anyone. Only ASCII alphanumerics survive and everything else becomes a
/// hyphen — an allow-list rather than a deny-list, because the set of characters that are special to
/// a Windows filesystem, a shell, or a path parser is larger than anyone remembers, and a list of
/// what is permitted does not have to be complete in order to be right.
///
/// The id is appended and is the part that survives truncation: it is what makes the name unique,
/// so trimming it away to fit would defeat the only property the suffix is there for.
pub fn folder_name(name: &str, id: i64) -> String {
    let suffix = format!("-{id}");
    let budget = FOLDER_MAX.saturating_sub(suffix.len());

    let mut slug = String::new();
    // Starts true so a name beginning with punctuation cannot produce a leading hyphen, which is
    // also what collapses `../../etc` down to `etc` rather than to `---etc`.
    let mut last_was_dash = true;
    for character in name.chars() {
        let mapped = if character.is_ascii_alphanumeric() {
            character.to_ascii_lowercase()
        } else {
            '-'
        };
        if mapped == '-' {
            if last_was_dash {
                continue;
            }
            last_was_dash = true;
        } else {
            last_was_dash = false;
        }
        if slug.len() + 1 > budget {
            break;
        }
        slug.push(mapped);
    }
    while slug.ends_with('-') {
        slug.pop();
    }

    // A name with nothing sluggable in it is still a name somebody chose. Refusing it would leave an
    // errand that exists in the database with nowhere to write; the id is the half that has to be
    // unique, and the id is always there.
    if slug.is_empty() {
        slug.push_str("assunto");
    }
    format!("{slug}{suffix}")
}

/// The errand's notebook, inside its folder. A file and not a column, because it is meant to be
/// openable, and because the shape of a note is not a decision that belongs in a schema.
pub const NOTEBOOK: &str = "caderno.md";

/// This errand's folder, created if it is not there yet.
///
/// The containment check is `files::resolve_within` and is not repeated here — one function in the
/// daemon decides what is reachable, so a mistake has one place to be.
///
/// The root is canonicalised first, for the reason `files::ensure_root` gives about doing it once at
/// startup: a root still holding a symlink or a short path (`C:\PROGRA~1`) compares unequal to the
/// resolved children it really does contain, and `resolve_within` would then refuse everything under
/// it. The daemon hands in a root that is already canonical, so this costs it nothing and is what
/// makes every other caller — a test, a tool — get the same answer.
pub fn folder_path(files_root: &Path, errand: &Errand) -> std::io::Result<PathBuf> {
    let root = std::fs::canonicalize(files_root)?;
    let path = crate::files::resolve_within(&root, &errand.folder).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("errand folder {:?} refused: {error:?}", errand.folder),
        )
    })?;
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

/// The notebook as it stands.
///
/// A notebook that does not exist yet reads as empty rather than as an error: a freshly opened
/// errand has never answered anything, and "nothing has been written" is the answer to the question,
/// not a failure to answer it.
pub fn read_notebook(files_root: &Path, errand: &Errand) -> std::io::Result<String> {
    let path = folder_path(files_root, errand)?.join(NOTEBOOK);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error),
    }
}

/// Adds what a turn answered to the end of the notebook.
///
/// Written by the núcleo and never by the model. A local model can simply not call a tool, and an
/// errand that forgets by omission is precisely the failure the notebook exists to abolish — so the
/// entry does not depend on anything remembering to make it. It adds no surface either: this is the
/// same text already being sent back to the topic.
///
/// Appended, never rewritten, and each entry names its run. Order is the whole value of a notebook,
/// and a line in it has to be traceable back to the turn that produced it.
pub fn append_notebook(
    files_root: &Path,
    errand: &Errand,
    run_id: i64,
    answer: &str,
) -> std::io::Result<()> {
    use std::io::Write;

    let path = folder_path(files_root, errand)?.join(NOTEBOOK);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(
        file,
        "\n## {} — run {run_id}\n\n{answer}",
        chrono::Utc::now().to_rfc3339()
    )
}

/// How many notebook entries a turn is shown, at most.
///
/// Twenty is roughly a month of an errand answering once a working day, which is the horizon over
/// which "what has this errand been doing" is still a useful question. Older than that and the
/// answer belongs in the file, which keeps everything.
pub const NOTEBOOK_PREAMBLE_ENTRIES: usize = 20;

/// And how many characters, which is the limit that actually binds.
///
/// Entries alone bound nothing: one turn can answer with a page, and twenty pages is a preamble
/// larger than several context windows. Whichever limit is reached first wins.
pub const NOTEBOOK_PREAMBLE_CHARS: usize = 12_000;

/// As much of a notebook as a turn is shown, and how many entries were left out of it.
pub struct NotebookExcerpt {
    pub text: String,
    /// Entries not shown. Zero is the ordinary answer and means the turn is reading the whole
    /// notebook — which is what makes it worth reporting when it is not.
    pub omitted: usize,
}

/// The newest end of a notebook, bounded.
///
/// **Cut from the old end**, the way `recent_exchanges` cuts. A notebook is a record of work still
/// in progress and the last thing written is what the next turn continues from; dropping the newest
/// entries to stay under a limit would leave an errand re-deciding what it had just decided, every
/// turn, for ever.
///
/// This bounds the PREAMBLE and never the file. `read_notebook` keeps returning everything: the file
/// is the person's, they opened the folder to read it, and a rotation that deleted an errand's
/// history to save a model some tokens would be this module destroying the one artifact it exists
/// to produce.
///
/// An entry begins at a line starting with `## `, which is what [`append_notebook`] writes. Text
/// before the first such line — a note somebody typed into the file by hand — is an entry too, and
/// the oldest one, so it is the first thing dropped rather than a header that survives for ever.
fn push_entry<'a>(entries: &mut Vec<&'a str>, candidate: &'a str) {
    // Blank fragments are not entries. `append_notebook` opens every entry with a newline, so the
    // first split of a real notebook yields a lone `\n` — counted, it would report one more entry
    // dropped than a person reading the file could find.
    if !candidate.trim().is_empty() {
        entries.push(candidate);
    }
}

pub fn recent_notebook(notebook: &str) -> NotebookExcerpt {
    let mut entries: Vec<&str> = Vec::new();
    let mut start = 0;
    for (offset, _) in notebook.match_indices("## ") {
        // Only at the beginning of a line, or a `## ` heading inside an answer would split one
        // entry into two and the count would drift from what a person reading the file sees.
        if offset == 0 || notebook[..offset].ends_with('\n') {
            push_entry(&mut entries, &notebook[start..offset]);
            start = offset;
        }
    }
    push_entry(&mut entries, &notebook[start..]);

    let mut kept = 0;
    let mut chars = 0;
    for entry in entries.iter().rev().take(NOTEBOOK_PREAMBLE_ENTRIES) {
        let length = entry.chars().count();
        if chars + length > NOTEBOOK_PREAMBLE_CHARS {
            break;
        }
        chars += length;
        kept += 1;
    }

    // Nothing fits: the newest entry alone is over budget. Keeping it whole would mean the limit
    // does not hold, and dropping it would hand the turn a notebook with nothing recent in it —
    // which is worse than a cut one, because the turn cannot tell it is missing the part it needed.
    if kept == 0 {
        let Some(newest) = entries.last() else {
            return NotebookExcerpt {
                text: String::new(),
                omitted: 0,
            };
        };
        let text: String = newest.chars().take(NOTEBOOK_PREAMBLE_CHARS).collect();
        return NotebookExcerpt {
            text,
            omitted: entries.len(),
        };
    }

    NotebookExcerpt {
        text: entries[entries.len() - kept..].concat(),
        omitted: entries.len() - kept,
    }
}

/// Records that a file was written, and what the turn that wrote it had already read.
///
/// The `MAX` in the upsert is the rule in SQL: the mark rises and never falls. A clean turn
/// rewriting a tainted file does not launder it — it may have read the tainted part and copied it
/// forward, and it cannot know what it left untouched. "The last writer was clean" and "this file is
/// clean" are different claims, and only the first one is something a writer can attest to.
pub async fn record_artifact(
    pool: &sqlx::SqlitePool,
    errand_id: i64,
    path: &str,
    tainted: bool,
    run_id: Option<i64>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO errand_artifacts (errand_id, path, tainted, written_by, created_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(errand_id, path) DO UPDATE SET
           tainted = MAX(errand_artifacts.tainted, excluded.tainted),
           written_by = excluded.written_by",
    )
    .bind(errand_id)
    .bind(path)
    .bind(i64::from(tainted))
    .bind(run_id)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await
    .map(|_| ())
}

/// Whether reading this file brings a stranger's words into the turn — or `None` for "cannot say".
///
/// `None` covers both a path nobody recorded and a database that would not answer, and the two are
/// deliberately the same answer. Callers treat it as tainted: the question being asked is whether
/// untrusted text is about to enter a turn, and "I could not find out" is not "no". Collapsing it to
/// `false` here would make every unrecorded path — including one dropped into the folder by hand —
/// read as vouched for.
pub async fn artifact_tainted(pool: &sqlx::SqlitePool, errand_id: i64, path: &str) -> Option<bool> {
    sqlx::query_scalar::<_, i64>(
        "SELECT tainted FROM errand_artifacts WHERE errand_id = ? AND path = ?",
    )
    .bind(errand_id)
    .bind(path)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .map(|value| value != 0)
}

/// One file inside this errand's folder, or a refusal.
///
/// The relative path reaches here from a model that has been reading the open web, so it is
/// untrusted in the strictest sense — but no check is written out below. [`folder_path`]
/// canonicalises the root and hands the folder to `files::resolve_within`, and this asks that same
/// pair about the file. One function in the daemon decides what is reachable; a second check written
/// beside it would be a second place for the rule to be wrong, and the wrong one would be whichever
/// of them nobody remembered to update.
fn file_path(files_root: &Path, errand: &Errand, relative: &str) -> std::io::Result<PathBuf> {
    let folder = folder_path(files_root, errand)?;
    crate::files::resolve_within(&folder, relative).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("errand file {relative:?} refused: {error:?}"),
        )
    })
}

/// A file of this errand, read back by name.
///
/// A file that is not there is an error, and deliberately not the empty string [`read_notebook`]
/// answers with. The notebook is defined to start empty; a file asked for by name is a question that
/// failed, and answering it with `""` would let a caller quietly carry on with nothing.
pub fn read_file(files_root: &Path, errand: &Errand, relative: &str) -> std::io::Result<String> {
    std::fs::read_to_string(file_path(files_root, errand, relative)?)
}

/// Writes a file into the errand's folder, and records in the same breath what the turn that wrote it
/// had already read.
///
/// One operation and not two, because the second one is the one a caller forgets. A file left on
/// disk with no row reads back as `None` from [`artifact_tainted`] — "cannot say", which callers
/// treat as tainted, so the barrier still holds. It holds by making a file the model filled with a
/// stranger's words indistinguishable from one dropped in the folder by hand, which is one step from
/// a mark that carries no information at all.
///
/// The bytes land before the row on purpose. A write that succeeded with its mark unwritten reads as
/// unknown, which is the cautious answer; the other order would leave a row vouching for a file that
/// was never written.
pub async fn write_file(
    pool: &sqlx::SqlitePool,
    files_root: &Path,
    errand: &Errand,
    relative: &str,
    content: &str,
    tainted: bool,
    run_id: Option<i64>,
) -> std::io::Result<()> {
    std::fs::write(file_path(files_root, errand, relative)?, content)?;
    // Marked under the name the caller used, which is the name it will ask about later — the
    // resolved path is absolute and belongs to this machine, not to the conversation.
    record_artifact(pool, errand.id, relative, tainted, run_id)
        .await
        .map_err(|error| std::io::Error::other(format!("marking {relative:?} failed: {error}")))
}

/// What is in this errand's folder, and nothing else's.
///
/// Scoped by the folder and never by the files root, which is the whole of it: there is one root and
/// many errands, so a listing taken at the root would hand every errand every other errand's
/// investigation — including the marks saying which of those files carry a stranger's words.
///
/// Delegated to `files::list` rather than reading the directory here, so the order is the one the
/// file manager already shows and an errand's folder is not a second opinion about what a listing is.
pub fn list_files(files_root: &Path, errand: &Errand) -> std::io::Result<Vec<String>> {
    let folder = folder_path(files_root, errand)?;
    crate::files::list(&folder, "")
        .map(|entries| entries.into_iter().map(|entry| entry.name).collect())
        .map_err(|error| {
            std::io::Error::other(format!("listing {:?} failed: {error:?}", errand.folder))
        })
}

/// One standing instruction of an errand: when it fires, what it says, and where its window stands.
///
/// The rule and its scheduler state in one struct because they are one row — see migration 0076 for
/// why they are one row. `last_fired_at` and the day's count go out on the wire with the rest: what
/// a person wants from a list of rules is mostly "did it run", and answering that from a second
/// route would be a second read of a fact this one already had in its hand.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow, serde::Serialize)]
pub struct Rule {
    pub id: i64,
    pub errand_id: i64,
    pub name: String,
    pub cron: String,
    pub prompt: String,
    pub timezone: Option<String>,
    /// Verbatim, never parsed and re-spelled, for the reason `scheduler::RuleState` gives: this is
    /// the value the window claim compare-and-sets against, and a round trip through `DateTime` can
    /// change the spelling without changing the instant — after which the claim matches nothing.
    pub last_fired_at: String,
    pub fires_date: Option<String>,
    pub fires_today: i64,
    pub created_at: String,
}

/// Why a rule was not written down.
///
/// Three outcomes and not one, because the caller owes three different answers: a cron nobody can
/// read is the writer's to fix, a name already taken is a collision they can rename around, and a
/// database that would not take the row is neither of those and is not their fault.
#[derive(Debug)]
pub enum RuleError {
    /// The cron, the zone, or the pair of them mean a rule that never fires. Carries the reason
    /// `scheduler::next_occurrence` gave, which quotes what was written — a refusal that does not
    /// say which word was wrong cannot be acted on from a phone.
    Unreadable(String),
    /// This errand already has a rule of this name.
    Duplicate,
    Db(sqlx::Error),
}

impl std::fmt::Display for RuleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable(reason) => write!(formatter, "{reason}"),
            Self::Duplicate => write!(formatter, "this errand already has a rule with that name"),
            Self::Db(error) => write!(formatter, "{error}"),
        }
    }
}

/// The rules of one errand, by name.
///
/// Alphabetical rather than by creation, because the name is the handle: it is what a person types
/// to delete one, so the list they read to find it should be ordered the way they would look.
pub async fn list_rules(pool: &sqlx::SqlitePool, errand_id: i64) -> sqlx::Result<Vec<Rule>> {
    sqlx::query_as::<_, Rule>(
        "SELECT id, errand_id, name, cron, prompt, timezone, last_fired_at, fires_date,
                fires_today, created_at
         FROM errand_rules WHERE errand_id = ? ORDER BY name",
    )
    .bind(errand_id)
    .fetch_all(pool)
    .await
}

/// Writes a standing instruction down, answering with its id — or refuses it.
///
/// The cron is proved to have a next occurrence BEFORE the row exists, which is the one thing an
/// errand's rules can do that a project's cannot. `scheduler.rs` reads a project's rules out of a
/// file long after whoever wrote it walked away, so an unreadable one is armed anyway and announced
/// once to the feed; this one arrives over a route with somebody still there to be told.
///
/// Armed at `now`, not at the epoch: the scheduler calls a window due when the cron's next
/// occurrence after `last_fired_at` has passed, so a column left to default would owe this rule
/// every window since the beginning of time — and "every morning at eight", written at half past
/// nine at night, would fire immediately.
///
/// `now` is a parameter rather than a call to the clock because this row is scheduler state and the
/// scheduler is parameterised by `now` end to end. A test that cannot say WHICH instant a rule was
/// armed at cannot test arming at all.
pub async fn create_rule(
    pool: &sqlx::SqlitePool,
    errand_id: i64,
    name: &str,
    cron: &str,
    prompt: &str,
    timezone: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<i64, RuleError> {
    crate::scheduler::next_occurrence(cron, timezone, now).map_err(RuleError::Unreadable)?;

    let stamp = now.to_rfc3339();
    sqlx::query(
        "INSERT INTO errand_rules
             (errand_id, name, cron, prompt, timezone, last_fired_at, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(errand_id)
    .bind(name)
    .bind(cron)
    .bind(prompt)
    .bind(timezone)
    .bind(&stamp)
    .bind(&stamp)
    .execute(pool)
    .await
    .map(|done| done.last_insert_rowid())
    .map_err(|error| match &error {
        sqlx::Error::Database(database) if database.is_unique_violation() => RuleError::Duplicate,
        _ => RuleError::Db(error),
    })
}

/// Turns an errand into an investigation, or back into an ordinary one.
///
/// Both halves in one statement because they are one decision. A criterion with no windows is a
/// sentence nothing reads; windows with no criterion is a loop with no early stop, which is the
/// shape this whole piece exists to avoid. Clearing either — `None`, or zero — ends the
/// investigation and leaves the errand answering when spoken to, which is how an owner calls it off
/// without closing anything.
pub async fn set_investigation(
    pool: &sqlx::SqlitePool,
    id: i64,
    done_when: Option<&str>,
    windows: i64,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE errands SET done_when = ?, windows_left = ? WHERE id = ?")
        .bind(done_when)
        .bind(windows.max(0))
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// Every investigation with windows left to spend.
///
/// Active only, for the same reason `armed_rules` filters on status: a paused investigation must
/// stop spending, and applied downstream that filter leaves the window already gone.
pub async fn open_investigations(pool: &sqlx::SqlitePool) -> sqlx::Result<Vec<Errand>> {
    let rows = sqlx::query_as::<_, ErrandRow>(
        "SELECT id, name, chat_key, brain, folder, status, done_when, windows_left FROM errands
         WHERE status = 'active' AND windows_left > 0 AND done_when IS NOT NULL
         ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(from_row).collect())
}

/// Spends one window, answering whether this caller is the one that got it.
///
/// A compare-and-set on the count the caller read, so two ticks cannot both spend the last one. Like
/// the rule claim, it happens BEFORE the turn starts: a window spent on a turn that failed to start
/// is a window lost, and a turn started on a window nobody spent is the loop with no floor.
pub async fn spend_window(pool: &sqlx::SqlitePool, id: i64, was: i64) -> sqlx::Result<bool> {
    sqlx::query("UPDATE errands SET windows_left = ? WHERE id = ? AND windows_left = ?")
        .bind(was - 1)
        .bind(id)
        .bind(was)
        .execute(pool)
        .await
        .map(|done| done.rows_affected() == 1)
}

/// Gives a window back when nothing was started with it.
///
/// `was` is the count BEFORE it was spent, so the compare-and-set matches the value this caller
/// wrote and nothing else — a concurrent tick that has since spent one of its own is not clobbered.
/// The mirror of [`spend_window`], and called only where the turn provably did not start: past that
/// point a window stays spent, because refunding on a maybe is how a budget stops being one.
pub async fn refund_window(pool: &sqlx::SqlitePool, id: i64, was: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE errands SET windows_left = ? WHERE id = ? AND windows_left = ?")
        .bind(was)
        .bind(id)
        .bind(was - 1)
        .execute(pool)
        .await
        .map(|_| ())
}

/// Ends an investigation without ending the errand.
///
/// What the verifier saying "enough" comes to, and what running out of windows comes to as well.
/// The criterion is deliberately LEFT in place: it is the record of what was being looked for, and
/// an owner who wants another few windows should not have to write it again.
pub async fn close_investigation(pool: &sqlx::SqlitePool, id: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE errands SET windows_left = 0 WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// One rule of an errand still answering, with everything firing it needs.
///
/// Flat rather than a [`Rule`] with the errand beside it, because `sqlx::FromRow` does not flatten
/// and a hand-written mapping of eleven columns is the drift `from_row` above exists to prevent. The
/// errand's name and topic ride along because the scheduler needs both and neither is worth a second
/// query per rule per tick.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ArmedRule {
    pub id: i64,
    pub errand_id: i64,
    pub errand_name: String,
    /// Where the turn goes. Opaque here, as everywhere in this module.
    pub chat_key: String,
    pub name: String,
    pub cron: String,
    pub prompt: String,
    pub timezone: Option<String>,
    pub last_fired_at: String,
    pub fires_date: Option<String>,
    pub fires_today: i64,
}

/// Every rule of every errand that is still answering.
///
/// `status = 'active'` is the whole of "a paused errand does not fire" and "a closed one does not
/// either", and it is here rather than in the scheduler on purpose: the status is this module's
/// fact, and a filter applied by the caller is a filter the next caller forgets. A pause that
/// stopped the answers reaching the topic but not the work reaching the model would keep spending
/// the bill with its one visible sign switched off.
pub async fn armed_rules(pool: &sqlx::SqlitePool) -> sqlx::Result<Vec<ArmedRule>> {
    sqlx::query_as::<_, ArmedRule>(
        "SELECT r.id, r.errand_id, e.name AS errand_name, e.chat_key, r.name, r.cron, r.prompt,
                r.timezone, r.last_fired_at, r.fires_date, r.fires_today
         FROM errand_rules r
         JOIN errands e ON e.id = r.errand_id
         WHERE e.status = 'active'
         ORDER BY r.errand_id, r.id",
    )
    .fetch_all(pool)
    .await
}

/// Spends a rule's window, answering whether this caller is the one that got it.
///
/// A compare-and-set against the `last_fired_at` the caller read, so two ticks racing over one rule
/// cannot both fire it. Claimed BEFORE anything starts, which is the ordering `scheduler.rs` argues
/// for at length and which matters more here: an autonomous turn repeated is not a retry, it is the
/// same question asked twice and paid for twice.
///
/// The daily allowance is spent in the same statement, for the same reason it is in the project
/// path — two writes leave a window where the rule is claimed and not counted, and a daemon that
/// dies in it comes back with the allowance intact. `fires_date` carries the day the count belongs
/// to, so yesterday's count resets by comparison rather than by a midnight sweep nobody runs.
pub async fn claim_rule_window(
    pool: &sqlx::SqlitePool,
    rule_id: i64,
    previous_fired_at: &str,
    now: chrono::DateTime<chrono::Utc>,
    today: &str,
) -> sqlx::Result<bool> {
    sqlx::query(
        "UPDATE errand_rules
         SET last_fired_at = ?,
             fires_today = CASE WHEN fires_date = ? THEN fires_today + 1 ELSE 1 END,
             fires_date = ?
         WHERE id = ? AND last_fired_at = ?",
    )
    .bind(now.to_rfc3339())
    .bind(today)
    .bind(today)
    .bind(rule_id)
    .bind(previous_fired_at)
    .execute(pool)
    .await
    .map(|done| done.rows_affected() == 1)
}

/// Hands a claimed window back, count and all, when nothing was started.
///
/// The count goes back too, which is where this parts company with `scheduler::release_window`. That
/// one restores the timestamp and leaves the day's allowance spent — an asymmetry a project can
/// afford because it releases only on the two errors decided before any row exists, which are rare.
/// An errand releases whenever its owner happens to be mid-conversation with it, which is not rare
/// at all, and a cap that counted windows nothing came of would quietly starve a busy errand of the
/// schedule it was given.
///
/// Compare-and-set on the value just written, so a concurrent tick that has already claimed the next
/// window is not clobbered. Only ever called where nothing started: past that point the window stays
/// spent, because re-firing on a maybe is the duplicate the claim-first ordering exists to prevent.
pub async fn release_rule_window(
    pool: &sqlx::SqlitePool,
    rule_id: i64,
    previous: &ArmedRule,
    claimed_at: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE errand_rules
         SET last_fired_at = ?, fires_date = ?, fires_today = ?
         WHERE id = ? AND last_fired_at = ?",
    )
    .bind(&previous.last_fired_at)
    .bind(previous.fires_date.as_deref())
    .bind(previous.fires_today)
    .bind(rule_id)
    .bind(claimed_at.to_rfc3339())
    .execute(pool)
    .await
    .map(|_| ())
}

/// Re-arms a rule whose stored timestamp cannot be read.
///
/// Only reachable by a hand edit — `create_rule` and `claim_rule_window` both write RFC 3339 — but
/// the failure mode if it happens is the silent one: `due_rules` skips a rule it cannot date, so the
/// rule simply never fires again and nothing anywhere says so.
pub async fn rearm_rule(
    pool: &sqlx::SqlitePool,
    rule_id: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE errand_rules SET last_fired_at = ? WHERE id = ?")
        .bind(now.to_rfc3339())
        .bind(rule_id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// Removes a rule of THIS errand, answering whether there was one to remove.
///
/// Keyed by the errand as well as by the rule, and that is the whole point of the second argument:
/// both ids arrive from outside, so nothing stops a caller pairing one errand with another's rule.
/// Keyed by both, such a pairing matches no row — and the `false` says so, rather than reporting a
/// deletion that did not happen.
pub async fn delete_rule(
    pool: &sqlx::SqlitePool,
    errand_id: i64,
    rule_id: i64,
) -> sqlx::Result<bool> {
    sqlx::query("DELETE FROM errand_rules WHERE id = ? AND errand_id = ?")
        .bind(rule_id)
        .bind(errand_id)
        .execute(pool)
        .await
        .map(|done| done.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    /// A notebook of `count` entries in the shape `append_notebook` writes, numbered so a test can
    /// say WHICH ones survived and not merely how many.
    fn notebook_of(count: usize) -> String {
        (1..=count)
            .map(|n| format!("\n## 2026-08-16T10:0{n}:00Z — run {n}\n\nentrada {n}\n"))
            .collect()
    }

    /// A notebook shorter than the bound reaches the turn as written.
    ///
    /// The bound exists for an errand that has been running for months; the common case is an
    /// errand three days old, and paying for the rare case with a lossy common one would be the
    /// wrong trade. Byte for byte over the content, so a cut off by an entry cannot hide here — the
    /// only difference allowed is the leading newline `append_notebook` opens the file with, which
    /// is a blank fragment and not an entry.
    #[test]
    fn a_short_notebook_is_shown_whole() {
        let notebook = notebook_of(3);

        let excerpt = recent_notebook(&notebook);

        assert_eq!(excerpt.text, notebook.trim_start());
        assert_eq!(excerpt.omitted, 0);
    }

    /// §13's last risk: "the notebook grows without limit. There is no rotation at this stage; an
    /// errand of months fills the preamble." With piece 4 running errands unattended, an errand of
    /// months stops being hypothetical.
    ///
    /// Cut from the OLD end, the way `recent_exchanges` cuts. A notebook is a record of work in
    /// progress and the last thing written is the thing the next turn continues from; dropping the
    /// newest entries to stay under a limit would leave the errand re-deciding what it had just
    /// decided, every turn, for ever.
    #[test]
    fn a_long_notebook_keeps_its_newest_entries_and_says_what_it_dropped() {
        let notebook = notebook_of(NOTEBOOK_PREAMBLE_ENTRIES + 5);

        let excerpt = recent_notebook(&notebook);

        assert_eq!(excerpt.omitted, 5);
        assert!(
            excerpt
                .text
                .contains(&format!("entrada {}", NOTEBOOK_PREAMBLE_ENTRIES + 5)),
            "the newest entry must survive: {}",
            excerpt.text
        );
        assert!(
            !excerpt.text.contains("entrada 1\n"),
            "the oldest must not: {}",
            excerpt.text
        );
    }

    /// Entries alone do not bound anything: one turn can answer with a page of text, and twenty of
    /// those is a preamble larger than most context windows. Whichever limit is reached first wins.
    ///
    /// The sizes are deliberately unequal, oldest largest. Equal ones would let a budget measured
    /// from the wrong end of the notebook keep exactly the same number of entries and pass — the
    /// count would be right and the answer wrong, which is the shape of bug this whole function
    /// exists to avoid.
    #[test]
    fn a_notebook_of_few_but_enormous_entries_is_cut_by_size() {
        let enormous = "x".repeat(NOTEBOOK_PREAMBLE_CHARS);
        let small = "y".repeat(NOTEBOOK_PREAMBLE_CHARS / 4);
        let notebook = format!(
            "\n## a — run 1\n\n{enormous}\n\n## b — run 2\n\n{small}\n\n## c — run 3\n\n{small}\n"
        );

        let excerpt = recent_notebook(&notebook);

        assert!(
            excerpt.text.chars().count() <= NOTEBOOK_PREAMBLE_CHARS,
            "kept {} characters",
            excerpt.text.chars().count()
        );
        // Both small ones fit and the enormous one does not, so the count is pinned as well as the
        // size — a budget spent from the old end would have room for one entry, not two.
        assert_eq!(excerpt.omitted, 1);
        assert!(
            excerpt.text.contains("run 2") && excerpt.text.contains("run 3"),
            "the two newest are what stay: {}",
            &excerpt.text[..excerpt.text.len().min(80)]
        );
    }

    /// The pathological case, and the one a bound written carelessly gets wrong: a single entry
    /// bigger than the whole budget. Keeping it whole would mean the limit does not hold; dropping
    /// it would show the turn a notebook with nothing recent in it, which is worse than a cut one.
    /// So it is kept and cut, and the cut is announced.
    #[test]
    fn one_entry_larger_than_the_whole_budget_is_cut_rather_than_dropped() {
        let notebook = format!(
            "\n## a — run 1\n\n{}\n",
            "y".repeat(NOTEBOOK_PREAMBLE_CHARS * 3)
        );

        let excerpt = recent_notebook(&notebook);

        assert!(excerpt.text.chars().count() <= NOTEBOOK_PREAMBLE_CHARS);
        assert!(excerpt.text.contains('y'), "it must not come back empty");
        assert!(excerpt.omitted > 0, "and it must say it was cut");
    }

    /// An errand for the tests that are about something else — the notebook, the artifacts.
    /// Born the only way an errand can be born, so those tests never have to restate it.
    async fn an_errand(pool: &SqlitePool) -> Errand {
        create(pool, "carros para importar", "-1001234:7")
            .await
            .unwrap();
        resolve(pool, "-1001234:7").await.unwrap().unwrap()
    }

    /// The folder is derived from the name so a human can find it, and carries the id so two
    /// errands that were named the same thing cannot land in one directory.
    #[test]
    fn a_pasta_e_um_slug_com_o_id_colado() {
        assert_eq!(
            folder_name("carros para importar", 7),
            "carros-para-importar-7"
        );
    }

    /// The name arrives from a chat message, which is to say from anyone. A separator that
    /// survived would let a name choose where on disk the errand's files go.
    #[test]
    fn a_pasta_nao_pode_sair_da_raiz() {
        assert_eq!(folder_name("../../etc", 3), "etc-3");
        assert_eq!(folder_name("a/b\\c", 4), "a-b-c-4");
    }

    /// A name with nothing sluggable in it is still a name someone chose. Refusing it would
    /// mean an errand that exists in the database and has nowhere to write, so there is always
    /// a folder — the id is the part that has to be unique, and the id is always there.
    #[test]
    fn um_nome_sem_nada_aproveitavel_ainda_da_uma_pasta() {
        assert_eq!(folder_name("🚗🚗🚗", 9), "assunto-9");
        assert_eq!(folder_name("", 10), "assunto-10");
    }

    /// Windows still has a path length worth respecting, and the whole point of the id suffix is
    /// uniqueness — so the truncation happens to the name and never to the id.
    #[test]
    fn um_nome_longo_e_cortado() {
        let folder = folder_name(&"a".repeat(300), 12);

        assert!(
            folder.len() <= 64,
            "folder was {} bytes: {folder}",
            folder.len()
        );
        assert!(
            folder.ends_with("-12"),
            "the id was truncated away: {folder}"
        );
    }

    /// Most topics are not errands. Resolution has to answer "no" cheaply and without inventing
    /// one, because every message from every group runs through it.
    #[tokio::test]
    async fn um_topico_sem_assunto_nao_resolve() {
        let pool = test_pool().await;

        assert!(resolve(&pool, "-1001234:7").await.unwrap().is_none());
    }

    /// The topic IS the handle: you are in the errand because you are in the thread, and the
    /// defaults are the conservative ones — the question stays on this machine, and a fresh
    /// errand is one you can talk to.
    #[tokio::test]
    async fn um_assunto_resolve_pelo_seu_topico() {
        let pool = test_pool().await;

        let id = create(&pool, "carros importar", "-1001234:7")
            .await
            .unwrap();
        let found = resolve(&pool, "-1001234:7").await.unwrap().unwrap();

        assert_eq!(found.id, id);
        // Local by default: an errand is a standing thread about someone's business, and the
        // default has to be the one that does not ship it off the machine to find that out.
        assert_eq!(found.brain, Brain::Local);
        assert_eq!(found.status, Status::Active);
    }

    /// One errand per topic, enforced by the database. This uniqueness is what removes the need
    /// for any "enter this errand" command — and a second `create` must not quietly take the
    /// topic over from the errand already living there.
    #[tokio::test]
    async fn o_segundo_assunto_no_mesmo_topico_e_recusado() {
        let pool = test_pool().await;
        create(&pool, "carros importar", "-1001234:7")
            .await
            .unwrap();

        assert!(create(&pool, "outra coisa", "-1001234:7").await.is_err());

        let found = resolve(&pool, "-1001234:7").await.unwrap().unwrap();
        assert_eq!(found.name, "carros importar");
    }

    /// Reading the notebook of an errand that has never answered is the normal first case, not
    /// an error — the folder does not exist yet, and "nothing has been written" is the answer.
    #[tokio::test]
    async fn o_caderno_de_um_assunto_novo_esta_vazio() {
        let pool = test_pool().await;
        let errand = an_errand(&pool).await;
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(read_notebook(dir.path(), &errand).unwrap(), "");
    }

    /// The notebook is the errand's memory, and memory that reorders itself is not memory. Each
    /// entry names its run so a line in the notebook can be traced back to what produced it.
    #[tokio::test]
    async fn as_entradas_ficam_por_ordem_de_chegada() {
        let pool = test_pool().await;
        let errand = an_errand(&pool).await;
        let dir = tempfile::tempdir().unwrap();

        append_notebook(dir.path(), &errand, 1, "o primeiro carro custa 12k").unwrap();
        append_notebook(dir.path(), &errand, 2, "o segundo carro custa 9k").unwrap();

        let text = read_notebook(dir.path(), &errand).unwrap();
        let first = text
            .find("o primeiro carro custa 12k")
            .expect("first entry missing");
        let second = text
            .find("o segundo carro custa 9k")
            .expect("second entry missing");
        assert!(first < second, "entries came back out of order:\n{text}");
        assert!(
            text.contains("run 1"),
            "entry does not name its run:\n{text}"
        );
        assert!(
            text.contains("run 2"),
            "entry does not name its run:\n{text}"
        );
    }

    /// The notebook is a file on disk, not a column. That is what lets it outlive the database
    /// handle that happened to be open when it was written — a restart reads back the same text.
    #[tokio::test]
    async fn o_caderno_sobrevive_a_um_pool_novo() {
        let dir = tempfile::tempdir().unwrap();
        let pool = test_pool().await;
        let errand = an_errand(&pool).await;
        append_notebook(dir.path(), &errand, 1, "o preço fechou em 12k").unwrap();
        drop(pool);

        let _pool = test_pool().await;

        let text = read_notebook(dir.path(), &errand).unwrap();
        assert!(
            text.contains("o preço fechou em 12k"),
            "notebook lost its text:\n{text}"
        );
    }

    /// A file written by a turn that saw nothing untrusted is clean, and has to read back as
    /// clean — otherwise the mark says nothing and everything is quarantined forever.
    #[tokio::test]
    async fn um_ficheiro_escrito_por_um_turno_limpo_nao_esta_marcado() {
        let pool = test_pool().await;
        let errand = an_errand(&pool).await;

        record_artifact(&pool, errand.id, "relatorio.md", false, Some(1))
            .await
            .unwrap();

        assert_eq!(
            artifact_tainted(&pool, errand.id, "relatorio.md").await,
            Some(false)
        );
    }

    /// The other half: a file written by a turn that had read untrusted input carries that fact
    /// with it, because the file is where the untrusted input ends up.
    #[tokio::test]
    async fn um_ficheiro_escrito_por_um_turno_contaminado_fica_marcado() {
        let pool = test_pool().await;
        let errand = an_errand(&pool).await;

        record_artifact(&pool, errand.id, "relatorio.md", true, Some(1))
            .await
            .unwrap();

        assert_eq!(
            artifact_tainted(&pool, errand.id, "relatorio.md").await,
            Some(true)
        );
    }

    /// Unknown must not read as clean. A path nobody recorded is a path nobody vouched for —
    /// it might have been dropped in the folder by hand — so it answers `None`, and the caller
    /// treats `None` as tainted.
    #[tokio::test]
    async fn um_ficheiro_desconhecido_nao_e_limpo() {
        let pool = test_pool().await;
        let errand = an_errand(&pool).await;

        assert_eq!(
            artifact_tainted(&pool, errand.id, "nunca-visto.md").await,
            None
        );
    }

    /// The mark only ever goes one way. A clean turn rewriting a file does not launder what was
    /// already there: the untrusted text may still be in the file, in the parts it did not touch,
    /// and "the last writer was clean" is not the same claim as "this file is clean".
    #[tokio::test]
    async fn a_marca_so_anda_num_sentido() {
        let pool = test_pool().await;
        let errand = an_errand(&pool).await;

        record_artifact(&pool, errand.id, "relatorio.md", false, Some(1))
            .await
            .unwrap();
        record_artifact(&pool, errand.id, "relatorio.md", true, Some(2))
            .await
            .unwrap();
        assert_eq!(
            artifact_tainted(&pool, errand.id, "relatorio.md").await,
            Some(true)
        );

        record_artifact(&pool, errand.id, "relatorio.md", false, Some(3))
            .await
            .unwrap();

        assert_eq!(
            artifact_tainted(&pool, errand.id, "relatorio.md").await,
            Some(true),
            "a clean turn laundered a file that was already tainted"
        );
    }

    /// Pausing is how an errand stops answering without losing what it found, and it has to be
    /// reversible in both directions — `/pausa` and `/retomar` are one switch, not two doors. A
    /// pause nothing could undo would leave closing as the only way out of a topic gone noisy,
    /// and closing is the one move that ends the errand.
    #[tokio::test]
    async fn pausar_e_retomar_um_assunto() {
        let pool = test_pool().await;
        let errand = an_errand(&pool).await;

        set_status(&pool, errand.id, Status::Paused).await.unwrap();
        assert_eq!(
            resolve(&pool, "-1001234:7").await.unwrap().unwrap().status,
            Status::Paused
        );

        set_status(&pool, errand.id, Status::Active).await.unwrap();
        assert_eq!(
            resolve(&pool, "-1001234:7").await.unwrap().unwrap().status,
            Status::Active
        );
    }

    /// Which model answers is a per-errand decision, and it is the whole mitigation for the local
    /// model not being good enough: an errand that needs judgement is moved to the cloud and pays,
    /// then moved back. The column is read directly against `as_str` because `brain` carries a
    /// CHECK constraint on the wire spellings — a value written in any other spelling is refused by
    /// the database rather than by a reviewer, and that failure would surface far from here.
    #[tokio::test]
    async fn mudar_o_cerebro_de_um_assunto() {
        let pool = test_pool().await;
        let errand = an_errand(&pool).await;

        set_brain(&pool, errand.id, Brain::Cloud).await.unwrap();

        assert_eq!(
            resolve(&pool, "-1001234:7").await.unwrap().unwrap().brain,
            Brain::Cloud
        );
        let stored: String = sqlx::query_scalar("SELECT brain FROM errands WHERE id = ?")
            .bind(errand.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, Brain::Cloud.as_str());

        set_brain(&pool, errand.id, Brain::Local).await.unwrap();
        assert_eq!(
            resolve(&pool, "-1001234:7").await.unwrap().unwrap().brain,
            Brain::Local
        );
    }

    /// Closing is not deleting. `/fim` ends the asking and keeps the answer: the row stays, the
    /// folder stays, and the notebook in it is the record of what was found. The topic simply goes
    /// back to being loose conversation. A close that removed the row would throw the answer away
    /// along with the question — and `closed_at` is what says WHEN the asking stopped, which is the
    /// only thing distinguishing a finished errand from one that was never opened.
    #[tokio::test]
    async fn fechar_um_assunto_nao_o_apaga() {
        let pool = test_pool().await;
        let errand = an_errand(&pool).await;

        close(&pool, errand.id).await.unwrap();

        let found = resolve(&pool, "-1001234:7")
            .await
            .unwrap()
            .expect("closing an errand removed the row");
        assert_eq!(found.status, Status::Done);
        let closed_at: Option<String> =
            sqlx::query_scalar("SELECT closed_at FROM errands WHERE id = ?")
                .bind(errand.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            closed_at.is_some(),
            "the errand was closed without recording when"
        );
    }

    /// The list is what `/assuntos` answers with, and it is read to find the one you were just
    /// working on — so the newest belongs at the top. The ids are what is asserted rather than the
    /// names, because three errands opened in the same instant share a timestamp: an order that
    /// falls back to insertion order there would show the oldest first exactly when the list is
    /// busiest, which is the one case the ordering exists for.
    #[tokio::test]
    async fn a_lista_traz_o_mais_recente_primeiro() {
        let pool = test_pool().await;
        let first = create(&pool, "carros para importar", "-1001234:7")
            .await
            .unwrap();
        let second = create(&pool, "obras no telhado", "-1001234:8")
            .await
            .unwrap();
        let third = create(&pool, "seguro do carro", "-1001234:9")
            .await
            .unwrap();

        let listed = list(&pool).await.unwrap();

        let ids: Vec<i64> = listed.iter().map(|errand| errand.id).collect();
        assert_eq!(ids, vec![third, second, first]);
    }

    /// The folder is where an investigation puts what it found, and a file is only worth writing if
    /// it reads back. Both halves are asked about the ERRAND and never about a path: neither caller
    /// gets to say where on disk it landed, which is what makes the containment check unavoidable
    /// rather than something a caller remembers to ask for.
    #[tokio::test]
    async fn escrever_e_ler_um_ficheiro_do_assunto() {
        let pool = test_pool().await;
        let errand = an_errand(&pool).await;
        let dir = tempfile::tempdir().unwrap();

        write_file(
            &pool,
            dir.path(),
            &errand,
            "relatorio.md",
            "o primeiro carro custa 12k",
            false,
            Some(1),
        )
        .await
        .unwrap();

        assert_eq!(
            read_file(dir.path(), &errand, "relatorio.md").unwrap(),
            "o primeiro carro custa 12k"
        );
    }

    /// The relative path reaches here from a model that has been reading the open web, so it is
    /// untrusted input in the strictest sense. Both directions are asserted because a guard on one
    /// of them is a guard on neither: a read that refuses `..` while a write accepts it lets a turn
    /// place a file anywhere under the root and then read it back through its own folder.
    ///
    /// The target of the escape is made to EXIST first, so an implementation that simply joins the
    /// path and asks the filesystem would succeed — without it the test would pass on a plain
    /// "no such file", which is the wrong reason and would keep passing after the guard was lost.
    #[tokio::test]
    async fn um_ficheiro_fora_da_pasta_do_assunto_e_recusado() {
        let pool = test_pool().await;
        let errand = an_errand(&pool).await;
        let dir = tempfile::tempdir().unwrap();
        // Through `folder_path`, never from `dir.path()` directly: it is what canonicalises the
        // root, and a root still holding a short path (`C:\PROGRA~1`) compares unequal to the
        // children it really does contain.
        let folder = folder_path(dir.path(), &errand).unwrap();
        let root = folder.parent().unwrap().to_path_buf();
        std::fs::write(root.join("segredo.md"), "o que está fora da pasta").unwrap();

        assert!(
            read_file(dir.path(), &errand, "../segredo.md").is_err(),
            "an errand read a file outside its own folder"
        );
        assert!(
            write_file(
                &pool,
                dir.path(),
                &errand,
                "../escapou.md",
                "isto não devia estar aqui",
                false,
                None,
            )
            .await
            .is_err(),
            "an errand wrote a file outside its own folder"
        );
        assert!(
            !root.join("escapou.md").exists(),
            "the write was reported as refused and happened anyway"
        );
    }

    /// There is one files root and many errands, so scoping the listing by the root instead of by
    /// the errand would hand every errand every other errand's investigation — including the marks
    /// that say which of those files carry a stranger's words.
    #[tokio::test]
    async fn listar_ficheiros_nao_mostra_os_de_outro_assunto() {
        let pool = test_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let carros = an_errand(&pool).await;
        create(&pool, "obras no telhado", "-1009999:4")
            .await
            .unwrap();
        let telhado = resolve(&pool, "-1009999:4").await.unwrap().unwrap();

        write_file(&pool, dir.path(), &carros, "carros.md", "12k", false, None)
            .await
            .unwrap();
        write_file(&pool, dir.path(), &telhado, "telhado.md", "3k", false, None)
            .await
            .unwrap();

        let listed = list_files(dir.path(), &carros).unwrap();

        assert!(
            listed.iter().any(|name| name == "carros.md"),
            "an errand cannot see its own file: {listed:?}"
        );
        assert!(
            !listed.iter().any(|name| name == "telhado.md"),
            "another errand's file leaked into this one's listing: {listed:?}"
        );
    }

    /// The mark is placed by the write itself, and not by a second call the writer has to remember.
    /// A turn that skipped that call would leave a file it filled with a stranger's words reading
    /// back as unrecorded — and unrecorded is the answer a later turn treats as "cannot say", not
    /// as "tainted", which is one hop away from the barrier being decoration.
    #[tokio::test]
    async fn escrever_um_ficheiro_regista_o_artefacto() {
        let pool = test_pool().await;
        let errand = an_errand(&pool).await;
        let dir = tempfile::tempdir().unwrap();

        write_file(
            &pool,
            dir.path(),
            &errand,
            "relatorio.md",
            "o stand diz que são 12k",
            true,
            Some(1),
        )
        .await
        .unwrap();

        assert_eq!(
            artifact_tainted(&pool, errand.id, "relatorio.md").await,
            Some(true)
        );
    }

    /// The topic an errand sits on has to come back with it.
    ///
    /// Without it the sidecar cannot answer `/pausa` in a topic: it knows the chat key it is in and
    /// nothing else, so finding "the errand of this topic" in a list that does not say which topic
    /// each errand is on is not possible. It is also what makes `/assuntos` worth reading — a list
    /// of names with no topics tells you almost nothing.
    #[tokio::test]
    async fn an_errand_carries_the_topic_it_sits_on() {
        let pool = test_pool().await;
        let id = create(&pool, "carros", "-100200300:7").await.unwrap();

        for (label, errand) in [
            (
                "resolve",
                resolve(&pool, "-100200300:7").await.unwrap().unwrap(),
            ),
            ("get", get(&pool, id).await.unwrap().unwrap()),
            (
                "list",
                list(&pool).await.unwrap().into_iter().next().unwrap(),
            ),
        ] {
            assert_eq!(errand.chat_key, "-100200300:7", "{label}");
        }
    }

    /// A fixed instant, so a test about arming can say WHICH instant and not merely "recently".
    fn at(text: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// A rule comes back the way it was written down.
    ///
    /// The whole of piece 4 in one assertion: a project's rules live in a YAML file inside its
    /// repository, and an errand has no repository, so the rule has to survive in the database or
    /// there is nowhere for it to be. The prompt is checked as carefully as the cron because the
    /// prompt is the half that will be sent to a model with nobody watching.
    #[tokio::test]
    async fn a_rule_comes_back_the_way_it_was_written() {
        let pool = test_pool().await;
        let errand = create(&pool, "carros", "-100200300:7").await.unwrap();

        let rule_id = create_rule(
            &pool,
            errand,
            "manhã",
            "0 8 * * *",
            "vê se apareceram anúncios novos",
            Some("Europe/Lisbon"),
            at("2026-08-16T09:00:00Z"),
        )
        .await
        .unwrap();

        let rules = list_rules(&pool, errand).await.unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, rule_id);
        assert_eq!(rules[0].errand_id, errand);
        assert_eq!(rules[0].name, "manhã");
        assert_eq!(rules[0].cron, "0 8 * * *");
        assert_eq!(rules[0].prompt, "vê se apareceram anúncios novos");
        assert_eq!(rules[0].timezone.as_deref(), Some("Europe/Lisbon"));
    }

    /// A cron nothing can read is refused while a person is still holding the keyboard.
    ///
    /// This is the one thing an errand's rules can do that a project's cannot, and it is worth
    /// having: a project rule with a typo is armed anyway and announced once to the feed
    /// (`scheduler.rs`), because by the time the daemon sees the file the person who wrote it has
    /// gone. A rule arrives here over a route, so the refusal reaches whoever typed it.
    ///
    /// Nothing is stored, which is the half that makes it a refusal rather than a warning.
    #[tokio::test]
    async fn a_cron_that_will_never_fire_is_refused_at_the_door() {
        let pool = test_pool().await;
        let errand = create(&pool, "carros", "-100200300:7").await.unwrap();

        let refusal = create_rule(
            &pool,
            errand,
            "manhã",
            "todas as manhãs",
            "vê os anúncios",
            None,
            at("2026-08-16T09:00:00Z"),
        )
        .await
        .unwrap_err();

        assert!(
            matches!(refusal, RuleError::Unreadable(_)),
            "expected an unreadable rule, got {refusal:?}"
        );
        assert!(
            refusal.to_string().contains("todas as manhãs"),
            "the refusal has to quote what was written, or it cannot be corrected: {refusal}"
        );
        assert!(list_rules(&pool, errand).await.unwrap().is_empty());
    }

    /// An unknown zone is an error, never a silent UTC.
    ///
    /// `scheduler.rs:62-67` gives the reason and it holds identically here: reading `Europe/Lisbon`
    /// as UTC fires the rule an hour off and looks like it worked. The difference is that there the
    /// rule is skipped at every tick, and here it never gets written at all.
    #[tokio::test]
    async fn a_zone_that_does_not_exist_is_refused_rather_than_read_as_utc() {
        let pool = test_pool().await;
        let errand = create(&pool, "carros", "-100200300:7").await.unwrap();

        let refusal = create_rule(
            &pool,
            errand,
            "manhã",
            "0 8 * * *",
            "vê os anúncios",
            Some("Europe/Lisboa"),
            at("2026-08-16T09:00:00Z"),
        )
        .await
        .unwrap_err();

        assert!(
            matches!(refusal, RuleError::Unreadable(_)),
            "expected an unreadable rule, got {refusal:?}"
        );
        assert!(
            refusal.to_string().contains("Europe/Lisboa"),
            "the refusal has to name the zone that was not found: {refusal}"
        );
        assert!(list_rules(&pool, errand).await.unwrap().is_empty());
    }

    /// One errand cannot hold two rules of one name; two errands can.
    ///
    /// The name is what the scheduler keys a rule's state by within its errand, and what a person
    /// says to delete one. Two of them under one errand leaves both questions without an answer.
    /// Across errands it is not ambiguous at all — "manhã" is what everyone calls the morning one.
    #[tokio::test]
    async fn one_errand_cannot_hold_two_rules_of_one_name_but_two_errands_can() {
        let pool = test_pool().await;
        let carros = create(&pool, "carros", "-100200300:7").await.unwrap();
        let casa = create(&pool, "casa", "-100200300:9").await.unwrap();
        let now = at("2026-08-16T09:00:00Z");

        create_rule(&pool, carros, "manhã", "0 8 * * *", "anúncios", None, now)
            .await
            .unwrap();

        let refusal = create_rule(&pool, carros, "manhã", "0 9 * * *", "outra", None, now)
            .await
            .unwrap_err();
        assert!(
            matches!(refusal, RuleError::Duplicate),
            "expected a duplicate name, got {refusal:?}"
        );

        create_rule(&pool, casa, "manhã", "0 8 * * *", "contas", None, now)
            .await
            .expect("the same name under another errand is not a collision");

        assert_eq!(list_rules(&pool, carros).await.unwrap().len(), 1);
        assert_eq!(list_rules(&pool, casa).await.unwrap().len(), 1);
    }

    /// A rule id from another errand is not yours to delete.
    ///
    /// The id arrives in a path and the errand arrives in the path beside it, so nothing stops a
    /// caller pairing one errand with another's rule. Keyed by both, the pairing simply matches no
    /// row — and the caller is told nothing was deleted rather than being told it succeeded.
    #[tokio::test]
    async fn a_rule_of_another_errand_is_not_yours_to_delete() {
        let pool = test_pool().await;
        let carros = create(&pool, "carros", "-100200300:7").await.unwrap();
        let casa = create(&pool, "casa", "-100200300:9").await.unwrap();
        let now = at("2026-08-16T09:00:00Z");
        let rule = create_rule(&pool, carros, "manhã", "0 8 * * *", "anúncios", None, now)
            .await
            .unwrap();

        assert!(
            !delete_rule(&pool, casa, rule).await.unwrap(),
            "another errand's rule must not be reachable through this one"
        );
        assert_eq!(list_rules(&pool, carros).await.unwrap().len(), 1);

        assert!(delete_rule(&pool, carros, rule).await.unwrap());
        assert!(list_rules(&pool, carros).await.unwrap().is_empty());
    }

    /// A new rule is armed at the moment it is written, and owes nothing for the windows before it.
    ///
    /// The same thing `scheduler.rs` does when it first sees a rule in a YAML file, and for a
    /// sharper reason here: the scheduler counts a window as due when the cron's next occurrence
    /// after `last_fired_at` has passed. Left unset, the rule would be owed every window since
    /// whatever the column defaulted to — and a rule created this evening would fire immediately,
    /// which is not what anybody means by "every morning at eight".
    #[tokio::test]
    async fn a_new_rule_is_armed_now_and_owes_nothing_for_the_windows_before_it() {
        let pool = test_pool().await;
        let errand = create(&pool, "carros", "-100200300:7").await.unwrap();
        let now = at("2026-08-16T21:30:00Z");

        create_rule(&pool, errand, "manhã", "0 8 * * *", "anúncios", None, now)
            .await
            .unwrap();

        let rules = list_rules(&pool, errand).await.unwrap();
        assert_eq!(rules[0].last_fired_at, now.to_rfc3339());
        assert_eq!(rules[0].fires_today, 0);
    }
}
