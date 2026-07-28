const DAEMON_URL = "http://127.0.0.1:8791";

export type ConnectionState = "checking" | "connected" | "disconnected";

export async function checkHealth(): Promise<ConnectionState> {
  try {
    const res = await fetch(`${DAEMON_URL}/health`);
    return res.ok ? "connected" : "disconnected";
  } catch {
    return "disconnected";
  }
}

export async function getStatus(token: string): Promise<string | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/status`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return await res.text();
  } catch {
    return null;
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

export type SetModeResult = { ok: true } | { ok: false; status: number };

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
    return res.ok ? { ok: true } : { ok: false, status: res.status };
  } catch {
    return { ok: false, status: 0 };
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

/** What the pillar knows about, waiting mail first. */
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
