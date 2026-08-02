# NucleOS threat model

## Trust boundary

NucleOS is a single-user, localhost-bound desktop application. The owner of the desktop is the administrator. The daemon binds to localhost and is guarded by bearer authentication. Its long-lived control token is persisted in the OS Credential Manager, then loaded into the daemon; run and email-sidecar tokens are minted separately and scoped by `core/src/auth.rs`.

Email ingestion now reads the inbox and a configured sent folder. For sent mail, `core/src/email.rs` `ingest_batch` persists recipients in `emails.to_addrs`, the subject, the message's `From:` value in `emails.from_addr`, `from_name`, the message id, the uid, the mailbox name, `received_at`, and `ingested_at`; `has_attachments` is stored as false for an outbound row.
The outbound body and attachments do not cross the whole ingestion path: `extract.SentMessage` in `sidecars/email/extract/extract.go` does not send them, and `ingest_batch` in `core/src/email.rs` does not store them if a client sends them anyway. Unlike an inbound body, an outbound body is not governed by `retain_bodies_days`, and no setting turns it on.

## What this does NOT try to prevent

This product does not try to prevent the owner from running arbitrary code through the agent. That is the product, not a vulnerability.

## What this DOES try to prevent

NucleOS does try to prevent these failures:

1. An email body causing a tool call.
2. A run acting outside its worktree.
3. The long-lived daemon control token reaching an autonomous run.
4. An approval that the classifier should have blocked.
5. Mail the user wrote being sent to a model or shown back to them as though it needed attention.

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

## Mail the user wrote

The sent folder is read to learn who the user writes to, so a stranger's first message is not mistaken for an urgent one.
`core/src/priority.rs` is the only runtime consumer of the accumulated `outbound_ever` fact.

Sent mail is never triaged. `pending_rows` in `core/src/triage.rs` selects only `direction = 'inbound'`, so no subject the user wrote
reaches `build_prompt`.

Sent mail is never listed in the email queue. `get_email_queue` in `core/src/http.rs` filters on `direction = 'inbound'`.

Recipients are forwarded from the sent path alone. `extract.Message` in `sidecars/email/extract/extract.go` does not forward `To:`; only
`extract.SentMessage` does. An inbox message's `To:` line names third parties the user did not choose to disclose. `poll.ExtractorFor` in
`sidecars/email/poll/poll.go` returns the inbox reader for every direction it does not recognise, so the narrower reader is the default.

A mislabelled batch is refused, not guessed at. `post_email_incoming` in `core/src/http.rs` returns `400` for a `direction` it does not
recognise; absent, `null`, and empty all mean inbound. Defaulting an unknown value would have stored sent bodies as inbound.

The IMAP credential is unchanged and remains read-only. `poll.Once` in `sidecars/email/poll/poll.go` selects each folder with the same
credential, while `sidecars/email/imap/imap.go` opens it read-only and fetches with `Peek: true`, so it does not set `\Seen`. The folder is
optional: an empty `sent_mailbox` makes `poll.Targets` poll the inbox alone.

An outbound row expires after `ROW_RETENTION_DAYS` in `core/src/triage.rs`, on the same thirty-day clock as mail in the content triage
classes. The accumulated fact in `contact_addresses` survives, which is why ingestion accumulates it instead of counting retained mail.

## The files folder

`core/src/files.rs` owns one directory under the daemon's own data directory (`…\data\files`, renamed once from `…\data\mail`). It is the only place on this disk a stranger's bytes are written, and it is now also where the owner uploads files of their own, browses them, renames them and deletes them, from the Files tab.

Every route resolves its path through `files::resolve_within` and nothing else: a whitelist of ordinary named components, each held to `email::safe_filename`, canonicalised against the root so a symlink placed inside it pointing out is caught by the filesystem rather than by string inspection. Adding read, upload, move and delete did not widen that rule — the four new handlers are the same one-line wrapper the two original ones were, which is the property worth keeping when this surface grows again.

What is deliberately not symmetric:

- `GET /files/download` always answers `application/octet-stream` with `Content-Disposition: attachment`, never `inline` and never the type an extension suggests. Half of what is in this folder arrived as mail; a webview asked to render one of those files in place would be executing it.
- `POST /files/upload` writes through the same `write_file` as filing an attachment: the name is made safe and a collision is numbered, never overwritten. `files::MAX_UPLOAD_BYTES` caps a request at 100 MB, which is a memory ceiling as much as a policy one because the body is buffered whole; a download is streamed and has no matching cap.
- `POST /files/move` refuses a destination that exists instead of numbering it, refuses a folder moved inside itself, and refuses to create a parent that is not there.
- `DELETE /files` removes a file or an empty folder outright and answers `409` for a folder that still has something in it, until the caller repeats itself with `recursive=true`. The copy in this folder is the only one once the mail an attachment came from has expired.
- `GET /files/search` walks the tree rather than consulting an index, under two ceilings (`SEARCH_HITS`, `SEARCH_VISITS`) and reporting `truncated` when one of them cuts in. It never descends a symlinked directory: `resolve_within` already refuses a link pointing out of the root, and a link pointing back inside it is a cycle a walk would not survive.
- In `core/src/auth.rs`, reading (`GET /files`, `GET /files/download`, `GET /files/search`) is allowlisted for a read-only key because it discloses what `GET /email/{id}/attachments/{position}` already does. The four routes that change the folder are in no scope table, so only Admin and the control token reach them.
- The MCP tool `list_files` is the agent's whole reach into this folder: one verb, `ToolEffect::ReadsUntrusted`, with no client method in `core/src/daemon_client.rs` for downloading, writing, moving or deleting. An agent can see the folder and cannot touch it.

### Files dragged in from Windows

The webview never receives an HTML drop: Tauri intercepts the OS drop so it can hand over real paths, and a path is not something JavaScript can open. `shell/src-tauri/src/drop.rs` therefore resolves a drop into a manifest, and the page asks it for one file's bytes at a time before uploading them through `POST /files/upload` like any other upload.

That gives the frontend a command that reads an absolute path, which is the part worth stating plainly: **a path is readable only after the OS told the Rust side it was dropped on our window.** The allowed set is written by the drag-drop event handler in `shell/src-tauri/src/lib.rs` and consulted by `read_dropped`; nothing the page sends can add to it, and each drop REPLACES the set rather than extending it, so the window in which this process would open a file is as short as the gesture that opened it. A path outside the set is refused with the same message as a file that is not there, because distinguishing them would answer whether a path exists.

The shell already holds the daemon's control token, so this does not widen who can act as the owner on this machine; it narrows what the webview can name. `drop.rs` also caps a drop at 500 files and refuses to read one larger than the daemon would accept, so a dropped folder cannot spend the daemon's memory before being told no.

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
4. `GET /email/{id}` in `core/src/http.rs` still returns an outbound row to a caller that knows its id. The queue no longer links to one, and the row carries no body, so this is recorded rather than fixed: it is the owner's own mail, on the owner's machine, behind bearer authentication.
5. The correspondence graph now has direction, and it outlives the mail. A read of the database reveals who the owner writes to, not only who writes to them, and `contact_addresses` is not pruned. That is deliberate: surviving the thirty-day window is why the facts are accumulated at ingestion, but it makes the table long-lived personal data.
6. `sent_mailbox` is owner configuration and nothing validates what it points at. Aimed at a shared or archive folder, it would latch `outbound_ever` for people the owner never wrote to and silently weaken the one derived rule in `core/src/priority.rs`. This is misconfiguration rather than attack, and it fails quietly.
