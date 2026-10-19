//! The `nucleos-core --filter-run` runtime: reads a command's output from stdin (or spawns the
//! command itself), keeps a tail-bounded raw log, prints the filtered view and exits with the
//! command's own exit code. It fails open: a log that cannot be written never hides the output.

use super::{
    EXIT_SENTINEL, FLAG, LINE_MAX_CHARS, LOG_CAP_BYTES, PS_POSTAMBLE, View, footer,
    quote_powershell,
};
use base64::Engine as _;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;

/// How many trailing bytes are held back from the log and the view until the stream ends, so a
/// sentinel split across two reads is still found. Longer than any sentinel.
const HOLD_BACK: usize = 64;
/// The most bytes of one line the view's line buffer keeps; the view cuts lines far earlier.
const LINE_BUFFER_MAX: usize = LINE_MAX_CHARS * 4 + 16;

/// The args after `--filter-run`, only when it is argv[1]: anywhere else the flag is just text of
/// someone else's command.
pub fn filter_run_args(args: &[String]) -> Option<&[String]> {
    if args.get(1).map(String::as_str) == Some(FLAG) {
        Some(&args[2..])
    } else {
        None
    }
}

enum Mode {
    Stdin,
    Shell { shell: String, encoded: String },
    Argv(Vec<String>),
}

struct Parsed {
    log: PathBuf,
    mode: Mode,
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut log: Option<PathBuf> = None;
    let mut shell: Option<String> = None;
    let mut encoded: Option<String> = None;
    let mut argv: Option<Vec<String>> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--log" => {
                log = Some(PathBuf::from(
                    args.get(i + 1).ok_or("--log needs a directory")?,
                ));
                i += 2;
            }
            "--shell" => {
                shell = Some(args.get(i + 1).ok_or("--shell needs a path")?.clone());
                i += 2;
            }
            "--encoded" => {
                encoded = Some(args.get(i + 1).ok_or("--encoded needs a command")?.clone());
                i += 2;
            }
            "--" => {
                argv = Some(args[i + 1..].to_vec());
                break;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let log = log.ok_or("--log is required")?;
    let mode = match (shell, encoded, argv) {
        (None, None, None) => Mode::Stdin,
        (Some(shell), Some(encoded), None) => Mode::Shell { shell, encoded },
        (None, None, Some(argv)) if !argv.is_empty() => Mode::Argv(argv),
        _ => return Err("--shell and --encoded go together and exclude `--`".to_string()),
    };
    Ok(Parsed { log, mode })
}

/// The runtime entry: returns the exit code the process should leave with.
pub fn main(args: &[String]) -> i32 {
    let parsed = match parse(args) {
        Ok(parsed) => parsed,
        Err(_) => {
            // Fail open: whatever is wrong with the arguments, the output still goes through.
            let stdin = std::io::stdin();
            let mut stdout = std::io::stdout();
            return passthrough(stdin.lock(), &mut stdout);
        }
    };
    match parsed.mode {
        Mode::Stdin => {
            let stdin = std::io::stdin();
            let mut stdout = std::io::stdout();
            filter_stream(stdin.lock(), &mut stdout, &parsed.log, LOG_CAP_BYTES)
        }
        Mode::Shell { shell, encoded } => {
            let mut candidates = vec![shell];
            if cfg!(windows) {
                candidates.push("pwsh".to_string());
                candidates.push("powershell.exe".to_string());
            }
            let commands = candidates.into_iter().map(|program| {
                let mut command = Command::new(program);
                command.args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-EncodedCommand",
                    encoded.as_str(),
                ]);
                command
            });
            run_spawned(commands.collect(), &parsed.log)
        }
        Mode::Argv(argv) => run_spawned(vec![argv_command(&argv)], &parsed.log),
    }
}

fn encode_command(script: &str) -> String {
    let bytes: Vec<u8> = script
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn argv_command(argv: &[String]) -> Command {
    if cfg!(windows) {
        let quoted: Vec<String> = argv.iter().map(|a| quote_powershell(a)).collect();
        let script = format!("& {}\n{PS_POSTAMBLE}", quoted.join(" "));
        let mut command = Command::new("powershell.exe");
        command.args(["-NoProfile", "-NonInteractive", "-EncodedCommand"]);
        command.arg(encode_command(&script));
        command
    } else {
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]);
        command
    }
}

fn exit_code_of(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

fn spawn_first(commands: Vec<Command>) -> Result<Child, std::io::Error> {
    let mut last = None;
    for mut command in commands {
        command
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("nothing to run")))
}

