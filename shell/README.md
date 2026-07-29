# NucleOS shell

The desktop face of the núcleo: a tray-resident Tauri app that watches a daemon
running on this machine and holds the controls a person is not allowed to
delegate — approving what an autonomous run wants to do outside its allowlist,
rejecting it, and stopping everything.

It owns no state. Every number on screen came from the daemon a moment ago, and
every button is a request back to it. Closing the window hides it to the tray
rather than quitting, because the approval queue is only useful if it is
reachable at the moment a run stops to ask.

## How it talks to the daemon

- **Transport:** plain HTTP to `http://127.0.0.1:8791`. Loopback only; the shell
  has no notion of a remote núcleo.
- **Credential:** a bearer token the daemon writes to the OS Credential Manager
  under service `nucleos`, key `daemon-token`. The shell reads it through the
  `get_daemon_token` Tauri command (`src-tauri/src/lib.rs`) because both run as
  the same OS user — no shared file, no path to agree on.
- **Cadence:** a 3-second poll, one round at a time. Rounds never overlap, so a
  daemon slower than the tick cannot stack requests whose answers then land out
  of order.
- **Failure states are kept distinct.** "Daemon unreachable" is fixed by
  waiting; "daemon rejected the stored token" never is. The shell says which one
  it hit instead of retrying forever behind a green light.

## Layout

| Path | What lives there |
|---|---|
| `src/api.ts` | Every daemon call, and the only place a URL or a header appears. |
| `src/derive.ts` | Pure derivations — readiness, promotion gates, formatting. No I/O, heavily tested. |
| `src/App.tsx` | Connection handshake, token read, kill switch, tab shell. |
| `src/Home.tsx` | The read-only digest: what needs you right now. |
| `src/Autopilot.tsx` | The cockpit: approval queue, projects, shadow review, scoreboard, budget. |
| `src/Mail.tsx` | The mailbox the núcleo triages. |
| `src/ui/` | The primitives (`Button`, `ConfirmButton`, `Panel`, …); their styling is `src/ui.css`. |
| `src-tauri/` | The Rust side: tray, window behaviour, credential read. |

## Working on it

```sh
npm ci
npm run tauri dev     # the real app, Rust included
npm run dev           # the web view alone; no daemon token is available there
npm test              # vitest, jsdom
npx tsc -b            # NOT `tsc -b --noEmit` — a referenced project may not disable emit
```

Those last two are the shell's half of the gate; `bash scripts/gates.sh shell`
from the repo root runs exactly them, and `.github/workflows/ci.yml` calls that
same script. The workflow does not run yet — the repository has no remote, and
Actions reads workflows server-side — so for now nothing runs these but you.

## Things that are load-bearing

**The CSP is a real one** (`src-tauri/tauri.conf.json`), and JSON cannot carry
the reason: this webview holds a token that grants full control of the daemon —
approving proposals, disengaging the kill switch — so any script injected into
the page could exfiltrate it. `default-src 'self'` with no `unsafe-inline` in
production is what stops an injected `<script>` from running at all, and
`connect-src` names exactly two destinations: the Tauri IPC channel and the
daemon. `devCsp` is the looser twin that lets Vite's HMR work; it applies only
under `tauri dev`.

**`ConfirmButton` is the only interlock.** Every irreversible control is that
component: two clicks, a minimum dwell between them, and browser key-repeat
cancelled — otherwise a held Enter or a stray double-click crosses both states in
~50ms and confirms nothing. The approval queue additionally holds its order
still while a decision is open, because the list re-sorts itself every 3 seconds
and a row would otherwise move under the cursor between the two clicks.

**Autostart is offered once.** The first launch enables it and drops a marker in
the app config dir; later launches never assert it again, so turning it off in
Windows Settings stays off.

**No updater.** The plugin was registered with a placeholder signing key and an
`example.invalid` endpoint, so it could never have run; it is gone rather than
half-present. Bringing it back means a real key, a real release feed and the
updater capability, in one change.
