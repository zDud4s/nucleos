const DAEMON_URL = "http://127.0.0.1:8791";

export type ConnectionState = "checking" | "connected" | "disconnected";

/**
 * Why a token-carrying request failed.
 *
 * Two of these are different problems wearing the same face: "the daemon isn't
 * there" is fixed by waiting, while "the daemon is there and refused this
 * token" never is — a stale credential collapsed into `null` looks like an
 * outage and buys the user a retry loop that cannot succeed.
 */
export type ApiFault = "unauthorized" | "unreachable" | "failed";

export type ApiResult<T> =
  | { ok: true; value: T }
  | { ok: false; fault: ApiFault; status: number };

/** 403 counts as unauthorized: from the shell's side, both mean "this token buys nothing". */
export function faultForStatus(status: number): ApiFault {
  return status === 401 || status === 403 ? "unauthorized" : "failed";
}

export async function checkHealth(): Promise<ConnectionState> {
  try {
    const res = await fetch(`${DAEMON_URL}/health`);
    return res.ok ? "connected" : "disconnected";
  } catch {
    return "disconnected";
  }
}

/**
 * The daemon's own status line — and, because it is the first authenticated
 * call of every poll, the shell's proof that the stored token still works.
 * That is why this one reports HOW it failed and the read-only getters below
 * still do not: the distinction is only actionable once per round.
 */
