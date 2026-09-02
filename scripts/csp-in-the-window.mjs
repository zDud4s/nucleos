/**
 * The Content-Security-Policy, measured inside the window that ships.
 *
 * # The gap this closes
 *
 * `scripts/csp-gate.mjs` runs every style-writing library in this app under the production policy
 * and fails on the first refusal. It runs them in Chromium, over `http://127.0.0.1`, under a policy
 * it composes from `tauri.conf.json` and a nonce it substitutes itself. Its header says, of the
 * difference:
 *
 * > What it is NOT is the Tauri window: Tauri also injects nonces into tags it finds in the HTML,
 * > which can only widen what is allowed, so a page that passes here passes there.
 *
 * Every word of that is an **argument**, and the app's whole style pipeline rests on it. Tauri
 * stamps `nonce="__TAURI_STYLE_NONCE__"` on every `<style>` at compile time and replaces it per
 * page load, and it is supposed to add `'nonce-<value>'` to the `style-src` it sends. `index.html`
 * carries an empty `<style></style>` for no other purpose than to be that channel, and
 * `src/lib/style-nonce.ts` reads the value back out and hands it to `get-nonce` so that
 * `react-remove-scroll` — which arrives with every Radix modal — can sign what it injects.
 *
 * If any link in that chain is not true in a real WebView2 window, the gate is green and the app
 * is broken, and nothing anybody runs day to day would say so. **This measures the chain.**
 *
 * # Why it is not in `scripts/gates.sh`
 *
 * It builds the application — minutes, not seconds — so it cannot sit on the path between somebody
 * writing a line and committing it. It is the thing to run when the answer might have changed: a
 * Tauri or wry upgrade, a change to `index.html`, a change to the `csp` block, or a doubt.
 *
 * # How a window that ships no debugger is watched anyway
 *
 * WebView2 opens a CDP endpoint when its browser process is given `--remote-debugging-port`, and
 * the documented way to pass one is the `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` environment
 * variable. **It does not work here, and the reason is worth writing down** — an hour goes into
 * rediscovering it. wry builds its own argument string and always calls
 * `set_additional_browser_arguments` (`wry-0.55.1/src/webview2/mod.rs:294`, `:327`), defaulting to
 * `--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection`; WebView2 reads the environment
 * variable only when nothing set that programmatically. The app launches, paints, and never answers
 * the port — with no error anywhere, in debug or release alike.
 *
 * So the port goes in the same way the app's own arguments do: `additionalBrowserArgs` on the
 * window, merged in at build time through the CLI's `--config`. wry's defaults are repeated in it
 * because the setting REPLACES them rather than adding to them, and the rest of the window's
 * configuration is copied across from `tauri.conf.json` because a `--config` merge replaces an
 * array whole and would otherwise drop the window's label, title and size.
 *
 * **And a changed `identifier` beside it, which is not optional.** WebView2 runs ONE browser
 * process per user data folder and shares it between host processes, so a second NucleOS started
 * while the app is already running joins the browser process that is already there — arguments and
 * all — and the ones asked for here are dropped without a word. Tauri's own documentation says the
 * rule in one line: *WebViews with different values for settings like `additionalBrowserArgs` must
 * have different data directories.*
 *
 * Which is what the `dataDirectory` field is for, **and it does not work**. Tauri resolves it in
 * `WebviewBuilder::from_config` and then drops it on the floor: `impl From<&WindowConfig> for
 * WebviewAttributes` (`tauri-runtime-2.11/src/webview.rs:448`) copies `additional_browser_args` and
 * never copies `data_directory`, so a window declared in configuration cannot set one. The folder
 * it names is never created and the shared profile is used anyway. Two hours, and worth the
 * sentence.
 *
 * The lever that does work is one level up: on Windows the folder is
 * `%LOCALAPPDATA%/<identifier>` (`tauri-2.11/src/manager/webview.rs:537`), so overriding the
 * identifier gives the probe a profile of its own and a browser process of its own with it. It
 * changes where this build keeps its data and nothing else — no bundle is produced and nothing
 * reads the identifier back.
 *
 * What that leaves is a binary identical to the shipped one except for the string handed to the
 * browser process and the folder its profile sits in. The policy, the nonce substitution,
 * `index.html` and the bundle are the same bytes. This adds an observer; it does not change the
 * subject.
 *
 * # Why a `--debug` build and not the release one
 *
 * `tauri-codegen` picks `devCsp` when its `dev` flag is set, and `tauri-macros` sets that from
 * `cfg!(not(feature = "custom-protocol"))`. Any `tauri build` turns that feature on, `--debug`
 * included — so a debug build serves the PRODUCTION `csp`, exactly like the installer's. It is
 * `tauri dev` that serves `devCsp`, and that is the run that can prove nothing.
 *
 * Debug also leaves devtools enabled, and wry gates them with `SetAreDevToolsEnabled`
 * (`webview2/mod.rs:573`) — whether WebView2 would still answer a debugging port with that off is
 * untested here, and `--debug` costs nothing that matters, since the release binary differs from it
 * in optimisation and not in what is served.
 *
 * # Why the control comes first here, and last in the gate
 *
 * In the gate, the control is last because the surfaces before it are the point. Here the control
 * IS the point: it is what tells the two policies apart. Under `devCsp` an unsigned `<style>` is
 * allowed, so a run that finds no refusal has either measured a correct app or measured the wrong
 * policy, and those must never be reported the same way. A run that cannot refuse says so and
 * fails, rather than printing a green it did not earn.
 *
 * Usage: node scripts/csp-in-the-window.mjs
 */
