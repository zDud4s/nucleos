use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

pub async fn supervise(name: String, binary_path: PathBuf, env: Vec<(String, String)>) {
    loop {
        let mut cmd = Command::new(&binary_path);
        for (k, v) in &env {
            cmd.env(k, v);
        }
        match cmd.spawn() {
            Ok(mut child) => {
                let status = child.wait().await;
                tracing::warn!(
                    "sidecar '{}' exited ({:?}) — restarting in 2s",
                    name,
                    status
                );
            }
            Err(e) => {
                tracing::warn!(
                    "sidecar '{}' failed to spawn ({}) — retrying in 2s",
                    name,
                    e
                );
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

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

/// The email sidecar's environment (spec §3.4). This list is the contract between the núcleo and
/// the Go sidecar: it reads nothing from disk and holds no config of its own, so anything it needs
/// is here or it does not exist. The password comes from Credential Manager and never touches a
/// file — it is passed to the child process and nowhere else.
pub fn email_env(
    daemon_url: &str,
    daemon_token: &str,
    config: &crate::config::EmailConfig,
    password: &str,
) -> Vec<(String, String)> {
    vec![
        ("NUCLEOS_DAEMON_URL".to_string(), daemon_url.to_string()),
        ("NUCLEOS_DAEMON_TOKEN".to_string(), daemon_token.to_string()),
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

        assert_eq!(env.len(), 8);
        assert_eq!(env["NUCLEOS_DAEMON_URL"], "http://127.0.0.1:8791");
        assert_eq!(env["NUCLEOS_DAEMON_TOKEN"], "tok");
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
