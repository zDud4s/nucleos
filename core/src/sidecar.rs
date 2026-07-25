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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