import { spawn } from "node:child_process";
import { readFile, writeFile, mkdtemp } from "node:fs/promises";
import { existsSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { ownUntilExit } from "./leave-nothing-behind.mjs";

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SHELL = join(REPO, "shell");
const CONF = join(SHELL, "src-tauri/tauri.conf.json");
const EXE = join(SHELL, "src-tauri/target/debug/shell.exe");

/** Long, because this waits on a whole application starting, not on a page. */
const ATTACH_TIMEOUT = 60_000;
const MOUNT_TIMEOUT = 30_000;
const PORT = 9333;
/** wry's own defaults, which `additionalBrowserArgs` replaces rather than extends. */
const WRY_DEFAULTS = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection";

function die(message) {
  console.error("csp window: " + message);
  process.exit(1);
}

/* ------------------------------------------------------------------ the build */

const conf = JSON.parse(await readFile(CONF, "utf8"));
const windows = conf.app?.windows ?? [];
if (windows.length === 0) die("tauri.conf.json declares no window to open");

const scratch = await mkdtemp(join(tmpdir(), "nucleos-csp-window-"));
const overridePath = join(scratch, "watched.conf.json");
await writeFile(
  overridePath,
  JSON.stringify(
    {
      // A profile of this run's own, which is the only way the arguments above survive an app that
      // is already running. See the header — the `dataDirectory` field looks like the answer and
      // is not one.
      identifier: conf.identifier + ".cspwindow",
      app: {
        windows: windows.map((window, index) =>
          index === 0
            ? {
                ...window,
                additionalBrowserArgs: `${WRY_DEFAULTS} --remote-debugging-port=${PORT}`,
              }
            : window,
        ),
      },
    },
    null,
    2,
  ),
);

console.log("csp window: building the application (minutes, not seconds)…");
const build = spawn(
  process.execPath,
  [
    join(SHELL, "node_modules/@tauri-apps/cli/tauri.js"),
    "build",
    "--debug",
    "--no-bundle",
    "--config",
    overridePath,
  ],
  { cwd: SHELL, stdio: "inherit" },
);
const buildCode = await new Promise((ok) => build.on("close", ok));
if (buildCode !== 0) die("the application did not build (tauri exited " + buildCode + ")");
if (!existsSync(EXE)) die("the build produced no executable at " + EXE);
console.log("\ncsp window: " + EXE);
console.log("            built " + statSync(EXE).mtime.toISOString() + "\n");

/* ---------------------------------------------------------------- the window */

const app = spawn(EXE, [], { stdio: "ignore" });
/*
  Owned rather than stopped at the end. Every `die()` below used to leave the
  window running, and `app.kill()` would not have been enough even where it was
  called: the WebView2 processes are the application's children, so killing only
  the root orphans them under a name nobody can tell from the system's own.
*/
ownUntilExit(app, scratch);
let appAlive = true;
app.on("exit", () => {
  appAlive = false;
});

async function attach() {
  const deadline = Date.now() + ATTACH_TIMEOUT;
  while (Date.now() < deadline) {
    if (!appAlive) throw new Error("the application exited before it opened a debugging port");
    try {
      const targets = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json();
      const page = targets.find((target) => target.type === "page" && target.webSocketDebuggerUrl);
      if (page !== undefined) return page;
    } catch {
      /* the port answers a moment after the process starts */
    }
    await new Promise((ok) => setTimeout(ok, 500));
  }
  throw new Error(
    "nothing answered the debugging port in " +
      ATTACH_TIMEOUT / 1000 +
      "s.\n  Either the build did not take `additionalBrowserArgs`, or this window joined a WebView2\n" +
      "  browser process that was already running under the same profile and inherited ITS\n" +
      "  arguments. Both look exactly like this. See this file's header.",
  );
}

let page;
try {
  page = await attach();
} catch (error) {
  die(error.message);
}
console.log("ok   attached");

const socket = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((ok, no) => {
  socket.onopen = ok;
  socket.onerror = () => no(new Error("could not attach to the window"));
});

let nextId = 0;
const pending = new Map();
/** The document's own response headers, which is where a CSP actually arrives. */
let documentHeaders = null;
let mainFrameRequest = null;

socket.onmessage = (message) => {
  const frame = JSON.parse(message.data);
  if (frame.id !== undefined) {
    pending.get(frame.id)?.(frame);
    pending.delete(frame.id);
    return;
  }
  if (frame.method === "Network.requestWillBeSent" && frame.params.type === "Document") {
    mainFrameRequest = frame.params.requestId;
  }
  if (frame.method === "Network.responseReceived" && frame.params.requestId === mainFrameRequest) {
    documentHeaders = frame.params.response.headers;
  }
};

function send(method, params = {}) {
  const id = ++nextId;
  return new Promise((ok, no) => {
    pending.set(id, (frame) =>
      frame.error ? no(new Error(method + ": " + JSON.stringify(frame.error))) : ok(frame.result),
    );
    socket.send(JSON.stringify({ id, method, params }));
  });
}

async function evaluate(expression) {
  const result = await send("Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
  });
  if (result.exceptionDetails) {
    throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
  }
  return result.result.value;
}

