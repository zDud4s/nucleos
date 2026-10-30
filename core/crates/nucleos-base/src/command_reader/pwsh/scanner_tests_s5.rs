//! S5 RED: fail-closed table, no-panic sweep, size limit, oracle-fixed odd inputs.

use super::scanner::command_names;
use super::scanner_tests_common::{err, ok};

#[test]
fn s5_fail_closed_table() {
    // `--%` stops parsing (oracle accepts it; the subset refuses)
    err("a --% b");
    // `--%`
    err("--%");
    // `--%` (oracle accepts it; the subset refuses)
    err("echo --% $x | y");
    // trailing backtick
    err("a `");
    // unterminated single quote
    err("echo 'abc");
    // unterminated double quote
    err("echo \"abc");
    // unterminated here-string
    err("@'\nabc\n");
    // unterminated expandable here-string
    err("@\"\nabc\n");
    // terminator not at line start
    err("@'\nabc\n'@ x\n'@");
    // unterminated block comment
    err("<# abc");
    // unterminated block comment mid-line
    err("a <# b");
    // open brace
    err("{ a");
    // extra close brace
    err("a }");
    // open paren
    err("( a");
    // extra close paren
    err("a )");
    // open subexpression
    err("$(a");
    // open subexpression in arg
    err("echo $(a");
    // open subexpression in string
    err("echo \"$(a\"");
    // open array
    err("@(a");
    // open hashtable
    err("@{ a = 1");
    // NUL byte (oracle accepts it; the subset refuses)
    err("a\0b");
    // lone NUL (oracle accepts it; the subset refuses)
    err("\0");
    // label (oracle accepts it; the subset refuses)
    err(":outer foreach ($i in a) { b }");
}

#[test]
fn s5_odd_inputs_follow_the_oracle() {
    // curly quotes
    ok("echo \u{201c}a\u{201d}", &["echo"]);
    // curly quotes with space
    ok("echo \u{201c}a b\u{201d}", &["echo"]);
    // subexpression inside single quotes is text
    ok("'$(a)'", &[]);
    // escaped subexpression inside double quotes
    ok("\"`$(a)\"", &[]);
    // live subexpression inside double quotes
    ok("\"$(a)\"", &["a"]);
    // dynamic call target
    ok("& $cmd", &["$cmd"]);
    // dynamic call target with arg
    ok("& $cmd arg", &["$cmd"]);
    // dynamic dot-source target
    ok(". $cmd", &["$cmd"]);
    // scriptblock factory is an expression
    ok("[scriptblock]::Create('a')", &[]);
    // Invoke-Expression is just a name
    ok("Invoke-Expression $x", &["Invoke-Expression"]);
    // iex is just a name
    ok("iex $x", &["iex"]);
    // lone CR inside here-string
    ok("@'\r\nabc\rdef\r\n'@", &[]);
    // CRLF here-string
    ok("@'\r\nabc\r\n'@", &[]);
    // NBSP
    ok("a\u{a0}b", &["a"]);
    // en dash
    ok("echo a\u{2013}b", &["echo"]);
    // curly single quotes
    ok("echo \u{2018}a\u{2019}", &["echo"]);
}

/// Real corpus lines (plus two synthetic ones) used by the truncation sweep.
const SWEEP_LINES: &[&str] = &[
    "cd C:\\Projects\\nucleos\\sidecars\\remote; gofmt -l .; \"gofmt done\"",
    "Get-CimInstance Win32_Process -Filter \"Name='nucleos-core.exe'\" | ForEach-Object { \"{0} parent={1} {2}\" -f $_.ProcessId, $_.ParentProcessId, $_.CommandLine }",
    "$env:COMPUTERNAME; $env:USERNAME; (Get-CimInstance Win32_ComputerSystem | Select-Object Manufacturer,Model | Format-List | Out-String).Trim()",
    "winget install --id Ollama.Ollama -e --accept-source-agreements --accept-package-agreements --silent",
    "Get-ChildItem -Recurse -Filter *.rs | Select-Object -First 5",
    "Get-Content foo.txt | Select-String 'bar' | Measure-Object",
    "git add -A && git commit -m \"msg\"",
    "Test-Path .\\a.txt || Write-Host \"missing\"",
    "$x = Get-Process | Where-Object { $_.CPU -gt 10 } | Sort-Object CPU -Descending",
    "foreach ($f in Get-ChildItem *.txt) { Remove-Item $f }",
    "Write-Host \"value: $(Get-Date -Format yyyy)\"",
    "if (Test-Path x) { Get-Content x } else { Write-Host none }",
    "try { Invoke-WebRequest http://x } catch { Write-Error $_ }",
    "Get-Content a.log -Tail 20 | Where-Object { $_ -match 'ERR' }",
    "& \"C:\\Program Files\\Git\\bin\\git.exe\" log --oneline -5",
    "1..5 | ForEach-Object { $_ * 2 }",
    "Get-Content x | ForEach-Object { if ($_ -eq 'a') { 'A' } else { $_ } }",
    "Get-ChildItem | % { $_.FullName } | ? { $_ -like '*x*' }",
    "git log --format='%H %s' -n 3 | ForEach-Object { $_.Split(' ')[0] }",
    "switch ($x) { 1 { 'one' } default { 'other' } }",
    "Get-Process -Name \"a`;b\"",
    "$x = if ($true) { 1 } else { 2 }",
    "$r = Invoke-RestMethod -Uri \"http://localhost:1/api\" -Method Post -Body (@{a=1} | ConvertTo-Json)",
    "Get-Content a.txt | ForEach-Object -Process { $_.ToUpper() } | Out-Null",
    "@'\nabc $(a)\n'@ | b",
    "<# c #> a `\n b",
];

/// Every prefix cut at a char boundary of every line returns `Err` or a list, never a panic.
#[test]
fn s5_no_panic_on_any_truncation() {
    for line in SWEEP_LINES {
        for cut in 0..=line.len() {
            if !line.is_char_boundary(cut) {
                continue;
            }
            let prefix = &line[..cut];
            let got = std::panic::catch_unwind(|| command_names(prefix));
            assert!(got.is_ok(), "scanner panicked on prefix {prefix:?}");
        }
    }
}

/// The sweep is only meaningful if the scanner reads most untruncated lines.
#[test]
fn s5_sweep_lines_are_mostly_readable_untruncated() {
    let readable = SWEEP_LINES
        .iter()
        .filter(|l| command_names(l).is_ok())
        .count();
    assert!(
        readable * 2 >= SWEEP_LINES.len(),
        "only {readable} of {} sweep lines are readable",
        SWEEP_LINES.len()
    );
}

#[test]
fn s5_size_limit_is_256_kib() {
    // 1 MB of `a;` is refused outright, quickly, without exhausting the stack.
    err(&"a;".repeat(500_000));
    // Just over the limit.
    err(&"a;".repeat(256 * 1024 / 2 + 1));
    // Well under the limit still reads.
    let want: Vec<&str> = vec!["a"; 1000];
    ok(&"a;".repeat(1000), &want);
}