export async function getStatus(token: string): Promise<ApiResult<string>> {
  try {
    const res = await fetch(`${DAEMON_URL}/status`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    return { ok: true, value: await res.text() };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

export type AutopilotMode = "off" | "shadow" | "active";

export interface ProjectSummary {
  project_id: string;
  mode: AutopilotMode;
  project_root: string | null;
  pending: number;
  /** Action classes clearing the shadow-exit bar, out of those the project has exercised. */
  classes_ready: number;
  classes_total: number;
  /**
   * How many of those ready classes are ones the classifier WITHHELD (`pending_approval`/`deny`).
   *
   * Optional because the shell can be newer than the daemon it is talking to, and an absent value
   * reads as zero — which keeps the promote control locked rather than unlocking it, the safe
   * direction for a field that is not there.
   */
  withheld_classes_ready?: number;
  /** Whether the promote-to-active control should unlock. Decided by the daemon (`shadow.rs`). */
  promotable: boolean;
  /** WIP brake: proposals waiting on you, the ceiling (null = off), and whether it is reached. */
  open_proposals: number;
  wip_limit: number | null;
  queue_full: boolean;
}

export interface FeedEntry {
  id: number;
  project_id: string | null;
  kind: string;
  summary: string;
  run_id: number | null;
  created_at: string;
}

export interface ClassTally {
  mode: string;
  action_class: string;
  total: number;
  would_allow: number;
  would_pend: number;
  would_deny: number;
  reviewed: number;
  agree: number;
  disagree: number;
}

export interface ShadowDecision {
  id: number;
  run_id: number;
  tool_name: string;
  tool_input: string | null;
  decision: string;
  reason: string | null;
  action_class: string;
  classifier_version: number;
  human_verdict: string | null;
  reviewed_at: string | null;
  created_at: string;
}

export interface AwaitingRun {
  id: number;
  project_id: string | null;
  prompt: string;
  cwd: string | null;
  created_at: string;
}

export interface Proposal {
  id: number;
  kind: string;
  status: string;
  run_id: number | null;
  session_id: string | null;
  project_id: string | null;
  tool_name: string | null;
  reasoning: string;
  tool_input: string | null;
  created_at: string;
  decided_at: string | null;
}

export interface ScopedKill {
  scope_type: string;
  scope_id: string;
  engaged: boolean;
}

/** One message the pillar knows about: waiting, or already judged. */
export interface QueuedEmail {
  id: number;
  from_addr: string;
  from_name: string | null;
  subject: string | null;
  received_at: string;
  /** null means still waiting to be triaged. */
  triage_class: string | null;
  triage_summary: string | null;
  triaged_at: string | null;
  has_attachments: number;
}

/** One attachment, described. The bytes are not stored; opening one asks the mailbox. */
export interface EmailAttachment {
  position: number;
  filename: string | null;
  mime_type: string | null;
  /** Decoded size, so it matches what the file would weigh on disk. */
  size_bytes: number;
}

/** One message in full. The queue deliberately does not carry bodies; this is what opening reads. */
export interface EmailDetail {
  id: number;
  from_addr: string;
  from_name: string | null;
  subject: string | null;
  received_at: string;
  triage_class: string | null;
  triage_summary: string | null;
  triaged_at: string | null;
  /** null once retention pruned it — a state to show, not an empty message. */
  body_text: string | null;
  has_attachments: number;
  attachments: EmailAttachment[];
}

/** What a requested triage pass started. The verdicts arrive later, in the feed. */
export interface TriageOutcome {
  queued: number;
  run_id: number | null;
  reason: string | null;
}

export interface Budget {
  limit_usd: number | null;
  period: "daily" | "weekly" | "monthly";
  hourly_limit_usd: number | null;
  per_run_reserve_usd: number;
  time_cost_per_hour_usd: number;
  window_spend_usd: number;
  hourly_spend_usd: number;
  paused: boolean;
  reason: string | null;
}

export interface BudgetConfigInput {
  limit_usd: number | null;
  period: "daily" | "weekly" | "monthly";
  hourly_limit_usd: number | null;
  per_run_reserve_usd: number;
  time_cost_per_hour_usd: number;
}

export type SetModeResult =
  | { ok: true }
  | { ok: false; fault: ApiFault; status: number };

export async function getProjects(
  token: string,
): Promise<ProjectSummary[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/projects`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return await res.json();
  } catch {
    return null;
  }
}

export async function getFeed(
  token: string,
  opts?: { scope?: "all"; projectId?: string },
): Promise<FeedEntry[] | null> {
  let path = "/feed";
  if (opts?.scope === "all") {
    path += "?scope=all";
  } else if (opts?.projectId) {
    path += `?project_id=${encodeURIComponent(opts.projectId)}`;
  }

  try {
    const res = await fetch(`${DAEMON_URL}${path}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return await res.json();
  } catch {
    return null;
  }
}

export async function getScoreboard(
  token: string,
  projectId: string,
): Promise<ClassTally[] | null> {
  try {
    const res = await fetch(
      `${DAEMON_URL}/scoreboard?project_id=${encodeURIComponent(projectId)}`,
      { headers: { Authorization: `Bearer ${token}` } },
    );
    if (!res.ok) return null;
    return await res.json();
  } catch {
    return null;
  }
}

export async function getShadowDecisions(
  token: string,
  projectId: string,
): Promise<ShadowDecision[] | null> {
  try {
    const res = await fetch(
      `${DAEMON_URL}/shadow-decisions?project_id=${encodeURIComponent(projectId)}`,
      { headers: { Authorization: `Bearer ${token}` } },
    );
    if (!res.ok) return null;
    return await res.json();
  } catch {
    return null;
  }
}

export async function setVerdict(
  token: string,
  id: number,
  verdict: "approve" | "reject",
): Promise<boolean> {
  try {
    const res = await fetch(`${DAEMON_URL}/shadow-decisions/${id}/verdict`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ verdict }),
    });
    return res.ok;
  } catch {
    return false;
  }
}

export async function getAwaitingApproval(
  token: string,
): Promise<AwaitingRun[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/runs/awaiting-approval`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return await res.json();
  } catch {
    return null;
  }
}

export async function getProposals(
  token: string,
): Promise<Proposal[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/proposals`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return await res.json();
  } catch {
    return null;
  }
}

