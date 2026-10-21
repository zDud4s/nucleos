import type { Capabilities, FleetEvent, FleetNode, NodeMetrics, TermKind, TermLine, TranscriptPage } from "../fleet/types";
import { NOW } from "./daemon";

/**
 * A fleet made of fixtures, for `node scripts/preview-why.mjs /fleet`. Mirrors the owner-approved
 * mockup (`.ai/specs/2026-10-20-fleet-arvore-viva-mockup.html`): an orchestrating session with
 * subagents on two harnesses and a folded group of finished work, a chat waiting for you, a daemon
 * job, a second project, and a session that ended without closing.
 *
 * Times are relative to the preview's fixed `NOW`, so a screenshot taken twice is the same one.
 */

const MIN = 60_000;
const ago = (min: number) => new Date(NOW - min * MIN).toISOString();
const NO_CAPS: Capabilities = { stop: false, open_run: false, full_transcript: false, pin: false };
const SESSION: Capabilities = { stop: false, open_run: true, full_transcript: true, pin: true };
const t = (kind: TermKind, text: string): TermLine => ({ kind, text });

function make(id: string, over: Partial<FleetNode> & { label: string }): FleetNode {
  return {
    id,
    parent_id: null,
    root_id: id,
    depth: 0,
    harness: "claude-code",
    model: "sonnet-5.5",
    role: null,
    state: "busy",
    started_at: ago(10),
    ended_at: null,
    pinned: false,
    done_omitted: 0,
    project: "nucleos",
    run_id: null,
    capabilities: NO_CAPS,
    ...over,
  };
}
function under(parent: FleetNode, id: string, over: Partial<FleetNode> & { label: string }): FleetNode {
  return make(id, { parent_id: parent.id, root_id: parent.root_id, depth: parent.depth + 1, project: parent.project, ...over });
}

function build(): { nodes: FleetNode[]; lines: Record<string, TermLine[]> } {
  const r1 = make("claude-code:r1", {
    label: "nucleos-37", role: "orchestrate-tdd", model: "opus-5.5", started_at: ago(23), done_omitted: 2,
    run_id: 12, capabilities: SESSION,
  });
  const p1 = under(r1, "claude-code:p1", { label: "Plan chunks 1-3", role: "wf-planner", state: "done", started_at: ago(21), ended_at: ago(17) });
  const e1 = under(r1, "claude-code:e1", { label: "Implement chunk 2 — FleetHub", role: "wf-executor", started_at: ago(9), capabilities: { ...NO_CAPS, full_transcript: true } });
  const x1 = under(e1, "claude-code:x1", { label: "Find run tail readers", role: "Explore", model: "haiku-5.5", started_at: ago(3), capabilities: { ...NO_CAPS, full_transcript: true } });
  const c1 = under(r1, "codex-cli:c1", {
    label: "Implement chunk 3 — layoutForest", role: "wf-executor", harness: "codex-cli", model: "gpt-5-codex", started_at: ago(8),
    run_id: 31, capabilities: { stop: true, open_run: true, full_transcript: true, pin: false },
  });
  const finished = Array.from({ length: 7 }, (_, i) =>
    under(r1, `claude-code:d${i + 1}`, { label: `Task ${i + 1}`, role: "wf-executor", state: "done", started_at: ago(20 - i), ended_at: ago(19 - i) }),
  );
  const r2 = make("claude-code:r2", {
    label: "nucleos-e3", role: "chat", state: "idle", started_at: ago(41), run_id: 18,
    capabilities: SESSION,
  });
  const r3 = make("daemon:run-44", {
    label: "job · nightly triage", role: "job", harness: "daemon", model: "ollama qwen3:14b", started_at: ago(5),
    run_id: 44, capabilities: { stop: true, open_run: true, full_transcript: false, pin: true },
  });
  const r4 = make("claude-code:r4", {
    label: "deal-sniper-c0", role: "chat", model: "opus-5.5", project: "deal_sniper", started_at: ago(14),
    capabilities: { ...SESSION, open_run: false },
  });
  const d1 = under(r4, "claude-code:g1", { label: "Compare price APIs", role: "general-purpose", started_at: ago(12), capabilities: { ...NO_CAPS, full_transcript: true } });
  const r5 = make("claude-code:r5", {
    label: "scratch session", role: "chat", state: "lost", project: "deal_sniper", started_at: ago(55),
    capabilities: { ...NO_CAPS, full_transcript: true },
  });

  const lines: Record<string, TermLine[]> = {
    [r1.id]: [t("prompt", "implementa o plano do fleet ao vivo, chunks 1-3"), t("text", "Plan is approved. Dispatching chunk 2 to a Claude executor and chunk 3 to Codex in parallel."), t("tool", "Agent wf-executor · Implement chunk 2 — FleetHub"), t("tool", "Agent codex · Implement chunk 3 — layoutForest"), t("result", "2 background agents running"), t("status", "waiting on 2 background agents")],
    [e1.id]: [t("prompt", "Execution packet: chunk 2 (FleetHub, NDJSON stream)"), t("tool", "Read core/src/browser/live.rs:240-290"), t("text", "live.rs already streams with Body::from_stream; FleetHub can reuse ReaderStream over an mpsc."), t("tool", "Agent Explore · Find run tail readers"), t("tool", "Edit core/src/http/fleet.rs"), t("result", "+18 −3")],
    [x1.id]: [t("prompt", "Where does /runs/{id}/tail read from? Callers and file paths."), t("tool", "Grep \"fn tail\" core/src"), t("result", "3 matches"), t("tool", "Read core/src/http/runs.rs:410-470")],
    [c1.id]: [t("prompt", "Execution packet: chunk 3 (layoutForest + tests)"), t("tool", "shell rg -n \"positionsFor\" shell/src"), t("result", "4 matches"), t("tool", "apply_patch shell/src/fleet/layout.ts"), t("result", "+88"), t("tool", "shell npx tsc --noEmit -p shell"), t("error", "layout.ts:41 Type 'undefined' is not assignable to 'number'")],
    [p1.id]: [t("final", "Plan written: 3 chunks, 14 tasks")],
    [r2.id]: [t("prompt", "porque é que o gate do shell falha com os error 32?"), t("tool", "Read .ai/WINDOWS_GOTCHAS.md:120-160"), t("text", "npm run tauri dev is holding the target dir. Stop it with Ctrl+C before scripts/gates.sh shell."), t("text", "Do you want me to add a pre-flight check to gates.sh that detects the running dev server?")],
    [r3.id]: [t("prompt", "Triage the 9 new issues since yesterday"), t("tool", "issues.list since=2026-10-19"), t("result", "9 issues"), t("text", "#412 is a duplicate of #398 (same os error 5 on nucleos-core.exe).")],
    [r4.id]: [t("prompt", "compara as APIs de preços e escolhe uma"), t("tool", "Agent general-purpose · Compare price APIs"), t("status", "waiting on 1 background agent")],
    [d1.id]: [t("prompt", "Compare Keepa, Rainforest and PriceAPI: cost, limits, coverage"), t("tool", "WebSearch Keepa API pricing 2026"), t("result", "8 results"), t("tool", "WebFetch rainforestapi.com/pricing"), t("result", "200 · 41 KB")],
    [r5.id]: [t("text", "Sketching the scraper layout…")],
  };
  for (const n of finished) lines[n.id] = [t("final", `${n.label} done`)];

  return { nodes: [r1, p1, e1, x1, c1, ...finished, r2, r3, r4, d1, r5], lines };
}

