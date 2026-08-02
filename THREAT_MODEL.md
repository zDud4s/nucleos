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
6. A web page causing a tool call, and in particular causing one that governs autonomy.
7. The web sidecar being used to reach this machine's own services or the network it sits on.

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
- In `core/src/auth.rs`, reading (`GET /files`, `GET /files/download`) is allowlisted for a read-only key because it discloses what `GET /email/{id}/attachments/{position}` already does. The four routes that change the folder are in no scope table, so only Admin and the control token reach them.
- The MCP tool `list_files` is the agent's whole reach into this folder: one verb, `ToolEffect::ReadsUntrusted`, with no client method in `core/src/daemon_client.rs` for downloading, writing, moving or deleting. An agent can see the folder and cannot touch it.

## Web content

The web is not a trigger. It is a content origin *inside* triggers that already exist, and it is the first one that
enters a turn holding tools — which is what makes it different from mail rather than a second copy of it. Email triage
answers untrusted content by removing every tool (`ToolPolicy::None`); web reading cannot, because the point is to read
and then act.

**Trust is a property of the origin, and it is a conjunction.** `core/src/trust.rs` returns `Raw` only when the owner is
present in the foreground (`attention::owner_is_present`) AND both the requested host and the final host are on
`trusted_hosts` in `.ai/web.yaml`. Everything else is `Quarantined`: a local model reads the page and the agent receives
a summary. The default is quarantine, reached by omission — there is no denylist, so a host nobody has listed is never
trusted by accident. An absent, unreadable or malformed `.ai/web.yaml` yields an EMPTY allowlist rather than a
convenient one, so a broken file costs fidelity and never safety.

**Trust never travels up.** The decision is made over the requested URL and the final URL together. An allowlisted host
that redirects out of the allowlist loses trust, which stops an open redirect on a trusted domain from laundering any
destination into `Raw`. An unknown host that redirects INTO the allowlist gains nothing, because the owner chose the
first URL and not the second.

**A cached page grants nothing.** `web_pages.trust_at_fetch` records what happened once, for the audit trail and the
shell's badge. `web::deliver` is the single door from a stored row to text a model sees, it takes the decision made for
the request in hand, and it never reads that column. Without this, a page the owner read from an allowlisted host would
arrive raw to a cron run months later.

**Reading marks the turn.** `web_read` and `web_search` are `ToolEffect::ReadsUntrusted` in `core/src/mcp_tools.rs`, so
`hooks.rs` refuses every `Acts` tool for the rest of that turn — `set_kill`, `approve_proposal`, `create_run`,
`cancel_run`. This is the barrier that matters, because the assistant's tool set was designed for a context whose only
input was the paired owner. `web_search` is classified `ReadsUntrusted` and not `ReadsOwn` deliberately: a result's title
and snippet are written by whoever owns the page, and ranking for a query somebody expects an agent to run is a thing
people already do on purpose.

**The tools are read-only, and the absence is the property.** There is no NucleOS tool that submits a form, logs in,
posts, or sends — the same asymmetry `get_email` has, for a sharper reason. `no_web_tool_can_write_anywhere` fixes it.

**The daemon opens no connection off this machine.** Every fetch goes through the Go sidecar, whose `safe.Control` hook
runs on `net.Dialer` after DNS resolution, for every connection, including each redirect hop — so loopback, private
ranges, link-local (169.254.169.254) and multicast are refused with no window between deciding and dialling, and a name
that resolves differently the second time it is asked does not get through. `safe.CheckURL` deliberately does NOT judge
the host, and a test fails if someone adds that.

`.ai/web.yaml` is in `classifier.rs`'s `SELF_GOVERNING_FILES`. Appending to `trusted_hosts` is not a file write, it is
granting trust, and an autonomous run that could add a host it controls would be writing its own permission slip.

### What this does not solve

A quarantined summary is still text derived from a stranger. The grammar guarantees the SHAPE and never the content —
measured in the email pillar, recorded in `.ai/memory.md` — so a local model can be talked into writing an instruction
into its own `summary` field. Quarantine reduces the surface from a whole page to a few hundred structured tokens; it
does not reach zero. The barrier that does the work is the turn marking above, not the summary.

**When `render: true` stops answering 501, this section has to be rewritten first, not after.** A browser driving real
sessions is a different threat model, and the University of Washington's July 2026 study found four of seven agentic
browsers letting attackers bypass the same-origin policy. The seam is in `sidecars/web/serve/serve.go` and
`web_client::fetch`.

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
7. A `Unrestricted` run has two ways to reach the web with different policies, and only one is audited. `WebFetch`/`WebSearch` remain in `BUILTIN_TOOLS` and are therefore available to cron, repo and manual runs, bypassing `trust.rs`, the cache, the index, the feed and the turn marking. Removing them would unify the path at the cost of a real capability; the measurement that decides it is how many `Unrestricted` runs actually use the CLI's own web tools.
8. `trusted_hosts` is judged by host and nothing else, so an allowlisted host that serves user-published content grants `Raw` to whoever published it. The shipped list is two curated documentation sites for this reason, and the rule for adding one is written in `.ai/web.yaml`: the allowlist does not say "this site will not attack me", it says "summarising this costs fidelity AND I asked for it". A forum, a wiki or a code-hosting domain is the worst candidate precisely when it is otherwise trustworthy.
9. The search query leaves the machine. Brave is the shipped provider partly because it does not log API queries, but the query is still data, and a pillar searching on its own would send a correspondent's name to a third party. `pillar_search_enabled` exists in `.ai/web.yaml` for that reason and is off; nothing consumes it yet, so no pillar can search today.
10. Nothing in the web pillar has been exercised against a real server. There is no provider key on this machine and no test leaves it, deliberately. The first `enabled: true` is the first contact.
