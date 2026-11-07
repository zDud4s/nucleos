//! Guided login: a new account is created by running the provider's own login command in the
//! account's own directory (spec section 15, B8). The daemon opens no browser: it surfaces
//! `auth_url` and the shell opens it. A CLI that wants a code gets it through `submit_code`; a CLI
//! that needs a terminal is an error state (no pseudo-terminal is served).
//!
//! The process is behind [`Spawner`] and the provider behind [`Lookup`], so the state machine is
//! tested with scripted processes and the fake adapter. The pasted code is written to the
//! process' stdin and never logged or stored.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use rand::RngExt;
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use super::adapters::adapter_for;
use super::provider::{LoginCommand, LoginEvent, ProviderAdapter};
use super::{Account, AccountId, AccountKind, AccountState, account_dir, startup, store};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginPhase {
    Starting,
    AwaitingBrowser,
    AwaitingCode,
    Verifying,
    Done,
    Failed,
    Cancelled,
}

impl LoginPhase {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LoginView {
    pub login_id: String,
    pub account_id: String,
    pub engine: String,
    pub label: String,
    pub phase: LoginPhase,
    pub auth_url: Option<String>,
    pub message: Option<String>,
}

/// A running login command: its output lines (stdout and stderr merged, ANSI stripped), a way to
/// write to its stdin, and a way to stop it. Dropping `kill` also stops it.
pub struct LoginProcess {
    pub lines: mpsc::Receiver<String>,
    pub stdin: mpsc::Sender<String>,
    pub kill: oneshot::Sender<()>,
}

pub type Spawner = Arc<dyn Fn(&LoginCommand) -> io::Result<LoginProcess> + Send + Sync>;
pub type Lookup = fn(&str) -> Option<&'static dyn ProviderAdapter>;

/// How long a CLI may take to finish a login (the owner has to do things in a browser).
const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);
/// How long to wait for the credential file after the CLI says it is done.
const CREDENTIAL_GRACE: Duration = Duration::from_secs(5);
/// A prompt without a newline ("Paste code here >") is emitted as a line after this idle time.
const PARTIAL_FLUSH: Duration = Duration::from_millis(300);
/// Finished logins stay readable this long.
const KEEP_FINISHED: Duration = Duration::from_secs(3600);
const MAX_CODE_LEN: usize = 2048;

/// Whether a login is for a new account (its directory is ours to delete) or an existing one.
enum Mode {
    New { dir: PathBuf },
    Relogin,
}

struct Entry {
    view: LoginView,
    stdin: Option<mpsc::Sender<String>>,
    kill: Option<oneshot::Sender<()>>,
    /// The directory a failed or cancelled new login removes.
    new_dir: Option<PathBuf>,
    finished: Option<Instant>,
}

struct Shared {
    entries: Mutex<HashMap<String, Entry>>,
    lookup: Lookup,
    spawner: Spawner,
    credential_grace: Duration,
    timeout: Duration,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Applies `change` unless the login already ended (a cancel wins over a late event).
    fn update(&self, login_id: &str, change: impl FnOnce(&mut LoginView)) {
        if let Some(entry) = self.lock().get_mut(login_id)
            && !entry.view.phase.is_terminal()
        {
            change(&mut entry.view);
        }
    }

    fn is_ended(&self, login_id: &str) -> bool {
        self.lock()
            .get(login_id)
            .is_none_or(|entry| entry.view.phase.is_terminal())
    }

    /// Ends the login in `phase` and stops its process. Returns the directory to delete.
    fn end(&self, login_id: &str, phase: LoginPhase, message: Option<String>) -> Option<PathBuf> {
        let mut entries = self.lock();
        let entry = entries.get_mut(login_id)?;
        if entry.view.phase.is_terminal() {
            return None;
        }
        entry.view.phase = phase;
        entry.view.message = message;
        entry.finished = Some(Instant::now());
        entry.stdin = None;
        if let Some(kill) = entry.kill.take() {
            let _ = kill.send(());
        }
        if phase == LoginPhase::Done {
            None
        } else {
            entry.new_dir.take()
        }
    }
}

