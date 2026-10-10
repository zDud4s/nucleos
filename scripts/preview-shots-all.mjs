/**
 * Build the shell, serve it, and photograph it.
 *
 * A passing suite and a screen that reads well are different claims. jsdom can
 * make the first one and cannot make the second: it computes no layout, applies
 * no stylesheet and has no pixels, so spacing, contrast, what a long name does
 * to a table and whether the eye lands where it should are all invisible to it.
 * This drives a real engine over a real bundle and writes PNGs somebody can
 * look at.
 *
 * Deliberately modelled on `csp-gate.mjs` — same Chromium discovery, same CDP
 * plumbing — because that file already solved "drive a browser over this app's
 * production bundle" and a second, different answer to the same problem is how
 * two harnesses start disagreeing about what the app is.
 *
 * Scaffolding, not a gate. It asserts nothing and fails nothing; it produces
 * pictures. Nothing in `scripts/gates.sh` calls it.
 *
 *   node scripts/preview-shots.mjs [outdir] [name filter]
 *
 * The filter is a plain substring against the shot's name, for the loop this
 * turns into while a page is being worked on: `… preview-shots map` builds
 * once and photographs only the map. Absent, it takes everything.
 */

import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { existsSync, readFileSync } from "node:fs";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { randomInt } from "node:crypto";
import { tmpdir } from "node:os";
import { dirname, extname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { ownUntilExit } from "./leave-nothing-behind.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SHELL = join(ROOT, "shell");
const OUT = join(SHELL, "dist-preview");
const SHOTS = resolve(process.argv[2] ?? join(ROOT, "preview-shots"));
const ONLY = process.argv[3];

const WIDTH = 1440;
const HEIGHT = 960;
const READY_TIMEOUT = 20_000;

function die(why) {
  console.error("preview: " + why);
  process.exit(1);
}

/** Chromium, by whichever name this machine has it — the same list the CSP gate uses. */
const BROWSERS = [
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
  "C:/Program Files (x86)/Google/Chrome/Application/chrome.exe",
  "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
  "C:/Program Files/Microsoft/Edge/Application/msedge.exe",
];
const browser = BROWSERS.find((path) => existsSync(path));
if (browser === undefined) die("no Chromium found. Looked for Chrome and Edge in Program Files.");

/* ------------------------------------------------------------------ build */

console.log("preview: building the bundle…");
const build = spawn(
  process.execPath,
  ["node_modules/vite/bin/vite.js", "build", "--config", "preview.vite.config.mjs"],
  { cwd: SHELL, stdio: "inherit" },
);
const buildCode = await new Promise((ok) => build.on("close", ok));
if (buildCode !== 0) die("the preview bundle did not build (vite exited " + buildCode + ")");
if (!existsSync(join(OUT, "preview.html"))) die("the build produced no preview.html in " + OUT);

/* ------------------------------------------------- the policy, and a server */

/*
  Served under the PRODUCTION policy, exactly as the CSP gate does, and for a
  reason worth stating: a preview served without one would render happily even
  if the page were refused in a packaged build, and the picture would be of a
  screen nobody will ever see.
*/
const conf = JSON.parse(await readFile(join(SHELL, "src-tauri/tauri.conf.json"), "utf8"));
const csp = Object.entries(conf.app.security.csp)
  .map(([directive, value]) => `${directive} ${value}`)
  .join("; ");

const TYPES = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
  ".woff2": "font/woff2",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".json": "application/json",
};

/** Tauri's two-half nonce, composed — see the same function in `csp-gate.mjs`. */
function stampNonces(html) {
  const nonces = [];
  const stamped = html.replace(/<style(?![^>]*\snonce=)/g, () => {
    const nonce = String(randomInt(1, 2 ** 48 - 1));
    nonces.push(nonce);
    return `<style nonce="${nonce}"`;
  });
  if (nonces.length === 0) return { html, policy: csp };
  const policy = csp
    .split("; ")
    .map((directive) =>
      directive.startsWith("style-src ")
        ? directive + " " + nonces.map((nonce) => `'nonce-${nonce}'`).join(" ")
        : directive,
    )
    .join("; ");
  return { html: stamped, policy };
}

