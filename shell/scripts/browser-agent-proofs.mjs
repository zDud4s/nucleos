// Security proofs owed from the WebView2 CDP spike, section (c):
//   1. a web page loaded in an owner tab cannot reach the CDP websocket, and
//   2. a rebinding-style Host header cannot reach /json/*.
//
// Run: node shell/scripts/browser-agent-proofs.mjs
//
// Prerequisites: Windows, Microsoft Edge or the WebView2 runtime, Node >= 18.
// No cargo, no npm dependencies. Set BROWSER_EXE to override browser discovery.
// Exit code 0 only if both proofs pass (or the browser is missing: SKIP).

import http from "node:http";
import net from "node:net";
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function findBrowser() {
  if (process.env.BROWSER_EXE && fs.existsSync(process.env.BROWSER_EXE)) {
    return process.env.BROWSER_EXE;
  }
  const pf86 = process.env["ProgramFiles(x86)"] || "C:\\Program Files (x86)";
  const pf = process.env.ProgramFiles || "C:\\Program Files";
  for (const base of [pf86, pf]) {
    const p = path.join(base, "Microsoft", "Edge", "Application", "msedge.exe");
    if (fs.existsSync(p)) return p;
  }
  for (const base of [pf86, pf]) {
    const root = path.join(base, "Microsoft", "EdgeWebView", "Application");
    try {
      const vers = fs
        .readdirSync(root)
        .filter((d) => fs.existsSync(path.join(root, d, "msedgewebview2.exe")))
        .sort()
        .reverse();
      if (vers.length) return path.join(root, vers[0], "msedgewebview2.exe");
    } catch {
      /* directory missing */
    }
  }
  return null;
}

function freePort() {
  return new Promise((resolve, reject) => {
    const s = net.createServer();
    s.once("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address();
      s.close(() => resolve(port));
    });
  });
}

// Raw GET with a Host header override. Never rejects: socket errors yield status 0.
function rawGet({ port, path: p, host, method = "GET" }) {
  return new Promise((resolve) => {
    let done = false;
    const finish = (v) => {
      if (!done) {
        done = true;
        resolve(v);
      }
    };
    const req = http.request(
      { host: "127.0.0.1", port, path: p, method, headers: { Host: host }, agent: false },
      (res) => {
        const chunks = [];
        res.on("data", (c) => chunks.push(c));
        res.on("end", () =>
          finish({ status: res.statusCode, body: Buffer.concat(chunks).toString("utf8") }),
        );
        res.on("error", (e) => finish({ status: 0, body: "", error: String(e) }));
      },
    );
    req.setTimeout(5000, () => {
      req.destroy();
      finish({ status: 0, body: "", error: "timeout" });
    });
    req.on("error", (e) => finish({ status: 0, body: "", error: String(e) }));
    req.end();
  });
}

// Attempt a websocket upgrade with a given Origin. Resolves with the HTTP status (101 on success) or 0.
function wsUpgrade(wsUrl, origin) {
  return new Promise((resolve) => {
    const u = new URL(wsUrl);
    let done = false;
    const finish = (v) => {
      if (!done) {
        done = true;
        resolve(v);
      }
    };
    const req = http.request({
      host: u.hostname,
      port: u.port,
      path: u.pathname + u.search,
      agent: false,
      headers: {
        Connection: "Upgrade",
        Upgrade: "websocket",
        "Sec-WebSocket-Version": "13",
        "Sec-WebSocket-Key": crypto.randomBytes(16).toString("base64"),
        Origin: origin,
      },
    });
    req.setTimeout(5000, () => {
      req.destroy();
      finish({ status: 0, error: "timeout" });
    });
    req.on("upgrade", (res, socket) => {
      socket.destroy();
      finish({ status: res.statusCode });
    });
    req.on("response", (res) => {
      res.resume();
      finish({ status: res.statusCode });
    });
    req.on("error", (e) => finish({ status: 0, error: String(e) }));
    req.end();
  });
}