fn pump(
    mut source: impl Read + Send + 'static,
    tx: mpsc::Sender<Vec<u8>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match source.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    })
}

/// Spawn mode: run the first command that starts, merge its stdout and stderr, filter them.
fn run_spawned(commands: Vec<Command>, log_dir: &Path) -> i32 {
    let mut child = match spawn_first(commands) {
        Ok(child) => child,
        Err(error) => {
            let mut stdout = std::io::stdout();
            let _ = writeln!(stdout, "[nucleos] could not start the command: {error}");
            return 127;
        }
    };
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let mut threads = Vec::new();
    if let Some(out) = child.stdout.take() {
        threads.push(pump(out, tx.clone()));
    }
    if let Some(err) = child.stderr.take() {
        threads.push(pump(err, tx.clone()));
    }
    drop(tx);

    let mut sink = Sink::new(log_dir, LOG_CAP_BYTES);
    sink.announce(&mut std::io::stdout());
    for chunk in rx {
        sink.push(&chunk);
    }
    for thread in threads {
        let _ = thread.join();
    }
    let code = match child.wait() {
        Ok(status) => exit_code_of(status),
        Err(_) => 1,
    };
    let mut stdout = std::io::stdout();
    sink.finish(code, None, &mut stdout)
}

/// The raw log of one run: it keeps the tail by rotating the file over `<stem>.1.log` once the
/// current one passes half the cap. Any IO error stops logging and is remembered for the footer.
struct Log {
    dir: PathBuf,
    stem: String,
    file: Option<File>,
    written: u64,
    rotated: bool,
    failed: Option<String>,
    cap: u64,
}

impl Log {
    fn open(dir: &Path, cap: u64) -> Log {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let mut log = Log {
            dir: dir.to_path_buf(),
            stem: format!("{millis}-{}", std::process::id()),
            file: None,
            written: 0,
            rotated: false,
            failed: None,
            cap,
        };
        if let Err(error) = std::fs::create_dir_all(dir) {
            log.failed = Some(error.to_string());
            return log;
        }
        log.reopen();
        log
    }

    fn current_path(&self) -> PathBuf {
        self.dir.join(format!("{}.log", self.stem))
    }

    fn rotated_path(&self) -> PathBuf {
        self.dir.join(format!("{}.1.log", self.stem))
    }

    fn reopen(&mut self) {
        match File::create(self.current_path()) {
            Ok(file) => {
                self.file = Some(file);
                self.written = 0;
            }
            Err(error) => self.failed = Some(error.to_string()),
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.failed.is_some() || bytes.is_empty() {
            return;
        }
        if self.file.is_none() {
            self.reopen();
        }
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if let Err(error) = file.write_all(bytes) {
            self.failed = Some(error.to_string());
            self.file = None;
            return;
        }
        self.written += bytes.len() as u64;
        if self.written > self.cap / 2 {
            // Close first (Windows cannot rename an open file over another), then keep this half
            // as the tail and start a fresh file on the next write.
            self.file = None;
            match std::fs::rename(self.current_path(), self.rotated_path()) {
                Ok(()) => self.rotated = true,
                Err(error) => self.failed = Some(error.to_string()),
            }
        }
    }

    fn paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if self.rotated {
            paths.push(self.rotated_path());
        }
        if self.current_path().exists() {
            paths.push(self.current_path());
        }
        paths
    }
}

/// Everything the output goes to while it streams: the log and the view.
struct Sink {
    log: Log,
    view: View,
    partial: Vec<u8>,
}

