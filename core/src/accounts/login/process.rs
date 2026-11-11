//! The login process I/O of the no-TTY path: spawn the CLI with piped stdio, merge stdout and
//! stderr into lines without ANSI, and keep stdin open for a pasted code.

use std::io;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use super::LoginProcess;
use crate::accounts::provider::LoginCommand;

/// A prompt without a newline ("Paste code here >") is emitted as a line after this idle time.
const PARTIAL_FLUSH: Duration = Duration::from_millis(300);

/// The production spawner: the real CLI, output lines merged, stdin open for the code.
pub(super) fn spawn_process(command: &LoginCommand) -> io::Result<LoginProcess> {
    let executable = super::notty::resolve_executable(&command.program)?;
    let mut process = tokio::process::Command::new(executable);
    // No console window flashing over the app while the login runs.
    #[cfg(windows)]
    process.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
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
