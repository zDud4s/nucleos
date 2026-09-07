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
8. A council seat acting, or reading a peer's answer before it is shown one to rank.

## Tool allowlist by trigger class

Spec §9 requires a tool allowlist derived from trigger confidence: externally sourced content marked untrusted must never trigger high-risk actions on its own.

| Trigger | Content origin | Confidence | Tools permitted | Requires approval |
| --- | --- | --- | --- | --- |
| cron (scheduled) | Owner configuration | Trusted | `ToolPolicy::Unrestricted` | When `classifier.rs` returns `pending_approval` |
| repo (branch SHA moved) | Owner repository configuration and branch state | Trusted | `ToolPolicy::Unrestricted` | When `classifier.rs` returns `pending_approval` |
| email (triage) | Email content chosen by a stranger | Untrusted | `ToolPolicy::None` — no tools at all | No — no tool can be invoked |
| telegram / assistant | Paired owner over a network | Semi-trusted | `ToolPolicy::McpOnly` — NucleOS MCP tools only; built-ins denied | No — `hooks.rs` allows only sanctioned NucleOS MCP tools and bypasses the classifier |
| manual (`POST /runs`) | Owner action | Trusted | `ToolPolicy::Unrestricted` for `real`, `shadow`, and `worktree`; `ToolPolicy::None` when the submitted mode is `email_triage` | For unrestricted modes, when `classifier.rs` returns `pending_approval`; no for `email_triage`, which has no tools |
| council (`POST /council`) | The owner's question in phase 1; other models' answers in phases 2 and 3 | Trusted at the door, model-generated thereafter | Phase 1: `ToolPolicy::McpOnly` narrowed to `mcp_tools::COUNCIL_TOOLS`. Phases 2 and 3: `ToolPolicy::None` | No — nothing on that list acts, and the classifier never sees a seat |

The scheduler (`core/src/scheduler.rs`) creates cron runs through `runs::create_run_inner`. The repository trigger (`core/src/repo_trigger.rs`) fires runs the same way. Email triage (`core/src/triage.rs` and `core/src/email.rs`) uses `ToolPolicy::None`. Assistant turns from `core/src/assistant.rs` use `ToolPolicy::McpOnly`. `POST /runs` in `core/src/runs.rs` passes its submitted mode to `create_run_inner`, which selects `ToolPolicy::None` only for `email_triage` and `ToolPolicy::Unrestricted` otherwise. Council seats are launched by `core/src/council.rs` and do not go through `create_run_inner` at all: `run_cloud_seat` and `run_local_seat` build the request themselves, so the policy above is set at the seat rather than inherited from a mode.

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

## The browser

The previous version of this file said, of the `render: true` seam, *"when this stops answering 501, this section has to
be rewritten first, not after."* This is that rewrite, and it lands before the fence is built rather than after — the
pillar is still `enabled: false`.

A browser driving real sessions is a different threat model from reading a page. The University of Washington's July 2026
study found four of seven agentic browsers letting attackers bypass the same-origin policy, and the honest reading of
that is not that those teams were careless. **An agentic browser cannot stop a page from convincing the agent.** The page
is the input; persuasion is what text does. So this pillar does not try to bound the deception. It bounds the
CONSEQUENCE: being fooled must not be able to leave the browser.

That claim is worth exactly as much as the fence behind it, which is why every hole below is named rather than implied.

### What the fence is, after measurement

The design originally named one mechanism and got it wrong. A spike against Chrome 151 (2026-08-15) measured each one,
and the fence is now three things, none of which is sufficient alone:

1. **`Fetch` interception on the BROWSER session** — not a page session. With the interception on a page, a service
   worker's script fetch never appears at all: the origin serves it and the worker installs into the profile. On the
   browser session the same request is intercepted and the registration does not happen. This was never a limit of
   Chrome; it was where the fence hung.