export async function approveProposal(
  token: string,
  id: number,
): Promise<number | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/proposals/${id}/approve`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
    });
    if (!res.ok) return null;
    const data = await res.json();
    return data.resume_run_id;
  } catch {
    return null;
  }
}

export async function rejectProposal(
  token: string,
  id: number,
): Promise<boolean> {
  try {
    const res = await fetch(`${DAEMON_URL}/proposals/${id}/reject`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
    });
    return res.ok;
  } catch {
    return false;
  }
}

export async function releaseWorktree(
  token: string,
  runId: number,
): Promise<boolean> {
  try {
    const res = await fetch(`${DAEMON_URL}/worktrees/${runId}/release`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
    });
    return res.ok;
  } catch {
    return false;
  }
}

export async function getKillSwitch(token: string): Promise<boolean | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/autopilot/kill`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    const data = await res.json();
    return data.engaged;
  } catch {
    return null;
  }
}

export async function setKillSwitch(
  token: string,
  engaged: boolean,
): Promise<boolean> {
  try {
    const res = await fetch(`${DAEMON_URL}/autopilot/kill`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ engaged }),
    });
    return res.ok;
  } catch {
    return false;
  }
}

export async function setProjectMode(
  token: string,
  projectId: string,
  mode: AutopilotMode,
  projectRoot?: string,
): Promise<SetModeResult> {
  try {
    const res = await fetch(`${DAEMON_URL}/autopilot/state`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        project_id: projectId,
        mode,
        project_root: projectRoot,
      }),
    });
    return res.ok
      ? { ok: true }
      : { ok: false, fault: faultForStatus(res.status), status: res.status };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

export async function getScopedKills(token: string): Promise<ScopedKill[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/autopilot/kill/scoped`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return await res.json();
  } catch {
    return null;
  }
}

/**
 * The mailbox, newest arrival first — the daemon's order, which callers should not second-guess:
 * it sorts the whole table and then truncates, so re-sorting the page you received would only
 * reorder a slice of the answer.
 */
export async function getEmailQueue(token: string): Promise<QueuedEmail[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/email/queue`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as QueuedEmail[];
  } catch {
    return null;
  }
}

/**
 * One attachment's bytes.
 *
 * Nothing is stored anywhere: the daemon asks the sidecar, the sidecar opens the mailbox, and the
 * file travels straight through. Which is also why this can fail for a message that was deleted in
 * Gmail after it was read here.
 */
export async function fetchAttachment(
  token: string,
  emailId: number,
  position: number,
): Promise<Blob | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/email/${emailId}/attachments/${position}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return await res.blob();
  } catch {
    return null;
  }
}

/** One entry in the mail folder. */
export interface MailFile {
  name: string;
  is_dir: boolean;
  /** Zero for a folder — its size is a different question, answered by walking it. */
  size_bytes: number;
  modified: string | null;
}

/** What is in a folder under the mail root. `path` empty means the root itself. */
export async function listMailFiles(token: string, path = ""): Promise<MailFile[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/mail-files?path=${encodeURIComponent(path)}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as MailFile[];
  } catch {
    return null;
  }
}

export async function createMailFolder(token: string, path: string): Promise<boolean> {
  try {
    const res = await fetch(`${DAEMON_URL}/mail-files/folder`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ path }),
    });
    return res.ok;
  } catch {
    return false;
  }
}

/**
 * Files an attachment into the mail folder.
 *
 * Returns the name it was ACTUALLY stored under, which can differ from the sender's twice over:
 * once because the name was made safe, once because it collided with something already there.
 */
export async function saveAttachment(
  token: string,
  emailId: number,
  position: number,
  folder: string,
): Promise<string | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/email/${emailId}/attachments/${position}/save`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ folder }),
    });
    if (!res.ok) return null;
    return ((await res.json()) as { filename: string }).filename;
  } catch {
    return null;
  }
}

/** One attachment with its bytes, as the bulk read hands them over. */
export interface BulkAttachment {
  position: number;
  filename: string | null;
  mime_type: string | null;
  size_bytes: number;
  content_base64: string;
}

/**
 * Every attachment of one message, in ONE trip to the mailbox.
 *
 * Not a loop over `fetchAttachment`: each of those downloads the whole message, so eight
 * attachments meant pulling the same eight files eight times over eight connections.
 */
export async function fetchAllAttachments(
  token: string,
  emailId: number,
): Promise<BulkAttachment[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/email/${emailId}/attachments`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as BulkAttachment[];
  } catch {
    return null;
  }
}

