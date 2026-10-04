<div align="center">

<img src="shell/src-tauri/icons/128x128@2x.png" alt="NucleOS" width="112" height="112">

# NucleOS

**An autonomous engineering colleague for your desktop.**<br>
NucleOS runs coding agents on your projects in the background, within the limits you set.

[![CI](https://github.com/zDud4s/nucleos/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/zDud4s/nucleos/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-lightgrey)
![Status](https://img.shields.io/badge/status-pre--release-orange)

[Features](#features) ·
[Architecture](#architecture) ·
[Getting started](#getting-started) ·
[Development](#development) ·
[Security](#security) ·
[License](#license)

</div>

---

> [!IMPORTANT]
> NucleOS is **pre-release software**. There are no packaged builds yet, and its interfaces may
> change between commits. Windows is the primary platform; every commit is also built and tested on
> Linux and macOS in CI.

## Overview

Most agent tools wait for a prompt. NucleOS keeps working after you stop typing: it handles routine
upkeep, makes progress on real issues, and asks you only when something warrants a decision.

It does this without replacing your agent. NucleOS is a model-agnostic harness: it drives existing
agent CLIs, such as [Claude Code](https://docs.claude.com/en/docs/claude-code),
[Codex CLI](https://github.com/openai/codex) or a local model through [Ollama](https://ollama.com).
Around them it adds what unattended work needs: scheduling, isolation, a risk model and an audit
trail.

## Features

| | |
|---|---|
| **Autopilot** | Starts runs from schedules, events or rules. Covers triage, dependency upkeep, flaky tests, and feature and bug work. |
| **Risk classification** | The daemon checks every tool call an agent makes before it runs. Risky actions are held as proposals until you approve or reject them. |
| **Worktree isolation** | Each run works in its own git worktree. A queue merges one branch at a time, and a failed merge leaves no conflicted state behind. |
| **Shadow mode** | A project can be set to propose only, so you can watch what NucleOS would do before it acts. |
| **Budgets and kill switch** | Spend is capped per window, and one switch stops all activity. |
| **Isolated integrations** | Email, Telegram, web fetch, a fenced browser and quota tracking each run as a separate Go sidecar that can fail and restart on its own. |
| **Desktop shell** | A tray app shows what ran, what is waiting on you, and whether anything needs attention. |

## Architecture

```
┌──────────────────────────────────────┐
│ shell/   Tauri v2 + React            │  Thin client. Holds no state of its own.
└──────────────────┬───────────────────┘
                   │ HTTP + WebSocket · localhost · bearer token
┌──────────────────▼───────────────────┐
│ core/    nucleos-core  (Rust)        │  Scheduler, runs, classifier, git queue.
│                                      │  The only process that writes SQLite.
└──────┬───────────────────────┬───────┘
       │ spawns                │ supervises
┌──────▼───────────┐   ┌───────▼──────────────────────────────────┐
│ agent runner     │   │ sidecars/  (Go)                          │
│ Claude Code,     │   │ email · telegram · web · browser · quota │
│ Codex, local …   │   └──────────────────────────────────────────┘
│ one per run, in  │
│ its own worktree │
└──────────────────┘
```

**Design invariants**

- Only the daemon writes the database. Sidecars never open it.
- Secrets are kept in the OS credential store (currently the Windows Credential Manager), never in
  plain files.
- The local API binds to localhost and requires a bearer token.
- Each integration is its own process and fails on its own.

The module map and extension points are documented in [`core/AGENTS.md`](core/AGENTS.md). The
threat model is in [`THREAT_MODEL.md`](THREAT_MODEL.md).

## Getting started

### Prerequisites

| Tool | Version |
|---|---|
| Rust | Pinned in [`rust-toolchain.toml`](rust-toolchain.toml); installed by rustup |
| Go | 1.26 |
| Node.js | Current LTS |
| Python | 3.x |
| An agent runner | At least one: `claude` (Claude Code) or `codex` on `PATH` and logged in, or a local model served by Ollama |
| Tauri v2 | Platform prerequisites from [v2.tauri.app](https://v2.tauri.app/start/prerequisites/) |

Run `scripts/doctor.sh` to check your environment. It reports each missing tool and how to install
it.

<details>
<summary><strong>Windows notes</strong></summary>

- Run the scripts from **Git Bash**. On many machines the `bash` on `PATH` is WSL, which does not
  have this toolchain.
- The tested Rust host is GNU: `rustup set default-host x86_64-pc-windows-gnu`.
- Install the Rust toolchain on a path without spaces. A space in the profile path breaks the GNU
  linker.

</details>

### Build and run

```sh
git clone https://github.com/zDud4s/nucleos.git
cd nucleos

# Daemon and sidecars
cargo build -p nucleos-core
scripts/build-sidecars.sh
cargo run -p nucleos-core
```

In a second terminal:

```sh
cd shell
npm install
npm run tauri dev
```

On Windows, use `powershell -File scripts/run-daemon.ps1` instead of `cargo run`. It runs a copy of
the binaries, so the running daemon does not lock the files your next build needs to replace.

### Add a project

Open **Projects** in the shell and add a repository. Onboarding installs the classifier hook in the
project; for Claude Code that means its `.claude/settings.json`. Until the project is onboarded,
autopilot will not act on it.

## Repository layout

| Path | Contents |
|---|---|
| [`core/`](core) | The daemon, `nucleos-core` (Rust) |
| [`core/hooks/`](core/hooks) | Source of the classifier hook, compiled into the daemon |
| [`sidecars/`](sidecars) | One Go module per integration |
| [`shell/`](shell) | Desktop client (Tauri + React) |
| [`scripts/`](scripts) | Quality gates, diagnostics and build helpers |

## Development

`scripts/gates.sh` defines what green means, and CI runs the same script:

```sh
scripts/gates.sh            # everything
scripts/gates.sh core       # one target: core | sidecars | shell | tauri | hooks | security
```

Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request.

## Security

Please do not report vulnerabilities in public issues. Use a
[private security advisory](https://github.com/zDud4s/nucleos/security/advisories/new) instead.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

The fonts bundled in [`shell/src/assets/fonts/`](shell/src/assets/fonts) are distributed under the
SIL Open Font License 1.1. Each licence file sits next to its font.
