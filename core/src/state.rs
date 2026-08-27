use crate::auth::Token;
use crate::runner::CommandRunner;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::AbortHandle;

/// Production default for how long a single run may take before it's marked `"timed_out"` and its
/// process killed. Tests override `AppState.run_timeout` to something much shorter.
///
/// This is the clock for a run somebody is waiting on. Autonomous runs get a longer one — see
/// [`AUTONOMOUS_RUN_TIMEOUT_MULTIPLIER`].
pub const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_secs(600);

/// How much longer an autonomous run may take than an interactive one.
///
/// A wall clock and a progress deadline answer different questions. The progress deadline
/// ([`DEFAULT_PROGRESS_TIMEOUT`]) is the one that catches a run that has stopped getting anywhere;
/// the wall clock is a backstop against a run that keeps making progress and never finishes. Set
/// below the length of an ordinary task, a backstop stops being a backstop and becomes the usual
/// way runs end.
///
/// **Measured, 2026-07-30.** Three `worktree` runs of one real task were killed at the 600-second
/// wall clock with 400+ streamed events each and sub-tasks still open, while the same task in
/// `real` mode finished in 20 turns. The failure was the deadline, not the runs: nothing about
/// them was stuck, and each one spent money the daemon then had to write off. A worktree run does
/// setup, edits, a build and a gate; 10 minutes does not buy that.
///
/// 30 minutes is a bound, not a measurement — nobody has yet run one to completion to find out
/// what it needs. It is three times the observed floor and still finite, which is the property
/// that matters: an autonomous run must not be able to run all day.
pub const AUTONOMOUS_RUN_TIMEOUT_MULTIPLIER: u32 = 3;

/// Production default for how long a run may stay silent between streamed events.
///
/// This measures silence, not total run duration. The independent `DEFAULT_RUN_TIMEOUT` remains
/// unchanged at 600 seconds.
pub const DEFAULT_PROGRESS_TIMEOUT: Duration = Duration::from_secs(300);

/// Production default for how long a repository verification gate may run.
///
/// A gate is a subprocess over a repository the daemon does not control, so its deadline answers a
/// different question from how long the agent itself may run and must remain independently chosen.
pub const DEFAULT_GATE_TIMEOUT: Duration = Duration::from_secs(900);

/// In-flight runs' abort handles, keyed by `runs.id`.
pub type RunHandles = Arc<Mutex<HashMap<i64, AbortHandle>>>;

/// In-flight steerable runs' turn channels, keyed by `runs.id`.
///
/// `LaterTurn` and not `String`: a turn arriving after the one a run was launched with can carry
/// pictures too, which is what lets a conversation keep its process when somebody pastes one.
pub type RunMessages =
    Arc<Mutex<HashMap<i64, tokio::sync::mpsc::UnboundedSender<crate::runner::LaterTurn>>>>;

/// In-flight runs' transcripts as they fill, keyed by `runs.id`.
///
/// The third map of this shape, and the one whose absence means the most. `run_events` is written
/// ONCE, when a run ends (`runs::append_run_events` has two callers and both are terminal), so
/// nothing durable can be read while a run is working — the buffer behind this handle is the only
/// place its output exists in the meantime. The timeout branch already relies on that, which is why
/// it is the one path that salvages anything from a run the clock killed.
///
/// **Deliberately ephemeral.** A restarted daemon has no entry here for a run it did not start, and
/// a finished run is removed. Neither means the run produced nothing — it means the durable copy
/// (`runs.stdout`, `run_events`) is now the only one. A reader that shows an absent tail as an empty
/// transcript is lying about a run that may have written thousands of lines.
pub type RunTails = Arc<Mutex<HashMap<i64, Arc<Mutex<String>>>>>;

