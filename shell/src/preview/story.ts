import type { FeedEntry } from "../data/feed";
import type { OwnerDecision } from "../data/owner-decisions";
import type { BudgetView, HealthReadout, Proposal, ProjectSummary } from "../data/system";
import type { Capabilities, FleetEvent, FleetNode, NodeMetrics, TermKind, TermLine, TranscriptPage } from "../fleet/types";
import { NOW } from "./daemon";

/**
 * The demo story, selected by `?story=demo`: one coherent, fictional afternoon for README shots.
 *
 * The default fixtures are a design-QA dataset (degraded health, a tight budget, departments of
 * every shape). This is the opposite: a fictional project, `invoicer`, running the spec "PDF
 * invoice export" as an agent tree, and the three screens that matter (Fleet, the decisions
 * inbox, Home) all read the same story. Everything here is invented; no real path, name or
 * address appears.
 *
 * Consulted before the default table and only for the routes it names; any other route falls
 * through to `answer()`, so without the flag nothing changes.
 */

export const STORY_PROJECT = "invoicer";
/** Node ids as the daemon names them; the fleet addresses a node as `<harness>:<node id>`. */
export const STORY_TREE_ID = "pdf-invoice-export";
export const STORY_WAITING_NODE_ID = "storage-pick-backend";

/** Whether the page was opened with `?story=demo`. Safe where there is no `location` (node tests). */
export function storyActive(): boolean {
  return typeof location !== "undefined" && new URLSearchParams(location.search).get("story") === "demo";
}

const MIN = 60_000;
const ago = (min: number) => new Date(NOW - min * MIN).toISOString();
const NO_CAPS: Capabilities = { stop: false, open_run: false, full_transcript: false, pin: false };
const SESSION: Capabilities = { stop: false, open_run: true, full_transcript: true, pin: true };
const READ: Capabilities = { ...NO_CAPS, full_transcript: true };
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
    project: STORY_PROJECT,
    run_id: null,
    capabilities: NO_CAPS,
    ...over,
  };
}
function under(parent: FleetNode, id: string, over: Partial<FleetNode> & { label: string }): FleetNode {
  return make(id, { parent_id: parent.id, root_id: parent.root_id, depth: parent.depth + 1, project: parent.project, ...over });
}

function buildFleet(): { nodes: FleetNode[]; lines: Record<string, TermLine[]>; cost: Record<string, number> } {
  const root = make(`claude-code:${STORY_TREE_ID}`, {
    label: "PDF export", role: "tree", model: "opus-5.5", started_at: ago(96), run_id: 52, capabilities: SESSION,
  });

  const render = under(root, "claude-code:inv-render", { label: "Render", role: "branch", model: "opus-5.5", started_at: ago(92), capabilities: READ });
  const rTemplate = under(render, "claude-code:inv-render-template", {
    label: "Template", role: "wf-leaf", state: "done", started_at: ago(64), ended_at: ago(37), capabilities: READ,
  });
  const rGolden = under(render, "codex-cli:inv-render-golden", {
    label: "Golden tests", role: "wf-leaf", harness: "codex-cli", model: "gpt-5-codex", started_at: ago(66), run_id: 58,
    capabilities: { stop: true, open_run: true, full_transcript: true, pin: false },
  });

  const storage = under(root, "claude-code:inv-store", { label: "Storage", role: "branch", model: "opus-5.5", state: "idle", started_at: ago(91), capabilities: READ });
  const sTrait = under(storage, "claude-code:inv-store-trait", {
    label: "Store trait", role: "wf-leaf", state: "done", started_at: ago(88), ended_at: ago(51), capabilities: READ,
  });
  const sChoose = under(storage, `claude-code:${STORY_WAITING_NODE_ID}`, { label: "Pick backend", role: "wf-leaf", state: "idle", started_at: ago(50), capabilities: READ });

  const lines: Record<string, TermLine[]> = {
    [root.id]: [t("prompt", "implement the spec: PDF invoice export"), t("text", "Split into two branches: render and storage. Storage is parked on a product call."), t("tool", "Agent branch · Render"), t("tool", "Agent branch · Storage"), t("status", "waiting on 1 leaf and 1 decision")],
    [render.id]: [t("prompt", "Branch: Render (src/render/**)"), t("status", "waiting on 1 background agent")],
    [rTemplate.id]: [t("prompt", "Execution packet: HTML template for the invoice"), t("tool", "apply_patch src/render/template.rs"), t("result", "+112"), t("status", "done")],
    [sTrait.id]: [t("prompt", "Execution packet: the PdfStore trait and a local backend"), t("tool", "apply_patch src/storage/mod.rs"), t("result", "+86"), t("status", "done")],
    [rGolden.id]: [t("prompt", "Execution packet: golden-file tests for the renderer"), t("tool", "Read src/render/template.rs:1-120"), t("tool", "apply_patch tests/golden/invoice_basic.rs"), t("result", "+64"), t("text", "Basic and multi-page invoices render byte-identically across two runs; adding the VAT-exempt case.")],
    [storage.id]: [t("prompt", "Branch: Storage (src/storage/**)"), t("status", "parked: waiting on the owner (S3 or local disk)")],
    [sChoose.id]: [t("prompt", "Execution packet: wire the production storage backend"), t("tool", "Read src/storage/mod.rs:1-80"), t("text", "The trait has a local backend only. An S3 backend changes the monthly bill and the retention story, so this needs a product call before I write it."), t("status", "waiting on the owner: Store generated PDFs in S3 or on local disk?")],
  };

  const cost: Record<string, number> = {
    [root.id]: 1.84, [render.id]: 0.62, [rTemplate.id]: 0.48, [rGolden.id]: 0.33,
    [storage.id]: 0.41, [sTrait.id]: 0.39, [sChoose.id]: 0.17,
  };
  return { nodes: [root, render, rTemplate, rGolden, storage, sTrait, sChoose], lines, cost };
}

