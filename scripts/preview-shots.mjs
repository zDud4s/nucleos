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
 *   node scripts/preview-shots.mjs [outdir]
 */

import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { existsSync, readFileSync } from "node:fs";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { randomInt } from "node:crypto";
import { tmpdir } from "node:os";
import { dirname, extname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SHELL = join(ROOT, "shell");
const OUT = join(SHELL, "dist-preview");
const SHOTS = resolve(process.argv[2] ?? join(ROOT, "preview-shots"));

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
  process.platform === "win32" ? "npx.cmd" : "npx",
  ["vite", "build", "--config", "preview.vite.config.mjs"],
  { cwd: SHELL, stdio: "inherit", shell: process.platform === "win32" },
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

async function debuggerUrl() {
  const portFile = join(profile, "DevToolsActivePort");
  for (let attempt = 0; attempt < 120; attempt++) {
    if (existsSync(portFile)) {
      const [port] = readFileSync(portFile, "utf8").split("\n");
      try {
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
  ["01-console-dark", { path: "/teams" }],
  ["02-console-light", { path: "/teams", theme: "light" }],
  ["03-bench-work", { path: "/teams/financas" }],
  ["04-bench-decisions", { path: "/teams/financas", tab: "Decisions" }],
  ["05-bench-routines", { path: "/teams/financas", tab: "Routines" }],
  ["06-bench-charter", { path: "/teams/financas", tab: "Charter" }],
  ["07-bench-no-ceiling", { path: "/teams/seguranca", tab: "Routines" }],
  ["08-bench-empty", { path: "/teams/operacoes", tab: "Charter" }],
  ["09-bench-charter-light", { path: "/teams/financas", tab: "Charter", theme: "light" }],
  ["10-catalogue-dark", { path: "/agents" }],
  ["11-catalogue-light", { path: "/agents", theme: "light" }],
  /* The editor, which is the half of the page that is not on screen at rest —
     and the row it opens from is the renamed one, so the shot carries the two
     things this page exists for at once. */
  ["12-catalogue-editor", { path: "/agents", press: "Auditor Sénior" }],
  ["13-catalogue-editor-light", { path: "/agents", press: "Auditor Sénior", theme: "light" }],
  ["14-catalogue-new", { path: "/agents", press: "New agent" }],

  /* The inspector. Four views over four deliberately awkward projects: `alpha`
     works and is busy, `bravo` is stopped in the two ways nothing else in the
     app reports, `charlie` was never given a folder, `delta` has one and no
     rules — which is ordinary and must not photograph as a fault. */
  ["20-inspect-browse", { path: "/projects/alpha/inspect/browse" }],
  ["21-inspect-browse-light", { path: "/projects/alpha/inspect/browse", theme: "light" }],
  /* The folder and the file come out of the URL now, so the shot is the reload
     rather than a click the harness has to fake — and it photographs the two
     column split, which only exists when a file is open. */
  ["22-inspect-file", { path: "/projects/alpha/inspect/browse?path=core/src&file=core/src/config.rs" }],
  ["23-inspect-search", { path: "/projects/alpha/inspect/search?q=gate_before_publish" }],
  ["24-inspect-diff", { path: "/projects/alpha/inspect/diff" }],
  ["25-inspect-rules", { path: "/projects/alpha/inspect/rules" }],
  ["26-inspect-rules-light", { path: "/projects/alpha/inspect/rules", theme: "light" }],
  ["27-inspect-stopped", { path: "/projects/bravo/inspect/rules" }],
  ["28-inspect-no-folder", { path: "/projects/charlie/inspect/browse" }],
  ["29-inspect-nothing-runs", { path: "/projects/delta/inspect/rules" }],

  /* The Codigo mode's empty state, which is where both doors into the inspector
     are drawn — and the only place in the app that opens it. */
  ["30-codigo-doors", { path: "/projects/alpha/codigo" }],

  /* The calendar. Its month, view and selected day come out of the URL, so
     these are reloads rather than clicks the harness has to fake — the same
     move the inspector made, and the only way the week of a clock change is
     reachable at all.

     The clock is pinned in `preview/main.tsx` to the week the fixtures live
     in. Without that a calendar photographs a different month every day and
     two shots can never be compared. */
  ["40-calendar-month", { path: "/calendar?on=2026-08-24" }],
  ["41-calendar-month-light", { path: "/calendar?on=2026-08-24", theme: "light" }],
  /* Tuesday the 25th: three meetings genuinely overlapping at ten in the
     morning, which is the only thing that exercises the lane arithmetic — and
     the one shape the month grid cannot draw at all. */
  ["42-calendar-week", { path: "/calendar?view=week&on=2026-08-25" }],
  ["43-calendar-week-light", { path: "/calendar?view=week&on=2026-08-25", theme: "light" }],
  /* Lisbon's 23-hour day. Six columns of 24 bands and one of 23, with the
     badge in its heading that says why. If the compression reads as a
     rendering fault rather than as the fact it is, this is the picture that
     will say so. */
  ["44-calendar-dst-week", { path: "/calendar?view=week&on=2026-03-29" }],
  /* A month with nothing in it, which must read as an empty calendar and not
     as a page that failed to load — the state every grid gets wrong first. */
  ["45-calendar-empty", { path: "/calendar?on=2027-02-15" }],
  ["46-calendar-empty-light", { path: "/calendar?on=2027-02-15", theme: "light" }],
  /* Mid-drag, which is the only state where the drop targets exist at all —
     and the one thing about dragging no test can judge: whether the dashed
     "this would take it" reads as a different thing from the solid selection
     ring, and whether thirty-odd outlined cells at once is legible or a mess. */
  ["47-calendar-dragging", { path: "/calendar?on=2026-08-24", drag: "Reconcile the ledger" }],
  ["48-calendar-dragging-light", { path: "/calendar?on=2026-08-24", drag: "Reconcile the ledger", theme: "light" }],
  ["49-calendar-week-dragging", { path: "/calendar?view=week&on=2026-08-25", drag: "Design review" }],
];

let clean = true;
for (const [name, options] of SHOTS_TO_TAKE) {
  clean = (await shoot(name, options)) && clean;
}

console.log("\npreview: " + SHOTS_TO_TAKE.length + " shots in " + SHOTS);
if (!clean) console.log("preview: at least one page threw — see the lines above");

chromium.kill();
server.close();
process.exit(0);
