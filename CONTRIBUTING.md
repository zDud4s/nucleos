# Contributing to NucleOS

Thank you for your interest in NucleOS. It is an early-stage project with one maintainer, so it
helps to agree on direction before you put in the work.

- [Before you start](#before-you-start)
- [Development setup](#development-setup)
- [Making a change](#making-a-change)
- [Quality gates](#quality-gates)
- [Conventions](#conventions)
- [Security-sensitive areas](#security-sensitive-areas)
- [Pull requests](#pull-requests)
- [Working through NucleOS itself](#working-through-nucleos-itself)
- [License](#license)

## Before you start

- **Bugs:** open an issue with steps to reproduce, what you expected and what happened. Include
  your OS and the daemon's log lines if you have them.
- **Features and larger changes:** open an issue first and describe the problem you want to solve.
  A short discussion up front saves a rewrite later.
- **Vulnerabilities:** do not open a public issue. See [Security](README.md#security).

## Development setup

Follow [Getting started](README.md#getting-started) in the README, then run:

```sh
scripts/doctor.sh
```

It checks every toolchain the monorepo needs and tells you how to fix anything that is missing.

## Making a change

The repository has three parts, and each has its own rules:

| Part | Read first |
|---|---|
| `core/` (Rust daemon) | [`core/AGENTS.md`](core/AGENTS.md): architectural invariants and the module map |
| `sidecars/` (Go) | The doc comment at the top of each sidecar's `main.go` |
| `shell/` (Tauri + React) | The test file next to each page and component |

Rules that apply everywhere:

- **Keep changes small and focused.** One concern per pull request.
- **Add tests with the change.** A bug fix should come with a test that fails without it.
- **Respect the invariants.** Only the daemon writes SQLite. Secrets go through the OS credential
  store. The local API binds to localhost. Each integration runs as its own sidecar.
- **Add new modules to the module map.** A new file in `core/src/` must be listed in the map in
  `core/AGENTS.md`, and `core/tests/module_map.rs` fails until it is.
- **Edit the classifier hook in one place.** `core/hooks/ask_daemon.py` is the source, and it is
  compiled into the daemon. `.claude/hooks/ask_daemon.py` is a byte-for-byte copy, and
  `scripts/test-hook-filter.py` keeps the two equal. Edit the source, then copy it over.

## Quality gates

`scripts/gates.sh` is the single definition of green, and CI runs the same script on Windows, Linux
and macOS:

```sh
scripts/gates.sh                 # everything
scripts/gates.sh core            # Rust: fmt, clippy, tests
scripts/gates.sh sidecars        # Go: gofmt, vet, tests
scripts/gates.sh shell           # TypeScript: typecheck, tests, CSP check
scripts/gates.sh tauri           # the Tauri crate: fmt, clippy, tests
scripts/gates.sh hooks           # the classifier hook and the repository scripts
scripts/gates.sh security        # dependency audit and secret scanning
```

Run the targets your change touches before you open a pull request. On Windows, run it from Git Bash.

**About `.gitleaksignore`:** test fixtures that look like secrets (fake keys used to test the
redactor) are listed there by fingerprint. Add an entry only for a value you have read and
confirmed is not a credential, and add a comment explaining why.

## Conventions

**Language.** Code comments, commit messages and UI strings are in English.

**Comments explain why.** The code shows what it does. A comment is worth writing when it records
a reason, a constraint, or a mistake that should not be repeated.

**Commit messages** follow `type(scope): summary`:

```
fix(shell): Waiting's git section lists /waiting/git and can put a row away
feat(core): run the email triage loop
```

- Types: `feat`, `fix`, `test`, `refactor`, `perf`, `docs`, `style`, `chore`.
- Scopes: `core`, `shell`, `sidecars`, or a narrower area such as `classifier` or `gates`.
- Write the summary as a sentence about the behaviour, not about which file changed.

**Formatting** is enforced by the gates: `cargo fmt`, `gofmt`, and the shell's TypeScript checks.

## Security-sensitive areas

Changes to these areas get closer review. Explain the threat you considered in the pull request,
and update [`THREAT_MODEL.md`](THREAT_MODEL.md) if what the system defends against changes:

- the tool-call classifier and its hook (`core/src/classifier.rs`, `core/hooks/`)
- secret redaction (`core/src/redact.rs`)
- the git queue and worktree handling
- the browser and web sidecars, and their fences
- anything that opens a network connection or reads a credential

## Pull requests

1. Fork the repository and create a branch from `master`.
2. Make your change, with tests.
3. Run the relevant gates until they are green.
4. Open a pull request that describes the problem, the change, and how you verified it.

CI must pass before a pull request is merged. Keep the pull request up to date with `master` by
merging, not rebasing, once review has started.

## Working through NucleOS itself

This section applies only if you run NucleOS against this repository, so that agents work on it in
worktrees managed by the daemon. If you don't, you can skip it.

In that setup the classifier hook routes git operations through a **queue**, which runs them one at
a time so that parallel sessions don't race on the shared `.git`:

- `git merge`, `git push`, `git tag`, `git fetch` and `git branch -d` are taken over by the queue,
  which returns a ticket number instead of running them directly.
- Force pushes, a bare `git push`, `git pull` and `git branch -D` are refused. The refusal tells
  you what to use instead.
- Rebasing a session's own branch is always refused.
- To deliver finished work, run `nucleos-core --land` from the worktree. It asks the queue to merge
  the branch into the project's branch.
- If that merge conflicts, nothing is published and no conflicted state is left behind. Merge the
  target branch into yours, resolve the conflict there, and land again.

When the hook blocks a command, it says why. Do not work around it with a different spelling of
the same command.

## License

By contributing, you agree that your contributions will be licensed under the
[Apache License 2.0](LICENSE), the same license that covers the project.