const server = createServer(async (request, response) => {
  const url = new URL(request.url, "http://127.0.0.1");
  const file = url.pathname === "/" ? join(OUT, "preview.html") : join(OUT, url.pathname);
  if (!file.startsWith(OUT) || !existsSync(file)) {
    response.statusCode = 404;
    response.end("not here");
    return;
  }
  const raw = await readFile(file);
  const html = extname(file) === ".html";
  const { html: body, policy } = html ? stampNonces(raw.toString("utf8")) : { policy: csp };
  response.setHeader("Content-Security-Policy", policy);
  response.setHeader("Content-Type", TYPES[extname(file)] ?? "application/octet-stream");
  response.end(html ? body : raw);
});
await new Promise((ok) => server.listen(0, "127.0.0.1", ok));
const origin = `http://127.0.0.1:${server.address().port}`;

/* ----------------------------------------------------------------- browser */

const profile = await mkdtemp(join(tmpdir(), "nucleos-preview-"));
const chromium = spawn(browser, [
  "--headless=new",
  "--remote-debugging-port=0",
  `--user-data-dir=${profile}`,
  "--no-first-run",
  "--no-default-browser-check",
  "--disable-gpu",
  "--force-device-scale-factor=2",
  "--font-render-hinting=none",
  `--window-size=${WIDTH},${HEIGHT}`,
  "about:blank",
]);
/* Everything below can fail. None of it can leave the browser or its profile behind. */
ownUntilExit(chromium, profile);

async function debuggerUrl() {
  const portFile = join(profile, "DevToolsActivePort");
  for (let attempt = 0; attempt < 120; attempt++) {
    if (existsSync(portFile)) {
      try {
        /*
          The read belongs inside the `try`. The file exists for a moment before
          it can be read, and the EBUSY from reading it too early used to escape
          this loop and fail the run — a red that said nothing about the app.
        */
        const [port] = readFileSync(portFile, "utf8").split("\n");
        const response = await fetch(`http://127.0.0.1:${port.trim()}/json/version`);
        return (await response.json()).webSocketDebuggerUrl;
      } catch {
        /* the port file lands a moment before the endpoint answers */
      }
    }
    await new Promise((ok) => setTimeout(ok, 250));
  }
  throw new Error("the browser never opened a debugging port");
}

const socket = new WebSocket(await debuggerUrl());
await new Promise((ok, no) => {
  socket.onopen = ok;
  socket.onerror = () => no(new Error("could not attach to the browser"));
});

let nextId = 0;
const pending = new Map();
const pageErrors = [];

socket.onmessage = (message) => {
  const frame = JSON.parse(message.data);
  if (frame.id !== undefined) {
    pending.get(frame.id)?.(frame);
    pending.delete(frame.id);
    return;
  }
  if (frame.method === "Runtime.exceptionThrown") {
    const detail = frame.params.exceptionDetails;
    pageErrors.push(detail.exception?.description ?? detail.text);
  }
};

function send(method, params = {}, sessionId) {
  const id = ++nextId;
  return new Promise((ok, no) => {
    pending.set(id, (frame) =>
      frame.error ? no(new Error(method + ": " + JSON.stringify(frame.error))) : ok(frame.result),
    );
    socket.send(JSON.stringify({ id, method, params, sessionId }));
  });
}

const { targetId } = await send("Target.createTarget", { url: "about:blank" });
const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
await send("Runtime.enable", {}, sessionId);
await send("Page.enable", {}, sessionId);
await send("Emulation.setDeviceMetricsOverride", {
  width: WIDTH,
  height: HEIGHT,
  deviceScaleFactor: 2,
  mobile: false,
}, sessionId);

async function evaluate(expression) {
  const result = await send(
    "Runtime.evaluate",
    { expression, awaitPromise: true, returnByValue: true },
    sessionId,
  );
  if (result.exceptionDetails) {
    throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
  }
  return result.result.value;
}

await mkdir(SHOTS, { recursive: true });

/**
 * One picture: load, wait for the page to say it has settled, capture the whole
 * document rather than the fold.
 *
 * `captureBeyondViewport` with the document's own height, because a console
 * that reads well above the fold and falls apart below it is exactly the defect
 * a viewport-sized screenshot hides.
 */