async function waitForVersion(port, ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const r = await rawGet({ port, path: "/json/version", host: `127.0.0.1:${port}` });
    if (r.status === 200) {
      try {
        const j = JSON.parse(r.body);
        if (j.webSocketDebuggerUrl) return j.webSocketDebuggerUrl;
      } catch {
        /* retry */
      }
    }
    await sleep(300);
  }
  return null;
}

function killTree(child) {
  if (!child || child.pid == null) return;
  try {
    if (process.platform === "win32") {
      spawnSync("taskkill", ["/T", "/F", "/PID", String(child.pid)], { stdio: "ignore" });
    } else {
      child.kill("SIGKILL");
    }
  } catch {
    /* already gone */
  }
}

function sleepSync(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function removeDir(dir) {
  for (let i = 0; i < 6; i++) {
    try {
      fs.rmSync(dir, { recursive: true, force: true });
      return;
    } catch {
      sleepSync(500);
    }
  }
}

// Shared, idempotent, synchronous cleanup: used by main's finally, the global
// guard and the signal handlers, always before process.exit.
const cleanupState = { child: null, dir: null, done: false };
function cleanupSync() {
  if (cleanupState.done) return;
  cleanupState.done = true;
  killTree(cleanupState.child);
  sleepSync(500);
  if (cleanupState.dir) removeDir(cleanupState.dir);
}

async function proofHost(port) {
  const detail = [];
  let ok = true;
  const control = await rawGet({ port, path: "/json/version", host: `127.0.0.1:${port}` });
  const controlOk = control.status === 200 && control.body.includes("webSocketDebuggerUrl");
  detail.push(`control Host 127.0.0.1:${port} -> status ${control.status} (${controlOk ? "ok" : "BAD"})`);
  if (!controlOk) ok = false;
  for (const host of ["evil.example", `evil.example:${port}`]) {
    for (const p of ["/json/version", "/json/list"]) {
      const r = await rawGet({ port, path: p, host });
      const harmless = r.status !== 200 || !r.body.includes("webSocketDebuggerUrl");
      detail.push(`attack Host ${host} ${p} -> status ${r.status} (${harmless ? "harmless" : "LEAKED"})`);
      if (!harmless) ok = false;
    }
  }
  return { ok, detail };
}

async function proofOrigin(port, wsUrl) {
  const detail = [];
  const good = await wsUpgrade(wsUrl, "http://127.0.0.1:1");
  const controlOk = good.status === 101;
  detail.push(`control Origin http://127.0.0.1:1 -> ${good.status} (${controlOk ? "connected" : "NOT connected"})`);
  const bad = await wsUpgrade(wsUrl, "http://evil.test");
  const badOk = bad.status !== 101;
  detail.push(`raw Origin http://evil.test -> ${bad.status} (${badOk ? "rejected" : "ACCEPTED"})`);

  const reports = [];
  let firstReport = null;
  let resolveFirst;
  const first = new Promise((r) => (resolveFirst = r));
  // Random nonce: only the page we served knows the report path.
  const nonce = crypto.randomBytes(16).toString("hex");
  const reportPath = `/report/${nonce}`;
  const html = `<!doctype html><meta charset="utf-8"><script>
const report = (s) => fetch(${JSON.stringify(reportPath)}, { method: 'POST', body: s }).catch(() => {});
let seen = false;
try {
  const ws = new WebSocket(${JSON.stringify(wsUrl)});
  ws.onopen = () => { seen = true; report('open'); };
  ws.onerror = () => { seen = true; report('error'); };
  ws.onclose = () => { seen = true; report('close'); };
} catch (e) { seen = true; report('error'); }
setTimeout(() => { if (!seen) report('timeout'); }, 8000);
</script>`;
  const server = http.createServer((req, res) => {
    if (req.method === "POST" && req.url.startsWith("/report")) {
      if (req.url !== reportPath) {
        // Report without the nonce: ignore it.
        req.resume();
        res.writeHead(403).end();
        return;
      }
      let b = "";
      req.on("data", (c) => (b += c));
      req.on("end", () => {
        reports.push(b);
        if (firstReport === null) {
          firstReport = b;
          resolveFirst(b);
        }
        res.writeHead(204).end();
      });
    } else {
      res.writeHead(200, { "Content-Type": "text/html" }).end(html);
    }
  });
  const p2 = await new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(server.address().port)));
  try {
    const target = `/json/new?http://127.0.0.1:${p2}/`;
    let r = await rawGet({ port, path: target, host: `127.0.0.1:${port}`, method: "PUT" });
    if (r.status < 200 || r.status >= 300) {
      r = await rawGet({ port, path: target, host: `127.0.0.1:${port}`, method: "GET" });
    }
    detail.push(`open page target -> status ${r.status}`);
    if (r.status < 200 || r.status >= 300) {
      detail.push("could not open owner page target");
      return { ok: false, detail };
    }
    const got = await Promise.race([first, sleep(15000).then(() => null)]);
    if (got === null) {
      detail.push("no report from the page within 15s: inconclusive (vacuous), counted as FAIL");
      return { ok: false, detail };
    }
    await sleep(500);
    detail.push(`page reports: ${reports.join(", ")}`);
    const everOpen = reports.includes("open");
    const firstOk = got === "error" || got === "close";
    if (everOpen) detail.push("page websocket reached 'open'");
    return { ok: controlOk && badOk && firstOk && !everOpen, detail };
  } finally {
    server.closeAllConnections?.();
    server.close();
  }
}

