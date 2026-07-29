# NucleOS threat model

## Trust boundary

NucleOS is a single-user, localhost-bound desktop application. The owner of the desktop is the administrator. The daemon binds to localhost and is guarded by bearer authentication. Its long-lived control token is persisted in the OS Credential Manager, then loaded into the daemon; run and email-sidecar tokens are minted separately and scoped by `core/src/auth.rs`.

## What this does NOT try to prevent

This product does not try to prevent the owner from running arbitrary code through the agent. That is the product, not a vulnerability.

## What this DOES try to prevent

NucleOS does try to prevent these failures:

1. An email body causing a tool call.
2. A run acting outside its worktree.
3. The long-lived daemon control token reaching an autonomous run.
4. An approval that the classifier should have blocked.

## Tool allowlist by trigger class

Spec §9 requires a tool allowlist derived from trigger confidence: externally sourced content marked untrusted must never trigger high-risk actions on its own.

| Trigger | Content origin | Confidence | Tools permitted | Requires approval |
| --- | --- | --- | --- | --- |
| cron (scheduled) | Owner configuration | Trusted | `ToolPolicy::Unrestricted` | When `classifier.rs` returns `pending_approval` |
| repo (branch SHA moved) | Owner repository configuration and branch state | Trusted | `ToolPolicy::Unrestricted` | When `classifier.rs` returns `pending_approval` |
| email (triage) | Email content chosen by a stranger | Untrusted | `ToolPolicy::None` — no tools at all | No — no tool can be invoked |
| telegram / assistant | Paired owner over a network | Semi-trusted | `ToolPolicy::McpOnly` — NucleOS MCP tools only; built-ins denied | No — `hooks.rs` allows only sanctioned NucleOS MCP tools and bypasses the classifier |
| manual (`POST /runs`) | Owner action | Trusted | `ToolPolicy::Unrestricted` for `real`, `shadow`, and `worktree`; `ToolPolicy::None` when the submitted mode is `email_triage` | For unrestricted modes, when `classifier.rs` returns `pending_approval`; no for `email_triage`, which has no tools |

The scheduler (`core/src/scheduler.rs`) creates cron runs through `runs::create_run_inner`. The repository trigger (`core/src/repo_trigger.rs`) fires runs the same way. Email triage (`core/src/triage.rs` and `core/src/email.rs`) uses `ToolPolicy::None`. Assistant turns from `core/src/assistant.rs` use `ToolPolicy::McpOnly`. `POST /runs` in `core/src/runs.rs` passes its submitted mode to `create_run_inner`, which selects `ToolPolicy::None` only for `email_triage` and `ToolPolicy::Unrestricted` otherwise.

## Existing barriers

- `core/src/classifier.rs` is a pure deterministic lexical classifier. `CLASSIFIER_VERSION = 2`; it returns `allow`, `deny`, or `pending_approval` together with an `action_class`. It performs no I/O, makes no database access, and has no knowledge of run state.
- `core/src/hooks.rs` provides the `PreToolUse` decision endpoint. It is the enforcement point.
- Spec §5.5 requires two independent triage barriers, documented at the top of `core/src/triage.rs`. Barrier 1 is `ToolPolicy::None` in `core/src/runner.rs`: the CLI refuses on its own. Barrier 2 is the cooperative `PreToolUse` hook. It runs only when the `.claude/settings.json` resolved from the run's working directory registers it. A triage run therefore receives its own sandbox directory instead of inheriting the daemon's current working directory.
- `core/src/worktree.rs` provides disposable `git worktree` isolation for autonomous runs.
- `core/src/budget.rs` and `core/src/wip.rs` are the spend brake and review-backlog brake. Both fail closed.

## Known gaps

1. `core/src/auth.rs` has scoped run and email-service tokens, but its `Control` scope reaches every route. `core/src/assistant.rs` deliberately gives that control token to an MCP-only assistant turn, so the token's safety depends on the assistant tool restriction as well as bearer authentication.
2. Sender-chosen email content still reaches `core/src/triage.rs` `build_prompt` and can influence the model's categorisation. The current code neutralises fence syntax: `header_field` removes control characters, `fenced_body` indents marker-like body lines, and attachment names use `email::safe_filename`; this is not a general defence against semantic prompt injection. The two spec §5.5 tool barriers prevent that content from causing a tool call.
3. Runs outside a worktree have no filesystem sandbox. The classifier can require approval when it lacks a known workspace, but that is an application-level decision, not operating-system isolation.