impl Sink {
    fn new(log_dir: &Path, cap: u64) -> Sink {
        Sink {
            log: Log::open(log_dir, cap),
            view: View::new(),
            partial: Vec::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.log.write(bytes);
        let mut rest = bytes;
        while let Some(at) = rest.iter().position(|&b| b == b'\n') {
            self.extend_partial(&rest[..at]);
            let line = String::from_utf8_lossy(&self.partial).into_owned();
            self.view.push_line(&line);
            self.partial.clear();
            rest = &rest[at + 1..];
        }
        self.extend_partial(rest);
    }

    fn extend_partial(&mut self, bytes: &[u8]) {
        let room = LINE_BUFFER_MAX.saturating_sub(self.partial.len());
        self.partial
            .extend_from_slice(&bytes[..bytes.len().min(room)]);
    }

    /// Prints ONE line before any output is read, flushed, saying where the full log is. The view
    /// only appears when the command exits; if the CLI kills the call first (the Bash tool's
    /// timeout), this line is all the agent ever sees.
    fn announce(&self, out: &mut impl Write) {
        let place = match &self.log.failed {
            Some(_) => "full log unavailable".to_string(),
            None => format!("full log: {}", self.log.current_path().display()),
        };
        let _ = writeln!(
            out,
            "nucleos: output is filtered until the command exits; {place}"
        );
        let _ = out.flush();
    }

    /// Prints the view and returns `code`. `note` is appended to the footer.
    fn finish(mut self, code: i32, note: Option<&str>, out: &mut impl Write) -> i32 {
        if !self.partial.is_empty() {
            let line = String::from_utf8_lossy(&self.partial).into_owned();
            self.view.push_line(&line);
        }
        let paths = self.log.paths();
        let log = match &self.log.failed {
            Some(why) => Err(why.as_str()),
            None => Ok(paths.as_slice()),
        };
        let mut line = footer(code, self.view.total_lines(), log);
        if let Some(note) = note {
            line.push_str(" · ");
            line.push_str(note);
        }
        let _ = out.write_all(self.view.render(code, &line).as_bytes());
        let _ = out.write_all(b"\n");
        let _ = out.flush();
        code
    }
}

/// Reads `input` to EOF, handing `emit` everything but the trailing exit sentinel, and returns the
/// code the sentinel carried (`None` when it never came). Never stops early.
fn drain_stream(mut input: impl Read, mut emit: impl FnMut(&[u8])) -> Option<i32> {
    let marker = format!("\n{EXIT_SENTINEL}");
    let mut held: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                held.extend_from_slice(&buf[..n]);
                if held.len() > HOLD_BACK {
                    let release = held.len() - HOLD_BACK;
                    emit(&held[..release]);
                    held.drain(..release);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    let at = held
        .windows(marker.len())
        .rposition(|window| window == marker.as_bytes());
    match at {
        Some(at) => {
            emit(&held[..at]);
            let digits = String::from_utf8_lossy(&held[at + marker.len()..]).into_owned();
            Some(digits.trim().parse::<i32>().unwrap_or(1))
        }
        None => {
            emit(&held);
            None
        }
    }
}

/// The output filtered and logged; `input` is a command's merged output ending in the sentinel.
fn filter_stream(input: impl Read, mut out: impl Write, log_dir: &Path, cap: u64) -> i32 {
    let mut sink = Sink::new(log_dir, cap);
    sink.announce(&mut out);
    let sentinel = drain_stream(input, |bytes| sink.push(bytes));
    match sentinel {
        Some(code) => sink.finish(code, None, &mut out),
        None => sink.finish(1, Some("exit status not reported"), &mut out),
    }
}

/// Argument parse failed: copy the output through untouched, minus the sentinel.
fn passthrough(input: impl Read, out: &mut impl Write) -> i32 {
    let sentinel = drain_stream(input, |bytes| {
        let _ = out.write_all(bytes);
    });
    let _ = out.flush();
    sentinel.unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::path::Path;

    /// Hands out at most `chunk` bytes per `read`, so the log cap is checked often enough for a
    /// small cap to mean something.
    struct Trickle<'a> {
        rest: &'a [u8],
        chunk: usize,
    }

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.rest.len().min(self.chunk).min(buf.len());
            buf[..n].copy_from_slice(&self.rest[..n]);
            self.rest = &self.rest[n..];
            Ok(n)
        }
    }

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn log_files(dir: &Path) -> Vec<(String, String)> {
        let mut found: Vec<(String, String)> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                let body = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
                (name, body)
            })
            .collect();
        found.sort();
        found
    }

    #[test]
    fn stdin_mode_exits_with_the_sentinel_code_and_logs_everything() {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().join("logs");
        let input = "alpha\nbeta\n\n\u{1e}NUCLEOS-EXIT:3\n";
        let mut out = Vec::new();

        let code = filter_stream(input.as_bytes(), &mut out, &log_dir, 1_000_000);

        assert_eq!(code, 3);
        let printed = String::from_utf8(out).unwrap();
        assert!(
            printed.contains("alpha") && printed.contains("beta"),
            "{printed}"
        );
        assert!(
            !printed.contains("NUCLEOS-EXIT"),
            "the sentinel leaked: {printed}"
        );
        assert!(printed.contains("exit 3"), "no footer: {printed}");
        let files = log_files(&log_dir);
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(files[0].0.ends_with(".log"), "{files:?}");
        assert!(
            files[0].1.contains("alpha\nbeta"),
            "the log lost output: {files:?}"
        );
        assert!(
            printed.contains(&files[0].0),
            "the footer does not name the log: {printed}"
        );
    }

    #[test]
    fn the_first_line_says_where_the_full_log_is() {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().join("logs");
        let mut out = Vec::new();

        filter_stream(
            "alpha\n\n\u{1e}NUCLEOS-EXIT:0\n".as_bytes(),
            &mut out,
            &log_dir,
            1_000_000,
        );

        let printed = String::from_utf8(out).unwrap();
        let first = printed.lines().next().unwrap();
        assert!(
            first.starts_with("nucleos: output is filtered until the command exits; full log: "),
            "{printed}"
        );
        assert!(!first.contains("NUCLEOS-EXIT"), "{first}");
        let files = log_files(&log_dir);
        let path = first.rsplit("full log: ").next().unwrap();
        assert!(path.ends_with(&files[0].0), "{first} vs {files:?}");
        let footer_line = printed.lines().last().unwrap();
        assert!(footer_line.ends_with(path), "{footer_line} vs {path}");
    }

    #[test]
    fn a_missing_sentinel_exits_nonzero_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut out = Vec::new();

        let code = filter_stream("only output\n".as_bytes(), &mut out, dir.path(), 1_000_000);

        assert_eq!(code, 1);
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("only output"), "{printed}");
        assert!(printed.contains("exit status not reported"), "{printed}");
    }

    #[test]
    fn the_log_cap_keeps_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let log_dir = dir.path().join("logs");
        let cap = 1_000u64;
        let mut body = String::new();
        for n in 1..=2_000 {
            body.push_str(&format!("line {n:04}\n"));
        }
        body.push_str("\n\u{1e}NUCLEOS-EXIT:0\n");
        let reader = Trickle {
            rest: body.as_bytes(),
            chunk: 50,
        };
        let mut out = Vec::new();

        let code = filter_stream(reader, &mut out, &log_dir, cap);

        assert_eq!(code, 0);
        let files = log_files(&log_dir);
        assert!(
            files.len() <= 2,
            "more than the current log and one rotation: {files:?}"
        );
        assert!(
            files.iter().any(|(name, _)| name.ends_with(".1.log")),
            "no rotation: {files:?}"
        );
        let kept: String = files.iter().map(|(_, text)| text.as_str()).collect();
        assert!(kept.contains("line 2000"), "the tail was lost");
        assert!(!kept.contains("line 0001"), "the head survived the cap");
        assert!(
            kept.len() as u64 <= cap * 2,
            "the log outgrew its cap: {}",
            kept.len()
        );
        // The view is built from the stream, not from the capped log.
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("line 2000"), "{printed}");
    }

    #[test]
    fn an_unwritable_log_still_prints_the_view() {
        let dir = tempfile::tempdir().unwrap();
        // A regular file where the log directory should be: create_dir_all cannot succeed.
        let blocker = dir.path().join("not-a-dir");
        std::fs::write(&blocker, "x").unwrap();
        let mut out = Vec::new();

        let code = filter_stream(
            "one\ntwo\n\n\u{1e}NUCLEOS-EXIT:5\n".as_bytes(),
            &mut out,
            &blocker,
            1_000_000,
        );

        assert_eq!(code, 5);
        let printed = String::from_utf8(out).unwrap();
        assert!(
            printed.contains("one") && printed.contains("two"),
            "{printed}"
        );
        assert!(printed.contains("full log unavailable"), "{printed}");
    }

    #[test]
    fn filter_run_args_are_read_only_in_first_position() {
        let first = args(&[
            "nucleos-core",
            "--filter-run",
            "--log",
            "dir",
            "--",
            "run",
            "it",
        ]);
        assert_eq!(
            filter_run_args(&first),
            Some(&first[2..]),
            "the args after --filter-run are the runtime's"
        );
        let bare = args(&["nucleos-core", "--filter-run"]);
        assert_eq!(filter_run_args(&bare), Some(&bare[2..]));

        // Anywhere else the flag is just text of someone else's command.
        assert!(filter_run_args(&args(&["nucleos-core"])).is_none());
        assert!(filter_run_args(&args(&["nucleos-core", "--print-token"])).is_none());
        assert!(filter_run_args(&args(&["nucleos-core", "x", "--filter-run"])).is_none());
        assert!(filter_run_args(&args(&["nucleos-core", "--", "run", "--filter-run"])).is_none());
        assert!(filter_run_args(&[]).is_none());
    }
}
