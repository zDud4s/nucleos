# Remote access

Remote access lets you use the NucleOS shell from another device's browser (a laptop, a phone). The
daemon, the agents, the files and every model call stay on the host; the other device only shows the
interface.

```
browser --HTTPS--> your HTTPS front (Tailscale, or your own) --> gateway 127.0.0.1:8797 --> daemon
```

The gateway (`sidecars/remote`) is started by the daemon. It serves the remote build of the shell and
proxies `/api/*` and `/auth/*` to the daemon. The network decides who can reach the page; a pairing
code decides who can use it.

## Turn it on (no terminal)

Open **Settings, System, Remote devices**. The panel shows what it can see on this machine.

- **Tailscale not installed.** Press **Install Tailscale**, install it, sign in on both devices (the
  same tailnet), then reopen the panel.
- **Signed in, HTTPS off.** Press **Open the Tailscale admin console** and turn on MagicDNS and
  **HTTPS Certificates**. Come back.
- **Ready.** Press **Turn on remote access**. NucleOS publishes the gateway on your tailnet with
  `tailscale serve`, writes `remote.yaml` and starts the gateway.
- **Your own HTTPS.** If something you already run terminates TLS and forwards to
  `http://127.0.0.1:8797`, type its `https://host` address in the field instead. Tailscale is then not
  needed.

NucleOS looks for the Tailscale CLI where each system installs it: `C:\Program Files\Tailscale` on
Windows, the app bundle, Homebrew or `/usr/local/bin` on macOS, `/usr/bin`, `/usr/sbin`,
`/usr/local/bin` or `/snap/bin` on Linux, then `PATH`. **Funnel is never used**: it would publish the
page to the public internet, and the app has no way to run it.

**Tailscale asks you to approve Serve.** On a tailnet that never used Serve, Tailscale waits for you to
approve it on its website. The panel then shows **Open the Tailscale approval page**. Open it, approve,
come back and press **Turn on remote access** again. Nothing was changed on your machine in between.

**Something already uses the HTTPS port.** If Tailscale already serves something else on this
machine's HTTPS port (443), the panel says so and **Turn on remote access** stays off: NucleOS never
replaces what it did not publish, and **Turn off remote access** never stops it either. Free the port
in Tailscale, or type an address you already have instead.

**Funnel is on.** If Tailscale Funnel has that port open to the public internet, the panel says so and
refuses to turn remote access on while it is. NucleOS does not turn Funnel on or off. Turn it off in
Tailscale, then try again.

**Turn off remote access** stops the gateway, removes its credential and stops publishing, but only
when Tailscale is publishing the gateway. The address stays in the file, so turning it on again needs
no typing. If `remote.yaml` exists and cannot be read, both buttons leave it alone and say so: fix or
remove the file first.

Only one NucleOS on a machine runs the gateway. Another one asked to turn remote access on says so and
changes nothing.

## Pair a device

1. In the panel, name the device, pick a level (read-only is the default; **admin** works like you
   on the host) and press **Create code**.
2. The code looks like `XXXXX-XXXXX`, is shown once, lasts 5 minutes and works once. Five wrong
   guesses burn it. A QR code of the address is shown beside it.
3. On the other device open the address (scan the QR code) and enter the code.
4. The browser then holds a session cookie: at most 30 days, and 14 days without use.

Revoke a device in the same panel; its session stops at once for new requests and open streams are cut
within `stream_max_seconds`. A device can also log itself out.

## Setup is host-only

`GET/POST/DELETE /remote/setup` answer only to the host's own control token, checked in the handler
itself. A paired device, whatever its level, cannot turn remote access on or off, create codes or
revoke devices.

## What stays host-only

- Dictation, native file dialogs, the updater, "open in VS Code" and the native browser panel.
- Daemon shutdown and updates, machine config and secrets, backups, API tokens, sidecar control,
  channel pairing, the browser's live pixels, and managing remote access itself.
- Starting a browser agent on the host. Any route family not listed for devices is closed.

## The file, if you want it

The panel writes `remote.yaml` in the NucleOS settings folder (`~/.nucleos`):

```yaml
enabled: true
public_origin: "https://mybox.tail1234.ts.net"
# dist_dir: "..."            # default: the page installed beside the app
# stream_max_seconds: 120    # 10..=900
```

`public_origin` must be `https://`, with no path, query or fragment, written as the browser shows it
(lower-case host). An invalid file with `enabled: true` starts nothing.

## From source

A release install ships the page (`dist-remote`) and the gateway. From a checkout, build them:

```
npm --prefix shell run build:remote
bash scripts/build-sidecars.sh
```

`scripts/run-daemon.ps1` mirrors `shell/dist-remote` next to the staged daemon.

## Troubleshooting

| Symptom | Likely cause and fix |
|---|---|
| Panel says the page is missing (`remote_page_missing`) | No built `dist-remote` was found. Reinstall, or build it as above. |
| 404 at `/` | The gateway has no page where it looks. Same fix; check `dist_dir`. |
| 421 | `Host` does not match `public_origin`. Open the page through exactly that address. |
| 403 `bad_origin` / `cross_site` / `csrf` | Open the page directly through the address, not an IP; hard-reload. |
| 401 `session_invalid` | Session expired or revoked. Pair again. |
| 502 `daemon_unreachable` | The daemon is down or restarting. |
| 409 `remote_disabled` when creating a code | Remote access is off. Turn it on in the panel. |
| "Tailscale refused to publish the page" | HTTPS certificates are off in the admin console. |
| "Tailscale needs you to approve Serve" | Open the approval page the panel offers, approve, press the button again. |
| "already serving something else" (`serve_port_taken`) | Free port 443 in Tailscale or use your own HTTPS address. |
| "Funnel has this machine open" (`funnel_enabled`) | Turn Funnel off in Tailscale. NucleOS never turns it on. |
| `config_unreadable` | `remote.yaml` does not parse. Fix or remove it; NucleOS did not change it. |
| `not_primary` | Another NucleOS on this machine runs the gateway. Use that one. |