2. **A loopback proxy the browser is launched behind.** `Network.setBlockedURLs` does NOT stop a WebSocket handshake —
   measured, with a control — and `Fetch` never sees a `ws://` url at all. The proxy sees a plaintext `ws://` handshake
   as an ordinary `GET` carrying `Upgrade` and refuses it. `setBlockedURLs` is no longer relied on for anything.
   **It is a thin layer and is described as one:** `wss://` reaches it as `CONNECT host:443`, byte-identical to the
   CONNECT for any https sub-resource, and everything inside that tunnel is TLS to a host the page chose. An earlier
   version of the design also had it refusing any CONNECT to a port other than 443; that rule was removed rather than
   kept for comfort, because a page wanting to reach a host of its choosing does it on 443, so the rule stopped nothing
   while breaking a configuration the design supports on purpose — an origin with its own port.
3. **CSP injected by rewriting response headers**, which is what covers the channels the other two cannot see —
   `connect-src 'none'` is what closes `wss://`, since no CSP source expression can admit `https:` while refusing `wss:`.
4. **Loopback is refused unless the profile names it.** Agent mode is launched with `--proxy-bypass-list=<-loopback>` so
   that the fence sees loopback traffic at all; the consequence is that a page can address the núcleo's own HTTP API, the
   other sidecars, and — this is the one that matters — the browser's own debugging port, which needs no token and grants
   control of every profile on the machine. The fence refuses every loopback destination, in both layers and for
   sub-resources as well as documents, unless an explicit per-profile list names it. That list is separate from the site
   allowlist on purpose: the two fail in opposite directions, and one list would mean an entry added to reach a site
   silently opening one of ours.

Plus `--block-new-web-contents`, which makes `window.open` return null, and `Target.setAutoAttach` with
`waitForDebuggerOnStart`, which is what closes the window in which a new target could navigate before the interception
was on it. A popup is identified by `openerId` and never by arrival order: Chrome raises several page attaches for one
`window.open`, and binding the fence to the wrong one fails silently.

**If the interception cannot be attached, the browser does not navigate.** In the code there is no unfenced state to
forget: `chrome.Connect` is the only constructor, it arms the fence first, and it returns an error instead of a driver.

### Holes, named

- **WebRTC egress is closed, and it took seven attempts.** A page can point `RTCPeerConnection` at a STUN server of its
  choosing and put bytes in the username; that is a UDP packet to an address the page picked, and it touches neither
  HTTP nor the proxy. **Nothing on the command line stops it.** CSP `webrtc 'block'` is ignored by this Chrome;
  `--disable-webrtc`, `--disable-features=WebRtc` and `--disable-blink-features=RTCPeerConnection` do not remove the
  global; deleting the global on every new document survives in a cross-site iframe even with recursive auto-attach and
  the target paused before it runs; and `--force-webrtc-ip-handling-policy=disable_non_proxied_udp` with a working proxy
  still lets the packet out. Two of those six were re-measured on 2026-08-16 against the pinned build and still leaked.

  The seventh is not a flag. `WebRtcIPHandlingPolicy` is an enterprise policy that maps to an ordinary **profile
  preference**. The policy itself needs an elevated shell — `HKCU\SOFTWARE\Policies` grants write to SYSTEM and
  Administrators only, by design, so even the per-user path is not the user's to set, and containment that depended on
  the owner having run something as admin would be off on most machines while reporting that it is on. The preference is
  a file in a directory we own. `launch.applyWebRTCPolicy` merges `webrtc.ip_handling_policy` into
  `<profile>/Default/Preferences` before every launch — restrictive for the agent, `default` for the person's window,
  which has no fence by §6.4. It merges rather than overwrites, because that file holds everything Chromium knows about
  a profile bar its cookies.

  MEASURED 2026-08-19 against the pinned Chromium by `gate.TestWebRTCUDPDoesNotLeaveTheFence`, which points a page's
  `RTCPeerConnection` at a UDP socket it owns: without the preference the binding request arrives, three runs out of
  three; with it, nothing arrives, three runs out of three, with the page proven to have run and reached
  `setLocalDescription` in both directions. While it was open the bound was same-origin — a page exfiltrated what was
  already its own — which is what kept the browser tools' non-`Acts` classification standing in the meantime.

  **What to watch.** The preference is now the only thing holding this and there is no command-line fallback, so a
  Chromium revision that stopped honouring it would reopen the hole in silence. That is what the gate test is for, and
  it fails rather than skips.
