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
    pub brain: Brain,
    /// Relative to the files root, and only ever resolved through [`folder_path`].
    pub folder: String,
    pub status: Status,
}

/// The errand of this topic, if there is one.
///
/// The absence of a row IS the answer, and it is the common one: every message from every group runs
/// through here, and almost none of them are errands. Nothing guesses an errand from the SHAPE of a
/// chat key, for the same reason `Origin` exists in `assistant.rs` — a fact we wrote down beats a
/// guess about a numbering scheme somebody else owns.
pub async fn resolve(pool: &sqlx::SqlitePool, chat_key: &str) -> sqlx::Result<Option<Errand>> {
    let row = sqlx::query_as::<_, ErrandRow>(
        "SELECT id, name, brain, folder, status FROM errands WHERE chat_key = ?",
    )
    .bind(chat_key)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(from_row))
}

/// The five columns every read of this table selects.
type ErrandRow = (i64, String, String, String, String);

/// The single place a row becomes an [`Errand`].
///
/// Written once because `brain` and `status` are both `TEXT` in a five-column tuple: a second
/// mapping that read one into the other would compile, and the error would surface as an errand that
/// is somehow paused because it runs on a local model.
fn from_row((id, name, brain, folder, status): ErrandRow) -> Errand {
    Errand {
        id,
        name,
        brain: Brain::from_wire(&brain),
        folder,
        status: Status::from_wire(&status),
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
        "SELECT id, name, brain, folder, status FROM errands
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
        "SELECT id, name, brain, folder, status FROM errands WHERE id = ?",
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
}