async function shoot(name, { path, tab, press, drag, theme = "dark" }) {
  await send("Emulation.setEmulatedMedia", {
    features: [{ name: "prefers-color-scheme", value: theme }],
  }, sessionId);

  pageErrors.length = 0;
  const query = new URLSearchParams({
    path,
    ...(tab === undefined ? {} : { tab }),
    /* The label of a button to click once the page has settled — how a surface
       that opens from a control gets photographed at all. See `preview/main.tsx`. */
    ...(press === undefined ? {} : { press }),
    /* The title of an occurrence to pick up and hold. A drag is a state the
       page enters, and everything it turns on exists only while it lasts. */
    ...(drag === undefined ? {} : { drag }),
  });
  await send("Page.navigate", { url: `${origin}/preview.html?${query}` }, sessionId);

  const deadline = Date.now() + READY_TIMEOUT;
  for (;;) {
    if (await evaluate("window.__preview?.ready === true").catch(() => false)) break;
    if (Date.now() > deadline) {
      throw new Error(
        name + ": never settled" + (pageErrors.length > 0 ? " — " + pageErrors[0].split("\n")[0] : ""),
      );
    }
    await new Promise((ok) => setTimeout(ok, 100));
  }

  /*
    The shell is a fixed-height desktop layout: the rail stays put and the main
    column scrolls INSIDE itself, so `document.scrollHeight` is the window and
    never the page. `captureBeyondViewport` then grows the canvas and paints
    nothing into the extra — which is how the first run of this produced a
    console with three cards and a foot of black under it.

    So: measure the scrolling column, grow the VIEWPORT to fit it, and let the
    layout reflow. The picture is then of a window tall enough to hold the page,
    which is the honest way to show a page taller than any window.
  */
  const needed = await evaluate(`(() => {
    const main = document.querySelector("main") ?? document.body;
    const chrome = window.innerHeight - main.clientHeight;
    return Math.ceil(chrome + main.scrollHeight);
  })()`);
  const tall = Math.min(Math.max(needed, HEIGHT), 5000);

  if (tall !== HEIGHT) {
    await send(
      "Emulation.setDeviceMetricsOverride",
      { width: WIDTH, height: tall, deviceScaleFactor: 2, mobile: false },
      sessionId,
    );
    // One beat for the reflow, and one for anything that measures itself.
    await new Promise((ok) => setTimeout(ok, 350));
  }

  /*
    Did a picture of the page, or a picture of the apology?

    A render that throws is CAUGHT by the router's error boundary, so nothing
    reaches `Runtime.exceptionThrown` and `pageErrors` stays empty — this
    harness reported `ok` over a black rectangle reading "Something went wrong!"
    and the fault was found by eye, days later, which is precisely the failure
    it exists to prevent. Detected by the boundary's own words because they are
    the only thing on the page: a screen that renders those two lines and
    nothing else has not been photographed, it has been missed.
  */
  const apology = await evaluate(`(() => {
    const said = document.body.innerText.trim();
    return said.startsWith("Something went wrong!") && said.length < 200;
  })()`).catch(() => false);
  if (apology) pageErrors.push("the router's error boundary — the page threw while rendering");

  const { data } = await send("Page.captureScreenshot", { format: "png" }, sessionId);

  if (tall !== HEIGHT) {
    await send(
      "Emulation.setDeviceMetricsOverride",
      { width: WIDTH, height: HEIGHT, deviceScaleFactor: 2, mobile: false },
      sessionId,
    );
  }
  const height = tall;

  const file = join(SHOTS, name + ".png");
  await writeFile(file, Buffer.from(data, "base64"));
  console.log(
    "  " + (pageErrors.length === 0 ? "ok  " : "err ") + name + "  " + Math.round(height) + "px" +
      (pageErrors.length > 0 ? "  — " + pageErrors[0].split("\n")[0] : ""),
  );
  return pageErrors.length === 0;
}

/* --------------------------------------------------------------------- run */

console.log("\npreview: " + origin + "\n");

