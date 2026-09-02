/**
 * What the preview page actually said.
 *
 * `Runtime.exceptionThrown` does not fire for anything a React error boundary
 * catches, which is precisely the case that renders "Something went wrong!" —
 * so a harness watching only that reports a clean run over a broken page. This
 * listens to the console as well and prints the rendered text.
 *
 * Reuses the bundle `preview-shots.mjs` already built; run that first.
 */

import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { existsSync, readFileSync } from "node:fs";
import { mkdtemp, readFile } from "node:fs/promises";
import { randomInt } from "node:crypto";
import { tmpdir } from "node:os";
import { dirname, extname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SHELL = join(ROOT, "shell");
const OUT = join(SHELL, "dist-preview");
const PATH_UNDER_TEST = process.argv[2] ?? "/teams";
/**
 * Buttons to press before reading the page, `|`-separated and pressed in order.
 *
 * Without it this tool can only ever report the surface a route loads with, and the ones that go
 * wrong are usually the ones behind a click — `node scripts/preview-why.mjs /fleet "Show
 * items|As a graph"`. `preview/main.tsx` does the pressing; this only forwards it.
 */
const PRESS = process.argv[3];

const BROWSERS = [
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
  "C:/Program Files (x86)/Google/Chrome/Application/chrome.exe",
  "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
  "C:/Program Files/Microsoft/Edge/Application/msedge.exe",
];
const browser = BROWSERS.find((path) => existsSync(path));

const conf = JSON.parse(await readFile(join(SHELL, "src-tauri/tauri.conf.json"), "utf8"));
const csp = Object.entries(conf.app.security.csp)
  .map(([directive, value]) => `${directive} ${value}`)
  .join("; ");

const TYPES = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".woff2": "font/woff2" };

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
    .map((d) => (d.startsWith("style-src ") ? d + " " + nonces.map((n) => `'nonce-${n}'`).join(" ") : d))
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

const profile = await mkdtemp(join(tmpdir(), "nucleos-why-"));
const chromium = spawn(browser, [
  "--headless=new",
  "--remote-debugging-port=0",
  `--user-data-dir=${profile}`,
  "--no-first-run",
  "--disable-gpu",
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
  socket.onerror = () => no(new Error("could not attach"));
});

let nextId = 0;
const pending = new Map();
const said = [];

socket.onmessage = (message) => {
  const frame = JSON.parse(message.data);
  if (frame.id !== undefined) {
    pending.get(frame.id)?.(frame);
    pending.delete(frame.id);
    return;
  }
  if (frame.method === "Runtime.consoleAPICalled") {
    said.push(
      frame.params.type +
        ": " +
        frame.params.args.map((a) => a.description ?? JSON.stringify(a.value)).join(" "),
    );
  }
  if (frame.method === "Runtime.exceptionThrown") {
    const d = frame.params.exceptionDetails;
    said.push("threw: " + (d.exception?.description ?? d.text));
  }
};

function send(method, params = {}, sessionId) {
  const id = ++nextId;
  return new Promise((ok, no) => {
    pending.set(id, (f) => (f.error ? no(new Error(method + ": " + JSON.stringify(f.error))) : ok(f.result)));
    socket.send(JSON.stringify({ id, method, params, sessionId }));
  });
}

const { targetId } = await send("Target.createTarget", { url: "about:blank" });
const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
await send("Runtime.enable", {}, sessionId);
await send("Page.enable", {}, sessionId);

const query =
  `path=${encodeURIComponent(PATH_UNDER_TEST)}` +
  (PRESS === undefined ? "" : `&press=${encodeURIComponent(PRESS)}`);
await send("Page.navigate", { url: `${origin}/preview.html?${query}` }, sessionId);
await new Promise((ok) => setTimeout(ok, 6000));

const text = await send(
  "Runtime.evaluate",
  { expression: "document.body.innerText.slice(0, 3000)", returnByValue: true },
  sessionId,
);

console.log("=== what the console said ===");
for (const line of said.slice(0, 40)) console.log("  " + line.split("\n").slice(0, 3).join("\n    "));
console.log("\n=== what the page rendered ===");
console.log(text.result.value);

chromium.kill();
server.close();
process.exit(0);