/// The email pillar's process-wide settings, resolved once at startup.
///
/// Grouped into one struct rather than spread across `AppState` because they are read together and
/// change together: spec §3.4 makes this config startup-time on purpose, so editing `.ai/email.yaml`
/// means restarting the daemon, never recompiling it.
/// `Debug` is written by hand rather than derived, and the test at the bottom of this file is what
/// keeps it that way — see `sidecar_token` below.
#[derive(Clone)]
pub struct EmailRuntime {
    /// False keeps every part of the pillar dormant — no polling, no triage, no digest.
    pub enabled: bool,
    /// Which triage classes are worth a notification. Empty means none, and that is a real setting.
    pub notify_classes: Vec<String>,
    pub digest_hour_utc: u8,
    pub retain_bodies_days: u8,
    /// The mailbox the sidecar collects from.
    ///
    /// Kept here so something can be ASKED which one it is. The shell reads the collection cursor
    /// per mailbox and had no route that reported the configured name, so it hard-coded `INBOX` —
    /// which reads empty, not wrong, for anyone collecting from anywhere else. That is the worst
    /// shape a wrong answer can take: it looks like nothing has arrived.
    pub mailbox: String,
    /// The mailbox the user's own sent mail is read from, when one is configured.
    pub sent_mailbox: Option<String>,
    /// The IMAP host and account, for saying WHICH mailbox this is. Never the password: that comes
    /// from Credential Manager, is handed to the sidecar process, and is not in this struct to leak.
    pub host: String,
    /// The submission host, or empty when nobody has configured one.
    ///
    /// Read by the send route as the first of its two "this daemon is not in a position to send"
    /// checks. Empty is the shipped state and must stay a refusal rather than a fallback to `host`:
    /// the IMAP server is not the submission server often enough that guessing would either fail
    /// the login or hand the message to somebody else's machine.
    pub smtp_host: String,
    pub username: String,
    pub poll_interval_secs: u64,
    /// The directory a triage run works in, so it never inherits the daemon's (spec §5.5).
    pub sandbox: std::path::PathBuf,
    /// Set once the hook barrier has been PROVEN at startup, and read by the triage loop before
    /// every batch.
    ///
    /// `enabled` alone is not enough to authorise reading mail into a prompt: the pillar's premise
    /// is that untrusted content never meets a tool, and an unverified barrier is not a barrier. The
    /// loop itself runs regardless, because it also owns retention — bodies already stored do not
    /// stop needing to expire because the barrier failed.
    pub armed: Arc<std::sync::atomic::AtomicBool>,
    /// The email sidecar's OWN key (`auth::Service::Email`), minted at startup — never the control
    /// token, and never `None` standing in for one.
    ///
    /// It lives here because the send route needs it at request time, and the alternative shapes
    /// are both worse: re-minting per request would rotate the key out from under a running
    /// sidecar, and reaching for `AppState::token` would hand the full daemon key to the one
    /// process this arrangement exists to keep it away from. `None` means minting failed or the
    /// pillar is off, and the route answers 503 — a deployment a person fixes, not a retry.
    ///
    /// Set once at startup and never mutated, like every other field here: this struct is read-only
    /// after `main.rs` builds it, which is why it can be shared behind an `Arc` without a lock.
    pub sidecar_token: Option<String>,
}

