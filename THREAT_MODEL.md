# NucleOS threat model

This file states what is true now: each barrier, the test that holds it, and what is still open. How a barrier came to be is in the git history, not here.

## Trust boundary

NucleOS is a single-user, localhost-bound desktop application. The owner of the desktop is the administrator. The daemon binds to localhost and is guarded by bearer authentication. Its long-lived control token is persisted in the OS Credential Manager, then loaded into the daemon; run and email-sidecar tokens are minted separately and scoped by `core/src/auth.rs`.

## What this does NOT try to prevent

This product does not try to prevent the owner from running arbitrary code through the agent. That is the product, not a vulnerability.

## What this DOES try to prevent

1. An email body causing a tool call.
2. A run acting outside its worktree.
3. The long-lived daemon control token reaching an autonomous run.
4. An approval that the classifier should have blocked.
5. Mail the user wrote being sent to a model or shown back to them as though it needed attention.
6. A web page causing a tool call, and in particular causing one that governs autonomy.
7. The web sidecar being used to reach this machine's own services or the network it sits on.
8. A council seat acting, or reading a peer's answer before it is shown one to rank.
9. A vendor credential the owner holds entering the núcleo, the database or a log.

## Existing barriers

- `core/src/classifier.rs` is a pure deterministic lexical classifier. `CLASSIFIER_VERSION = 2`; it returns `allow`, `deny`, or `pending_approval` together with an `action_class`. It performs no I/O, makes no database access, and has no knowledge of run state.
- `core/src/hooks.rs` provides the `PreToolUse` decision endpoint. It is the enforcement point.
- Spec §5.5 requires two independent triage barriers, documented at the top of `core/src/triage.rs`. Barrier 1 is `ToolPolicy::None` in `core/src/runner.rs`: the CLI refuses on its own. Barrier 2 is the cooperative `PreToolUse` hook. It runs only when the `.claude/settings.json` resolved from the run's working directory registers it. A triage run therefore receives its own sandbox directory instead of inheriting the daemon's current working directory.
- `core/src/worktree.rs` provides disposable `git worktree` isolation for autonomous runs.
- `core/src/budget.rs` and `core/src/wip.rs` are the spend brake and review-backlog brake. Both fail closed.

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

Email ingestion reads the inbox and a configured sent folder. The sent folder is read to learn who the user writes to, so a stranger's first message is not mistaken for an urgent one. `core/src/priority.rs` is the only runtime consumer of the accumulated `outbound_ever` fact.

**What is stored.** For sent mail, `core/src/email.rs` `ingest_batch` persists recipients in `emails.to_addrs`, the subject, the message's `From:` value in `emails.from_addr`, `from_name`, the message id, the uid, the mailbox name, `received_at`, and `ingested_at`; `has_attachments` is stored as false. The outbound body and attachments do not cross the ingestion path: `extract.SentMessage` in `sidecars/email/extract/extract.go` does not send them, and `ingest_batch` does not store them if a client sends them anyway. Unlike an inbound body, an outbound body is not governed by `retain_bodies_days`, and no setting turns it on.

**Sent mail is never triaged.** `pending_rows` in `core/src/triage.rs` selects only `direction = 'inbound'`, so no subject the user wrote reaches `build_prompt`.

**Sent mail is never listed in the email queue.** `get_email_queue` in `core/src/http.rs` filters on `direction = 'inbound'`.

**Recipients are forwarded from the sent path alone.** `extract.Message` does not forward `To:`; only `extract.SentMessage` does. An inbox message's `To:` line names third parties the user did not choose to disclose. `poll.ExtractorFor` in `sidecars/email/poll/poll.go` returns the inbox reader for every direction it does not recognise, so the narrower reader is the default.

**A mislabelled batch is refused, not guessed at.** `post_email_incoming` in `core/src/http.rs` returns `400` for a `direction` it does not recognise; absent, `null`, and empty all mean inbound. Defaulting an unknown value would have stored sent bodies as inbound.