async function main() {
  const exe = findBrowser();
  if (!exe) {
    console.log("SKIP: no msedge/WebView2 runtime found");
    return 0;
  }
  console.log(`browser: ${exe}`);
  const port = await freePort();
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "browser-agent-proofs-"));
  cleanupState.dir = dir;
  let child = null;
  let results = [];
  try {
    child = cleanupState.child = spawn(
      exe,
      [
        "--headless=new",
        `--remote-debugging-port=${port}`,
        "--remote-allow-origins=http://127.0.0.1:1",
        `--user-data-dir=${dir}`,
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-extensions",
        "about:blank",
      ],
      { stdio: "ignore", windowsHide: true },
    );
    child.on("error", () => {});
    const wsUrl = await waitForVersion(port, 20000);
    if (!wsUrl) {
      const msg = ["browser did not expose /json/version within 20s"];
      results = [
        { name: "PROOF 1 (page origin cannot open CDP websocket)", ok: false, detail: msg },
        { name: "PROOF 2 (rebinding Host header cannot reach /json/*)", ok: false, detail: msg },
      ];
    } else {
      const p1 = await proofOrigin(port, wsUrl);
      const p2 = await proofHost(port);
      results = [
        { name: "PROOF 1 (page origin cannot open CDP websocket)", ...p1 },
        { name: "PROOF 2 (rebinding Host header cannot reach /json/*)", ...p2 },
      ];
    }
  } finally {
    cleanupSync();
  }
  for (const r of results) {
    console.log(`${r.name}: ${r.ok ? "PASS" : "FAIL"}`);
    for (const d of r.detail) console.log(`  ${d}`);
  }
  const all = results.every((r) => r.ok);
  console.log(all ? "SUMMARY: both proofs PASS" : "SUMMARY: at least one proof FAILED");
  return all ? 0 : 1;
}

const guard = setTimeout(() => {
  console.log("FAIL: global 60s timeout");
  cleanupSync();
  process.exit(1);
}, 60000);

for (const sig of ["SIGINT", "SIGTERM"]) {
  process.on(sig, () => {
    console.log(`FAIL: interrupted by ${sig}`);
    cleanupSync();
    process.exit(1);
  });
}

main().then(
  (code) => {
    clearTimeout(guard);
    process.exit(code);
  },
  (e) => {
    console.log(`FAIL: ${e && e.stack ? e.stack : e}`);
    process.exit(1);
  },
);