await send("Runtime.enable");
await send("Page.enable");
await send("Network.enable");

/*
  Installed before the document exists and then the page is reloaded, because a listener added
  afterwards would miss every refusal the app's own boot caused — which is the half of the run that
  is about the app rather than about the plumbing.
*/
await send("Page.addScriptToEvaluateOnNewDocument", {
  source: `
    window.__cspWindow = { violations: [] };
    window.addEventListener("securitypolicyviolation", (event) => {
      window.__cspWindow.violations.push({
        directive: event.effectiveDirective,
        blocked: event.blockedURI,
        sample: (event.sample || "").slice(0, 80),
        where: (event.sourceFile || "") + ":" + event.lineNumber,
      });
    });
  `,
});
await send("Page.reload", { ignoreCache: true });

/* ------------------------------------------------------------------ the boot */

let failed = false;
function report(ok, name, detail) {
  console.log((ok ? "ok   " : "FAIL ") + name + " — " + detail);
  if (!ok) failed = true;
}

async function waitForMount() {
  const deadline = Date.now() + MOUNT_TIMEOUT;
  while (Date.now() < deadline) {
    const mounted = await evaluate("document.getElementById('root')?.childElementCount > 0").catch(
      () => false,
    );
    if (mounted) return true;
    await new Promise((ok) => setTimeout(ok, 250));
  }
  return false;
}