**The IMAP credential remains read-only.** `poll.Once` selects each folder with the same credential, while `sidecars/email/imap/imap.go` opens it read-only and fetches with `Peek: true`, so it does not set `\Seen`. The folder is optional: an empty `sent_mailbox` makes `poll.Targets` poll the inbox alone.

**Retention.** An outbound row expires after `ROW_RETENTION_DAYS` in `core/src/triage.rs`, on the same thirty-day clock as mail in the content triage classes. The accumulated fact in `contact_addresses` survives (gap 5), which is why ingestion accumulates it instead of counting retained mail.

## The files folder

`core/src/files.rs` owns one directory under the daemon's own data directory (`…\data\files`, renamed once from `…\data\mail`). It is the only place on this disk a stranger's bytes are written, and it is also where the owner uploads, browses, renames and deletes files of their own from the Files tab.

Every route resolves its path through `files::resolve_within` and nothing else: a whitelist of ordinary named components, each held to `email::safe_filename`, canonicalised against the root so a symlink placed inside it pointing out is caught by the filesystem rather than by string inspection. Every handler is the same one-line wrapper over it — the property to keep when this surface grows again.

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

That gives the frontend a command that reads an absolute path, so: **a path is readable only after the OS told the Rust side it was dropped on our window.** The allowed set is written by the drag-drop event handler in `shell/src-tauri/src/lib.rs` and consulted by `read_dropped`; nothing the page sends can add to it, and each drop REPLACES the set rather than extending it. A path outside the set is refused with the same message as a file that is not there, because distinguishing them would answer whether a path exists.

The shell already holds the daemon's control token, so this does not widen who can act as the owner on this machine; it narrows what the webview can name. `drop.rs` also caps a drop at 500 files and refuses to read one larger than the daemon would accept, so a dropped folder cannot spend the daemon's memory before being told no.

## The quota notch

### The second webview

The shell can open a second webview, labelled `notch` (`shell/src-tauri/src/notch.rs`): borderless, always in front, and alive while the main window sits hidden in the tray. It holds **no capability**: `capabilities/default.json` stays scoped to `main`, and a test in `lib.rs` holds it there. It still reaches every command the app itself defines, `get_daemon_token` and `read_dropped` included, because Tauri 2 checks the ACL of an app's own commands only when the app ships an ACL manifest or the caller is a remote origin. This app ships none, and the notch loads the app's own bundle under the same CSP. So the notch can do nothing a second copy of the main window could not do.

