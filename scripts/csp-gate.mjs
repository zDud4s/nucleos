/**
 * The gate that keeps `style-src 'self'` true.
 *
 * # Why this exists at all
 *
 * `shell/src-tauri/tauri.conf.json` carries TWO policies. `csp` is what a packaged NucleOS runs
 * under: `style-src 'self'`, no `unsafe-inline`. `devCsp` is what `tauri dev` runs under, and it
 * has `style-src 'unsafe-inline'` in it — necessarily, because Vite delivers CSS in development by
 * building a `<style>` element at runtime and `style-src-elem` governs that whether markup or a
 * script made it.
 *
 * So **development cannot catch a CSP style regression.** A dependency that locks scrolling by
 * injecting a `<style>`, or positions a popover by writing a `style=` attribute into markup, works
 * perfectly every day and is refused the first time somebody installs a build. `react-remove-scroll`
 * — which arrives with every Radix modal — is exactly that dependency, which is why it has a
 * surface of its own in the list.
 *
 * This runs the real bundle under the real policy and fails on the first refusal.
 *
 * # Why a browser and not a test runner
 *
 * A CSP is enforced by the engine, and jsdom does not enforce one. There is no way to observe this
 * property except in something that implements it — so this drives Chromium, which is the engine
 * WebView2 is. What it is NOT is the Tauri window: Tauri also injects nonces into tags it finds in
 * the HTML, which can only widen what is allowed, so a page that passes here passes there.
 *
 * # Why a positive control
 *
 * "No violations" and "no policy" look identical from inside a page. Every run ends by injecting a
 * `<style>` element and a `style` attribute that MUST be refused, and reports failure if they are
 * not — a gate that silently stopped enforcing anything would otherwise stay green for ever.
 *
 * Usage: node scripts/csp-gate.mjs
 */