export function storySnapshot(): FleetEvent {
  const { nodes, lines, cost } = buildFleet();
  const metrics: NodeMetrics[] = nodes.map((n, i) => ({
    node_id: n.id,
    last_activity_at: n.state === "done" ? (n.ended_at as string) : ago(1 + (i % 4)),
    context_tokens: n.state === "done" ? 18_000 + i * 2_300 : 41_000 + i * 3_100,
    cost_usd: cost[n.id] ?? 0,
  }));
  return { t: "snapshot", nodes, metrics, lines };
}

function storyTranscript(id: string, before: string | null): TranscriptPage {
  if (before === null) {
    return {
      before: "page-2",
      entries: [
        { kind: "prompt", text: `Resume the work of ${id}`, detail: null },
        { kind: "tool", text: "Read src/storage/mod.rs:1-80", detail: "path: src/storage/mod.rs\nlines: 1-80" },
        { kind: "result", text: "80 lines", detail: "pub trait PdfStore {\n    fn put(&self, key: &str, bytes: &[u8]) -> Result<()>;\n    fn get(&self, key: &str) -> Result<Vec<u8>>;\n}\n// … 74 more lines" },
        { kind: "text", text: "The local backend is done. An S3 backend adds a monthly cost and a retention policy, so I am asking before writing it.", detail: null },
      ],
    };
  }
  return { before: null, entries: [{ kind: "prompt", text: "Start from the spec: PDF invoice export", detail: null }] };
}