/// Registry of in-flight logins.
pub struct Logins {
    shared: Arc<Shared>,
}

impl Logins {
    pub fn global() -> &'static Logins {
        static LOGINS: OnceLock<Logins> = OnceLock::new();
        LOGINS.get_or_init(|| {
            Logins::with(
                adapter_for,
                Arc::new(spawn_process),
                CREDENTIAL_GRACE,
                LOGIN_TIMEOUT,
            )
        })
    }

    pub fn with(
        lookup: Lookup,
        spawner: Spawner,
        credential_grace: Duration,
        timeout: Duration,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                entries: Mutex::new(HashMap::new()),
                lookup,
                spawner,
                credential_grace,
                timeout,
            }),
        }
    }

    /// New account: creates its directory, runs the login command, and on success inserts the row.
    /// Failed or cancelled: the directory is removed and no row exists.
    pub async fn start_new(
        &self,
        db: &SqlitePool,
        data_dir: &Path,
        engine: &str,
        label: &str,
    ) -> Result<LoginView, String> {
        let adapter = (self.shared.lookup)(engine)
            .ok_or_else(|| format!("engine `{engine}` is not in the account pool"))?;
        let label = clean_label(label)?;
        let label = label.as_str();
        let taken = store::list(db).await.map_err(db_error)?;
        if taken.iter().any(|a| a.engine == engine && a.label == label) {
            return Err(format!(
                "an account named `{label}` already exists for {engine}"
            ));
        }
        let id = AccountId(format!("{engine}:{:08x}", rand::rng().random::<u32>()));
        let dir = account_dir(data_dir, &id);
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let account = Account {
            id,
            engine: engine.to_owned(),
            provider: adapter.provider().to_owned(),
            label: label.to_owned(),
            kind: AccountKind::Subscription,
            credential_locator: Some(dir.to_string_lossy().into_owned()),
            state: AccountState::Active,
            created_at: now.clone(),
            updated_at: now,
        };
        self.begin(db, adapter, account, Mode::New { dir }).await
    }

    /// Existing account (re-login): same directory, never deleted on failure; done => active.
    pub async fn start_relogin(
        &self,
        db: &SqlitePool,
        id: &AccountId,
    ) -> Result<LoginView, String> {
        let account = store::get(db, id)
            .await
            .map_err(db_error)?
            .ok_or_else(|| format!("no account `{}`", id.as_str()))?;
        if account.kind != AccountKind::Subscription {
            return Err("only a subscription account has a login".into());
        }
        let adapter = (self.shared.lookup)(&account.engine)
            .ok_or_else(|| format!("engine `{}` is not in the account pool", account.engine))?;
        self.begin(db, adapter, account, Mode::Relogin).await
    }

    async fn begin(
        &self,
        db: &SqlitePool,
        adapter: &'static dyn ProviderAdapter,
        account: Account,
        mode: Mode,
    ) -> Result<LoginView, String> {
        let shared = &self.shared;
        {
            let mut entries = shared.lock();
            entries.retain(|_, e| e.finished.is_none_or(|at| at.elapsed() < KEEP_FINISHED));
            // One login per engine at a time: Codex's callback port would clash.
            let busy = entries
                .values()
                .any(|e| e.view.engine == account.engine && !e.view.phase.is_terminal());
            if busy {
                return Err(format!(
                    "a login for {} is already in progress",
                    account.engine
                ));
            }
        }
        let command = adapter
            .login_command(&account)
            .ok_or_else(|| format!("login is not supported for engine `{}`", account.engine))?;
        let login_id = format!("login_{:016x}", rand::rng().random::<u64>());
        let mut view = LoginView {
            login_id: login_id.clone(),
            account_id: account.id.as_str().to_owned(),
            engine: account.engine.clone(),
            label: account.label.clone(),
            phase: LoginPhase::Starting,
            auth_url: None,
            message: None,
        };
        let new_dir = match &mode {
            Mode::New { dir } => {
                tokio::fs::create_dir_all(dir)
                    .await
                    .map_err(|e| format!("could not create the account directory: {e}"))?;
                Some(dir.clone())
            }
            Mode::Relogin => None,
        };
        if command.needs_tty {
            view.phase = LoginPhase::Failed;
            view.message = Some(format!(
                "the {} login needs a terminal, which the daemon does not serve",
                account.engine
            ));
            remove_dir(new_dir.as_deref()).await;
            shared.lock().insert(
                login_id,
                Entry {
                    view: view.clone(),
                    stdin: None,
                    kill: None,
                    new_dir: None,
                    finished: Some(Instant::now()),
                },
            );
            return Ok(view);
        }
        let process = match (shared.spawner)(&command) {
            Ok(process) => process,
            Err(error) => {
                remove_dir(new_dir.as_deref()).await;
                return Err(format!("could not start `{}`: {error}", command.program));
            }
        };
        let LoginProcess { lines, stdin, kill } = process;
        shared.lock().insert(
            login_id.clone(),
            Entry {
                view: view.clone(),
                stdin: Some(stdin),
                kill: Some(kill),
                new_dir,
                finished: None,
            },
        );
        tokio::spawn(drive(
            shared.clone(),
            db.clone(),
            login_id,
            adapter,
            account,
            mode,
            lines,
        ));
        Ok(view)
    }

    pub fn view(&self, login_id: &str) -> Option<LoginView> {
        self.shared.lock().get(login_id).map(|e| e.view.clone())
    }

    /// Writes the code and a newline to the login process' stdin.
    pub async fn submit_code(&self, login_id: &str, code: &str) -> Result<LoginView, String> {
        let code = code.trim();
        if code.is_empty() || code.len() > MAX_CODE_LEN || code.chars().any(char::is_control) {
            return Err("the code is empty, too long or has control characters".into());
        }
        let (stdin, view) = {
            let mut entries = self.shared.lock();
            let entry = entries.get_mut(login_id).ok_or("no such login")?;
            if !matches!(
                entry.view.phase,
                LoginPhase::AwaitingBrowser | LoginPhase::AwaitingCode
            ) {
                return Err("the login is not waiting for a code".into());
            }
            let stdin = entry.stdin.clone().ok_or("the login has no open input")?;
            entry.view.phase = LoginPhase::Verifying;
            entry.view.message = None;
            (stdin, entry.view.clone())
        };
        stdin
            .send(format!("{code}\n"))
            .await
            .map_err(|_| "the login process has already ended".to_owned())?;
        Ok(view)
    }

    pub async fn cancel(&self, login_id: &str) -> Option<LoginView> {
        let dir = self
            .shared
            .end(login_id, LoginPhase::Cancelled, Some("cancelled".into()));
        remove_dir(dir.as_deref()).await;
        self.view(login_id)
    }
}

