# core/ — Rust núcleo

The núcleo library crate (`nucleos-core`), with its binary in the `nucleos-daemon` package
(`crates/nucleos-daemon`, the exe is still `nucleos-core`) and the top modules nothing here reads in
`nucleos-app` (`crates/nucleos-app`, root `src/app.rs`). Owns the local HTTP API, the SQLite datastore,
secrets access, sidecar supervision, and the CLI-wrap agent loop. Entrypoint: `src/main.rs`
(pure startup orchestration — no domain logic of its own).

**Foundation status: complete.** Every module in spec §3.2's map is implemented and green:
`cargo test -p nucleos-core --lib` = 1128 passed + 3 ignored (the ignored ones touch the real OS Credential
Manager / Task Scheduler; run with `-- --include-ignored` on an interactive desktop session).
> A count in a document is a claim that goes stale without anybody editing it — this line read
> `276 passed + 2 ignored` for long enough that four times as many tests had been written under it.
> Treat it as the order of magnitude it is, and `cargo test -p nucleos-core --lib` (plus `-p nucleos-app`, `-p nucleos-daemon`) as the authority.


## Module map — the structural invariant (spec §3.2)

The module map (one row per `.rs` file; the contract, not a snapshot) lives in `core/MODULE_MAP.md`. A new module needs a row there.

## Autopilot extends, it does NOT restructure (spec §3.2 / §9)

The next pillar ADDS modules; it must not rewrite the map above. The seams are already in place —
build against them, don't reshape them:

- **Risk classifier** → replaces the stub inside `hooks.rs`. The payload already carries `run_id`,
  validated against in-flight runs; `pending_approval` already drives active termination.
- **Approval-pause / resume** → reuses `runs::finalize_termination` (the atomic-handle arbiter) plus
  the `session_id` / `cost_usd` columns already on `runs` (captured early, so a paused run is resumable).
- **Per-project rules** → superseded: `config::load_schedule_rules` (the typed `autopilot.yaml` file reader) is removed;
  the rules are the stored `autopilot` document, read from the settings snapshot (`project/rules.rs`), driven by
  `scheduler.rs` / `repo_trigger.rs`, + the `project_id` / `cwd` columns on `runs`.
  The untyped `discover_autopilot_config` this line used to name was superseded and is gone.
- **New tables** (`feed`, `proposals`, `budget_ledger`, …) → a new numbered migration under
  `migrations/` **and a new domain module each, owning its own SQL** (the `runs.rs` pattern) — never
  bloat `storage.rs`, which stays table-agnostic.
- **New cross-cutting deps** → add a field to `state.rs`'s `AppState`, nowhere else.

Rule of thumb: a new concern = a new file (+ its migration), wired in `main.rs` and routed in
`http.rs`. If a change would make one module know about another's transport internals or another
table's SQL, **stop** — that coupling is exactly the drift this map exists to prevent.

## Cancellation safety — the invariant that keeps being rediscovered

A dropped future stops at its last `.await` and never runs another line. Two things drop futures
here, and they are one mechanism in two costumes: `abort()` on a run's task
(`runs::finalize_termination`, reached by `/cancel` and by a `pending_approval` pause), and a client
disconnecting, which cancels the request it was making. Cleanup or follow-up written as statements
after an `.await` is therefore not cleanup — it is happy-path-only code.

It matters more here than in most services because of
`one_open_worktree_run_per_project` (migration 0009): a run stranded at `running` or
`awaiting_approval` blocks *every* later worktree run for that project, and only a daemon restart
reconciles `running`. A dropped request is not a lost reply, it is a jammed project.

Three rules, each with a worked example in the tree:

- **Own a resource with a guard, built before the task and captured by it — never inside.** See
  `assistant::ChatSlot` / `TurnGuard` and `runs::Registration`. A task aborted before its first poll
  drops its captured state without running a line of the body, so a guard constructed inside would
  never exist at all; a captured one is dropped either way. (A type implementing `Drop` is captured
  whole, so RFC-2229 precise capture cannot split it.)