function metrics(nodes: FleetNode[]): NodeMetrics[] {
  return nodes.map((n, i) => ({
    node_id: n.id,
    last_activity_at: ago(i % 5),
    context_tokens: 12_000 + ((i * 7919) % 90_000),
    cost_usd: Math.round(((i * 37) % 300)) / 100,
  }));
}

export function previewSnapshot(): FleetEvent {
  const { nodes, lines } = build();
  return { t: "snapshot", nodes, metrics: metrics(nodes), lines };
}

/** Two pages, newest first by `before`: the first carries a tool with a full input and result. */
export function previewTranscript(id: string, before: string | null): TranscriptPage {
  if (before === null) {
    return {
      before: "page-2",
      entries: [
        { kind: "prompt", text: `Resume the work of ${id}`, detail: null },
        { kind: "tool", text: "Read core/src/fleet/hub.rs:1-80", detail: "path: core/src/fleet/hub.rs\nlines: 1-80" },
        { kind: "result", text: "80 lines", detail: "//! The FleetHub: one task, started by the first subscriber.\nuse tokio::sync::mpsc;\n// … 78 more lines" },
        { kind: "text", text: "The hub starts with the first subscriber and stops with the last; lagged receivers get a fresh snapshot.", detail: null },
        { kind: "tool", text: "Edit core/src/fleet/hub.rs", detail: "- let (tx, rx) = mpsc::channel(64);\n+ let (tx, rx) = mpsc::channel(limits::CLIENT_CHANNEL);" },
        { kind: "result", text: "+22 −4", detail: null },
      ],
    };
  }
  return {
    before: null,
    entries: [
      { kind: "prompt", text: "Start from the plan in .ai/plans/2026-10-20-fleet-live.md", detail: null },
      { kind: "tool", text: "Read .ai/plans/2026-10-20-fleet-live.md", detail: "chunk 2 — FleetHub\n  task 4: subscriber counting" },
      { kind: "result", text: "412 lines", detail: null },
      { kind: "text", text: "Beginning with the watcher; the safety net re-discovers every five seconds.", detail: null },
    ],
  };
}

/**
 * The fleet's three routes for the preview, or `null` for anything else (which the table answers).
 * The stream is one snapshot and then a heartbeat every 15 s, so a screenshot is deterministic.
 */
export function answerFleet(path: string): Response | null {
  const cut = path.indexOf("?");
  const route = cut === -1 ? path : path.slice(0, cut);
  const query = new URLSearchParams(cut === -1 ? "" : path.slice(cut + 1));

  if (route === "/fleet/live") {
    const enc = new TextEncoder();
    let timer: ReturnType<typeof setInterval> | undefined;
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(enc.encode(JSON.stringify(previewSnapshot()) + "\n"));
        timer = setInterval(() => controller.enqueue(enc.encode('{"t":"heartbeat"}\n')), 15_000);
      },
      cancel() {
        clearInterval(timer);
      },
    });
    return new Response(body, { status: 200, headers: { "content-type": "application/x-ndjson" } });
  }

  const transcript = /^\/fleet\/nodes\/([^/]+)\/transcript$/.exec(route);
  if (transcript !== null) {
    return new Response(JSON.stringify(previewTranscript(decodeURIComponent(transcript[1]), query.get("before"))), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  }

  // `204` takes a null body: `new Response("", { status: 204 })` throws.
  if (/^\/fleet\/pins\/[^/]+$/.test(route)) return new Response(null, { status: 204 });
  return null;
}
