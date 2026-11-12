What the shell is for and not for, and how to write portable, safe commands.

# Shell and platform

## What shell is for
Read-only inspection:
- listing directories, `grep`/`find`-style searches when the search tool is not enough
- `git status`, `git diff`, `git log`, `git show` (always `git --no-pager`)
- reading tool versions, checking that a path exists
- copying files and creating directories, when the task needs it

## What shell is NOT for
- running tests, builds, linters, formatters or code generators (hand these back under `## Commands to run`)
- installing or updating dependencies
- network access
- git writes: commit, add, reset, checkout, stash, rebase, push
- deleting or moving files (a move deletes the original), killing processes
- changing system or user configuration

If a task seems to need one of these, stop and report it under Risks.

## Prefer dedicated tools
Use the read, search and edit tools instead of `cat`, `grep`, `sed`, `echo >`. They are bounded, portable and preserve file content. Editing through shell can change line endings (CRLF vs LF) or encoding (a BOM); use the edit tool so the file's existing endings survive.

## Detect OS and shell first
Do not assume POSIX. Check the platform stated in your context, or run a harmless probe, before writing commands.
- Windows PowerShell 5.1: no `&&` or `||` (run commands separately); `$null` not `/dev/null`; env vars as `$env:NAME`; `\` separators, though `/` usually works; quote paths with spaces in double quotes; `head`/`tail`/`wc`/`which` do not exist (use `Select-Object -First N`, `Get-Command`).
- Git Bash / POSIX sh: `/dev/null`, `$VAR`, forward slashes; `C:\x` appears as `/c/x`.
- A `bash` on PATH on Windows may be WSL, with a different filesystem view and different tools. If the project names a specific bash path, use that.
- macOS vs Linux: `sed -i` and `date` flags differ; avoid them.

## Paths
Use absolute paths; the working directory may reset between calls. Quote any path with spaces. Do not write outside the repo or the task's stated directories.

## Non-interactive only
Never start editors, pagers, REPLs or anything that prompts. Use `--no-pager` and pass all input as arguments. If a command might wait on stdin, do not run it.

## Environment
Do not change PATH, env vars, git config or shell profiles in ways project instructions warn against. Read CLAUDE.md/AGENTS.md first when unsure.

## Keep output bounded
Limit results: `git log -n 10`, `git diff --stat` before a full diff, `Select-Object -First 50` or `| head -50` (shell permitting), narrow globs. Large dumps waste context and hide the relevant line.