/** The fleet's routes under the story, or `null` for anything else. */
export function answerStoryFleet(path: string): Response | null {
  const cut = path.indexOf("?");
  const route = cut === -1 ? path : path.slice(0, cut);
  const query = new URLSearchParams(cut === -1 ? "" : path.slice(cut + 1));

  if (route === "/fleet/live") {
    const enc = new TextEncoder();
    let timer: ReturnType<typeof setInterval> | undefined;
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(enc.encode(JSON.stringify(storySnapshot()) + "\n"));
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
    return new Response(JSON.stringify(storyTranscript(decodeURIComponent(transcript[1]), query.get("before"))), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  }
  if (/^\/fleet\/pins\/[^/]+$/.test(route)) return new Response(null, { status: 204 });
  return null;
}

export const STORY_DECISIONS: OwnerDecision[] = [
  {
    id: "dec-invoicer-storage",
    tree_id: STORY_TREE_ID,
    node_id: STORY_WAITING_NODE_ID,
    question:
      "Store generated PDFs in S3 or on local disk?\n\n" +
      "Local disk: no new cost, but PDFs live on one machine and need their own backup. " +
      "S3: about 2 USD a month at today's volume (roughly 400 invoices), versioned and retained for the 10 years the tax rules ask for.\n\n" +
      "Options: S3 / local disk",
    at: ago(50),
    status: "open",
    answer: null,
    kind: "decision",
    obsolete_reason: null,
  },
];

export const STORY_READOUT: HealthReadout = {
  status: "ok",
  subsystems: [
    { name: "sqlite_pool", status: "ok" },
    { name: "cli_binary", status: "ok" },
    { name: "credential_manager", status: "ok" },
    { name: "worktree_disk", status: "ok" },
    { name: "echo_sidecar", status: "ok" },
    { name: "telegram_sidecar", status: "ok" },
    { name: "email_sidecar", status: "ok" },
    { name: "web_sidecar", status: "ok" },
    { name: "browser_sidecar", status: "ok" },
    { name: "voice_transcriber", status: "ok" },
  ],
};

export const STORY_BUDGET: BudgetView = {
  limit_usd: 25,
  period: "daily",
  hourly_limit_usd: null,
  per_run_reserve_usd: 0.25,
  time_cost_per_hour_usd: 0,
  window_spend_usd: 4.27,
  hourly_spend_usd: 0.58,
  paused: false,
  reason: null,
  relay_chain_limit_usd: 5,
};

export const STORY_QUOTA = {
  source: "sidecar",
  cached: false,
  providers: [
    {
      provider: "claude",
      fidelity: "official",
      read_at: ago(1),
      detail: "",
      severity: "",
      windows: [
        { window: "7d", used_fraction: 0.12, resets_at: new Date(NOW + 4 * 24 * 60 * MIN).toISOString(), stale: false, state: "ok" },
        { window: "5h", used_fraction: 0.09, resets_at: new Date(NOW + 3 * 60 * MIN).toISOString(), stale: false, state: "ok" },
      ],
    },
  ],
};

function project(over: Partial<ProjectSummary> & { project_id: string }): ProjectSummary {
  return {
    mode: "active", project_root: null, pending: 0, classes_ready: 0, classes_total: 0, promotable: false,
    open_review_items: 0, wip_limit: null, queue_full: false, root_exists: true, last_gate: null, last_gate_at: null,
    ...over,
  };
}

export const STORY_PROJECTS: ProjectSummary[] = [
  project({ project_id: STORY_PROJECT, project_root: "~/code/invoicer", wip_limit: 3, last_gate: "passed", last_gate_at: ago(38) }),
  project({ project_id: "storefront", project_root: "~/code/storefront", wip_limit: 2, last_gate: "passed", last_gate_at: ago(7 * 60) }),
  project({ project_id: "notes-cli", mode: "shadow", project_root: "~/code/notes-cli", classes_ready: 3, classes_total: 4, last_gate: "passed", last_gate_at: ago(26 * 60) }),
];

export const STORY_FEED: FeedEntry[] = [
  { id: 9, project_id: STORY_PROJECT, kind: "job_waiting", summary: "tree PDF export is waiting on you: store generated PDFs in S3 or on local disk?", run_id: 52, subject: "job:52", created_at: ago(50) },
  { id: 7, project_id: STORY_PROJECT, kind: "job_finished", summary: "leaf Template finished after 27 min", run_id: 52, subject: "job:52", created_at: ago(37) },
  { id: 6, project_id: STORY_PROJECT, kind: "job_started", summary: "leaf Golden tests started on codex", run_id: 58, subject: "job:58", created_at: ago(66) },
  { id: 5, project_id: STORY_PROJECT, kind: "job_finished", summary: "leaf Store trait finished after 37 min", run_id: 52, subject: "job:52", created_at: ago(51) },
  { id: 4, project_id: "storefront", kind: "job_finished", summary: "job 51 finished `completed` after 4 item(s)", run_id: 51, subject: "job:51", created_at: ago(7 * 60) },
  { id: 3, project_id: STORY_PROJECT, kind: "job_started", summary: "tree PDF export started with two branches", run_id: 52, subject: "job:52", created_at: ago(96) },
  { id: 2, project_id: null, kind: "email_digest", summary: "digest: 9 e-mails triaged, nothing urgent", run_id: null, subject: null, created_at: ago(5 * 60) },
  { id: 1, project_id: "notes-cli", kind: "job_finished", summary: "job 49 finished `completed` after 2 item(s)", run_id: 49, subject: "job:49", created_at: ago(26 * 60) },
].sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at));