- **An arbitrary `GET` under the person's authenticated identity.** `browser_open(url)` reaches any path of a permitted
  host, and `/logout`, `/unsubscribe?token=…`, `/approve?id=…` are all `GET`s that change things. Nothing mitigates this
  in v1; the designed path is a proposal the person approves.
- **`blob:` and `javascript:` navigate without the fence seeing them.** `Page.setControlNavigations` was removed with no
  successor, and top-level `data:` is refused by Chrome itself. What contains the other two is that a `blob:` document
  **inherits the parent's CSP** — measured, with a control that escapes when the parent carries none. So the residual is
  that the agent can be reading a document the URL bar misdescribes. It is not a path for data to leave.
- **Sub-resources are not filtered**, except on loopback. A permitted site that loads a script from a compromised CDN
  exposes the profile. This is equally true in the person's own browser; it is stated rather than solved. Loopback is the
  one exception, and it is an exception because nothing on `127.0.0.1` belongs to a web page.
- **Private address ranges are not refused.** `10/8`, `172.16/12` and `192.168/16` are somebody's intranet as often as
  they are an attack, and a browser that could not reach an internal Jira is a browser nobody uses. Loopback has no such
  reading, which is why it is treated differently rather than lumped in.
- **The site allowlist is not exercised against a real browser.** The rule itself is table-tested and the WIRING is
  proven against Chrome, but proving the https allowlist end to end needs two different hosts over https, and testing
  against the live internet is forbidden — a suite that depends on somebody else's site fails for reasons that are not
  ours. Chrome's `--host-resolver-rules` would map two names onto loopback, but with a proxy configured Chrome does not
  resolve at all. So what a real Chrome demonstrates is that a refused document really does stop; WHICH rule refused it
  is demonstrated elsewhere.
- **Chromium talks to Google on its own.** A `crashpad-handler` runs with `--url=https://clients2.google.com/cr/report`
  and survives `--disable-crash-reporter`, `--disable-breakpad`, `--no-report-upload` and
  `--disable-background-networking`. **No upload was demonstrated** — the process carrying a url is not a report being
  sent, and sending depends on a consent that is off in a fresh profile. It is an open verification, not a known leak.

### The partition, and what the agent may never choose

Profiles are per project, and a session's profile is chosen by the núcleo — `browser_policy::decide` in Rust, pure and
table-tested. The wire type the agent reaches has **no profile field**, guarded by a test, because a caller that could
name its own profile could name the identity it browses under and every list above would be decoration.

Matching is on the whole origin: scheme, host and port, exactly. This is deliberately stricter than `trust.rs`, which
covers subdomains — inside a profile holding live session cookies a subdomain is a different principal, and one XSS
anywhere in the zone would otherwise reach the session. A host that is not on the list is not refused; it is handed to a
throwaway profile, where a stranger's page runs with no login to steal. A login the person completes grants the whole
chain it traversed, once, at return, because real SSO is not one host and granting only the destination would leave every
later login looking like a broken allowlist.

**Reach: the v1 serves the assistant.** An autonomous pillar is refused structurally (`reach-undesigned`, not
recoverable); an assistant turn with nobody in the foreground is refused situationally (`no-one-present`, recoverable by
opening the shell). Those are two rules and not one because the requester is derived from owner presence, which cannot
tell a cron job from a Telegram message at midnight — and telling that person "autonomous reach is not designed" would
send them to fix something that is not broken.

### What this does not solve

Prompt injection. The agent reads text a stranger wrote and can be talked into anything that text can express. Everything
above is about what happens next, and there is no line in it that makes the agent harder to persuade.

The tool classification depends on the fence being real. The browser tools are registered as non-`Acts` — they do not
mark a run's taint barrier — **because** the fence means they cannot act off the machine. That is an assertion about the
fence, not about the tools, and it is why the tools are registered last, after the fence's tests are green. If a hole
above is ever found to be wider than stated, the classification is what has to be revisited, not just the hole.

## The council

A council is the first trigger that fans ONE owner sentence into up to nine model invocations, and
the first whose later phases feed one model's output into another model's prompt. Both facts are why
its tool posture is narrower than the assistant's rather than a copy of it.