/** Files every attachment into one folder. Returns the names actually stored, in order. */
export async function saveAllAttachments(
  token: string,
  emailId: number,
  folder: string,
): Promise<string[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/email/${emailId}/attachments/save-all`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ folder }),
    });
    if (!res.ok) return null;
    return ((await res.json()) as { filenames: string[] }).filenames;
  } catch {
    return null;
  }
}

/** One message with its body — read when a message is opened, never to draw the list. */
export async function getEmail(token: string, id: number): Promise<EmailDetail | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/email/${id}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as EmailDetail;
  } catch {
    return null;
  }
}

/**
 * Triage what is waiting, now.
 *
 * Mail is collected in the background because that costs nothing; classifying it costs a run, so
 * it happens only when asked. This is the asking.
 */
export async function triageEmail(token: string): Promise<TriageOutcome | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/email/triage`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as TriageOutcome;
  } catch {
    return null;
  }
}

export async function setScopedKill(
  token: string,
  scopeType: string,
  scopeId: string,
  engaged: boolean,
): Promise<boolean> {
  try {
    const res = await fetch(`${DAEMON_URL}/autopilot/kill/scoped`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        scope_type: scopeType,
        scope_id: scopeId,
        engaged,
      }),
    });
    return res.ok;
  } catch {
    return false;
  }
}

export async function getBudget(token: string): Promise<Budget | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/autopilot/budget`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return await res.json();
  } catch {
    return null;
  }
}

export async function setBudget(
  token: string,
  config: BudgetConfigInput,
): Promise<Budget | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/autopilot/budget`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify(config),
    });
    if (!res.ok) return null;
    return await res.json();
  } catch {
    return null;
  }
}

/**
 * The daemon's presence heartbeat.
 *
 * Sent explicitly rather than inferred from this shell's own polling, because `attention.rs` refuses
 * to derive presence from API traffic: the shell polls every 3 seconds whether anyone is watching or
 * not, so traffic-derived presence would mark the owner permanently present and stop autonomy
 * forever. This is the one call that means "a person is actually looking", and it expires on its own
 * when the shell closes.
 */
export async function sendAttentionHeartbeat(
  token: string,
  projectId?: string,
): Promise<boolean> {
  try {
    const res = await fetch(`${DAEMON_URL}/autopilot/attention`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify(projectId === undefined ? {} : { project_id: projectId }),
    });
    return res.ok;
  } catch {
    return false;
  }
}

// ── Runs ────────────────────────────────────────────────────────────────────

/** One row of the run index. Carries an excerpt, never the full prompt or output. */
export interface RunSearchResult {
  id: number;
  project_id: string | null;
  status: string;
  mode: string;
  created_at: string;
  completed_at: string | null;
  cost_usd: number | null;
  prompt_excerpt: string;
}

/** One run in full — what opening a row reads, including its gate verdict and output. */
export interface RunDetail {
  id: number;
  project_id: string | null;
  status: string;
  gate_status: string | null;
  gate_exit_code: number | null;
  gate_output: string | null;
  exit_code: number | null;
  stdout: string | null;
  stderr: string | null;
  session_id: string | null;
  cost_usd: number | null;
  input_tokens: number | null;
  output_tokens: number | null;
  cache_read_tokens: number | null;
  num_turns: number | null;
}

/** Every field the daemon's `/runs` filter accepts. All optional; omitted ones are not sent. */
export interface RunsFilter {
  projectId?: string;
  status?: string;
  mode?: string;
  q?: string;
  since?: string;
  until?: string;
  limit?: number;
}

export interface CreateRunInput {
  prompt: string;
  project_id: string | null;
  cwd: string | null;
  mode: string;
}