/// Describes everything except the one thing that must not be described.
///
/// Hand-written because `#[derive(Debug)]` on a struct holding a credential is a leak waiting for
/// its first `tracing::debug!(?state.email, …)` — and the person who writes that line will be
/// printing configuration, not a secret. Whether a key EXISTS is worth saying (it is the difference
/// between the two 503s this pillar can answer); what the key IS never is.
impl std::fmt::Debug for EmailRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmailRuntime")
            .field("enabled", &self.enabled)
            .field("notify_classes", &self.notify_classes)
            .field("digest_hour_utc", &self.digest_hour_utc)
            .field("retain_bodies_days", &self.retain_bodies_days)
            .field("mailbox", &self.mailbox)
            .field("sent_mailbox", &self.sent_mailbox)
            .field("host", &self.host)
            .field("smtp_host", &self.smtp_host)
            .field("username", &self.username)
            .field("poll_interval_secs", &self.poll_interval_secs)
            .field("sandbox", &self.sandbox)
            .field("armed", &self.armed)
            .field(
                "sidecar_token",
                &self.sidecar_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl Default for EmailRuntime {
    /// The pillar off, which is what every context that has not configured it should see —
    /// including every test that does not care about email.
    fn default() -> Self {
        Self {
            enabled: false,
            notify_classes: vec!["urgent".to_string()],
            digest_hour_utc: 7,
            retain_bodies_days: 14,
            mailbox: "INBOX".to_string(),
            sent_mailbox: None,
            host: String::new(),
            smtp_host: String::new(),
            username: String::new(),
            poll_interval_secs: 300,
            sandbox: std::path::PathBuf::new(),
            armed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sidecar_token: None,
        }
    }
}

impl EmailRuntime {
    /// `sidecar_token` is a parameter rather than something this reads from config, because it is
    /// not configuration: it is minted at startup against the database, and the only caller that
    /// can produce one is `main.rs`. Passing it in keeps this struct read-only afterwards.
    pub fn from_config(
        config: &crate::config::EmailConfig,
        sandbox: std::path::PathBuf,
        sidecar_token: Option<String>,
    ) -> Self {
        Self {
            enabled: config.enabled,
            notify_classes: config.notify_classes.clone(),
            digest_hour_utc: config.digest_hour_utc,
            retain_bodies_days: config.retain_bodies_days,
            mailbox: config.mailbox.clone(),
            sent_mailbox: config.sent_mailbox.clone(),
            host: config.host.clone(),
            smtp_host: config.smtp_host.clone(),
            username: config.username.clone(),
            poll_interval_secs: config.poll_interval_secs,
            sandbox,
            armed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sidecar_token,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub token: Token,
    pub pool: SqlitePool,
    pub runner: Arc<dyn CommandRunner>,
    /// Present only when startup proved a configured loopback model can hold the full local triage
    /// prompt. `None` preserves the CLI path, which is the feature's ship-dark default.
    pub triage_runner: Option<Arc<dyn CommandRunner>>,
    /// Why local triage is unavailable, when a local model WAS configured but could not be trusted.
    ///
    /// This is deliberately separate from `triage_runner` being `None`. `None` alone means "no local
    /// model configured", and that correctly falls back to the CLI — the ship-dark guarantee.
    /// `Some(reason)` means the operator ASKED for local inference and it could not be provided, and
    /// the whole reason they asked is that message bodies must not leave this machine. Falling back
    /// to the remote CLI there would violate that at exactly the moment nobody is watching, so triage
    /// stops instead and mail queues, the same way it already does for the kill switch and the budget.
    pub local_triage_disabled: Option<String>,
    /// The factory that builds the model answering a chat turn — the local one, the hosted one on
    /// OpenRouter, or a refusal — resolved per turn instead of two singletons baked at startup.
    ///
    /// Replaces what used to be two separate fields, `local_assistant` and `hosted_assistant`, each
    /// an `Option<Arc<LocalAssistant>>` built once at startup with a model string baked in. The
    /// ship-dark default they carried is preserved, but "no route configured" is now expressed the
    /// way this factory already expresses it — `Err(assistants::Refusal::RouteNotConfigured)` — and
    /// is deliberately NOT wrapped in a second `Option`: an `Option` around a type that can already
    /// say "not configured" would be two ways to say the same thing, which is two ways for a reader
    /// to get it wrong. An untouched install gets `assistants::NoAssistants` in production wiring's
    /// test doubles, or a `ConfiguredAssistants` whose routes are all unconfigured in production —
    /// either way, every route refuses rather than falling through to the cloud CLI.
    ///
    /// The local route falls back to `runner` above when this refuses (`None` was always the
    /// ship-dark default: every turn goes to `runner`, exactly as before). Unlike `triage_runner`, a
    /// failed probe here needs no separate "disabled" field and falls back instead of stopping — the
    /// two protect different things. Local triage exists so mail bodies do not leave, and falling
    /// back would defeat it. A local chat turn reads only the daemon's own state, so answering it in
    /// the cloud is what already happens today rather than a leak the operator asked to prevent.
    ///
    /// The hosted route does NOT get that same fallback: every route that reads this field and gets
    /// a refusal for `Brain::OpenRouter` must refuse outright rather than fall through to the cloud
    /// CLI — see `assistant::NO_HOSTED_MODEL` for why a hosted turn does not get the fallback a
    /// local one does.
    pub assistants: std::sync::Arc<dyn crate::assistants::Assistants>,
    /// The folder a person arranges their files in, canonicalised once at startup so every
    /// containment check compares against a path the filesystem has already resolved.
    ///
    /// It lived in `EmailRuntime` for its history — it began as the folder mail was filed into —
    /// and it stopped being the mail pillar's alone twice over: the Files tab writes here with the
    /// pillar off, and a team's workspace is a folder under this root that its own loop creates and
    /// collects. Three readers is where a field belongs to the daemon rather than to one pillar.
    ///
    /// `None` means startup could not make the directory, and every route beneath it answers 503
    /// (`http::files_root`). An `Option` rather than the empty path it used to be: "no folder" and
    /// "the folder at the empty path" are different facts, and only one of them can be a bug.
    pub files_root: Option<std::path::PathBuf>,
    /// Where this machine keeps its workflow bundles, resolved once at startup.
    ///
    /// The same shape `files_root` above has, and for the same two reasons. One: it is a path that
    /// depends on the machine rather than on any request, so resolving it per request would be one
    /// answer per caller to a question with one answer. Two: `None` is a real state — a machine
    /// with no home directory has nowhere for a library — and every route beneath it answers 503
    /// rather than showing an empty shelf, which would read as "you have installed nothing".
    ///
    /// A field and not `workflows::library_root()` called inline, because that is what makes the
    /// library a thing a test can point somewhere else. The alternative was an environment
    /// variable, which is process-global: two tests setting it would race, and `set_var` is
    /// `unsafe` in this edition for exactly that reason.
    pub workflow_library: Option<std::path::PathBuf>,
    /// The standing instructions a Telegram turn is launched with when the chat itself gave none,
    /// resolved once at startup from `.ai/telegram.yaml`.
    ///
    /// `None` — absent file, unreadable file, malformed file, or a `doctrine` that was blank —
    /// means every turn is launched exactly as it was before this field existed: nothing prepended,
    /// nothing changed. It is read only for `Origin::Telegram` turns (`assistant.rs`), and even
    /// then only fills the slot when the chat's own `system_prompt` is empty — a person's own
    /// instructions always win.
    pub telegram_doctrine: Option<String>,
    /// Read-only after startup, so it is shared rather than copied per clone of the state.
    pub email: Arc<EmailRuntime>,
    /// The voice pillar's settings, its transcriber and its HTTP client, resolved once at startup.
    ///
    /// One field rather than three because they are read together and switched on together, and
    /// because `VoiceRuntime::default()` means "off" — which is what keeps every test that does not
    /// care about voice from having to know it exists. Same reasoning as `email` above.
    pub voice: Arc<crate::voice::VoiceRuntime>,
    /// The web pillar: the trust allowlist, retention, and the client for the sidecar that is the
    /// only process here allowed to open a connection off this machine.
    pub web: Arc<crate::web::WebRuntime>,
    /// The browser pillar: whether it is on, and the client for the process that drives Chromium.
    ///
    /// The site lists are deliberately NOT here. They live in `browser_sites` and are read per
    /// request, because a list cached in the runtime would keep admitting a host somebody revoked —
    /// and the revocation screen would report success while the profile went on loading it.
    pub browser: Arc<crate::browser::BrowserRuntime>,
    /// The GitHub pillar: whether it is on, and the two lists deciding what runs without asking.
    ///
    /// The POLICY is here and not read per request, and that is the opposite choice from `browser`'s
    /// site lists a few lines up — for the reason `classifier.rs` gives about its fourth argument. A
    /// policy re-read per call is a policy a run could change in the middle of itself, and the hook
    /// and the resume would then answer differently about one command line.
    pub github: Arc<crate::github::GithubRuntime>,
    /// The calendar's settings — the default zone and the window a proposal may land in.
    ///
    /// Same shape and same reasoning as `email` and `voice` above: read together, changed together,
    /// and a `Default` that means "UTC, ordinary office hours" so no test that ignores calendars
    /// has to know this field exists.
    pub calendar: Arc<crate::calendar::CalendarRuntime>,
    /// The council: the roster, the per-seat clock, and the scoped key its seats reach the daemon
    /// with. `Default` means there is no council, which is the shipped state.
    pub council: Arc<crate::council::CouncilRuntime>,
    /// In-flight runs' abort handles, keyed by `runs.id`. Inserted when a run's task spawns
    /// (`runs::create_run`), removed when it completes/times out/is cancelled.
    pub run_handles: RunHandles,
    /// In-flight steerable runs' turn channels, keyed by `runs.id`. Inserted when a run that opted
    /// in spawns (`runs::spawn_run`), removed beside the abort handle when its task ends, so the two
    /// maps never disagree about which runs are still listening.
    ///
    /// Carries the caller's text, not the CLI's wire format: framing a turn is `runner.rs`'s job, so
    /// the receiving end wraps it. An entry here is plumbing and never permission —
    /// `http::post_run_message` decides on the run's own recorded facts and only then looks for the
    /// channel.
    pub run_messages: RunMessages,
    /// In-flight runs' transcripts as they fill, keyed by `runs.id`. Inserted beside the abort
    /// handle when a run's task spawns, removed beside it when the task ends — the three maps are
    /// populated and drained together so none of them can outlive the run it describes.
    pub run_tails: RunTails,
    /// Maximum silence between streamed events; independent of the total wall-clock run timeout.
    pub progress_timeout: Duration,
    pub run_timeout: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A credential this struct holds must not be published by the act of describing the struct.
    ///
    /// `EmailRuntime` is reached from `AppState`, and `AppState` is what every handler is handed —
    /// so a single `tracing::debug!(?state.email, ...)` written years from now, by someone printing
    /// configuration rather than a secret, would put the sidecar's key in a rotating log file on
    /// disk. The IMAP password is kept out of this struct entirely for exactly that reason (see the
    /// `host` field's comment); the sidecar's own token cannot be, because the send route needs it
    /// at request time. So it is stored under a `Debug` that refuses to print it, and this test is
    /// what stops a later `#[derive(Debug)]` from quietly undoing that.
    #[test]
    fn the_debug_of_the_email_runtime_does_not_carry_the_sidecar_token() {
        const TOKEN: &str = "super-secret-token-xyz";

        let runtime = EmailRuntime {
            smtp_host: "smtp.example.com".to_string(),
            sidecar_token: Some(TOKEN.into()),
            ..EmailRuntime::default()
        };

        let described = format!("{runtime:?}");
        assert!(
            !described.contains(TOKEN),
            "the sidecar's key is in the debug output: {described}"
        );
        // The struct must still be describable — redacting a secret is not an excuse to say nothing,
        // because a Debug that shows nothing is one nobody uses and everybody works around.
        assert!(
            described.contains("smtp.example.com"),
            "the rest of the configuration must still be readable: {described}"
        );
    }
}
