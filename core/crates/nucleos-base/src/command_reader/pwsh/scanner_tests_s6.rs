//! S6 RED: real corpus lines with oracle-generated expectations, and the `read_lines` policy.

use super::engine::EngineError;
use super::scanner_tests_common::ok;
use super::{NamesEngine, PwshReading, Source, read_lines_with};

/// (source, command names) as the pwsh 7 parser reports them.
const CORPUS: &[(&str, &[&str])] = &[
    (
        "winget install --id Ollama.Ollama -e --accept-source-agreements --accept-package-agreements --silent",
        &["winget"],
    ),
    (
        "Get-ChildItem -Recurse -Filter *.rs | Select-Object -First 5",
        &["Get-ChildItem", "Select-Object"],
    ),
    (
        "Get-Content foo.txt | Select-String 'bar' | Measure-Object",
        &["Get-Content", "Select-String", "Measure-Object"],
    ),
    ("git status; git diff --stat", &["git", "git"]),
    ("git add -A && git commit -m \"msg\"", &["git", "git"]),
    (
        "Test-Path .\\a.txt || Write-Host \"missing\"",
        &["Test-Path", "Write-Host"],
    ),
    (
        "cd C:\\Projects\\x; go test ./... 2>&1 | Select-Object -Last 20",
        &["cd", "go", "Select-Object"],
    ),
    (
        "$x = Get-Process | Where-Object { $_.CPU -gt 10 } | Sort-Object CPU -Descending",
        &["Get-Process", "Where-Object", "Sort-Object"],
    ),
    (
        "Get-ChildItem | ForEach-Object { $_.Name }",
        &["Get-ChildItem", "ForEach-Object"],
    ),
    (
        "foreach ($f in Get-ChildItem *.txt) { Remove-Item $f }",
        &["Get-ChildItem", "Remove-Item"],
    ),
    (
        "$n = (Get-ChildItem).Count; Write-Output \"n=$n\"",
        &["Get-ChildItem", "Write-Output"],
    ),
    (
        "Write-Host \"value: $(Get-Date -Format yyyy)\"",
        &["Write-Host", "Get-Date"],
    ),
    ("Write-Output \"a;b && c || d\"", &["Write-Output"]),
    ("Write-Output 'single ; quoted | pipe'", &["Write-Output"]),
    (
        "if (Test-Path x) { Get-Content x } else { Write-Host none }",
        &["Test-Path", "Get-Content", "Write-Host"],
    ),
    (
        "Get-Content a.log -Tail 20 | Where-Object { $_ -match 'ERR' }",
        &["Get-Content", "Where-Object"],
    ),
    (
        "& \"C:\\Program Files\\Git\\bin\\git.exe\" log --oneline -5",
        &["\"C:\\Program Files\\Git\\bin\\git.exe\""],
    ),
    ("npm run build 2>&1 | Out-String", &["npm", "Out-String"]),
    ("Remove-Item -Recurse -Force .\\target", &["Remove-Item"]),
    ("(Get-Item x.txt).Length", &["Get-Item"]),
    ("1..5 | ForEach-Object { $_ * 2 }", &["ForEach-Object"]),
    ("$h = @{ a = 1; b = 2 }; $h.Keys", &[]),
    (
        "Get-ChildItem . | Out-File out.txt",
        &["Get-ChildItem", "Out-File"],
    ),
    (
        "Set-Location ..; Get-Location",
        &["Set-Location", "Get-Location"],
    ),
    ("[Environment]::GetEnvironmentVariable('PATH','User')", &[]),
    (
        "python -c \"print(1)\"; echo $LASTEXITCODE",
        &["python", "echo"],
    ),
    (
        "Start-Process notepad -ArgumentList \"a.txt\"",
        &["Start-Process"],
    ),
    (
        "Get-Service | Where-Object Status -eq Running | Select-Object -First 3",
        &["Get-Service", "Where-Object", "Select-Object"],
    ),
    ("$ErrorActionPreference = 'Stop'; go build ./...", &["go"]),
    (
        "gh pr list --json number | ConvertFrom-Json | ForEach-Object { $_.number }",
        &["gh", "ConvertFrom-Json", "ForEach-Object"],
    ),
    (
        "Get-ChildItem -Path . -Include *.go -Recurse | Select-String 'TODO' | Group-Object Path",
        &["Get-ChildItem", "Select-String", "Group-Object"],
    ),
    (
        "Get-Content x | ForEach-Object { if ($_ -eq 'a') { 'A' } else { $_ } }",
        &["Get-Content", "ForEach-Object"],
    ),
    (
        "Get-ChildItem | % { $_.FullName } | ? { $_ -like '*x*' }",
        &["Get-ChildItem", "%", "?"],
    ),
    ("node -e \"console.log(1)\" && npm test", &["node", "npm"]),
    (
        "git log --format='%H %s' -n 3 | ForEach-Object { $_.Split(' ')[0] }",
        &["git", "ForEach-Object"],
    ),
    ("switch ($x) { 1 { 'one' } default { 'other' } }", &[]),
    (
        "Get-ChildItem | Sort-Object Length -Descending | Select-Object -First 3 Name, Length | Format-Table -AutoSize",
        &[
            "Get-ChildItem",
            "Sort-Object",
            "Select-Object",
            "Format-Table",
        ],
    ),
    ("$x = if ($true) { 1 } else { 2 }", &[]),
    ("while ($i -lt 3) { $i++; Write-Host $i }", &["Write-Host"]),
    (
        "Get-Content x | Set-Content y; Copy-Item y z",
        &["Get-Content", "Set-Content", "Copy-Item"],
    ),
    (
        "echo hi > out.txt; echo there >> out.txt",
        &["echo", "echo"],
    ),
    ("Start-Sleep 2; Get-Date", &["Start-Sleep", "Get-Date"]),
    ("ls | sort | select -First 2", &["ls", "sort", "select"]),
    (
        "Get-Content a.txt | ForEach-Object -Process { $_.ToUpper() } | Out-Null",
        &["Get-Content", "ForEach-Object", "Out-Null"],
    ),
];