**Three barriers, because the middle one is cooperative.** Barrier 1 is `ToolPolicy::McpOnly` in
`core/src/runner.rs`: the CLI denies every built-in and drops every ambient MCP server on its own.
Barrier 2 is the `PreToolUse` hook, which `core/src/hooks.rs` answers for `mode = 'council'` against
a named allow-list. Barrier 3 is `auth::Service::Council` and its `COUNCIL_ROUTES` table.

The third is not belt-and-braces. Barrier 2 fires only when the `.claude/settings.json` resolved from
the run's working directory registers the hook — and a seat runs with `cwd: None`, because a council
has no worktree and no project. Without a key that cannot reach a writing route, "a seat only reads"
would have been an intention rather than a property. `the_councils_key_reads_and_cannot_start_anything`
is what fixes it.

**The tool list is named, not derived.** `mcp_tools::COUNCIL_TOOLS` holds eight verbs and
`every_council_tool_only_reads` holds every one of them to `ReadsOwn` or `ReadsUntrusted` in
`TOOL_EFFECTS`, so reclassifying a tool as `Acts` without removing it here fails the gate. Three
absences are not explained by effect and so could only come from a list:

- `web_search` and `web_read` are `ReadsUntrusted`, not `Acts`. A council multiplies the egress of
  asking a question by eight, and a `kind: local` seat holding either would put the question on the
  network anyway — which would stop an all-local roster from being a statement about where the
  question goes.
- `get_run` reads any run by its id, and run ids are sequential integers. A seat that guessed a
  sibling's id would read that sibling's answer, and phase 1's independence is the only thing that
  makes phase 2 measure anything. `hooks.rs` refuses it by the named run's mode, failing closed.
- `vcs_ticket` is the read-back half of `vcs_request`; a seat that cannot queue an operation has
  nothing of its own to read back.

**Phases 2 and 3 hold no tools at all**, cloud or local. What a ranking seat reads is other models'
prose, and what the chairman reads is all of it — model-generated text is the input, so the phases
that consume it are the phases that can call nothing. This is the same answer email triage gives to a
stranger's words, reached from the other direction.

**A seat never holds the daemon's control token.** `run_cloud_seat` builds its environment from
`runs::run_env(&self.token, …)`, where `self.token` is the council's own scoped key, minted in
`main.rs` only when a roster exists.

**Anonymity is a measurement device, not a secret.** The phase-2 shuffle in `council::anonymize`
stops a seat from ranking itself and from ranking the model rather than the argument. It is not a
confidentiality boundary: phase 3 deliberately un-anonymises, because the chairman needs to know that
two agreeing answers came from two models rather than from one model asked twice.

**Spend is decided once, at the door.** `council::start` checks `budget.rs` before the first seat and
never again, and `'council'` is in the autonomy mode list. A council refused halfway has paid for
every answer and produced no synthesis, so it is refused whole or run whole. The accepted cost is
that one council started under a nearly-spent window can overshoot it.

**Starting one is an owner action.** `POST /council`, `GET /council/{id}` and `POST /council/{id}/cancel`
appear in no scope table in `core/src/auth.rs`, so only Admin and the control token reach them — the
closed default that module documents, and the right one for a route that spends in up to nine model
invocations. The pillar is off until `~/.nucleos/council.yaml` names a roster: absent, unreadable or invalid
yields `None` and `POST /council` answers `503`, because a roster nobody chose is a list of models
nobody agreed to pay for. The reads stay open — a council already run is still readable after its
roster is removed — and `config::load_council_config` warns and falls back rather than erroring, so a
typo in a list of model names cannot stop the daemon and take mail, autopilot and the API with it.

