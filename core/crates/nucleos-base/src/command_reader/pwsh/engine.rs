//! Second engine: a resident `pwsh` process that parses with the real PowerShell parser.
//!
//! Wire format, one request and one reply per line:
//! - request: the source text, base64 encoded (UTF-8 bytes);
//! - reply: `ok|<n parse errors>|<b64>,<b64>,...` or `err|<message>`.
//!
//! One request is in flight at a time and there is no automatic retry: once the
//! child dies or a request times out the engine is `Down` and the caller decides
//! whether to start a new one.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

/// Default per-request timeout.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// How long `start` waits for the `ready` line (pwsh start-up can be slow).
const READY_TIMEOUT: Duration = Duration::from_secs(30);

const SERVER_SCRIPT: &str = include_str!("server.ps1");

pub struct ResidentParser {
    child: Child,
    /// Request lines for the writer thread (keeps big writes off the caller thread).
    tx: Sender<Vec<u8>>,
    /// Reply lines from the reader thread; disconnects when the child's stdout closes.
    rx: Receiver<String>,
    timeout: Duration,
    down: bool,
    script: PathBuf,
}

#[derive(Debug)]
pub enum EngineError {
    Down(String),
    Protocol(String),
    Timeout,
    ParseErrors(usize),
}

impl ResidentParser {
    pub fn start(exe: &Path) -> std::io::Result<Self> {
        Self::start_with_timeout(exe, REQUEST_TIMEOUT)
    }

    /// Testability seam: same as `start` with a custom per-request timeout.
    pub fn start_with_timeout(exe: &Path, timeout: Duration) -> std::io::Result<Self> {
        let script = write_script()?;

        let mut cmd = Command::new(exe);
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = std::fs::remove_file(&script);
                return Err(e);
            }
        };
        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&script);
            return Err(std::io::Error::other("pwsh pipes unavailable"));
        };

        let (reply_tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match r.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let t = line.trim_end_matches(['\r', '\n']).to_string();
                        if reply_tx.send(t).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let (tx, req_rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            while let Ok(bytes) = req_rx.recv() {
                if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
                    break;
                }
            }
        });

        // On error `me` is dropped: the child is killed and the script removed.
        let me = Self {
            child,
            tx,
            rx,
            timeout,
            down: false,
            script,
        };
        match me.rx.recv_timeout(READY_TIMEOUT) {
            Ok(l) if l == "ready" => Ok(me),
            Ok(l) => Err(std::io::Error::other(format!("unexpected first line: {l}"))),
            Err(_) => Err(std::io::Error::other("pwsh did not become ready")),
        }
    }

    pub fn command_names(&mut self, src: &str) -> Result<Vec<String>, EngineError> {
        if !self.is_alive() {
            return Err(EngineError::Down("pwsh is not running".to_string()));
        }
        if let Some(why) = refused_input(src) {
            return Err(EngineError::Protocol(format!("input refused: {why}")));
        }
        let mut line = b64_encode(src.as_bytes()).into_bytes();
        line.push(b'\n');
        if self.tx.send(line).is_err() {
            return Err(self.mark_down("request writer stopped"));
        }
        match self.rx.recv_timeout(self.timeout) {
            Ok(reply) => parse_reply(&reply),
            Err(RecvTimeoutError::Timeout) => {
                self.kill();
                Err(EngineError::Timeout)
            }
            Err(RecvTimeoutError::Disconnected) => Err(self.mark_down("pwsh closed its output")),
        }
    }

    pub fn is_alive(&mut self) -> bool {
        if self.down {
            return false;
        }
        match self.child.try_wait() {
            Ok(None) => true,
            _ => {
                self.down = true;
                false
            }
        }
    }

    fn kill(&mut self) {
        self.down = true;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn mark_down(&mut self, why: &str) -> EngineError {
        self.kill();
        EngineError::Down(why.to_string())
    }

    /// Test seam: OS pid of the `pwsh` child.
    #[cfg(test)]
    pub(crate) fn child_id(&self) -> u32 {
        self.child.id()
    }

    /// Test seam: kills the child without going through `Drop`.
    #[cfg(test)]
    pub(crate) fn kill_child_for_test(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ResidentParser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.script);
    }
}

/// Input the engine must never see (same limits as the scanner): oversized, or holding NUL.
pub(super) fn refused_input(src: &str) -> Option<&'static str> {
    if src.len() > super::scanner::MAX_INPUT {
        Some("input too large")
    } else if src.contains('\0') {
        Some("NUL byte")
    } else {
        None
    }
}

/// Distinguishes several parsers started by the same process.
fn unique_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

/// Writes the server script to a fresh, unpredictable path; `create_new` fails if the
/// path exists and never follows a pre-existing symlink.
fn write_script() -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir();
    let mut last_err = None;
    for _ in 0..8 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = dir.join(format!(
            "nucleos-pwsh-server-{}-{}-{nanos:x}.ps1",
            std::process::id(),
            unique_suffix()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                if let Err(e) = f.write_all(SERVER_SCRIPT.as_bytes()) {
                    drop(f);
                    let _ = std::fs::remove_file(&path);
                    return Err(e);
                }
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last_err = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::other("could not create server script")))
}

