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
// Vite's own entry through this Node, not `npx`, which is what `csp-gate.mjs` does and the
// reason this file says it is modelled on that one: a `.cmd` shim needs a shell on Windows,
// and reaching for one buys a quoting bug, a DEP0190 warning about unescaped arguments, and
// five seconds of shim startup per run. Measured here: 5.35s through `npx.cmd` against 0.28s
// through this path, for the same `vite --version`.
const build = spawn(
  process.execPath,
  [join(SHELL, "node_modules/vite/bin/vite.js"), "build", "--config", "preview.vite.config.mjs"],
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
async function shoot(name, { path, tab, press, drag, hover, window: windowKind, viewport, theme = "dark" }) {
  await send("Emulation.setEmulatedMedia", {
    features: [{ name: "prefers-color-scheme", value: theme }],
  }, sessionId);

  /*
    A per-shot viewport, overriding WIDTH/HEIGHT for this one picture and restored afterward — same
    override/restore shape the tall-page growth below already uses, and for the same reason: a size
    one shot needs is not a size every shot should inherit. The floating quota notch is on the order
    of a couple hundred pixels; the default 1440x960 canvas would photograph it as a speck on a field
    of black.
  */
  const { width, height } = viewport ?? { width: WIDTH, height: HEIGHT };
  await send(
    "Emulation.setDeviceMetricsOverride",
    { width, height, deviceScaleFactor: 2, mobile: false },
    sessionId,
  );

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
    /* A selector to hover once the page has settled — the gesture a surface that opens on
       `onPointerEnter` needs, where `press`'s click would land on a control instead. */
    ...(hover === undefined ? {} : { hover }),
    /* Which face of the bundle to mount: absent is the router, `notch` is the floating quota
       window `?window=notch` opens in the packaged app. See `preview/main.tsx` and `main.tsx`. */
    ...(windowKind === undefined ? {} : { window: windowKind }),
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

    Skipped entirely when a `viewport` was given. That measurement reads `main`,
    which does not exist in the notch window — `NotchWindow` renders no `<main>`
    at all — and falls back to `document.body`, whose own height there is just
    the notch's drawing (`.notch-host body` sizes to content, `app.css`). Feeding
    THAT into the same "grow to fit a scrolling column" arithmetic a console page
    needs is not a smaller version of the same problem, it is a different
    question with no `main` to answer it: a `viewport` shot is asking for an
    exact frame around a small drawing, not for room to keep growing.
  */
  let tall = height;
  if (viewport === undefined) {
    const needed = await evaluate(`(() => {
      const main = document.querySelector("main") ?? document.body;
      const chrome = window.innerHeight - main.clientHeight;
      return Math.ceil(chrome + main.scrollHeight);
    })()`);
    tall = Math.min(Math.max(needed, HEIGHT), 5000);

    if (tall !== HEIGHT) {
      await send(
        "Emulation.setDeviceMetricsOverride",
        { width, height: tall, deviceScaleFactor: 2, mobile: false },
        sessionId,
      );
      // One beat for the reflow, and one for anything that measures itself.
      await new Promise((ok) => setTimeout(ok, 350));
    }
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

  // Restore the harness's default canvas, whether this shot grew for a tall page or was given its
  // own `viewport` — either way the NEXT shot in the loop has to start from a known size rather than
  // inherit whatever this one left behind.
  if (width !== WIDTH || tall !== HEIGHT) {
    await send(
      "Emulation.setDeviceMetricsOverride",
      { width: WIDTH, height: HEIGHT, deviceScaleFactor: 2, mobile: false },
      sessionId,
    );
  }

  const file = join(SHOTS, name + ".png");
  await writeFile(file, Buffer.from(data, "base64"));
  console.log(
    "  " + (pageErrors.length === 0 ? "ok  " : "err ") + name + "  " + Math.round(width) + "x" + Math.round(tall) + "px" +
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
  /* The org chart. `financas` is the department that earns it: `controller` both directs and
     holds round 2's item, so the shot carries the one dashed edge that skips a rank. */
  ["09a-bench-roster", { path: "/teams/financas", tab: "Roster" }],
  ["09b-bench-roster-light", { path: "/teams/financas", tab: "Roster", theme: "light" }],
  /* The department with nothing running, which is the case the chart is FOR: the structure is
     drawn whole, and only the work rank is missing. And the one nobody has staffed at all. */
  ["09c-bench-roster-idle", { path: "/teams/vendas", tab: "Roster" }],
  ["09d-bench-roster-unstaffed", { path: "/teams/operacoes", tab: "Roster" }],
  /* The composer, which is the densest object in the app and was the last one no shot covered.
     Both themes because the controls inside it carry no border and live on `--text-faint`: whether
     they are legible at all is a contrast question, and a contrast question is exactly what a dark
     shot alone cannot answer. */
  ["09e-chat-composer", { path: "/chats/c-preview" }],
  ["09f-chat-composer-light", { path: "/chats/c-preview", theme: "light" }],
  /* The OTHER box. `/chats` with nothing open is the front door, which is a different component
     from the one above and had none of its controls for a long time without anybody being able to
     see that from a test — jsdom computes no layout and applies no stylesheet. */
  ["09g-chat-front-door", { path: "/chats" }],
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

  /* The Code mode's empty state, which is where both doors into the inspector
     are drawn — and the only place in the app that opens it. */
  ["30-code-doors", { path: "/projects/alpha/code" }],

  /* The State mode, which is the densest collection of bordered ghost buttons
     in the app: `edit` on each owned file, `declare a command`, the three mode
     switches, both ceiling steppers and `no ceiling`. Every one of them was a
     white slab with an unreadable word in it until `base.css` gave `button` a
     transparent background — a defect no test could see and a picture cannot
     miss. Both themes, because the fault only looked like a fault in one. */
  ["31-state-controls", { path: "/projects/alpha/state" }],
  ["32-state-controls-light", { path: "/projects/alpha/state", theme: "light" }],

  /* The map, over a project the size of the real one — 245 modules and about
     nine hundred imports. Every complaint this page has ever drawn is a
     complaint about scale, so a fixture that fits comfortably would photograph
     a screen nobody is looking at. */
  ["33-map-matrix", { path: "/projects/alpha/map" }],
  /* And in light, because the matrix now carries a contrast decision and the light
     theme is the half of it that measures worst. Above and below the diagonal are two
     weights of one hue, and no pair of opacities on one hue reaches the 3:1 two marks
     need to be told apart in light -- the pair drawn here is 3.51:1 in dark and 2.95:1
     here. The ruled diagonal is what carries the reading when the weights cannot, and
     whether that works is a contrast question, which is exactly what a dark shot alone
     cannot answer. */
  ["33a-map-matrix-light", { path: "/projects/alpha/map", theme: "light" }],
  /* One level down, which is the state the rail exists for: the list of
     communities stays put, the open one is marked, and every sibling is one
     click away rather than three. */
  ["34-map-community", { path: "/projects/alpha/map", press: "council" }],

  /* The fourth mode, so the workspace can be looked at as the set of four it
     actually is rather than one page at a time. */
  ["35-workflows", { path: "/projects/alpha/workflows" }],

  /* The roster — every project at once, which is the page the workspace is
     reached from and the one surface of this pillar nothing had photographed. */
  ["37-roster", { path: "/projects" }],
  ["38-roster-light", { path: "/projects", theme: "light" }],
  /* The way out, open. The press lands on the FIRST `remove` in the table, and
     the ordering puts `bravo` there — which is the fixture with a record and
     nothing in flight, so the shot carries the ordinary case: the folder
     reassurance, the counts, and the checkbox left unticked. */
  ["39-roster-remove", { path: "/projects", press: "remove" }],
  /* The other exit, which is not on that page at all: the folder delete, at
     the foot of a project's own State mode. `alpha` because it is the fixture
     with work in flight AND uncommitted work — the panel says what would be
     lost and why the button is off, which is the pair worth a picture. */
  [
    "40-delete-folder",
    { path: "/projects/alpha/state", press: "delete this folder…" },
  ],

  /* A door that is not the picture, which is what the row of views exists for:
     the junction reached without scrolling a matrix, and the seam still above
     it — the one thing §16.5 says a view may not take with it. */
  ["36-map-junction", { path: "/projects/alpha/map", press: "Junction" }],

  /* The calendar. Its month, view and selected day come out of the URL, so
     these are reloads rather than clicks the harness has to fake — the same
     move the inspector made, and the only way the week of a clock change is
     reachable at all.

     The clock is pinned in `preview/main.tsx` to the week the fixtures live
     in. Without that a calendar photographs a different month every day and
     two shots can never be compared. */
  ["50-calendar-month", { path: "/calendar?on=2026-08-24" }],
  ["51-calendar-month-light", { path: "/calendar?on=2026-08-24", theme: "light" }],
  /* Tuesday the 25th: three meetings genuinely overlapping at ten in the
     morning, which is the only thing that exercises the lane arithmetic — and
     the one shape the month grid cannot draw at all. */
  ["52-calendar-week", { path: "/calendar?view=week&on=2026-08-25" }],
  ["53-calendar-week-light", { path: "/calendar?view=week&on=2026-08-25", theme: "light" }],
  /* Lisbon's 23-hour day. Six columns of 24 bands and one of 23, with the
     badge in its heading that says why. If the compression reads as a
     rendering fault rather than as the fact it is, this is the picture that
     will say so. */
  ["54-calendar-dst-week", { path: "/calendar?view=week&on=2026-03-29" }],
  /* A month with nothing in it, which must read as an empty calendar and not
     as a page that failed to load — the state every grid gets wrong first. */
  ["55-calendar-empty", { path: "/calendar?on=2027-02-15" }],
  ["56-calendar-empty-light", { path: "/calendar?on=2027-02-15", theme: "light" }],
  /* Mid-drag, which is the only state where the drop targets exist at all —
     and the one thing about dragging no test can judge: whether the dashed
     "this would take it" reads as a different thing from the solid selection
     ring, and whether thirty-odd outlined cells at once is legible or a mess. */
  ["57-calendar-dragging", { path: "/calendar?on=2026-08-24", drag: "Reconcile the ledger" }],
  ["58-calendar-dragging-light", { path: "/calendar?on=2026-08-24", drag: "Reconcile the ledger", theme: "light" }],
  ["59-calendar-week-dragging", { path: "/calendar?view=week&on=2026-08-25", drag: "Design review" }],

  /* The floating quota notch (design D8): `NotchWindow`, mounted the way `?window=notch` mounts it
     in the packaged app (`main.tsx`, mirrored by `preview/main.tsx`) — a borderless window of its
     own, over everything, rather than a piece of some page. Folded is the state it spends almost
     all its life in: two rings and nothing behind them but the transparent ground `.notch-host`
     gives it. A `viewport` frames it with a little room around the drawing instead of the harness's
     default 1440x960 canvas, which would photograph a couple hundred pixels of notch on most of a
     thousand pixels of black. */
  ["60-notch-floating-folded", { path: "/teams", window: "notch", viewport: { width: 640, height: 240 } }],
  /* Unfolded, via `hover` rather than `press`: `QuotaNotch` opens on `onPointerEnter`, and this is
     the one state of it nothing could photograph before — a click would land on whichever control
     sits under the pointer instead of on the wrapper the gesture actually needs. This is the
     provider names, the arcs read in full, and — if the sidecar has gone stale — the way back into
     the app, none of which the folded shot above shows at all. */
  ["61-notch-floating-unfolded", { path: "/teams", window: "notch", hover: ".quota-notch", viewport: { width: 640, height: 240 } }],
  /* And in light, for the reason several pairs above already are one: the unfolded notch draws its
     "last known" caption on `--text-faint`, and whether faint text over a transparent, borderless
     window still reads is a contrast question a dark shot alone cannot answer. */
  [
    "62-notch-floating-unfolded-light",
    { path: "/teams", window: "notch", hover: ".quota-notch", viewport: { width: 640, height: 240 }, theme: "light" },
  ],
  /* The other host, same component: `host="contained"` draws it at the top of an ordinary page
     (`AppShell.tsx`) rather than floating, and needs none of the options above — `contained` is
     always unfolded, so any page path already shows it. */
  ["63-notch-contained", { path: "/teams" }],
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