**Two of the daemon's own paths may now convene one, and both are opt-in and off by default.**
`consumers: { job_review, proposal_advice }` in the same roster file is what turns them on, so the
sentence above still holds in its stronger form: nothing convenes a council until the owner writes
a file, and nothing convenes one *without a person asking each time* until the owner writes two
more words in it. Neither consumer decides anything. A job's `review` node still runs and the
project's deterministic gate still holds ship/no-ship — the council's synthesis only lands in the
job's artifacts directory as `council.md` for the node to read. A proposal receives a
`proposal_events` **note** whose `from_status` and `to_status` are both the status it already had;
`proposals::transition` is never called, because the arbiter of an ambiguity is the human. The spend
is bounded by the same `budget_permits_new_run` read every council makes at `start`, and both
consumers fail open in the direction of *less* autonomy: a council that will not start, errors, is
cancelled, is pruned or leaves no synthesis is walked past, never waited on.

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
6. `sent_mailbox` is owner configuration and nothing validates what it points at — nor can anything, since every string is a legal IMAP folder name. What is closed is the consequence rather than the cause: `core/src/email.rs` `ingest_batch` records an outbound correspondent only for a message whose `From` is the configured account, compared through `contacts::normalize_address` so a display name or different casing does not withhold the owner's own mail. A folder holding other people's sent mail therefore no longer latches `outbound_ever` for people the owner never wrote to, and the one derived rule in `core/src/priority.rs` keeps working. The owner's address is resolved by the daemon from its own config and never read from the request body, for the same reason the direction is not. It also stopped being quiet: a batch carrying foreign mail writes one `email_sent_mailbox_foreign` feed row, once per batch. **Residual:** the guard trusts a `From` header, which its sender chooses, so mail forging the owner's address into that folder still latches a row. That is a far smaller surface than the folder itself, and it costs an attacker a delivered message rather than a mistyped setting.
7. A `Unrestricted` run has two ways to reach the web with different policies, and only one is audited. Cron, repo, manual and worktree runs can call the CLI's own `WebFetch`/`WebSearch`, bypassing `trust.rs`, the cache, the index, the feed and the turn marking.

   **This entry previously named the wrong mechanism, and acting on it as written would have widened the surface rather than narrowing it.** `BUILTIN_TOOLS` (`core/src/runner.rs`) is the DENY list passed as `--disallowedTools` under `ToolPolicy::McpOnly`, which is what every assistant turn runs under. An `Unrestricted` run reaches the web because that arm pushes *no restriction flag at all* — their presence in that list has nothing to do with it. So deleting them from `BUILTIN_TOOLS` would have granted web access to the one surface that reads summaries of mail written by strangers, and changed nothing about the runs the gap is about. Closing the real gap means adding `--disallowedTools WebFetch,WebSearch` to the `Unrestricted` arm — a different edit, in a different place. `removing_a_web_tool_from_the_denylist_widens_the_assistant_rather_than_narrowing_a_run` now fails in front of anyone who reaches for the old sentence.

   **The measurement this entry asked for has been taken** (2026-08-04, against the live datastore: 67 runs, 65 of them `Unrestricted`). `WebFetch`/`WebSearch` were called by **zero** runs. They appear in 15 runs' events only inside the `system`/`init` payload that advertises all 46 tools — capability advertised, never exercised. The same query shape finds 9 runs that called `Bash`, so it would have found a web call had there been one.

   **And it is still not closed, for a reason the measurement itself surfaces.** "Unify the path" assumes there is a path to unify onto, and there is not yet: `web_pages` is empty, `mcp__nucleos__web_read` has zero callers, and gap 10 below records that nothing in the pillar has met a real server. Today the edit would be a removal rather than a redirect — it would take the only working web access an agent has and give back nothing. **Trigger:** make it part of the same change that first sets `enabled: true` in `.ai/web.yaml`, which is the moment the redirect becomes real. The cost of waiting is bounded by the measurement above: a capability nothing has used cannot have been abused.
