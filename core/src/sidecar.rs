use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

/// First delay after a sidecar dies. A single crash should cost about as much as a restart.
const RESTART_BASE: Duration = Duration::from_secs(2);
/// Ceiling for the backoff: long enough that a binary which was never built is nearly free, short
/// enough that a sidecar which starts working again is back within a minute.
const RESTART_MAX: Duration = Duration::from_secs(60);

pub async fn supervise(name: String, binary_path: PathBuf, env: Vec<(String, String)>) {
    let mut delay = RESTART_BASE;
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
                let status = child.wait().await;
                tracing::warn!(sidecar = %name, ?status, "sidecar exited — restarting");
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
                delay = (delay * 2).min(RESTART_MAX);
            }
        }
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
    vec![
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
    ]
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
            username: "me@x.com".into(),
            mailbox: "INBOX".into(),
            poll_interval_secs: 300,
            ..Default::default()
        };
        let env: HashMap<String, String> =
            email_env("http://127.0.0.1:8791", "tok", &config, "app-password")
                .into_iter()
                .collect();

        assert_eq!(env.len(), 9);
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