/// Reads the login's output until it ends, moving the entry through its phases.
async fn drive(
    shared: Arc<Shared>,
    db: SqlitePool,
    login_id: String,
    adapter: &'static dyn ProviderAdapter,
    account: Account,
    mode: Mode,
    mut lines: mpsc::Receiver<String>,
) {
    let deadline = tokio::time::Instant::now() + shared.timeout;
    let outcome = loop {
        match tokio::time::timeout_at(deadline, lines.recv()).await {
            Err(_) => break Err("the login timed out".to_owned()),
            // The process ended: the credentials decide.
            Ok(None) => break verify(&shared, adapter, &account).await,
            Ok(Some(line)) => match adapter.parse_login_line(&line) {
                Some(LoginEvent::AuthUrl(url)) => shared.update(&login_id, |view| {
                    view.phase = LoginPhase::AwaitingBrowser;
                    view.auth_url = Some(url);
                }),
                Some(LoginEvent::CodePrompt) => shared.update(&login_id, |view| {
                    view.phase = LoginPhase::AwaitingCode;
                    view.message = Some("Paste the code the browser shows".into());
                }),
                Some(LoginEvent::Succeeded) => {
                    shared.update(&login_id, |view| view.phase = LoginPhase::Verifying);
                    break verify(&shared, adapter, &account).await;
                }
                Some(LoginEvent::Failed(why)) => break Err(why),
                None => {}
            },
        }
    };
    if shared.is_ended(&login_id) {
        return;
    }
    let outcome = match outcome {
        Ok(()) => save(&db, &account, &mode).await,
        Err(why) => Err(why),
    };
    match outcome {
        Ok(()) => {
            shared.end(&login_id, LoginPhase::Done, None);
            startup::accounts_changed(&db).await;
        }
        Err(why) => {
            let why: String = why.chars().take(300).collect();
            let dir = shared.end(&login_id, LoginPhase::Failed, Some(why));
            remove_dir(dir.as_deref()).await;
        }
    }
}

