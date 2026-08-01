use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tokio::process::Command;

/// First delay after a sidecar dies. A single crash should cost about as much as a restart.
const RESTART_BASE: Duration = Duration::from_secs(2);
/// Ceiling for the backoff: long enough that a binary which was never built is nearly free, short
/// enough that a sidecar which starts working again is back within a minute.
const RESTART_MAX: Duration = Duration::from_secs(60);

/// What each supervised sidecar is doing, as its own supervisor last saw it.
///
/// Process-wide rather than a field on `AppState`, the way `assistant::BUSY_CHATS` is: there is one
/// set of sidecars per daemon, not per handler state, and the supervisors are spawned as free tasks
/// that outlive every request.
///
/// It exists because a dead sidecar was previously visible nowhere. The supervisor restarts it and
/// writes one line to a log; if the email poller was failing to start, the Mail tab simply looked
/// like a quiet mailbox — the same thing an empty inbox looks like.
static SIDECARS: LazyLock<Mutex<BTreeMap<String, SidecarState>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// One sidecar, and what has happened to it since the daemon started.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SidecarState {
    pub name: String,
    /// `running`, or `down` while it is backing off before the next attempt.
    pub state: &'static str,
    /// When the current process started. `None` means it is not running right now.
    pub started_at: Option<String>,
    /// What went wrong last: an exit status, or the error that stopped it starting at all.
    ///
    /// Kept even while the sidecar is running again, because "up, but it has crashed nine times"
    /// is a different situation from "up", and only one of them is fine.
    pub last_failure: Option<String>,
    pub last_failure_at: Option<String>,
    /// How many times this sidecar has been restarted since the daemon started.
    pub restarts: u32,
}

/// Every supervised sidecar, in name order.
pub fn states() -> Vec<SidecarState> {
    SIDECARS.lock().unwrap().values().cloned().collect()
}

fn record(name: &str, update: impl FnOnce(&mut SidecarState)) {
    let mut sidecars = SIDECARS.lock().unwrap();
    let entry = sidecars
        .entry(name.to_owned())
        .or_insert_with(|| SidecarState {
            name: name.to_owned(),
            state: "down",
            started_at: None,
            last_failure: None,
            last_failure_at: None,
            restarts: 0,
        });
    update(entry);
}

pub async fn supervise(name: String, binary_path: PathBuf, env: Vec<(String, String)>) {
    let mut delay = RESTART_BASE;
    let mut attempts: u32 = 0;
    loop {
        let mut cmd = Command::new(&binary_path);
        for (k, v) in &env {
            cmd.env(k, v);
        }
        // A sidecar's environment holds the daemon token, and for email the IMAP password too.
        // Without this, shutting the daemon down left the process running with both.
        cmd.kill_on_drop(true);
        match cmd.spawn() {
            Ok(mut child) => {
                let started_at = chrono::Utc::now().to_rfc3339();
                let restarts = attempts;
                record(&name, |entry| {
                    entry.state = "running";
                    entry.started_at = Some(started_at);
                    entry.restarts = restarts;
                });
                let status = child.wait().await;
                tracing::warn!(sidecar = %name, ?status, "sidecar exited — restarting");
                let failed_at = chrono::Utc::now().to_rfc3339();
                let detail = match &status {
                    Ok(status) => format!("exited: {status}"),
                    Err(error) => format!("could not be waited on: {error}"),
                };
                record(&name, |entry| {
                    entry.state = "down";
                    entry.started_at = None;
                    entry.last_failure = Some(detail);
                    entry.last_failure_at = Some(failed_at);
                });
                // Ran and then died is a different event from cannot start: a rare crash should
                // restart promptly rather than inherit a backoff earned by something else.
                delay = RESTART_BASE;
            }
            Err(error) => {
                // Exponential, because the usual cause is a binary that was never built — `main.rs`
                // supervises the echo sidecar unconditionally, built or not. At a flat two seconds
                // that wrote a warning every two seconds forever, roughly 43k lines a day, into a
                // log directory that had no retention either.
                tracing::warn!(
                    sidecar = %name,
                    %error,
                    ?delay,
                    "sidecar failed to spawn — backing off"
                );
                let failed_at = chrono::Utc::now().to_rfc3339();
                let detail = format!("could not start: {error}");
                record(&name, |entry| {
                    entry.state = "down";
                    entry.started_at = None;
                    entry.last_failure = Some(detail);
                    entry.last_failure_at = Some(failed_at);
                });
                delay = (delay * 2).min(RESTART_MAX);
            }
        }
        attempts = attempts.saturating_add(1);
        tokio::time::sleep(delay).await;
    }
}