const SHOTS_TO_TAKE = [
  ["00-home", { path: "/" }],
  ["00-home-light", { path: "/", theme: "light" }],
  // `press` clicks a button by label and cannot send a chord.
  ["00-home-palette", { path: "/", press: "Go to" }],
  ["01-fleet", { path: "/fleet" }],
  ["01-fleet-light", { path: "/fleet", theme: "light" }],
  ["02-autopilot", { path: "/autopilot" }],
  /* The armed interlock. The carousel opens on bravo, the most urgent project (its queue is full),
     and shows one project's switch at a time — bravo's "Let it act" is the pressed setting, not an
     offer. So two presses: the first selects alpha's tab by its name (a tab's text starts with its
     project id), the second arms alpha's interlock, the only promotable project. The preview
     declares ready 400 ms after the last press, well inside the 4 s window. */
  ["02-autopilot-armed", { path: "/autopilot", press: "alpha|Let it act" }],
  ["03-waiting", { path: "/waiting" }],
  /* The first approval card's interlock, armed: the preview's `PROPOSALS` start at id 101. */
  ["03-waiting-armed", { path: "/waiting", press: "Approve #101" }],
  /* The batch. `press` clicks a BUTTON by label — exact first, then by prefix — which is why
     "Select all 5" is a button and not a header checkbox: a native checkbox cannot be reached
     from here at all. The second shot uses the `|` sequence form, 250 ms apart, so `Approve 5`
     exists in the DOM by the time it is asked for. */
  ["03-waiting-selected", { path: "/waiting", press: "Select all" }],
  ["03-waiting-selected-armed", { path: "/waiting", press: "Select all|Approve 5" }],
  ["03-waiting-light", { path: "/waiting", theme: "light" }],
  ["04-runs", { path: "/runs" }],
  ["05-run-detail", { path: "/runs/1" }],
  ["06-feed", { path: "/feed" }],
  ["07-learned", { path: "/learned" }],
  ["08-projects", { path: "/projects" }],
  ["09-new-project", { path: "/projects/new" }],
  ["10-project-state", { path: "/projects/alpha/state" }],
  ["11-project-code", { path: "/projects/alpha/code" }],
  ["12-project-map", { path: "/projects/alpha/map" }],
  ["13-project-workflows", { path: "/projects/alpha/workflows" }],
  ["14-inspect-browse", { path: "/projects/alpha/inspect/browse?path=core/src&file=core/src/config.rs" }],
  ["15-chats", { path: "/chats" }],
  ["16-chat-open", { path: "/chats/c-preview" }],
  ["17-errands", { path: "/errands" }],
  ["18-teams", { path: "/teams" }],
  ["19-bench", { path: "/teams/financas" }],
  ["20-team-run", { path: "/team-runs/run-live-1" }],
  ["21-agents", { path: "/agents" }],
  ["21-agents-editor", { path: "/agents", press: "Auditor Sénior" }],
  ["22-council", { path: "/council" }],
  ["23-mail", { path: "/mail" }],
  ["24-mail-detail", { path: "/mail/1" }],
  /* The first armed QUIET interlock anybody has photographed: `Requeue for triage` is
     `variant="quiet"`, and `EMAIL_DETAIL` has a body, which is what makes it eligible. */
  ["24-mail-detail-armed", { path: "/mail/1", press: "Requeue for triage" }],
  ["25-contacts", { path: "/contacts" }],
  ["26-calendar", { path: "/calendar?on=2026-08-24" }],
  ["26-calendar-week", { path: "/calendar?view=week&on=2026-08-25" }],
  ["27-voice", { path: "/voice" }],
  ["28-web", { path: "/web/archive" }],
  ["29-browser", { path: "/browser" }],
  ["30-files", { path: "/files" }],
  ["31-system", { path: "/system" }],
  /* The armed restart: `browser_sidecar` is the preview's one down sidecar, and one press arms the
     interlock. The second press is not taken — the preview daemon is stateless, so what there is to
     photograph here is the interlock, not the recovery. */
  ["31-system-armed", { path: "/system", press: "Restart" }],
  ["31-system-light", { path: "/system", theme: "light" }],
  ["31-system-backups", { path: "/system/backups" }],
  ["31-system-backups-light", { path: "/system/backups", theme: "light" }],
  ["31-system-tokens", { path: "/system/tokens" }],
  ["31-system-tokens-light", { path: "/system/tokens", theme: "light" }],
  ["31-system-notifications", { path: "/system/notifications" }],
  ["31-system-notifications-open", { path: "/system/notifications", press: "3 kinds" }],
  ["31-system-settings", { path: "/system/settings" }],
];

const wanted = SHOTS_TO_TAKE.filter(([name]) => ONLY === undefined || name.includes(ONLY));
if (wanted.length === 0) die(`no shot matches "${ONLY}"`);

let clean = true;
for (const [name, options] of wanted) {
  clean = (await shoot(name, options)) && clean;
}

console.log("\npreview: " + wanted.length + " shots in " + SHOTS);
if (!clean) console.log("preview: at least one page threw — see the lines above");

server.close();
process.exit(0);