/**
 * The modes a person may ASK for. `assistant` also appears in the status filter below because the
 * daemon writes it, but it is not offered here: an assistant turn is created by talking to the
 * assistant, not by filling in this form.
 */
export const RUN_MODES = ["real", "shadow", "worktree"] as const;

/** Every status a run row can carry, as `runs.rs` writes them. */
export const RUN_STATUSES = [
  "pending",
  "running",
  "awaiting_approval",
  "completed",
  "failed",
  "cancelled",
  "interrupted",
  "timed_out",
] as const;

/** Every mode a run row can carry — the askable three, plus the one only the assistant creates. */
export const RUN_MODE_FILTERS = [...RUN_MODES, "assistant"] as const;

function runsQuery(filter: RunsFilter): string {
  const params = new URLSearchParams();
  if (filter.projectId) params.set("project_id", filter.projectId);
  if (filter.status) params.set("status", filter.status);
  if (filter.mode) params.set("mode", filter.mode);
  if (filter.q) params.set("q", filter.q);
  if (filter.since) params.set("since", filter.since);
  if (filter.until) params.set("until", filter.until);
  if (filter.limit !== undefined) params.set("limit", String(filter.limit));
  const query = params.toString();
  return query === "" ? "" : `?${query}`;
}

export async function getRuns(
  token: string,
  filter: RunsFilter = {},
): Promise<RunSearchResult[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/runs${runsQuery(filter)}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as RunSearchResult[];
  } catch {
    return null;
  }
}

export async function getRun(token: string, id: number): Promise<RunDetail | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/runs/${id}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as RunDetail;
  } catch {
    return null;
  }
}

/**
 * Starts a run, reporting the status when it fails.
 *
 * Unlike the read-only getters, this one cannot collapse a refusal into `null`: the daemon answers
 * 503 while the kill switch is engaged and 429 while the budget is paused, and both are states the
 * person can act on — a bare "it didn't work" would send them looking for an outage instead.
 */