import { createServer } from "node:http";
import { randomInt } from "node:crypto";
import { spawn } from "node:child_process";
import { readFile, mkdtemp, rm } from "node:fs/promises";
import { existsSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, extname, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SHELL = join(REPO, "shell");
const OUT = join(SHELL, "dist-csp-gate");

/** How long one surface gets to mount and exercise itself before it counts as broken. */
const SURFACE_TIMEOUT = 15_000;

/**
 * Chromium, by whichever name this machine has it.
 *
 * Edge is the fallback and not the first choice only because Chrome's headless mode is the one
 * more people run; both are the same engine, and Edge's is the same one the Tauri window uses.
 * Missing entirely is a FAILURE and not a skip — `scripts/gates.sh` treats a missing tool that way
 * everywhere else, and a gate that quietly passes when it could not run is worse than no gate.
 */
const BROWSERS = [
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
  "C:/Program Files (x86)/Google/Chrome/Application/chrome.exe",
  "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
  "C:/Program Files/Microsoft/Edge/Application/msedge.exe",
];

function die(message) {
  console.error("csp gate: " + message);
  process.exit(1);
}

const browser = BROWSERS.find((path) => existsSync(path));
if (browser === undefined) {
  die(
    "no Chromium found. Looked for Chrome and Edge in Program Files.\n" +
      "  This gate needs a browser that enforces a Content-Security-Policy; a test runner cannot.",
  );
}

/* ---------------------------------------------------------------- the build */

/*
  Built here rather than expected to exist, unlike `shell/dist` in gates.sh. A prebuilt artefact
  would let this pass against source somebody changed ten minutes ago, and the whole claim of the
  gate is that it measures what the current source produces.
*/
console.log("csp gate: building the gate bundle…");
// Vite's own entry through this Node, not `npx`: a `.cmd` shim needs a shell on Windows, and a
// gate that reaches for one gains a quoting bug and nothing else.
const build = spawn(
  process.execPath,
  [
    join(SHELL, "node_modules/vite/bin/vite.js"),
    "build",
    "--config",
    "csp-gate.vite.config.mjs",
    "--logLevel",
    "warn",
  ],
  { cwd: SHELL, stdio: "inherit" },
);
const buildCode = await new Promise((ok) => build.on("close", ok));
if (buildCode !== 0) die("the gate bundle did not build (vite exited " + buildCode + ")");
if (!existsSync(join(OUT, "csp-gate.html"))) {
  die("the build produced no csp-gate.html in " + OUT);
}

/* --------------------------------------------------- the policy, and a server */

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

/**
 * Stamp a per-load nonce onto every `<style>` that has none, and say so in the policy.
 *
 * **This is an emulation of Tauri, and it has to be an exact one or the gate lies.** Tauri does it
 * in two halves: `tauri-codegen` puts a placeholder on every `<style>` element when it embeds the
 * HTML, and `manager::set_csp` replaces each placeholder at serve time with a fresh `getrandom`
 * u64, adding `'nonce-<value>'` to the directive it sends. Composed, that is this function — same
 * element selector, same skip-if-already-stamped rule, same decimal shape, and the value reaching
 * the policy only because a tag claimed one.
 *
 * It cannot make the gate more permissive than the app: no `<style>` in the HTML means no nonce in
 * the header, so a stylesheet that arrives from anywhere else is refused exactly as it would be.
 */
function stampNonces(html) {
  const nonces = [];
  const stamped = html.replace(/<style(?![^>]*\snonce=)/g, () => {
    // Node caps `randomInt` at 2^48; the shape that matters is "decimal digits, unguessable",
    // which is what Tauri's u64 is once it reaches the markup.
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
  const file = url.pathname === "/" ? join(OUT, "csp-gate.html") : join(OUT, url.pathname);
  if (!file.startsWith(OUT) || !existsSync(file)) {
    response.statusCode = 404;
    response.end("not here");
    return;
  }
  const raw = await readFile(file);
  const html = extname(file) === ".html";
  const { html: body, policy } = html ? stampNonces(raw.toString("utf8")) : { policy: csp };
  // The header, not a <meta>: this is how Tauri applies it, and a <meta> cannot carry
  // `frame-ancestors` at all.
  response.setHeader("Content-Security-Policy", policy);
  response.setHeader("Content-Type", TYPES[extname(file)] ?? "application/octet-stream");
  response.end(html ? body : raw);
});
await new Promise((ok) => server.listen(0, "127.0.0.1", ok));
const origin = `http://127.0.0.1:${server.address().port}`;

/* ------------------------------------------------------------- the browser */

const profile = await mkdtemp(join(tmpdir(), "nucleos-csp-gate-"));
const chromium = spawn(browser, [
  "--headless=new",
  // Port 0 and read it back from the profile: a fixed port is a gate that fails when somebody has
  // devtools open on something else.
  "--remote-debugging-port=0",
  `--user-data-dir=${profile}`,
  "--no-first-run",
  "--no-default-browser-check",
  "--disable-gpu",
  "--window-size=1280,900",
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

/** Load a page and wait for the surface on it to say it has finished exercising itself. */
async function load(path) {
  pageErrors.length = 0;
  await send("Page.navigate", { url: origin + path }, sessionId);
  const deadline = Date.now() + SURFACE_TIMEOUT;
  for (;;) {
    const ready = await evaluate("window.__cspGate?.ready === true").catch(() => false);
    if (ready) return;
    if (Date.now() > deadline) {
      throw new Error(
        "never finished within " +
          SURFACE_TIMEOUT / 1000 +
          "s" +
          (pageErrors.length > 0 ? " — the page threw: " + pageErrors[0].split("\n")[0] : ""),
      );
    }
    await new Promise((ok) => setTimeout(ok, 100));
  }
}

const violations = () => evaluate("JSON.stringify(window.__cspGate.violations)").then(JSON.parse);

/* ------------------------------------------------------------------ the run */

console.log("csp gate: " + csp + "\n");

let failed = false;

await load("/");
const surfaces = await evaluate("JSON.stringify(window.__cspGate.surfaces)").then(JSON.parse);
if (surfaces.length === 0) die("the page lists no surfaces — src/csp-gate/surfaces.tsx is empty");

/*
  Checked before any surface runs, because without it every failure below would be reported against
  a library when the cause is one deleted line of HTML.
*/
const nonce = await evaluate("window.__cspGate.nonce");
if (nonce === null) {
  die(
    "the page found no style nonce. The <style> element in the HTML entry is the only channel for\n" +
      "  it — see shell/src/lib/style-nonce.ts. Without it Radix's dialogs are refused in a build.",
  );
}
console.log("ok   nonce — the document carries a per-load style nonce and handed it on\n");

for (const surface of surfaces) {
  let refusals;
  try {
    await load("/?surface=" + encodeURIComponent(surface.name));
    const mounted = await evaluate("window.__cspGate.mounted");
    if (mounted !== surface.name) {
      throw new Error("the page mounted " + JSON.stringify(mounted) + " instead");
    }
    refusals = await violations();
  } catch (error) {
    console.log(`FAIL ${surface.name} — ${error.message}`);
    console.log(`       ${surface.why}`);
    failed = true;
    continue;
  }
  if (refusals.length === 0) {
    console.log(`ok   ${surface.name} — ${surface.why}`);
    continue;
  }
  failed = true;
  console.log(`FAIL ${surface.name} — ${surface.why}`);
  for (const refusal of refusals) {
    console.log(`       ${refusal.directive} refused ${refusal.blocked} at ${refusal.where}`);
  }
}

/*
  The control, last and on a page that has already reported clean. Two refusals and not one,
  because `style-src-elem` and `style-src-attr` are separate directives and a policy could lose
  either — an app whose popovers are refused while its stylesheets are fine is precisely the
  half-failure a single probe would miss.
*/
const control = await evaluate(`
  new Promise((ok) => {
    const before = window.__cspGate.violations.length;
    const style = document.createElement("style");
    style.textContent = ".csp-gate-control { color: red }";
    document.head.appendChild(style);
    const marked = document.createElement("div");
    marked.setAttribute("style", "color: green");
    document.body.appendChild(marked);
    setTimeout(() => ok(JSON.stringify(
      window.__cspGate.violations.slice(before).map((v) => v.directive)
    )), 400);
  })
`).then(JSON.parse);

const sawElement = control.some((directive) => directive.startsWith("style-src-elem"));
const sawAttribute = control.some((directive) => directive.startsWith("style-src-attr"));
if (sawElement && sawAttribute) {
  console.log("ok   control — a <style> element and a style attribute were both refused");
} else {
  failed = true;
  console.log(
    "FAIL control — the policy did not refuse what it must: saw " +
      (control.length > 0 ? control.join(", ") : "nothing") +
      "\n       Every ok above is meaningless until this passes.",
  );
}

socket.close();
chromium.kill();
server.close();
await rm(profile, { recursive: true, force: true }).catch(() => {});

if (failed) {
  console.error(
    "\ncsp gate: FAILED. The app would run under `style-src 'self'` and be refused.\n" +
      "  `tauri dev` will not reproduce this — it runs `devCsp`, which allows inline style.",
  );
  process.exit(1);
}
console.log("\ncsp gate: green.");
process.exit(0);