/// Logged in = a credential file exists. An account on the CLI's own directory has none to look
/// for, so the CLI's word is enough.
async fn verify(
    shared: &Shared,
    adapter: &dyn ProviderAdapter,
    account: &Account,
) -> Result<(), String> {
    let paths = adapter.credential_paths(account);
    if paths.is_empty() {
        return Ok(());
    }
    let end = Instant::now() + shared.credential_grace;
    loop {
        for path in &paths {
            if tokio::fs::try_exists(path).await.unwrap_or(false) {
                return Ok(());
            }
        }
        if Instant::now() >= end {
            return Err("the login ended but no credentials appeared".into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A new account gets its row; a re-login brings a `needs_login` account back (a paused one
/// stays paused: the owner took it out of the pool).
async fn save(db: &SqlitePool, account: &Account, mode: &Mode) -> Result<(), String> {
    match mode {
        Mode::New { .. } => store::insert(db, account)
            .await
            .map_err(|e| format!("could not save the account: {e}")),
        Mode::Relogin => {
            if account.state == AccountState::NeedsLogin {
                store::set_state(db, &account.id, AccountState::Active)
                    .await
                    .map_err(db_error)?;
            }
            Ok(())
        }
    }
}

async fn remove_dir(dir: Option<&Path>) {
    if let Some(dir) = dir
        && let Err(error) = tokio::fs::remove_dir_all(dir).await
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "could not remove the directory of an abandoned login");
    }
}

/// An account name: trimmed, 1 to 64 characters, no control characters.
pub fn clean_label(label: &str) -> Result<String, String> {
    let label = label.trim();
    if label.is_empty() || label.chars().count() > 64 || label.chars().any(char::is_control) {
        return Err("an account name is 1 to 64 characters, without control characters".into());
    }
    Ok(label.to_owned())
}

fn db_error(error: sqlx::Error) -> String {
    tracing::warn!(%error, "account login: database error");
    "the account database could not be read".to_owned()
}

/// The production spawner: the real CLI, output lines merged, stdin open for the code.
fn spawn_process(command: &LoginCommand) -> io::Result<LoginProcess> {
    let mut process = tokio::process::Command::new(&command.program);
    process
        .args(&command.args)
        .envs(command.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = process.spawn()?;
    let (line_tx, lines) = mpsc::channel(64);
    if let Some(out) = child.stdout.take() {
        tokio::spawn(pump(out, line_tx.clone()));
    }
    if let Some(err) = child.stderr.take() {
        tokio::spawn(pump(err, line_tx.clone()));
    }
    drop(line_tx);
    let (stdin, mut stdin_rx) = mpsc::channel::<String>(4);
    if let Some(mut pipe) = child.stdin.take() {
        tokio::spawn(async move {
            while let Some(text) = stdin_rx.recv().await {
                if pipe.write_all(text.as_bytes()).await.is_err() || pipe.flush().await.is_err() {
                    break;
                }
            }
        });
    }
    let (kill, mut kill_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        tokio::select! {
            _ = &mut kill_rx => { let _ = child.kill().await; }
            _ = child.wait() => {}
        }
    });
    Ok(LoginProcess { lines, stdin, kill })
}

/// Splits a stream into lines on `\n` or `\r`. A partial line idle for [`PARTIAL_FLUSH`] is
/// emitted as it is: CLI prompts end without a newline.
async fn pump<R: AsyncRead + Unpin>(mut reader: R, tx: mpsc::Sender<String>) {
    let mut chunk = [0u8; 1024];
    let mut partial: Vec<u8> = Vec::new();
    loop {
        let read = if partial.is_empty() {
            Ok(reader.read(&mut chunk).await)
        } else {
            tokio::time::timeout(PARTIAL_FLUSH, reader.read(&mut chunk)).await
        };
        let mut ready: Vec<Vec<u8>> = Vec::new();
        let mut done = false;
        match read {
            Err(_) => ready.push(std::mem::take(&mut partial)),
            Ok(Ok(0)) | Ok(Err(_)) => {
                ready.push(std::mem::take(&mut partial));
                done = true;
            }
            Ok(Ok(n)) => {
                for &byte in &chunk[..n] {
                    if byte == b'\n' || byte == b'\r' {
                        ready.push(std::mem::take(&mut partial));
                    } else {
                        partial.push(byte);
                    }
                }
            }
        }
        for bytes in ready {
            if let Some(line) = clean_line(&bytes)
                && tx.send(line).await.is_err()
            {
                return;
            }
        }
        if done {
            return;
        }
    }
}

/// UTF-8 text without ANSI escape sequences, trimmed; None when nothing is left.
fn clean_line(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            clean.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters up to a final byte in '@'..='~'.
            Some('[') => {
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or ST.
            Some(']') => {
                while let Some(next) = chars.next() {
                    if next == '\u{7}' || (next == '\u{1b}' && chars.next() == Some('\\')) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    let clean = clean.trim();
    (!clean.is_empty()).then(|| clean.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::adapters::fake::FakeAdapter;
    use crate::accounts::provider::SessionPortability;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    static FAKE: FakeAdapter = FakeAdapter {
        engine: "fake",
        provider: "fakeco",
        portability: SessionPortability::NotPortable,
        cache: None,
    };

    fn fake_lookup(engine: &str) -> Option<&'static dyn ProviderAdapter> {
        (engine == "fake").then_some(&FAKE as &'static dyn ProviderAdapter)
    }

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        crate::storage::MIGRATOR.run(&pool).await.unwrap();
        pool
    }

    #[derive(Clone, Copy)]
    enum Step {
        Say(&'static str),
        WaitCode,
        WriteCredential,
        Hang,
    }

    /// A process that plays `steps`; `WriteCredential` creates the fake adapter's credential file.
    fn scripted(steps: Vec<Step>) -> Spawner {
        Arc::new(move |command: &LoginCommand| {
            let home = command
                .env
                .iter()
                .find(|(key, _)| key == "FAKE_HOME")
                .map(|(_, value)| PathBuf::from(value));
            let (line_tx, lines) = mpsc::channel(16);
            let (stdin, mut stdin_rx) = mpsc::channel::<String>(4);
            let (kill, kill_rx) = oneshot::channel::<()>();
            let steps = steps.clone();
            tokio::spawn(async move {
                let mut kill_rx = kill_rx;
                for step in steps {
                    match step {
                        Step::Say(text) => {
                            let _ = line_tx.send(text.to_owned()).await;
                        }
                        Step::WaitCode => {
                            stdin_rx.recv().await;
                        }
                        Step::WriteCredential => {
                            if let Some(home) = &home {
                                std::fs::write(home.join("fake-auth.json"), "{}").unwrap();
                            }
                        }
                        Step::Hang => {
                            let _ = (&mut kill_rx).await;
                            return;
                        }
                    }
                }
            });
            Ok(LoginProcess { lines, stdin, kill })
        })
    }

    fn scripted_logins(steps: Vec<Step>) -> Logins {
        Logins::with(
            fake_lookup,
            scripted(steps),
            Duration::ZERO,
            Duration::from_secs(30),
        )
    }

    async fn reach(logins: &Logins, id: &str, phase: LoginPhase) -> LoginView {
        for _ in 0..200 {
            let view = logins.view(id).unwrap();
            if view.phase == phase {
                return view;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("login never reached {phase:?}: {:?}", logins.view(id));
    }

    fn data_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn phases_serialize_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&LoginPhase::AwaitingBrowser).unwrap(),
            "\"awaiting_browser\""
        );
        assert!(Logins::global().view("nope").is_none());
    }

    #[tokio::test]
    async fn a_new_login_surfaces_the_url_takes_the_code_and_inserts_the_account() {
        let db = pool().await;
        let data = data_dir();
        let logins = scripted_logins(vec![
            Step::Say("URL https://auth.example/login?x=1"),
            Step::Say("CODE?"),
            Step::WaitCode,
            Step::WriteCredential,
            Step::Say("OK"),
            Step::Hang,
        ]);
        let started = logins
            .start_new(&db, data.path(), "fake", "work")
            .await
            .unwrap();
        let waiting = reach(&logins, &started.login_id, LoginPhase::AwaitingCode).await;
        assert_eq!(
            waiting.auth_url.as_deref(),
            Some("https://auth.example/login?x=1")
        );
        let account_id = AccountId(started.account_id.clone());
        assert!(
            store::get(&db, &account_id).await.unwrap().is_none(),
            "no row before the login ends"
        );
        let after = logins
            .submit_code(&started.login_id, " 123-456 ")
            .await
            .unwrap();
        assert_eq!(after.phase, LoginPhase::Verifying);
        reach(&logins, &started.login_id, LoginPhase::Done).await;
        let stored = store::get(&db, &account_id).await.unwrap().unwrap();
        assert_eq!(stored.label, "work");
        assert_eq!(stored.state, AccountState::Active);
        assert_eq!(stored.provider, "fakeco");
        assert!(
            account_dir(data.path(), &account_id)
                .join("fake-auth.json")
                .exists()
        );
    }

    #[tokio::test]
    async fn a_failed_new_login_removes_the_directory_and_leaves_no_row() {
        let db = pool().await;
        let data = data_dir();
        let logins = scripted_logins(vec![Step::Say("FAIL denied by the provider")]);
        let started = logins
            .start_new(&db, data.path(), "fake", "work")
            .await
            .unwrap();
        let failed = reach(&logins, &started.login_id, LoginPhase::Failed).await;
        assert_eq!(failed.message.as_deref(), Some("denied by the provider"));
        let id = AccountId(started.account_id);
        assert!(!account_dir(data.path(), &id).exists());
        assert!(store::get(&db, &id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn cancelling_removes_the_directory_and_stops_the_process() {
        let db = pool().await;
        let data = data_dir();
        let logins = scripted_logins(vec![Step::Say("URL https://auth.example/x"), Step::Hang]);
        let started = logins
            .start_new(&db, data.path(), "fake", "work")
            .await
            .unwrap();
        reach(&logins, &started.login_id, LoginPhase::AwaitingBrowser).await;
        let cancelled = logins.cancel(&started.login_id).await.unwrap();
        assert_eq!(cancelled.phase, LoginPhase::Cancelled);
        let id = AccountId(started.account_id);
        assert!(!account_dir(data.path(), &id).exists());
        assert!(store::get(&db, &id).await.unwrap().is_none());
        assert!(logins.cancel("nope").await.is_none());
    }

    #[tokio::test]
    async fn a_process_that_ends_without_credentials_fails_the_login() {
        let db = pool().await;
        let data = data_dir();
        let logins = scripted_logins(vec![Step::Say("URL https://auth.example/x")]);
        let started = logins
            .start_new(&db, data.path(), "fake", "work")
            .await
            .unwrap();
        let failed = reach(&logins, &started.login_id, LoginPhase::Failed).await;
        assert!(failed.message.unwrap().contains("no credentials"));
    }

    #[tokio::test]
    async fn credentials_that_appear_complete_a_login_that_never_said_so() {
        let db = pool().await;
        let data = data_dir();
        let logins = scripted_logins(vec![Step::WriteCredential]);
        let started = logins
            .start_new(&db, data.path(), "fake", "work")
            .await
            .unwrap();
        reach(&logins, &started.login_id, LoginPhase::Done).await;
        assert!(
            store::get(&db, &AccountId(started.account_id))
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_relogin_brings_a_needs_login_account_back_and_keeps_its_directory_on_failure() {
        let db = pool().await;
        let data = data_dir();
        let id = AccountId("fake:0000000a".into());
        let dir = account_dir(data.path(), &id);
        std::fs::create_dir_all(&dir).unwrap();
        store::insert(
            &db,
            &Account {
                id: id.clone(),
                engine: "fake".into(),
                provider: "fakeco".into(),
                label: "old".into(),
                kind: AccountKind::Subscription,
                credential_locator: Some(dir.to_string_lossy().into_owned()),
                state: AccountState::NeedsLogin,
                created_at: "t".into(),
                updated_at: "t".into(),
            },
        )
        .await
        .unwrap();

        let failing = scripted_logins(vec![Step::Say("FAIL nope")]);
        let started = failing.start_relogin(&db, &id).await.unwrap();
        reach(&failing, &started.login_id, LoginPhase::Failed).await;
        assert!(dir.exists(), "a re-login never deletes the directory");
        assert_eq!(
            store::get(&db, &id).await.unwrap().unwrap().state,
            AccountState::NeedsLogin
        );

        let working = scripted_logins(vec![Step::WriteCredential, Step::Say("OK")]);
        let started = working.start_relogin(&db, &id).await.unwrap();
        reach(&working, &started.login_id, LoginPhase::Done).await;
        assert_eq!(
            store::get(&db, &id).await.unwrap().unwrap().state,
            AccountState::Active
        );
        assert!(
            working
                .start_relogin(&db, &AccountId("fake:ffffffff".into()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_tty_only_login_is_an_error_state_and_leaves_nothing_behind() {
        static TTY: FakeTty = FakeTty;
        struct FakeTty;
        impl ProviderAdapter for FakeTty {
            fn engine(&self) -> &'static str {
                "tty"
            }
            fn provider(&self) -> &'static str {
                "ttyco"
            }
            fn account_env(&self, _: &Account) -> Vec<(String, String)> {
                Vec::new()
            }
            fn login_command(&self, _: &Account) -> Option<LoginCommand> {
                Some(LoginCommand {
                    program: "tty-login".into(),
                    args: Vec::new(),
                    env: Vec::new(),
                    needs_tty: true,
                })
            }
            fn parse_login_line(&self, _: &str) -> Option<LoginEvent> {
                None
            }
            fn credential_paths(&self, _: &Account) -> Vec<PathBuf> {
                Vec::new()
            }
            fn usage_source(&self) -> crate::quota::Fidelity {
                crate::quota::Fidelity::Unmeasured
            }
            fn session_portability(&self) -> SessionPortability {
                SessionPortability::NotPortable
            }
            fn cache_model(&self) -> Option<crate::accounts::provider::CacheModel> {
                None
            }
            fn classify_failure(
                &self,
                _: &str,
                _: &str,
            ) -> Option<crate::accounts::provider::LimitHit> {
                None
            }
        }
        fn lookup(_: &str) -> Option<&'static dyn ProviderAdapter> {
            Some(&TTY)
        }
        let db = pool().await;
        let data = data_dir();
        let logins = Logins::with(
            lookup,
            scripted(vec![]),
            Duration::ZERO,
            Duration::from_secs(5),
        );
        let view = logins
            .start_new(&db, data.path(), "tty", "x")
            .await
            .unwrap();
        assert_eq!(view.phase, LoginPhase::Failed);
        assert!(view.message.unwrap().contains("terminal"));
        assert!(!account_dir(data.path(), &AccountId(view.account_id)).exists());
    }

    #[tokio::test]
    async fn the_code_is_taken_only_while_waiting_and_only_when_sane() {
        let db = pool().await;
        let data = data_dir();
        let logins = scripted_logins(vec![Step::Hang]);
        let started = logins
            .start_new(&db, data.path(), "fake", "work")
            .await
            .unwrap();
        assert!(logins.submit_code("nope", "1").await.is_err());
        // Starting: the CLI has printed nothing yet.
        assert!(logins.submit_code(&started.login_id, "1").await.is_err());
        logins.cancel(&started.login_id).await;
        assert!(logins.submit_code(&started.login_id, "1").await.is_err());

        let waiting = scripted_logins(vec![Step::Say("CODE?"), Step::Hang]);
        let started = waiting
            .start_new(&db, data.path(), "fake", "other")
            .await
            .unwrap();
        reach(&waiting, &started.login_id, LoginPhase::AwaitingCode).await;
        assert!(waiting.submit_code(&started.login_id, "  ").await.is_err());
        assert!(
            waiting
                .submit_code(&started.login_id, "a\nb")
                .await
                .is_err()
        );
        waiting.cancel(&started.login_id).await;
    }

    #[tokio::test]
    async fn starts_are_refused_for_unknown_engines_taken_names_and_a_busy_engine() {
        let db = pool().await;
        let data = data_dir();
        let logins = scripted_logins(vec![Step::Hang]);
        assert!(
            logins
                .start_new(&db, data.path(), "local", "x")
                .await
                .is_err()
        );
        assert!(
            logins
                .start_new(&db, data.path(), "fake", "  ")
                .await
                .is_err()
        );
        let first = logins
            .start_new(&db, data.path(), "fake", "work")
            .await
            .unwrap();
        let busy = logins
            .start_new(&db, data.path(), "fake", "personal")
            .await
            .unwrap_err();
        assert!(busy.contains("already in progress"), "{busy}");
        logins.cancel(&first.login_id).await;
        store::insert(
            &db,
            &Account {
                id: AccountId("fake:00000001".into()),
                engine: "fake".into(),
                provider: "fakeco".into(),
                label: "work".into(),
                kind: AccountKind::Subscription,
                credential_locator: None,
                state: AccountState::Active,
                created_at: "t".into(),
                updated_at: "t".into(),
            },
        )
        .await
        .unwrap();
        let taken = logins
            .start_new(&db, data.path(), "fake", "work")
            .await
            .unwrap_err();
        assert!(taken.contains("already exists"), "{taken}");
    }

    #[tokio::test]
    async fn output_is_split_on_newlines_and_an_idle_prompt_is_flushed_without_ansi() {
        let (mut writer, reader) = tokio::io::duplex(256);
        let (tx, mut rx) = mpsc::channel(8);
        tokio::spawn(pump(reader, tx));
        writer
            .write_all(b"\x1b[1mOpen\x1b[0m https://a.example/x\r\n\n")
            .await
            .unwrap();
        assert_eq!(rx.recv().await.unwrap(), "Open https://a.example/x");
        writer.write_all(b"Paste code here > ").await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), "Paste code here >");
        drop(writer);
        assert!(rx.recv().await.is_none());
        assert_eq!(clean_line(b"\x1b]0;title\x07  \x1b[2K"), None);
    }
}