**What would change this** is either condition flipping: an app ACL manifest (then the notch needs its own grant, or it goes mute), or a remote URL loaded in any window (then the app's commands, the token among them, would be one ACL decision away from that origin).

### What it sends out, and why it is not opt-in

Drawing the notch means one outbound call, made with the owner's Claude OAuth token. `sidecars/quota/claude/claude.go` reads `~/.claude/.credentials.json` — Claude Code's own file, never managed here — and sends the access token as a bearer to `https://api.anthropic.com/api/oauth/usage`, the endpoint the CLI's `/usage` reads. Nothing else goes with it: no request body, no mail, no project, no identifier this machine invented. What comes back is two utilisation figures and their reset times. The Codex ring makes no connection at all; it is read from rollout files already on this disk.

**The token never enters the núcleo** (design D2). It is held for the duration of one request inside the sidecar, is never written to the database, never logged and never returned; `core/src/quota.rs` holds no provider credential of any kind. The only secret on the daemon's side of this path is its own bearer for its own sidecar, which `QuotaClient`'s hand-written `Debug` keeps out of a log line. Errors from this package carry the vendor's URL and status, never a header, and the credential file's path is dropped from OS errors.

**It starts with the other sidecars rather than behind a pillar switch** (`core/src/main.rs`), although every other process that reaches off this machine is opt-in. It calls a vendor only where that vendor's CLI is already signed in on this machine, with the credential that CLI wrote — so the switch an opt-in would offer is one the owner has already thrown, in the other application. On a machine with no Claude Code it makes no outbound call: it answers `unmeasured` and stops. **Residual:** a machine that has Claude Code but does not want the notch has no way to say so until the phase-4 settings page exists.

## Web content

The web is not a trigger. It is a content origin *inside* triggers that already exist, and it is the first one that enters a turn holding tools — which is what makes it different from mail. Email triage answers untrusted content by removing every tool (`ToolPolicy::None`); web reading cannot, because the point is to read and then act.

**Trust is a property of the origin, and it is a conjunction.** `core/src/trust.rs` returns `Raw` only when the owner is present in the foreground (`attention::owner_is_present`) AND both the requested host and the final host are on `trusted_hosts` in `~/.nucleos/web.yaml`. Everything else is `Quarantined`: a local model reads the page and the agent receives a summary. The default is quarantine, reached by omission — there is no denylist, so a host nobody has listed is never trusted by accident. An absent, unreadable or malformed `~/.nucleos/web.yaml` yields an EMPTY allowlist, so a broken file costs fidelity and never safety.

**Trust never travels up.** The decision is made over the requested URL and the final URL together. An allowlisted host that redirects out of the allowlist loses trust, which stops an open redirect on a trusted domain from laundering any destination into `Raw`. An unknown host that redirects INTO the allowlist gains nothing, because the owner chose the first URL and not the second.

**A cached page grants nothing.** `web_pages.trust_at_fetch` records what happened once, for the audit trail and the shell's badge. `web::deliver` is the single door from a stored row to text a model sees; it takes the decision made for the request in hand and never reads that column. Otherwise a page the owner read from an allowlisted host would arrive raw to a cron run months later.

**Reading marks the turn.** `web_read` and `web_search` are `ToolEffect::ReadsUntrusted` in `core/src/mcp_tools.rs`, so `hooks.rs` refuses every `Acts` tool for the rest of that turn — `set_kill`, `approve_proposal`, `create_run`, `cancel_run`. This is the barrier that matters, because the assistant's tool set was designed for a context whose only input was the paired owner. `web_search` is `ReadsUntrusted` and not `ReadsOwn` deliberately: a result's title and snippet are written by whoever owns the page, and ranking for a query somebody expects an agent to run is a thing people already do on purpose.

**The tools are read-only, and the absence is the property.** There is no NucleOS tool that submits a form, logs in, posts, or sends — the same asymmetry `get_email` has. `no_web_tool_can_write_anywhere` fixes it.

**The daemon opens no connection off this machine.** Every fetch goes through the Go sidecar, whose `safe.Control` hook runs on `net.Dialer` after DNS resolution, for every connection, including each redirect hop — so loopback, private ranges, link-local (169.254.169.254) and multicast are refused with no window between deciding and dialling, and a name that resolves differently the second time it is asked does not get through. `safe.CheckURL` deliberately does NOT judge the host, and a test fails if someone adds that.

**The allowlist is guarded as a permission.** `~/.nucleos/web.yaml` is guarded by `classifier.rs`'s `names_machine_settings` (and its old `.ai/web.yaml` spelling by `SELF_GOVERNING_FILES` as well). Appending to `trusted_hosts` grants trust, and an autonomous run that could add a host it controls would be writing its own permission slip.

**What this does not solve.** A quarantined summary is still text derived from a stranger. The grammar guarantees the SHAPE and never the content — measured in the email pillar, recorded in `.ai/memory.md` — so a local model can be talked into writing an instruction into its own `summary` field. Quarantine reduces the surface from a whole page to a few hundred structured tokens; it does not reach zero. The barrier that does the work is the turn marking, not the summary. Open items for this pillar: gaps 7 to 10.

## The browser

The pillar is `enabled: false`; this section describes the fence it ships behind and was written before the fence was built.

A browser driving real sessions is a different threat model from reading a page. **An agentic browser cannot stop a page from convincing the agent** — the page is the input, and persuasion is what text does (a July 2026 University of Washington study found four of seven agentic browsers letting attackers bypass the same-origin policy). So this pillar does not try to bound the deception. It bounds the CONSEQUENCE: being fooled must not be able to leave the browser. That claim is worth exactly as much as the fence behind it, which is why every hole below is named rather than implied.

### The fence

Measured against Chrome 151 (spike 2026-08-15). None of these is sufficient alone:

1. **`Fetch` interception on the BROWSER session**, not a page session. With interception on a page, a service worker's script fetch never appears and the worker installs into the profile; on the browser session the same request is intercepted and the registration does not happen.
2. **A loopback proxy the browser is launched behind.** `Network.setBlockedURLs` does NOT stop a WebSocket handshake (measured, with a control), and `Fetch` never sees a `ws://` url at all. The proxy sees a plaintext `ws://` handshake as an ordinary `GET` carrying `Upgrade` and refuses it. `setBlockedURLs` is relied on for nothing. **It is a thin layer:** `wss://` reaches it as `CONNECT host:443`, byte-identical to the CONNECT for any https sub-resource, and everything inside that tunnel is TLS to a host the page chose. There is deliberately no rule refusing CONNECT to other ports: a page wanting a host of its choosing uses 443, so such a rule would stop nothing while breaking an origin with its own port, which the design supports.
3. **CSP injected by rewriting response headers** covers the channels the other two cannot see — `connect-src 'none'` is what closes `wss://`, since no CSP source expression can admit `https:` while refusing `wss:`.
4. **Loopback is refused unless the profile names it.** Agent mode is launched with `--proxy-bypass-list=<-loopback>` so that the fence sees loopback traffic at all; the consequence is that a page can address the núcleo's own HTTP API, the other sidecars, and — the one that matters — the browser's own debugging port, which needs no token and grants control of every profile on the machine. The fence refuses every loopback destination, in both layers and for sub-resources as well as documents, unless an explicit per-profile list names it. That list is separate from the site allowlist on purpose: the two fail in opposite directions, and one list would mean an entry added to reach a site silently opening one of ours.

Plus `--block-new-web-contents`, which makes `window.open` return null, and `Target.setAutoAttach` with `waitForDebuggerOnStart`, which closes the window in which a new target could navigate before the interception was on it. A popup is identified by `openerId` and never by arrival order: Chrome raises several page attaches for one `window.open`, and binding the fence to the wrong one fails silently.

**If the interception cannot be attached, the browser does not navigate.** There is no unfenced state to forget: `chrome.Connect` is the only constructor, it arms the fence first, and it returns an error instead of a driver.

### Holes, named

This list is the single description of each hole; the Known gaps entries 13–17 point here.

- **WebRTC egress — closed (gap 13).** A page can point `RTCPeerConnection` at a STUN server of its choosing and put bytes in the username: a UDP packet to an address the page picked, touching neither HTTP nor the proxy. Nothing on the command line stops it, measured against the pinned build: CSP `webrtc 'block'`, `--disable-webrtc`, `--disable-features=WebRtc`, `--disable-blink-features=RTCPeerConnection`, deleting the global on every new document (it survives in a cross-site iframe), and `--force-webrtc-ip-handling-policy=disable_non_proxied_udp` all leaked. The enterprise policy `WebRtcIPHandlingPolicy` needs an elevated shell even under HKCU, so containment depending on it would be off on most machines while reporting that it is on. What holds is the profile preference that policy maps to: `launch.applyWebRTCPolicy` merges `webrtc.ip_handling_policy` into `<profile>/Default/Preferences` before every launch — restrictive for the agent, `default` for the person's window, which has no fence by §6.4. It merges rather than overwrites, because that file holds everything Chromium knows about a profile bar its cookies. `gate.TestWebRTCUDPDoesNotLeaveTheFence` proves it against the pinned Chromium with a UDP socket the test owns: without the preference the binding request arrives, with it nothing arrives, and both controls fire. **What to watch:** the preference is the only thing holding this, so a Chromium revision that stopped honouring it would reopen the hole in silence. The gate test fails rather than skips, and the browser tools' non-`Acts` classification (§6.1a) depends on it.
- **An arbitrary `GET` under the person's authenticated identity (gap 14).** `browser_open(url)` reaches any path of a permitted host, and `/logout`, `/unsubscribe?token=…`, `/approve?id=…` are all `GET`s that change things. Nothing mitigates this in v1; the designed path is a proposal the person approves, which is not built.
- **`blob:` and `javascript:` navigate without the fence seeing them.** `Page.setControlNavigations` was removed with no successor, and top-level `data:` is refused by Chrome itself. What contains the other two is that a `blob:` document **inherits the parent's CSP** — measured, with a control that escapes when the parent carries none. The residual is that the agent can be reading a document the URL bar misdescribes; it is not a path for data to leave.
- **Sub-resources are not filtered**, except on loopback. A permitted site that loads a script from a compromised CDN exposes the profile. This is equally true in the person's own browser; it is stated rather than solved. Loopback is the exception because nothing on `127.0.0.1` belongs to a web page.
- **Private address ranges are not refused.** `10/8`, `172.16/12` and `192.168/16` are somebody's intranet as often as they are an attack, and a browser that could not reach an internal Jira is a browser nobody uses.
- **The loopback list has to be kept right (gap 16).** An entry added to reach a local dev server admits every path on that origin, and the entry outlives the reason it was added. Nothing expires it and nothing warns when a listed port starts answering as something else.
- **The site allowlist is not exercised against a real browser.** The rule is table-tested and the WIRING is proven against Chrome, but proving it end to end needs two different hosts over https, and testing against the live internet is forbidden. Chrome's `--host-resolver-rules` would map two names onto loopback, but with a proxy configured Chrome does not resolve at all. So a real Chrome demonstrates that a refused document really does stop; WHICH rule refused it is demonstrated elsewhere.
- **The real-browser test group is thin (gap 15).** It runs behind a build tag, skips without a browser present, and has only run against a system Chrome of the same major version — the pinned Chromium is not installed on any machine yet. It matters because a fake CDP endpoint answers everything: the group's first run found two defects the unit tests passed (`ServiceWorker.enable` does not exist on the browser session, and `Fetch.continueResponse` rejects a status without headers).
- **The pinned Chromium has no patch owner (gap 17).** The install refuses an archive without a pinned sha256, and a revision bump is a new directory rather than an overwrite. Nothing decides when to bump: a browser that never updates accumulates known holes, and owning the version is only an advantage while somebody moves it.
- **Chromium talks to Google on its own.** A `crashpad-handler` runs with `--url=https://clients2.google.com/cr/report` and survives `--disable-crash-reporter`, `--disable-breakpad`, `--no-report-upload` and `--disable-background-networking`. **No upload was demonstrated** — a process carrying a url is not a report being sent, and sending depends on a consent that is off in a fresh profile. It is an open verification, not a known leak.

### The partition, and what the agent may never choose

Profiles are per project, and a session's profile is chosen by the núcleo — `browser_policy::decide` in Rust, pure and table-tested. The wire type the agent reaches has **no profile field**, guarded by a test, because a caller that could name its own profile could name the identity it browses under and every list above would be decoration.

Matching is on the whole origin: scheme, host and port, exactly. This is deliberately stricter than `trust.rs`, which covers subdomains — inside a profile holding live session cookies a subdomain is a different principal, and one XSS anywhere in the zone would otherwise reach the session. A host that is not on the list is not refused; it is handed to a throwaway profile, where a stranger's page runs with no login to steal. A login the person completes grants the whole chain it traversed, once, at return, because real SSO is not one host and granting only the destination would leave every later login looking like a broken allowlist.

**Reach: the v1 serves the assistant.** An autonomous pillar is refused structurally (`reach-undesigned`, not recoverable); an assistant turn with nobody in the foreground is refused situationally (`no-one-present`, recoverable by opening the shell). Those are two rules and not one because the requester is derived from owner presence, which cannot tell a cron job from a Telegram message at midnight — and telling that person "autonomous reach is not designed" would send them to fix something that is not broken.

**The live view is a second pixel path: sidecar -> núcleo -> shell in agent mode, in wheel-requested and in human/shell; never in window.** `GET /browser/sessions/{id}/live` is reachable with the control token or the admin key and no narrower scope; frames are relayed and never persisted or logged. The núcleo cuts the stream when the session reaches a (mode, seat) that shows no pixels, i.e. window (a mode change, at a record boundary), and the sidecar cuts it again at Handoff, so neither side alone is what keeps a person's own screen out of the agent's view. Residual: a shell seat's `/live` pixels can show credentials being typed; `/live` is refused to a run requester in every mode (the same `from_a_run` check as `/take`, applied before any session lookup, so a stream a run opened in agent mode cannot outlive an approval), which a control-token holder omitting the run header does not trip.

While a person drives through the shell, input and prompt answers are accepted only with a `seat_nonce` issued by take or approval, and are refused to run requests (auth `Scope` plus the `x-nucleos-run-id` header). While the person drives the browser is unfenced, and the pool refuses other sessions on that profile. Residual: the same as `open_window`'s, plus any holder of the control token that omits the header, e.g. the Telegram sidecar.

### What this does not solve

Prompt injection. The agent reads text a stranger wrote and can be talked into anything that text can express. Everything above is about what happens next; nothing in it makes the agent harder to persuade.

**The tool classification depends on the fence being real.** The browser tools are registered as non-`Acts` — they do not mark a run's taint barrier — **because** the fence means they cannot act off the machine. That is an assertion about the fence, not about the tools, and it is why the tools are registered last, after the fence's tests are green. If a hole above is ever found to be wider than stated, the classification is what has to be revisited, not just the hole.

## The council

A council is the first trigger that fans ONE owner sentence into up to nine model invocations, and the first whose later phases feed one model's output into another model's prompt. Both facts are why its tool posture is narrower than the assistant's rather than a copy of it.

**Three barriers, because the middle one is cooperative.** Barrier 1 is `ToolPolicy::McpOnly` in `core/src/runner.rs`: the CLI denies every built-in and drops every ambient MCP server on its own. Barrier 2 is the `PreToolUse` hook, which `core/src/hooks.rs` answers for `mode = 'council'` against a named allow-list. Barrier 3 is `auth::Service::Council` and its `COUNCIL_ROUTES` table. The third is not belt-and-braces: barrier 2 fires only when the `.claude/settings.json` resolved from the run's working directory registers the hook, and a seat runs with `cwd: None`, because a council has no worktree and no project. Without a key that cannot reach a writing route, "a seat only reads" would be an intention rather than a property. `the_councils_key_reads_and_cannot_start_anything` fixes it.

**The tool list is named, not derived.** `mcp_tools::COUNCIL_TOOLS` holds eight verbs and `every_council_tool_only_reads` holds every one of them to `ReadsOwn` or `ReadsUntrusted` in `TOOL_EFFECTS`, so reclassifying a tool as `Acts` without removing it here fails the gate. Three absences are not explained by effect and so could only come from a list:

- `web_search` and `web_read` are `ReadsUntrusted`, not `Acts`. A council multiplies the egress of asking a question by eight, and a `kind: local` seat holding either would put the question on the network anyway — which would stop an all-local roster from being a statement about where the question goes.
- `get_run` reads any run by its id, and run ids are sequential integers. A seat that guessed a sibling's id would read that sibling's answer, and phase 1's independence is the only thing that makes phase 2 measure anything. `hooks.rs` refuses it by the named run's mode, failing closed.
- `vcs_ticket` is the read-back half of `vcs_request`; a seat that cannot queue an operation has nothing of its own to read back.

**Phases 2 and 3 hold no tools at all**, cloud or local. What a ranking seat reads is other models' prose, and what the chairman reads is all of it — model-generated text is the input, so the phases that consume it are the phases that can call nothing. This is the same answer email triage gives to a stranger's words, reached from the other direction.

**A seat never holds the daemon's control token.** `run_cloud_seat` builds its environment from `runs::run_env(&self.token, …)`, where `self.token` is the council's own scoped key, minted in `main.rs` only when a roster exists.

**Anonymity is a measurement device, not a secret.** The phase-2 shuffle in `council::anonymize` stops a seat from ranking itself and from ranking the model rather than the argument. It is not a confidentiality boundary: phase 3 deliberately un-anonymises, because the chairman needs to know that two agreeing answers came from two models rather than from one model asked twice.

**Spend is decided once, at the door.** `council::start` checks `budget.rs` before the first seat and never again, and `'council'` is in the autonomy mode list. A council refused halfway has paid for every answer and produced no synthesis, so it is refused whole or run whole. The accepted cost is that one council started under a nearly-spent window can overshoot it.

**Starting one is an owner action.** `POST /council`, `GET /council/{id}` and `POST /council/{id}/cancel` appear in no scope table in `core/src/auth.rs`, so only Admin and the control token reach them. The pillar is off until `~/.nucleos/council.yaml` names a roster: absent, unreadable or invalid yields `None` and `POST /council` answers `503`, because a roster nobody chose is a list of models nobody agreed to pay for. A council already run stays readable after its roster is removed, and `config::load_council_config` warns and falls back rather than erroring, so a typo in a list of model names cannot stop the daemon and take mail, autopilot and the API with it.

**Two of the daemon's own paths may convene one, and both are opt-in and off by default.** `consumers: { job_review, proposal_advice }` in the same roster file turns them on: nothing convenes a council until the owner writes a file, and nothing convenes one *without a person asking each time* until the owner writes two more words in it. Neither consumer decides anything. A job's `review` node still runs and the project's deterministic gate still holds ship/no-ship — the synthesis only lands in the job's artifacts directory as `council.md` for the node to read. A proposal receives a `proposal_events` **note** whose `from_status` and `to_status` are both the status it already had; `proposals::transition` is never called, because the arbiter of an ambiguity is the human. Spend is bounded by the same `budget_permits_new_run` read every council makes at `start`, and both consumers fail open in the direction of *less* autonomy: a council that will not start, errors, is cancelled, is pruned or leaves no synthesis is walked past, never waited on.

Open items for this pillar: gaps 11 and 12.

## Known gaps

Numbers are stable — code comments cite them (`core/src/email.rs` cites 6, `core/src/runner.rs` cites 7). A closed gap keeps its number and says so.

1. **The `Control` scope reaches every route.** `core/src/auth.rs` has scoped run and email-service tokens, but `core/src/assistant.rs` deliberately gives the control token to an MCP-only assistant turn, so that token's safety depends on the assistant tool restriction as well as bearer authentication.
2. **Sender-chosen email content still reaches `build_prompt`** in `core/src/triage.rs` and can influence the model's categorisation. `header_field` removes control characters, `fenced_body` indents marker-like body lines, and attachment names use `email::safe_filename`; that neutralises fence syntax and is not a defence against semantic prompt injection. The two spec §5.5 tool barriers prevent that content from causing a tool call.
3. **Runs outside a worktree have no filesystem sandbox.** The classifier can require approval when it lacks a known workspace, but that is an application-level decision, not operating-system isolation.
4. **`GET /email/{id}` still returns an outbound row** to a caller that knows its id. The queue no longer links to one and the row carries no body, so this is recorded rather than fixed: it is the owner's own mail, on the owner's machine, behind bearer authentication.
5. **The correspondence graph has direction, and it outlives the mail.** A read of the database reveals who the owner writes to, not only who writes to them, and `contact_addresses` is not pruned. That is deliberate — surviving the thirty-day window is why the facts are accumulated at ingestion — but it makes the table long-lived personal data.
6. **Nothing validates what `sent_mailbox` points at**, nor can anything, since every string is a legal IMAP folder name. What is closed is the consequence: `ingest_batch` in `core/src/email.rs` records an outbound correspondent only for a message whose `From` is the configured account, compared through `contacts::normalize_address` so a display name or different casing does not withhold the owner's own mail. A folder holding other people's sent mail therefore does not latch `outbound_ever` for people the owner never wrote to. The owner's address is resolved by the daemon from its own config and never read from the request body. A batch carrying foreign mail writes one `email_sent_mailbox_foreign` feed row, once per batch. **Residual:** the guard trusts a `From` header, which its sender chooses, so mail forging the owner's address into that folder still latches a row — a far smaller surface than the folder itself, and one that costs an attacker a delivered message rather than a mistyped setting.
7. **An `Unrestricted` run reaches the web unaudited.** Cron, repo, manual and worktree runs can call the CLI's own `WebFetch`/`WebSearch`, bypassing `trust.rs`, the cache, the index, the feed and the turn marking.
   - **The fix** is `--disallowedTools WebFetch,WebSearch` on the `Unrestricted` arm in `core/src/runner.rs`, which today pushes no restriction flag at all. It is **not** removing them from `BUILTIN_TOOLS`: that is the deny list for `ToolPolicy::McpOnly`, so removing them would grant web access to every assistant turn — the surface that reads summaries of mail written by strangers — and change nothing for the runs this gap is about. `removing_a_web_tool_from_the_denylist_widens_the_assistant_rather_than_narrowing_a_run` holds that.
   - **Measured** 2026-08-04 against the live datastore (67 runs, 65 `Unrestricted`): `WebFetch`/`WebSearch` were called by zero runs; they appear only in the `system`/`init` payload that advertises every tool. The same query shape finds 9 runs that called `Bash`, so it would have found a web call.
   - **Why it is still open:** there is nothing to redirect to yet — `web_pages` is empty, `mcp__nucleos__web_read` has zero callers, and the pillar has met no real server (gap 10). Today the fix would remove the only working web access an agent has and give back nothing. **Trigger:** close it in the same change that first sets `enabled: true` in `~/.nucleos/web.yaml`.
8. **`trusted_hosts` is judged by host and nothing else**, so an allowlisted host that serves user-published content grants `Raw` to whoever published it. The shipped list is two curated documentation sites for this reason, and the rule for adding one is written in `~/.nucleos/web.yaml`: the allowlist does not say "this site will not attack me", it says "summarising this costs fidelity AND I asked for it". A forum, a wiki or a code-hosting domain is the worst candidate precisely when it is otherwise trustworthy.
9. **The search query leaves the machine.** Brave is the shipped provider partly because it does not log API queries, but the query is still data, and a pillar searching on its own would send a correspondent's name to a third party. `pillar_search_enabled` exists in `~/.nucleos/web.yaml` for that reason and is off; nothing consumes it yet, so no pillar can search today.
10. **Nothing in the web pillar has met a real server.** There is no provider key on this machine and no test leaves it, deliberately. The first `enabled: true` is the first contact.
11. **A stranger's words can reach a council's synthesis.** A phase-1 seat holds `get_email`, `get_email_queue` and `list_files`, so it can read mail somebody else wrote; its answer then enters the ranking seats' prompts in phase 2 and the chairman's in phase 3. The turn-marking rule is redundant inside a council — there is no `Acts` on `COUNCIL_TOOLS` for it to refuse — so what bounds this is the absence of any acting tool in the pillar. **The residual is influence on text the owner reads, never a tool call.** Narrowing it further means removing the mail tools from the list, which would also remove the reason somebody would ask a council about their own correspondence. Secrets are not part of this residual: `redact_rendered` runs on every tool result on both paths a seat can take — inside `filter_outgoing` for a cloud seat's MCP call, and inside `LocalToolBox::call` for a local one. That filter recognises shapes, not meaning, which is why the residual is stated in terms of prose.
12. **Nothing in the council has met a real model.** Every integration test drives a scripted `CommandRunner`, and a local seat is proved only as far as landing its `runs` row. There is no `~/.nucleos/council.yaml` on this machine, so the pillar is dark; the first roster written is the first contact, and the phase-2 and phase-3 prompts have no evidence behind them yet.
13. **CLOSED 2026-08-19 — WebRTC egress from the browser fence.** Held only by `launch.applyWebRTCPolicy` and proven by `gate.TestWebRTCUDPDoesNotLeaveTheFence`; listed because nothing backs it up. If that test goes red, this is what it means. See "The browser" › Holes, named.
14. **An arbitrary `GET` under the owner's authenticated identity**, via `browser_open(url)`. Unmitigated in v1. See "The browser" › Holes, named.
15. **The browser fence's real-browser test group is thin** and has never run against the pinned Chromium. See "The browser" › Holes, named.
16. **The per-profile loopback list has no expiry and no drift warning.** See "The browser" › Holes, named.
17. **The pinned Chromium has no patch owner.** See "The browser" › Holes, named.