#[test]
fn s6_corpus_lines_read_as_the_oracle_does() {
    for (src, want) in CORPUS {
        ok(src, want);
    }
}

/// Fake engine: counts calls and replies with a canned result.
struct Fake {
    calls: usize,
    reply: Option<Result<Vec<String>, EngineError>>,
}

impl Fake {
    fn replying(reply: Result<Vec<String>, EngineError>) -> Self {
        Fake {
            calls: 0,
            reply: Some(reply),
        }
    }
}

impl NamesEngine for Fake {
    fn names(&mut self, _src: &str) -> Result<Vec<String>, EngineError> {
        self.calls += 1;
        self.reply
            .take()
            .unwrap_or_else(|| Err(EngineError::Down("no reply".to_string())))
    }
}

// The scanner reads `a | b`; the unterminated quote `'abc` is refused by the scanner.
const SCANNER_OK: &str = "a | b";
const SCANNER_ERR: &str = "'abc";

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn s6_scanner_ok_uses_scanner_and_skips_engine() {
    let mut fake = Fake::replying(Ok(names(&["from-engine"])));
    let got = read_lines_with(SCANNER_OK, Some(&mut fake));
    assert_eq!(
        got,
        PwshReading::Names {
            names: names(&["a", "b"]),
            source: Source::Scanner
        }
    );
    assert_eq!(
        fake.calls, 0,
        "engine must not be called when the scanner reads"
    );
}

#[test]
fn s6_scanner_err_asks_the_engine() {
    let mut fake = Fake::replying(Ok(names(&["x", "y"])));
    let got = read_lines_with(SCANNER_ERR, Some(&mut fake));
    assert_eq!(
        got,
        PwshReading::Names {
            names: names(&["x", "y"]),
            source: Source::Engine
        }
    );
    assert_eq!(fake.calls, 1);
}

#[test]
fn s6_engine_error_is_unreadable() {
    for reply in [
        EngineError::Down("gone".to_string()),
        EngineError::Protocol("bad".to_string()),
        EngineError::Timeout,
        EngineError::ParseErrors(2),
    ] {
        let mut fake = Fake::replying(Err(reply));
        let got = read_lines_with(SCANNER_ERR, Some(&mut fake));
        assert!(matches!(got, PwshReading::Unreadable(_)), "got {got:?}");
        assert_eq!(fake.calls, 1);
    }
}

#[test]
fn s6_oversized_input_never_reaches_the_engine() {
    let big = "a | ".repeat(100_000) + "b";
    assert!(big.len() > 256 * 1024);
    let mut fake = Fake::replying(Ok(names(&["from-engine"])));
    let got = read_lines_with(&big, Some(&mut fake));
    assert!(matches!(got, PwshReading::Unreadable(_)), "got {got:?}");
    assert_eq!(fake.calls, 0, "engine must not see oversized input");
}

#[test]
fn s6_nul_input_never_reaches_the_engine() {
    let mut fake = Fake::replying(Ok(names(&["from-engine"])));
    let got = read_lines_with("a\0 | b", Some(&mut fake));
    assert!(matches!(got, PwshReading::Unreadable(_)), "got {got:?}");
    assert_eq!(fake.calls, 0, "engine must not see NUL input");
}

#[test]
fn s6_no_engine_and_scanner_err_is_unreadable() {
    let got = read_lines_with::<Fake>(SCANNER_ERR, None);
    assert!(matches!(got, PwshReading::Unreadable(_)), "got {got:?}");
}

#[test]
fn s6_no_engine_but_scanner_ok_still_reads() {
    let got = read_lines_with::<Fake>(SCANNER_OK, None);
    assert_eq!(
        got,
        PwshReading::Names {
            names: names(&["a", "b"]),
            source: Source::Scanner
        }
    );
}