8. `trusted_hosts` is judged by host and nothing else, so an allowlisted host that serves user-published content grants `Raw` to whoever published it. The shipped list is two curated documentation sites for this reason, and the rule for adding one is written in `.ai/web.yaml`: the allowlist does not say "this site will not attack me", it says "summarising this costs fidelity AND I asked for it". A forum, a wiki or a code-hosting domain is the worst candidate precisely when it is otherwise trustworthy.
9. The search query leaves the machine. Brave is the shipped provider partly because it does not log API queries, but the query is still data, and a pillar searching on its own would send a correspondent's name to a third party. `pillar_search_enabled` exists in `.ai/web.yaml` for that reason and is off; nothing consumes it yet, so no pillar can search today.
10. Nothing in the web pillar has been exercised against a real server. There is no provider key on this machine and no test leaves it, deliberately. The first `enabled: true` is the first contact.
11. A stranger's words can reach a council's synthesis. A phase-1 seat holds `get_email`, `get_email_queue` and `list_files`, so it can read mail somebody else wrote; its answer then enters the ranking seats' prompts in phase 2 and the chairman's in phase 3. The turn-marking rule (`ReadsUntrusted` then no `Acts`) is redundant inside a council rather than protective — there is no `Acts` on `COUNCIL_TOOLS` for it to refuse — so what bounds this is the absence of any acting tool in the whole pillar, not the taint. **The residual is influence on text the owner reads, never a tool call**, which is a smaller claim than the one email triage makes and is stated here rather than in a comment. Narrowing it further means removing the mail tools from the list, which would also remove the reason somebody would ask a council about their own correspondence.

   What is NOT part of this residual is a secret carried out of the mailbox: `redact_rendered` runs on every tool result on both paths a seat can take — inside `filter_outgoing` for a cloud seat's MCP call, and inside `LocalToolBox::call` for a local one — so a key that happened to be in a message does not reach the answer, let alone the chairman's prompt. That filter recognises shapes it knows and is not a reader of meaning, which is exactly why the residual above is stated in terms of prose. Prose is what it lets through, and prose is what this entry is about.
12. Nothing in the council has been exercised against a real model. Every integration test drives a scripted `CommandRunner`, and a local seat is proved only as far as landing its `runs` row — no seat, cloud or local, has produced an answer. There is no `~/.nucleos/council.yaml` on this machine, so the pillar is dark; the first roster written is the first contact, and the phase-2 and phase-3 prompts are the part with no evidence behind them yet.
13. **WebRTC left the browser pillar's fence open, and no longer does** — see "The browser" above for the six mechanisms measured and rejected and for the seventh that worked. **CLOSED 2026-08-19** by `launch.applyWebRTCPolicy`, which writes `webrtc.ip_handling_policy` into the profile before every agent launch; no flag does this, and the enterprise policy that would needs an elevated shell even under HKCU. Proven by `gate.TestWebRTCUDPDoesNotLeaveTheFence` against the pinned build, three runs each way, with both of the test's controls firing.

    **Why it stays listed rather than being struck out.** Nothing on the command line backs it up, so this rests entirely on one Chromium revision continuing to honour one preference. The gate test is the whole of the early warning, and it is the browser tools' non-`Acts` classification (§6.1a) that depends on the answer — so if that test ever goes red, this is the entry that says what it means.
14. **An arbitrary `GET` under the owner's authenticated identity** is reachable by `browser_open(url)` on any path of a permitted host. `GET` is not a safe verb in practice — `/logout`, `/unsubscribe?token=…`, `/approve?id=…`. Nothing mitigates it in v1. The designed path is a human-approved proposal, which is not built.
15. **The browser pillar's fence is now exercised against a real browser, and the first run found two defects the unit tests could not.** `ServiceWorker.enable` does not exist on the browser session — real Chrome answers -32601 — and `Fetch.continueResponse` rejects a status without headers, which sent every non-document response down the failure path and blocked pages the fence meant to allow. Both passed the unit tests, because a fake CDP endpoint answers everything. The gap that remains is narrower and worth stating in its own terms: what runs against Chrome is a small group behind a build tag, it needs a browser present or it skips, and the pinned Chromium is not installed on any machine yet — so the group has only ever run against a system Chrome of the same major version.
16. **A page can address this machine unless the profile's loopback list says otherwise.** The fence refuses loopback by default and the browser's own debugging port is the destination that matters, but the defence is a list somebody has to keep right: an entry added to reach a local dev server admits every path on that origin, and the entry outlives the reason it was added. Nothing expires it and nothing warns when a listed port starts answering as something else.

17. **The Chromium is pinned but the patching has no owner.** The install refuses an archive without a pinned sha256, and a revision bump is a new directory rather than an overwrite. What does not exist is the process that decides when to bump: a browser that never updates is a browser accumulating known holes, and "we own the version" is only an advantage while somebody moves it.