/// `daemon_token` is the control token here, deliberately, where the email sidecar gets a scoped
/// one. This sidecar is the user's remote control: it approves proposals, works the kill switch and
/// cancels runs — the shell's surface, reached from a phone. An allowlist for it would be all of
/// Control minus a few routes, which reads like a boundary without being one. Narrowing it means
/// first deciding what a chat message is allowed to do, which is a product decision.
pub fn telegram_env(
    daemon_url: &str,
    daemon_token: &str,
    bot_token: &str,
) -> Vec<(String, String)> {
    vec![
        ("NUCLEOS_DAEMON_URL".to_string(), daemon_url.to_string()),
        ("NUCLEOS_DAEMON_TOKEN".to_string(), daemon_token.to_string()),
        ("TELEGRAM_BOT_TOKEN".to_string(), bot_token.to_string()),
    ]
}

/// Where the email sidecar serves attachments, and where the núcleo asks for them.
///
/// A constant rather than configuration because it is one fact shared by two processes, and a fact
/// with two homes eventually has two values. 8793 follows the daemon (8791) and the echo sidecar
/// (8792). Loopback is not a default here — the sidecar refuses to bind anything else.
pub const EMAIL_FETCH_ADDR: &str = "127.0.0.1:8793";

fn sent_mailbox_env(config: &crate::config::EmailConfig) -> Option<(String, String)> {
    config
        .sent_mailbox
        .as_ref()
        .map(|mailbox| ("EMAIL_SENT_MAILBOX".to_string(), mailbox.clone()))
}

/// The submission server, passed only once somebody has named one.
///
/// Conditional for the same reason `sent_mailbox_env` is, and with a sharper edge: the sidecar
/// decides whether it can send at all by whether this host reached it. Emitting the pair
/// unconditionally would hand it an empty host and a port, which reads like a configured server
/// right up to the point a person is told their message went out.
fn smtp_env(config: &crate::config::EmailConfig) -> Vec<(String, String)> {
    if config.smtp_host.trim().is_empty() {
        return Vec::new();
    }
    vec![
        ("EMAIL_SMTP_HOST".to_string(), config.smtp_host.clone()),
        ("EMAIL_SMTP_PORT".to_string(), config.smtp_port.to_string()),
    ]
}

