use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

pub async fn supervise(name: String, binary_path: PathBuf) {
    loop {
        match Command::new(&binary_path).spawn() {
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