- **A handler that mutates durable state across awaits goes through `http::uncancellable`.** Awaiting
  the JoinHandle leaves the response exactly as it was; dropping a JoinHandle only detaches its task.
- **Never sequence an irreversible act before the record that makes it actionable**, unless the pair
  is uncancellable together. `hooks::pause_for_approval` is the case that bites hardest: terminating
  a run kills the CLI whose hook script owns the very connection being answered.

  It now has **two** sides past that `.await`, and they order their writes differently because what
  they cannot afford to lose is different. An ordinary run terminates, records the proposal, and
  ROLLS THE PAUSE BACK if that write fails — a run parked with nothing to approve holds migration
  0009's index and jams the project. A job's node (`skip_the_item`) marks its item `skipped` FIRST
  and rolls nothing back: an item left `running` in a job nobody drives makes `next_step` answer
  `Wait` for ever, where a skipped item missing its proposal is only a reminder nobody gets. Same
  hazard, opposite worst case, so the order is opposite too.

  **Which side a run takes is decided by that mark, not by `job_id`.** A job's plan node runs before
  the queue exists and its review node runs after every item — both carry a `job_id` and neither owns
  an item, so `put_the_item_down` answers `false` and they park and ask like any other run. Branching
  on `job_id` alone left them terminated with no proposal: the exact state the rollback above exists
  to prevent, found by dogfooding on 2026-08-07 and not by the tests, which built their item by hand.

  **And a skip must clear `runs.status` as well as its own record**, or the job stops one step later
  for a different reason. `awaiting_approval` stops a job twice over: `job::node_awaiting_approval`
  parks the whole job on any run holding it, and migration 0009's index counts it as an open worktree
  slot, so the next item cannot start either. `skip_the_item` writes `interrupted` — the same verdict
  `reconcile_stranded_approvals` would write at the next startup — after the item's mark, because
  `job::reconcile_nodes` would read a still-`running` item whose run has left flight as `failed`.

## Constraints

- **This crate is the ONLY writer of SQLite.** Sidecars never open the DB file — they talk to the local HTTP API.
- **Wrap the existing agent CLI; never reimplement the agent loop** (spec §3.1).
- **Secrets go through the OS Credential Manager** (`secrets.rs`) — no own crypto, no plaintext secrets
  on disk (the daemon token included, since Chunk 1 Task 5).
- Bind the local API to **localhost** and guard it with the bearer token (spec §3.3–§3.4).

## Commands

On Windows, keep the Rust toolchain on a path without spaces: a user profile with a space in its name
breaks the GNU linker, so point `RUSTUP_HOME` and `CARGO_HOME` at a space-free directory and put its
`bin` on PATH. Then, from the repo root:

- Gate: `cargo nextest run -p nucleos-base -p nucleos-core -p nucleos-app -p nucleos-daemon --no-fail-fast` (every target of all four;
  `scripts/gates.sh core` falls back to `cargo test` without nextest). One test, one target:
  `-p nucleos-base <name>` for a module under `crates/nucleos-base/src`, `--lib <name>` for one in the
  core library (every other module but those below), `-p nucleos-app <name>` for one of the moved modules (`hooks`,
  `fleet`, `devtime`, `analytics`, `analytics_pipeline`, `price_refresh`, `verify_cli`, `presets`,
  `assistant`, `channels::{commands, ingest, media}`, `consolidate`, `map` but `map::intent`),
  `-p nucleos-daemon <name>` for one in `http.rs`, `main.rs`, `cli.rs` or `startup.rs` — each builds only that test binary.
- `cargo build` · `cargo fmt --all` (check: `cargo fmt --all -- --check`) · `cargo clippy --all-targets`
- After a `claude update`: run `node scripts/tool-surface.mjs`. It measures the CLI's own advertised
  tool set against `runner.rs`'s `BUILTIN_TOOLS` blocklist and exits non-zero naming any tool the CLI
  now offers that the blocklist does not yet deny — the check `advertised_tools_violate` otherwise
  makes at runtime, on a real conversation, too late to matter.