pub fn default_exe() -> Option<PathBuf> {
    let name = if cfg!(windows) { "pwsh.exe" } else { "pwsh" };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    #[cfg(windows)]
    {
        let p = PathBuf::from(r"C:\Program Files\PowerShell\7\pwsh.exe");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn b64_decode(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err("base64 length is not a multiple of 4".to_string());
    }
    let val = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some(u32::from(c - b'A')),
            b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let groups = bytes.len() / 4;
    for (i, g) in bytes.chunks(4).enumerate() {
        let last = i + 1 == groups;
        let pad = if last {
            g.iter().rev().take_while(|&&c| c == b'=').count()
        } else {
            0
        };
        if pad > 2 {
            return Err("bad base64 padding".to_string());
        }
        let mut n = 0u32;
        for (j, &c) in g.iter().enumerate() {
            let v = if j >= 4 - pad {
                0
            } else {
                val(c).ok_or_else(|| format!("bad base64 byte {c:#x}"))?
            };
            n = (n << 6) | v;
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

fn parse_reply(line: &str) -> Result<Vec<String>, EngineError> {
    let line = line.trim_end_matches(['\r', '\n']);
    if let Some(msg) = line.strip_prefix("err|") {
        // A caught server-side exception: the process is still alive, so not `Down`.
        return Err(EngineError::Protocol(format!("server error: {msg}")));
    }
    let mut parts = line.splitn(3, '|');
    let (Some("ok"), Some(n), Some(names)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(EngineError::Protocol(format!(
            "malformed reply: {line:.80}"
        )));
    };
    let n: usize = n
        .parse()
        .map_err(|_| EngineError::Protocol(format!("bad error count: {n:.20}")))?;
    if n > 0 {
        return Err(EngineError::ParseErrors(n));
    }
    if names.is_empty() {
        return Ok(Vec::new());
    }
    names
        .split(',')
        .map(|b| {
            let raw = b64_decode(b).map_err(EngineError::Protocol)?;
            String::from_utf8(raw).map_err(|e| EngineError::Protocol(e.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- base64 (no pwsh) ----

    #[test]
    fn b64_known_vectors() {
        assert_eq!(b64_encode(b""), "");
        assert_eq!(b64_encode(b"f"), "Zg==");
        assert_eq!(b64_encode(b"fo"), "Zm8=");
        assert_eq!(b64_encode(b"foo"), "Zm9v");
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn b64_decode_known_vectors() {
        assert_eq!(b64_decode("Zm9v").unwrap(), b"foo".to_vec());
        assert_eq!(b64_decode("Zg==").unwrap(), b"f".to_vec());
        assert_eq!(b64_decode("Zm8=").unwrap(), b"fo".to_vec());
        assert_eq!(b64_decode("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn b64_decode_rejects_garbage() {
        assert!(b64_decode("!!!!").is_err());
        assert!(b64_decode("Zm9").is_err());
    }

    #[test]
    fn b64_round_trip_cases() {
        let cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            "olá, mundo — ünïcödé ✓".as_bytes().to_vec(),
            b"a\r\nb\r\n".to_vec(),
            // 0xfb 0xff 0xbf encodes to "+/+/", 0xff 0xfe to "//4=": exercises + / and =.
            vec![0xfb, 0xff, 0xbf],
            vec![0xff, 0xfe],
            (0u8..=255).collect(),
        ];
        for bytes in cases {
            let enc = b64_encode(&bytes);
            assert_eq!(b64_decode(&enc).unwrap(), bytes, "encoded: {enc}");
        }
    }

    #[test]
    fn b64_uses_standard_alphabet_and_padding() {
        assert_eq!(b64_encode(&[0xfb, 0xff, 0xbf]), "+/+/");
        assert_eq!(b64_encode(&[0xff, 0xfe]), "//4=");
    }

    // ---- parse_reply (no pwsh) ----

    #[test]
    fn reply_ok_empty_list() {
        assert_eq!(parse_reply("ok|0|").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn reply_ok_names() {
        let line = format!("ok|0|{},{}", b64_encode(b"git"), b64_encode(b"ls"));
        assert_eq!(line, "ok|0|Z2l0,bHM=");
        assert_eq!(parse_reply("ok|0|Z2l0,bHM=").unwrap(), vec!["git", "ls"]);
    }

    #[test]
    fn reply_names_with_commas_are_protected_by_base64() {
        // The name itself holds commas and pipes; only base64 keeps the framing intact.
        let a = b64_encode(b"a,b|c");
        let b = b64_encode(b"d");
        let line = format!("ok|0|{a},{b}");
        assert_eq!(parse_reply(&line).unwrap(), vec!["a,b|c", "d"]);
    }

    #[test]
    fn reply_err_is_protocol() {
        // Decision: a server-side `err|x` (caught exception) is reported as Protocol,
        // not Down: the process is still alive and answering.
        match parse_reply("err|boom") {
            Err(EngineError::Protocol(m)) => assert!(m.contains("boom"), "{m}"),
            other => panic!("expected Protocol, got {other:?}"),
        }
    }

    #[test]
    fn reply_malformed_is_protocol() {
        for line in ["", "garbage", "ok", "ok|0", "ok|x|", "ok|0|!!!", "nope|0|"] {
            assert!(
                matches!(parse_reply(line), Err(EngineError::Protocol(_))),
                "line {line:?} should be Protocol"
            );
        }
    }

    #[test]
    fn reply_parse_errors_win_over_names() {
        let line = format!("ok|2|{}", b64_encode(b"git"));
        assert!(matches!(
            parse_reply(&line),
            Err(EngineError::ParseErrors(2))
        ));
        assert!(matches!(
            parse_reply("ok|1|"),
            Err(EngineError::ParseErrors(1))
        ));
    }

    // ---- real pwsh (ignored by default) ----

    fn pwsh() -> Option<std::path::PathBuf> {
        if let Some(p) = std::env::var_os("NUCLEOS_PWSH_EXE") {
            let p = std::path::PathBuf::from(p);
            if p.exists() {
                return Some(p);
            }
        }
        default_exe().filter(|p| p.exists())
    }

    fn start() -> Option<ResidentParser> {
        let exe = pwsh()?;
        Some(ResidentParser::start(&exe).expect("start resident pwsh"))
    }

    #[test]
    #[ignore = "needs pwsh 7"]
    fn pwsh_start_and_ready() {
        let Some(mut p) = start() else { return };
        assert!(p.is_alive());
    }

    #[test]
    #[ignore = "needs pwsh 7"]
    fn pwsh_foreach_object() {
        let Some(mut p) = start() else { return };
        assert_eq!(
            p.command_names("1..5 | ForEach-Object { $_ * 2 }").unwrap(),
            vec!["ForEach-Object"]
        );
    }

    #[test]
    #[ignore = "needs pwsh 7"]
    fn pwsh_chain_operators() {
        let Some(mut p) = start() else { return };
        assert_eq!(p.command_names("a && b || c").unwrap(), vec!["a", "b", "c"]);
    }

    #[test]
    #[ignore = "needs pwsh 7"]
    fn pwsh_hundred_requests() {
        let Some(mut p) = start() else { return };
        for i in 0..100 {
            let got = p.command_names(&format!("cmd{i} -x | other")).unwrap();
            assert_eq!(got, vec![format!("cmd{i}"), "other".to_string()]);
        }
        assert!(p.is_alive());
    }

    #[test]
    #[ignore = "needs pwsh 7"]
    fn pwsh_unterminated_quote_is_parse_errors() {
        let Some(mut p) = start() else { return };
        match p.command_names("echo 'abc") {
            Err(EngineError::ParseErrors(n)) => assert!(n > 0),
            other => panic!("expected ParseErrors, got {other:?}"),
        }
        // A parse error does not take the engine down.
        assert!(p.is_alive());
        assert_eq!(p.command_names("ls").unwrap(), vec!["ls"]);
    }

    #[test]
    #[ignore = "needs pwsh 7"]
    fn pwsh_killed_child_gives_down() {
        let Some(exe) = pwsh() else { return };
        let mut p = ResidentParser::start(&exe).unwrap();
        assert_eq!(p.command_names("ls").unwrap(), vec!["ls"]);
        kill_child(&mut p);
        assert!(matches!(p.command_names("ls"), Err(EngineError::Down(_))));
        assert!(!p.is_alive());
    }

    #[test]
    #[ignore = "needs pwsh 7"]
    fn pwsh_drop_kills_child() {
        let Some(exe) = pwsh() else { return };
        let p = ResidentParser::start(&exe).unwrap();
        let pid = child_pid(&p);
        drop(p);
        assert!(!pid_running(pid), "child {pid} outlived the parser");
    }

    #[test]
    #[ignore = "needs pwsh 7"]
    fn pwsh_timeout_then_down() {
        let Some(exe) = pwsh() else { return };
        // 1 ms cannot cover a ~240 KB, 60k-command parse: the request must time out. The input
        // stays under MAX_INPUT, otherwise `refused_input` answers before pwsh is ever asked.
        let mut p = ResidentParser::start_with_timeout(&exe, Duration::from_millis(1)).unwrap();
        let big = "a | ".repeat(60_000) + "b";
        assert!(refused_input(&big).is_none());
        assert!(matches!(p.command_names(&big), Err(EngineError::Timeout)));
        assert!(!p.is_alive());
        assert!(matches!(p.command_names("ls"), Err(EngineError::Down(_))));
    }

    // Test-only access to the child goes through `child_id` / `kill_child_for_test` on the parser.
    fn kill_child(p: &mut ResidentParser) {
        p.kill_child_for_test();
    }

    fn child_pid(p: &ResidentParser) -> u32 {
        p.child_id()
    }

    fn pid_running(pid: u32) -> bool {
        #[cfg(windows)]
        {
            let out = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH"])
                .output()
                .expect("tasklist");
            String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
        }
        #[cfg(not(windows))]
        {
            std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
    }
}