/// The email sidecar's environment (spec §3.4). This list is the contract between the núcleo and
/// the Go sidecar: it reads nothing from disk and holds no config of its own, so anything it needs
/// is here or it does not exist. The password comes from Credential Manager and never touches a
/// file — it is passed to the child process and nowhere else.
///
/// `daemon_token` is this sidecar's own key (`auth::Service::Email`), not the control token. It
/// opens `/email/cursor` and `/email/incoming` and nothing else, which is the complete set
/// `daemon/client.go` builds a URL for. The process on the other end parses MIME written by
/// strangers; a bug in that parser should cost the mailbox, not the daemon.
pub fn email_env(
    daemon_url: &str,
    daemon_token: &str,
    config: &crate::config::EmailConfig,
    password: &str,
) -> Vec<(String, String)> {
    let mut env = vec![
        ("NUCLEOS_DAEMON_URL".to_string(), daemon_url.to_string()),
        ("NUCLEOS_DAEMON_TOKEN".to_string(), daemon_token.to_string()),
        ("EMAIL_FETCH_ADDR".to_string(), EMAIL_FETCH_ADDR.to_string()),
        ("EMAIL_IMAP_HOST".to_string(), config.host.clone()),
        ("EMAIL_IMAP_PORT".to_string(), config.port.to_string()),
        ("EMAIL_IMAP_USERNAME".to_string(), config.username.clone()),
        ("EMAIL_IMAP_PASSWORD".to_string(), password.to_string()),
        ("EMAIL_MAILBOX".to_string(), config.mailbox.clone()),
        (
            "EMAIL_POLL_INTERVAL_SECS".to_string(),
            config.poll_interval_secs.to_string(),
        ),
    ];
    env.extend(sent_mailbox_env(config));
    env.extend(smtp_env(config));
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn email_env_carries_exactly_the_contract() {
        let config = crate::config::EmailConfig {
            enabled: true,
            host: "imap.gmail.com".into(),
            port: 993,
            smtp_host: "smtp.gmail.com".into(),
            smtp_port: 465,
            username: "me@x.com".into(),
            mailbox: "INBOX".into(),
            poll_interval_secs: 300,
            ..Default::default()
        };
        let env: HashMap<String, String> =
            email_env("http://127.0.0.1:8791", "tok", &config, "app-password")
                .into_iter()
                .collect();

        assert_eq!(env.len(), 11);
        assert_eq!(env["NUCLEOS_DAEMON_URL"], "http://127.0.0.1:8791");
        assert_eq!(env["NUCLEOS_DAEMON_TOKEN"], "tok");
        // The one address both processes have to agree on. The sidecar defaults to the same value,
        // so a mismatch would only ever come from this line drifting.
        assert_eq!(env["EMAIL_FETCH_ADDR"], EMAIL_FETCH_ADDR);
        assert_eq!(env["EMAIL_IMAP_HOST"], "imap.gmail.com");
        assert_eq!(env["EMAIL_IMAP_PORT"], "993");
        assert_eq!(env["EMAIL_IMAP_USERNAME"], "me@x.com");
        assert_eq!(env["EMAIL_IMAP_PASSWORD"], "app-password");
        assert_eq!(env["EMAIL_MAILBOX"], "INBOX");
        assert_eq!(env["EMAIL_POLL_INTERVAL_SECS"], "300");
        // Reading and sending are two servers as often as they are one, so the submission host
        // travels separately from the IMAP one rather than being derived from it.
        assert_eq!(env["EMAIL_SMTP_HOST"], "smtp.gmail.com");
        assert_eq!(env["EMAIL_SMTP_PORT"], "465");
    }

    /// An unconfigured submission host must reach the sidecar as an ABSENCE, not as an empty string.
    ///
    /// The sidecar decides whether it can send at all from whether this variable arrived. A blank
    /// `EMAIL_SMTP_HOST` alongside a perfectly ordinary `EMAIL_SMTP_PORT` looks like a configured
    /// server to anything reading the pair, and the failure surfaces only after somebody has been
    /// told their message went out — which for this pillar is the one failure that cannot be undone.
    #[test]
    fn the_submission_host_reaches_the_sidecar_only_once_somebody_has_named_one() {
        let unconfigured: HashMap<String, String> = email_env(
            "http://127.0.0.1:8791",
            "tok",
            &crate::config::EmailConfig::default(),
            "password",
        )
        .into_iter()
        .collect();
        assert!(
            !unconfigured.contains_key("EMAIL_SMTP_HOST")
                && !unconfigured.contains_key("EMAIL_SMTP_PORT"),
            "an unnamed submission host must leave BOTH variables absent, port included"
        );

        // Whitespace is the same absence wearing a hat: a host of spaces would connect to nothing.
        let blank = crate::config::EmailConfig {
            smtp_host: "   ".into(),
            ..Default::default()
        };
        let blank_env: HashMap<String, String> =
            email_env("http://127.0.0.1:8791", "tok", &blank, "password")
                .into_iter()
                .collect();
        assert!(!blank_env.contains_key("EMAIL_SMTP_HOST"));

        let configured = crate::config::EmailConfig {
            smtp_host: "smtp.example.com".into(),
            smtp_port: 587,
            ..Default::default()
        };
        let configured_env: HashMap<String, String> =
            email_env("http://127.0.0.1:8791", "tok", &configured, "password")
                .into_iter()
                .collect();
        assert_eq!(
            configured_env.get("EMAIL_SMTP_HOST").map(String::as_str),
            Some("smtp.example.com")
        );
        // The port travels with the host and is not assumed on the far side: an operator who moved
        // off 465 did so because their provider left them no choice.
        assert_eq!(
            configured_env.get("EMAIL_SMTP_PORT").map(String::as_str),
            Some("587")
        );
    }

    #[test]
    fn o_sidecar_recebe_a_pasta_de_enviados() {
        let without_sent = crate::config::EmailConfig {
            sent_mailbox: None,
            ..Default::default()
        };
        let without_sent_env: HashMap<String, String> =
            email_env("http://127.0.0.1:8791", "tok", &without_sent, "password")
                .into_iter()
                .collect();
        assert!(
            !without_sent_env.contains_key("EMAIL_SENT_MAILBOX"),
            "an unconfigured sent mailbox must leave EMAIL_SENT_MAILBOX absent"
        );

        let with_sent = crate::config::EmailConfig {
            sent_mailbox: Some("[Gmail]/Sent Mail".into()),
            ..Default::default()
        };
        let with_sent_env: HashMap<String, String> =
            email_env("http://127.0.0.1:8791", "tok", &with_sent, "password")
                .into_iter()
                .collect();
        assert_eq!(
            with_sent_env.get("EMAIL_SENT_MAILBOX").map(String::as_str),
            Some("[Gmail]/Sent Mail")
        );
    }

    #[test]
    fn telegram_env_has_the_three_expected_vars() {
        let env: HashMap<String, String> = telegram_env("http://127.0.0.1:8791", "tok", "bot")
            .into_iter()
            .collect();

        assert_eq!(env.len(), 3);
        assert_eq!(
            env.get("NUCLEOS_DAEMON_URL").map(String::as_str),
            Some("http://127.0.0.1:8791")
        );
        assert_eq!(
            env.get("NUCLEOS_DAEMON_TOKEN").map(String::as_str),
            Some("tok")
        );
        assert_eq!(
            env.get("TELEGRAM_BOT_TOKEN").map(String::as_str),
            Some("bot")
        );
    }
}