if (!(await waitForMount())) {
  die("the application never rendered anything into #root within " + MOUNT_TIMEOUT / 1000 + "s");
}
// A moment past first paint: the shell's own queries land after it, and a refusal caused by one of
// them would otherwise be counted against a run that had already finished looking.
await new Promise((ok) => setTimeout(ok, 2500));

/*
  Printed rather than asserted on, and printed as ORIGIN because that is the fact the gate's
  disclaimer is about: the gate serves over `http://127.0.0.1`, and this is the custom protocol.
  The policy beside it comes off the document's own response headers, which is where a CSP actually
  arrives — the doubled `'self'` in it is Tauri appending to what the configuration already said,
  and is not a fault.
*/
console.log("     document: " + (await evaluate("location.href")));
const served =
  documentHeaders?.["Content-Security-Policy"] ?? documentHeaders?.["content-security-policy"];
if (served !== undefined) {
  const styleSrc = served
    .split(";")
    .map((part) => part.trim())
    .find((part) => part.startsWith("style-src"));
  console.log("     style-src: " + (styleSrc ?? "(the policy names no style-src)"));
}

/* --------------------------------------------------------------- the control */

/*
  Two refusals and not one: `style-src-elem` and `style-src-attr` are separate directives, and an
  app whose popovers are refused while its stylesheets are fine is exactly the half-failure a single
  probe cannot see. Under `devCsp` neither of these is refused, which is the check that this ran
  against the policy it claims to have run against.
*/
const control = await evaluate(`
  new Promise((ok) => {
    const before = window.__cspWindow.violations.length;
    const style = document.createElement("style");
    style.textContent = ".csp-window-control { color: red }";
    document.head.appendChild(style);
    const marked = document.createElement("div");
    marked.setAttribute("style", "color: green");
    document.body.appendChild(marked);
    setTimeout(() => {
      const seen = window.__cspWindow.violations.splice(before).map((v) => v.directive);
      style.remove();
      marked.remove();
      ok(JSON.stringify(seen));
    }, 500);
  })
`).then(JSON.parse);

const refusedElement = control.some((directive) => directive.startsWith("style-src-elem"));
const refusedAttribute = control.some((directive) => directive.startsWith("style-src-attr"));
if (refusedElement && refusedAttribute) {
  report(true, "control", "an unsigned <style> and a style attribute were both refused");
} else {
  report(
    false,
    "control",
    "the window refused " +
      (control.length > 0 ? control.join(", ") : "nothing") +
      "\n       This is what `devCsp` looks like, and under it NOTHING below means anything." +
      "\n       Check the build was `tauri build --debug`, not `tauri dev`.",
  );
}

/* ----------------------------------------------------------------- the nonce */

const nonce = await evaluate(`
  (() => {
    const stamped = document.querySelector("style[nonce]");
    if (stamped === null) return null;
    return stamped.nonce || stamped.getAttribute("nonce") || null;
  })()
`);

if (typeof nonce === "string" && nonce.length > 0) {
  report(true, "nonce", "the document carries a per-load style nonce (" + nonce.length + " chars)");
} else {
  report(
    false,
    "nonce",
    "no <style> in the document carries a nonce.\n" +
      "       index.html's empty <style></style> is the only channel for it — see src/lib/style-nonce.ts.\n" +
      "       Reading it with getAttribute alone also looks like this: a document under a\n" +
      "       nonce-carrying policy has its nonce ATTRIBUTE blanked, and only the IDL property kept.",
  );
}