/**
 * The Waiting page does not draw owner decisions (the inbox does), so what it holds is the action
 * the same "Pick backend" leaf was stopped on: adding the S3 SDK before the owner has chosen S3.
 */
export const STORY_APPROVALS: Proposal[] = [
  {
    id: 14,
    kind: "action-approval",
    status: "pending",
    run_id: 52,
    session_id: "5c1e90a7",
    project_id: STORY_PROJECT,
    tool_name: "Bash",
    reasoning: "unrecognized shell commands and code execution require approval: adding a dependency changes what the project builds and ships",
    tool_input: JSON.stringify({
      command: "cargo add aws-sdk-s3 --features behavior-version-latest",
      description: "Add the S3 client the production backend would use",
    }),
    read_from: null,
    created_at: ago(49),
    decided_at: null,
    run_status: "awaiting_approval",
    run_cwd: "~/code/invoicer/.nucleos/worktrees/inv-store-pick-backend",
    run_prompt: "Execution packet: wire the production storage backend behind the PdfStore trait.",
    run_recent: JSON.stringify([
      { name: "Read", detail: "src/storage/mod.rs:1-80" },
      { name: "Bash", detail: "cargo test -p invoicer storage" },
    ]),
    action_class: "shell_exec",
    culprit_segment: "cargo add aws-sdk-s3 --features behavior-version-latest",
    culprit_reason: "adds a new dependency to Cargo.toml",
    segments: null,
    run_title: "Pick backend",
    run_mode: "tree leaf",
    node_id: STORY_WAITING_NODE_ID,
    branch: "pdf-export/storage-pick-backend",
    worktree_path: "~/code/invoicer/.nucleos/worktrees/inv-store-pick-backend",
  },
];

/** What `GET /waiting/count` says: the one held action, which is the one thing Waiting lists. */
export const STORY_WAITING_COUNT = {
  wheel: 0, approvals: STORY_APPROVALS.length, team_actions: 0, recruits: 0, merges: 0, exclusions: 0, git: 0,
};

/**
 * The story's answer to `path`, or `undefined` when the story does not name it (the default table
 * then answers). Plain data only; the fleet stream is `answerStoryFleet`.
 */
export function answerStory(path: string): unknown {
  const cut = path.indexOf("?");
  const route = cut === -1 ? path : path.slice(0, cut);
  const query = new URLSearchParams(cut === -1 ? "" : path.slice(cut + 1));

  if (route === "/health/readout") return STORY_READOUT;
  if (route === "/projects") return STORY_PROJECTS;
  if (route === "/autopilot/budget") return STORY_BUDGET;
  if (route === "/waiting/count") return STORY_WAITING_COUNT;
  // The Waiting page's own lists: the one held action, every other queue answered and empty.
  if (route === "/proposals") return STORY_APPROVALS;
  if (
    [
      "/browser/sessions", "/proposals/team-actions", "/proposals/recruits", "/proposals/skipped-items",
      "/proposals/refused-actions", "/team-actions", "/contacts/merges", "/fleet/exclusions/requests",
      "/waiting/git", "/vcs/requests", "/runs/awaiting-approval",
    ].includes(route)
  ) {
    return [];
  }
  // Sidebar noise the default fixtures raise: Brain captures, held notifications, a tight quota.
  if (route === "/capture-requests") return [];
  if (route === "/notifications/pending") return [];
  if (route === "/quota") return STORY_QUOTA;
  if (route === "/decisions") {
    const status = query.get("status");
    return status === null || status === "open" ? STORY_DECISIONS : [];
  }
  if (route === "/feed") {
    const limit = Number(query.get("limit"));
    return Number.isFinite(limit) && limit > 0 ? STORY_FEED.slice(0, limit) : STORY_FEED;
  }
  return undefined;
}
