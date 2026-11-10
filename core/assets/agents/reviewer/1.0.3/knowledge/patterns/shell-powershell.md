Review patterns for bash and PowerShell 5.1 scripts on Windows/macOS/Linux: bugs checkable by reading.

Bash
- No `set -euo pipefail` (or `-e` defeated by `||`, `if cmd`, or a pipeline). Tell: `cmd | tee log` hides cmd's failure without pipefail.
- Unquoted `$var`/`$@`/`$(...)`: word splitting and globbing on paths with spaces. Use `"$var"`, `"$@"`, arrays.
- `cd dir` with no `|| exit`: later commands run in the wrong directory.
- `rm -rf "$VAR/"` or `"$VAR"/*` with an unset or empty var. Want `${VAR:?}` plus a validated path.
- `[ ]` vs `[[ ]]`: `[ $x = y ]` breaks on empty or spaced values.
- `read` without `-r` eats backslashes; `for f in $(ls)` or `find | while read` mishandles names (use `-print0`, `read -d ''`).
- `mktemp` without a `trap ... EXIT` cleanup; predictable `/tmp/name` (symlink race).
- CRLF line endings: `\r` in the shebang or `$'\r'` in vars; `bad interpreter`. Check `.gitattributes` (`eol=lf` for `*.sh`).
- `sed -i`: GNU `-i` vs BSD `-i ''`; `grep -P`, `readlink -f`, `date -d`, are GNU-only (macOS breaks).
- Injection: `eval`, `bash -c "$x"`, `sh -c "...$input..."`, or interpolating untrusted text (branch names, file names, agent output) into a command or `sed` expression. Pass as arguments, not strings.

PowerShell 5.1
- `&&` and `||` are parser errors; use `A; if ($?) { B }`.
- `2>&1` on a native exe wraps stderr lines in ErrorRecords and sets `$?` false even on exit 0.
- `$ErrorActionPreference = 'Stop'` does not cover native exit codes; check `$LASTEXITCODE` after each native call.
- `Set-Content`/`Add-Content` default to ANSI; pass `-Encoding utf8` (5.1 writes a BOM, which breaks JSON, shebang and YAML consumers).
- `Remove-Item -Recurse -Force $path`: `$path` empty, `$null` or a parent resolves wrongly; 5.1 `Remove-Item -Recurse` can follow directory symlinks or junctions into the target (e.g. `shell\node_modules`). Tell: no `Test-Path`, no containment check, no junction check (`Attributes -band ReparsePoint`).
- `-Filter` (provider-side, one pattern) vs `-Include` (needs `-Recurse` or a wildcard path, else silently ignored).
- Unquoted paths with spaces (`C:\Users\PC Multimedia`); `Start-Process -ArgumentList` joins args without quoting: embedded quotes or spaces split; pass an array and pre-quote.
- `ConvertFrom-Json` yields PSCustomObject (no `-AsHashtable`).
- Executing downloaded content: `iwr | iex`, running a fetched script with no hash or signature check.

Windows
- Path length (260), case-insensitive paths (comparisons must not be case-sensitive), locked files (running exe, `os error 5/32`).
- `python` on Windows, `python3` on macOS/Linux: deliberately not unified here; do not flag. `bash` on PATH may be WSL; Git Bash is `C:\Program Files\Git\bin\bash.exe`.

Destructive or mutating scripts
- Is there a dry-run or confirmation? Is the target validated (non-empty, inside the expected root, not a junction/symlink)?