/*
  **The link nothing else can check.** A nonce in the markup is worth nothing unless the same value
  is in the policy that was SENT, and the two are produced by different halves of Tauri — the
  codegen stamps the tag, `manager::set_csp` fills the header. This signs a style with the value the
  document handed out and asks the engine whether it is now allowed.
*/
if (typeof nonce === "string" && nonce.length > 0) {
  const signed = await evaluate(`
    new Promise((ok) => {
      const before = window.__cspWindow.violations.length;
      const style = document.createElement("style");
      style.setAttribute("nonce", ${JSON.stringify(nonce)});
      style.nonce = ${JSON.stringify(nonce)};
      style.textContent = ".csp-window-signed { color: blue }";
      document.head.appendChild(style);
      setTimeout(() => {
        const refused = window.__cspWindow.violations.splice(before).length > 0;
        // Whether the rule actually took, which a refusal would have prevented.
        const applied = [...document.styleSheets].some((sheet) => {
          try {
            return [...sheet.cssRules].some((rule) => rule.selectorText === ".csp-window-signed");
          } catch { return false; }
        });
        style.remove();
        ok(JSON.stringify({ refused, applied }));
      }, 400);
    })
  `).then(JSON.parse);

  if (!signed.refused && signed.applied) {
    report(true, "nonce in policy", "a <style> signed with that nonce was accepted and applied");
  } else {
    report(
      false,
      "nonce in policy",
      "a <style> signed with the document's own nonce was " +
        (signed.refused ? "REFUSED" : "accepted but never applied") +
        ".\n       The tag carries a nonce the sent policy does not list. Every Radix modal in this\n" +
        "       app injects a <style> through react-remove-scroll and would be refused.",
    );
  }
}

/* -------------------------------------------------------------- the app itself */

const bootViolations = await evaluate("JSON.stringify(window.__cspWindow.violations)").then(
  JSON.parse,
);
if (bootViolations.length === 0) {
  report(true, "boot", "the app started and rendered with no refusal");
} else {
  report(false, "boot", bootViolations.length + " refusal(s) while starting:");
  for (const refusal of bootViolations) {
    console.log(`       ${refusal.directive} refused ${refusal.blocked} at ${refusal.where}`);
    if (refusal.sample !== "") console.log(`         sample: ${refusal.sample}`);
  }
}

/*
  A few routes past the first, driven through the router's own history rather than by loading URLs:
  the asset protocol serves files, and a deep path is not one. What this buys is the pages that
  mount the heavy libraries under the real policy — `/fleet` is here because it is `FleetCanvas`,
  the xyflow surface the gate deliberately leaves out on the grounds that the workflow canvas
  already covers that library. That reasoning is about libraries; this is about the app.

  How many of them actually arrived is printed rather than assumed: a route that never mounted
  cannot refuse anything, and a silent zero would read exactly like a clean sweep.
*/
const ROUTES = ["/fleet", "/projects", "/runs", "/chats", "/waiting", "/"];
const walked = await evaluate(`
  (async () => {
    const visited = [];
    for (const path of ${JSON.stringify(ROUTES)}) {
      window.history.pushState({}, "", path);
      window.dispatchEvent(new PopStateEvent("popstate"));
      await new Promise((ok) => setTimeout(ok, 1500));
      visited.push({ path, at: window.location.pathname });
    }
    return JSON.stringify({ visited, violations: window.__cspWindow.violations });
  })()
`).then(JSON.parse);

const arrived = walked.visited.filter((step) => step.at === step.path);
const routeViolations = walked.violations.slice(bootViolations.length);
if (routeViolations.length === 0) {
  report(
    true,
    "routes",
    arrived.length + " of " + ROUTES.length + " routes mounted, with no refusal on any of them",
  );
} else {
  report(false, "routes", routeViolations.length + " refusal(s) while navigating:");
  for (const refusal of routeViolations) {
    console.log(`       ${refusal.directive} refused ${refusal.blocked} at ${refusal.where}`);
    if (refusal.sample !== "") console.log(`         sample: ${refusal.sample}`);
  }
}

/* ------------------------------------------------------------------ the end */

socket.close();

if (failed) {
  console.error("\ncsp window: FAILED. What ships is not what the gate measured.");
  process.exit(1);
}
console.log("\ncsp window: green. The policy the gate composes is the policy the window enforces.");
process.exit(0);