export async function createRun(
  token: string,
  input: CreateRunInput,
): Promise<ApiResult<number>> {
  try {
    const res = await fetch(`${DAEMON_URL}/runs`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify(input),
    });
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    const data = (await res.json()) as { id: number };
    return { ok: true, value: data.id };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

/** 404 here means the run already ended — a race with its own completion, not a failure. */
export async function cancelRun(token: string, id: number): Promise<boolean> {
  try {
    const res = await fetch(`${DAEMON_URL}/runs/${id}/cancel`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}` },
    });
    return res.ok;
  } catch {
    return false;
  }
}

// ── Presets ─────────────────────────────────────────────────────────────────

/** A saved run request: a name, plus the exact body `/runs` would have taken. */
export interface Preset {
  id: number;
  name: string;
  prompt: string;
  project_id: string | null;
  cwd: string | null;
  mode: string;
  created_at: string;
  updated_at: string;
}

export interface PresetInput {
  name: string;
  prompt: string;
  project_id: string | null;
  cwd: string | null;
  mode: string;
}

export async function listPresets(token: string): Promise<Preset[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/presets`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as Preset[];
  } catch {
    return null;
  }
}

/** 409 means the name is taken — the one failure worth naming, so the status travels back. */
export async function createPreset(
  token: string,
  input: PresetInput,
): Promise<ApiResult<Preset>> {
  return writePreset(token, `${DAEMON_URL}/presets`, "POST", input);
}

export async function updatePreset(
  token: string,
  id: number,
  input: PresetInput,
): Promise<ApiResult<Preset>> {
  return writePreset(token, `${DAEMON_URL}/presets/${id}`, "PUT", input);
}

async function writePreset(
  token: string,
  url: string,
  method: "POST" | "PUT",
  input: PresetInput,
): Promise<ApiResult<Preset>> {
  try {
    const res = await fetch(url, {
      method,
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify(input),
    });
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    return { ok: true, value: (await res.json()) as Preset };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

export async function deletePreset(token: string, id: number): Promise<boolean> {
  try {
    const res = await fetch(`${DAEMON_URL}/presets/${id}`, {
      method: "DELETE",
      headers: { Authorization: `Bearer ${token}` },
    });
    return res.ok;
  } catch {
    return false;
  }
}

/**
 * Runs a preset.
 *
 * The daemon delegates this to the same front door as `createRun`, so it refuses for the same
 * reasons and the status matters here for the same reason it does there.
 */
export async function runPreset(token: string, id: number): Promise<ApiResult<number>> {
  try {
    const res = await fetch(`${DAEMON_URL}/presets/${id}/run`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    const data = (await res.json()) as { id: number };
    return { ok: true, value: data.id };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

// ── Assistant ───────────────────────────────────────────────────────────────

/**
 * The chat this shell speaks as.
 *
 * The turn slot in `assistant.rs` is held per chat, so naming the shell's own chat keeps it from
 * colliding with the Telegram sidecar's: both can be mid-turn at once, and neither cancels the
 * other. A 409 from `sendAssistantMessage` therefore means THIS chat is still busy.
 */
export const SHELL_CHAT_ID = "shell";

export async function sendAssistantMessage(
  token: string,
  chatId: string,
  text: string,
): Promise<ApiResult<number>> {
  try {
    const res = await fetch(`${DAEMON_URL}/assistant/message`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ chat_id: chatId, text }),
    });
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    const data = (await res.json()) as { turn_id: number };
    return { ok: true, value: data.turn_id };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

/**
 * How a turn is going. The daemon answers this from the runs table, so a turn IS a run and carries
 * the same fields — which is why this reuses `RunDetail` rather than inventing a parallel shape.
 */
export async function getAssistantTurn(
  token: string,
  turnId: number,
): Promise<RunDetail | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/assistant/${turnId}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as RunDetail;
  } catch {
    return null;
  }
}

// ── Project inspection ──────────────────────────────────────────────────────

export interface InspectEntry {
  name: string;
  is_dir: boolean;
}

export interface InspectMatch {
  path: string;
  line: number;
  text: string;
}

/**
 * What is in a directory of a project's tree. `path` empty means the project root.
 *
 * Every call here is scoped to a project the daemon already knows the root of, and the daemon
 * refuses a path that leaves it — so the shell passes what the person typed rather than trying to
 * validate a traversal it cannot see the filesystem to check.
 *
 * Reports the status like `getProjectCat` does, rather than collapsing every refusal to `null`. The
 * inspect routes answer 404 for two very different things — a path that is gone, and a project
 * whose RECORDED ROOT is gone from disk — and a caller that only knows "it failed" cannot tell
 * someone which one they are looking at.
 */
export async function getProjectLs(
  token: string,
  projectId: string,
  path = "",
): Promise<ApiResult<InspectEntry[]>> {
  try {
    const res = await fetch(
      `${DAEMON_URL}/projects/${encodeURIComponent(projectId)}/ls?path=${encodeURIComponent(path)}`,
      { headers: { Authorization: `Bearer ${token}` } },
    );
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    return { ok: true, value: (await res.json()) as InspectEntry[] };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

/** One file's text. The daemon caps how much it will read, so this can be a truncated file. */
export async function getProjectCat(
  token: string,
  projectId: string,
  path: string,
): Promise<ApiResult<string>> {
  try {
    const res = await fetch(
      `${DAEMON_URL}/projects/${encodeURIComponent(projectId)}/cat?path=${encodeURIComponent(path)}`,
      { headers: { Authorization: `Bearer ${token}` } },
    );
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    return { ok: true, value: await res.text() };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

export async function getProjectGrep(
  token: string,
  projectId: string,
  q: string,
  path = "",
): Promise<ApiResult<InspectMatch[]>> {
  try {
    const res = await fetch(
      `${DAEMON_URL}/projects/${encodeURIComponent(projectId)}/grep?q=${encodeURIComponent(q)}&path=${encodeURIComponent(path)}`,
      { headers: { Authorization: `Bearer ${token}` } },
    );
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    return { ok: true, value: (await res.json()) as InspectMatch[] };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

/** The project's uncommitted diff, as `git diff` wrote it. Empty text means a clean tree. */
export async function getProjectDiff(
  token: string,
  projectId: string,
): Promise<ApiResult<string>> {
  try {
    const res = await fetch(
      `${DAEMON_URL}/projects/${encodeURIComponent(projectId)}/diff`,
      { headers: { Authorization: `Bearer ${token}` } },
    );
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    return { ok: true, value: await res.text() };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

// ── Backups ─────────────────────────────────────────────────────────────────

export interface BackupInfo {
  name: string;
  /** null when the file could not be opened to read its schema version. */
  migration_version: number | null;
  size_bytes: number;
}

/**
 * A restore that has been PREPARED, not performed.
 *
 * `applies` says when it takes effect — the daemon writes "on next daemon start". Nothing has been
 * swapped when this comes back, which is the whole reason it is worth showing rather than a bare
 * "done".
 */
export interface StagedRestore {
  name: string;
  migration_version: number;
  applies: string;
}

export async function getBackups(token: string): Promise<BackupInfo[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/backups`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as BackupInfo[];
  } catch {
    return null;
  }
}

export async function takeBackup(token: string): Promise<BackupInfo | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/backup`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as BackupInfo;
  } catch {
    return null;
  }
}

export async function restoreBackup(
  token: string,
  name: string,
): Promise<StagedRestore | null> {
  try {
    const res = await fetch(
      `${DAEMON_URL}/backups/${encodeURIComponent(name)}/restore`,
      { method: "POST", headers: { Authorization: `Bearer ${token}` } },
    );
    if (!res.ok) return null;
    return (await res.json()) as StagedRestore;
  } catch {
    return null;
  }
}

// ── Health readout ──────────────────────────────────────────────────────────

export type HealthState = "ok" | "degraded" | "down" | "disabled";

/**
 * One subsystem's verdict.
 *
 * `reason` is a closed vocabulary and never free text: the daemon deliberately keeps error strings
 * out of this, because IMAP and network errors embed credential-bearing URLs. It is absent when the
 * subsystem is fine.
 */
export interface SubsystemReadout {
  name: string;
  status: HealthState;
  reason?: string;
}

export interface HealthReadout {
  status: HealthState;
  subsystems: SubsystemReadout[];
}

export async function getHealthReadout(token: string): Promise<HealthReadout | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/health/readout`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as HealthReadout;
  } catch {
    return null;
  }
}

// ── API tokens ──────────────────────────────────────────────────────────────

/** The three durable key levels, mirroring `ApiTokenLevel` in `core/src/auth.rs`. */
export type ApiTokenLevel = "read-only" | "run-creating" | "admin";

export const API_TOKEN_LEVELS: ApiTokenLevel[] = ["read-only", "run-creating", "admin"];

export interface ApiTokenSummary {
  name: string;
  level: ApiTokenLevel;
  created_at: string;
}

/** Creation's answer, and the only time the secret exists outside the daemon. */
export interface CreatedApiToken extends ApiTokenSummary {
  token: string;
}

export async function listApiTokens(token: string): Promise<ApiTokenSummary[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/api-tokens`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as ApiTokenSummary[];
  } catch {
    return null;
  }
}

/**
 * Mints a key. The secret comes back exactly once — listing never returns it again — so a caller
 * that drops this answer has lost the key and has to revoke and mint another.
 *
 * 409 means the name is taken; the status travels so the page can say which of the two it was.
 */
export async function createApiToken(
  token: string,
  name: string,
  level: ApiTokenLevel,
): Promise<ApiResult<CreatedApiToken>> {
  try {
    const res = await fetch(`${DAEMON_URL}/api-tokens`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ name, level }),
    });
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    return { ok: true, value: (await res.json()) as CreatedApiToken };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

export async function revokeApiToken(token: string, name: string): Promise<boolean> {
  try {
    const res = await fetch(`${DAEMON_URL}/api-tokens/${encodeURIComponent(name)}`, {
      method: "DELETE",
      headers: { Authorization: `Bearer ${token}` },
    });
    return res.ok;
  } catch {
    return false;
  }
}

// ── Email cursor and requeue ────────────────────────────────────────────────

/** Where collection got to in one mailbox. `null` means nothing has been collected from it yet. */
export interface EmailCursor {
  uidvalidity: number;
  last_uid: number;
}

export async function getEmailCursor(
  token: string,
  mailbox: string,
): Promise<EmailCursor | null> {
  try {
    const res = await fetch(
      `${DAEMON_URL}/email/cursor?mailbox=${encodeURIComponent(mailbox)}`,
      { headers: { Authorization: `Bearer ${token}` } },
    );
    if (!res.ok) return null;
    return (await res.json()) as EmailCursor | null;
  } catch {
    return null;
  }
}

/** Why a requeue was refused — each has a different answer, so they are not collapsed. */
export type RequeueFailure = "unknown" | "conflict" | "failed";

/**
 * Puts a message back in the queue to be read again.
 *
 * 409 is the interesting one and covers two situations the daemon distinguishes internally: the body
 * was already pruned by retention, or a run currently holds the message. Neither is fixed by
 * pressing again, which is why this does not report a bare false.
 */
export async function requeueEmail(
  token: string,
  id: number,
): Promise<true | RequeueFailure> {
  try {
    const res = await fetch(`${DAEMON_URL}/email/${id}/requeue`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}` },
    });
    if (res.ok) return true;
    if (res.status === 404) return "unknown";
    if (res.status === 409) return "conflict";
    return "failed";
  } catch {
    return "failed";
  }
}

export type VoiceKind = "dictation" | "memo";

/**
 * How a capture's text came out. `cleaned` is the only one whose `clean_text` is populated;
 * `raw` means the model was unreachable or unarmed, and `shrunk` means it answered with so much
 * less text than it was given that the answer was refused and the transcript kept instead.
 */
export type VoiceCleanupState = "cleaned" | "raw" | "shrunk";

/**
 * What `GET /voice/config` reports.
 *
 * Every value is the daemon's. The shell displays them and owns none of them — `.ai/voice.yaml` is
 * a self-governing file (it names a program the daemon will execute), so it is edited by hand and
 * never through this window.
 */
export interface VoiceConfigView {
  armed: boolean;
  hints: string[];
  cleanup_prompt: string;
  cleanup_model: string | null;
  retain_dictations_days: number;
  max_capture_seconds: number;
  max_body_bytes: number;
}

/** One stored capture. `clean_text` is null for any `cleanup_state` other than `cleaned`. */
export interface VoiceCapture {
  id: number;
  kind: VoiceKind;
  created_at: string;
  duration_ms: number;
  raw_text: string;
  clean_text: string | null;
  cleanup_state: VoiceCleanupState;
  model: string | null;
}

export async function getVoiceConfig(token: string): Promise<VoiceConfigView | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/voice/config`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as VoiceConfigView;
  } catch {
    return null;
  }
}

export async function listVoiceMemos(token: string): Promise<VoiceCapture[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/voice/memos`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as VoiceCapture[];
  } catch {
    return null;
  }
}

/**
 * Deletes a memo. `false` covers both "no such memo" and a failed request, because the page's only
 * response to either is to reload the list and show what is actually there.
 *
 * The route is scoped to memos on the daemon's side: a dictation's id here answers 404 rather than
 * deleting it, even though both kinds draw their ids from one sequence.
 */
export async function deleteVoiceMemo(token: string, id: number): Promise<boolean> {
  try {
    const res = await fetch(`${DAEMON_URL}/voice/memos/${id}`, {
      method: "DELETE",
      headers: { Authorization: `Bearer ${token}` },
    });
    return res.ok;
  } catch {
    return false;
  }
}
