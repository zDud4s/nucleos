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

/**
 * One job: a sequence of runs over a shared worktree, so a night's work is not capped by one
 * context window.
 */
export interface Job {
  id: number;
  project_id: string;
  rule_name: string | null;
  /**
   * `planning` | `implementing` | `gating` | `reviewing` | `waiting` | `awaiting_approval`, then
   * one of the endings: `completed`, `failed`, `gate_failed`, `gate_errored`, `expired`,
   * `stopped`, `cancelled`, `interrupted`.
   */
  status: string;
  /**
   * Why a `waiting` job waits. Budget and contention ask opposite things of a reader — "spend more
   * or wait it out" versus "something else has the project" — so `waiting` alone leaves them
   * guessing which.
   */
  wait_reason: string | null;
  max_items: number;
  created_at: string;
  completed_at: string | null;
}

export interface JobItem {
  ordinal: number;
  description: string;
  /** `pending` | `running` | `implemented` | `passed` | `failed` | `cancelled` | `gate_*`. */
  status: string;
  run_id: number | null;
  /**
   * Whether anything measured this item, kept apart from `status` because they answer different
   * questions. An item reading `passed` with a null gate status was never measured — the project
   * configures no gate, or this was an intermediate item under `gate_after_each_item: false`.
   */
  gate_status: string | null;
}

export interface JobDetail extends Job {
  items: JobItem[];
  /** Where the work is, so a stopped job's partial can be found. Null once the GC took the tree. */
  branch: string | null;
}

export async function getJobs(
  token: string,
  projectId?: string,
): Promise<Job[] | null> {
  const path = projectId === undefined
    ? "/jobs"
    : `/jobs?project_id=${encodeURIComponent(projectId)}`;
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

/**
 * What a job may spend and how far it may go, or the house limits if left out.
 *
 * Both are optional in the daemon's sense, not the shell's convenience: `budget_usd: null` means
 * "only the house limit governs", which is what every `graph:` rule has always meant, and
 * `max_rounds: null` is one round. Sending `0` for either would mean something else entirely, so
 * the form omits the field rather than sending a falsy number.
 */
export interface NewJob {
  projectId: string;
  prompt: string;
  budgetUsd?: number;
  maxRounds?: number;
}

/**
 * Why a job did not start, in the daemon's own words.
 *
 * Its own type rather than `ApiResult`, because the status alone does not identify the refusal
 * here: `409` is both "the kill switch is engaged" and "no room — something else is running", and
 * those have different remedies. The daemon writes a sentence for every refusal it raises, so the
 * sentence travels instead of being reconstructed from a number the shell would have to guess at.
 */
export type CreateJobOutcome =
  | { ok: true; jobId: number }
  | { ok: false; status: number; reason: string };

/**
 * Asks for a job the way `createRun` asks for a run.
 *
 * Note what the request cannot carry: `max_items`. Fan-out per round has a hard ceiling in
 * `config::MAX_ITEMS_CEILING` that no caller may raise, so there is deliberately no field for it —
 * how long and how much are the caller's to choose, how wide is not.
 */
export async function createJob(
  token: string,
  job: NewJob,
): Promise<CreateJobOutcome> {
  try {
    const res = await fetch(`${DAEMON_URL}/jobs`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        project_id: job.projectId,
        prompt: job.prompt,
        budget_usd: job.budgetUsd ?? null,
        max_rounds: job.maxRounds ?? null,
      }),
    });
    if (!res.ok) {
      // The body is the reason, as plain text. An empty one is possible for a status raised by the
      // middleware rather than the handler, so there is a fallback — but it is a fallback, not the
      // usual path.
      const reason = (await res.text()).trim();
      return {
        ok: false,
        status: res.status,
        reason: reason === "" ? "The daemon refused this job and gave no reason." : reason,
      };
    }
    const data = await res.json();
    return { ok: true, jobId: data.job_id };
  } catch {
    return { ok: false, status: 0, reason: "The daemon is not reachable." };
  }
}

export async function getJob(
  token: string,
  id: number,
): Promise<JobDetail | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/jobs/${id}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return await res.json();
  } catch {
    return null;
  }
}

/**
 * Stops a whole job: the node in flight, and the sequence behind it.
 *
 * Distinct from cancelling a run, which stops one node. Both end the job — a stopped node leaves
 * the tree holding edits no gate measured, so the next item must not build on them — but only this
 * reaches a job with nothing running: one parked for the budget, waiting for the slot, or between
 * two nodes.
 *
 * A 409 means the job had already ended, which is a different answer from "stopped" and is why
 * this reports the status rather than a bare boolean.
 */
export async function cancelJob(
  token: string,
  id: number,
): Promise<ApiResult<null>> {
  try {
    const res = await fetch(`${DAEMON_URL}/jobs/${id}/cancel`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    return { ok: true, value: null };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
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
  /**
   * The standing decision about this sender: `"pin"`, `"mute"`, or null for none.
   *
   * It belongs to the contact rather than to this message, so two addresses the daemon has merged
   * into one person report the same thing. It is what the row's button is drawn from.
   */
  sender_verdict: string | null;
}

// ── The processes beside the daemon, and how it is configured ───────────────

/** One supervised sidecar, and what has happened to it since the daemon started. */
export interface SidecarState {
  name: string;
  /** `"running"`, or `"down"` while it backs off before the next attempt. */
  state: string;
  started_at: string | null;
  /** The last exit status or spawn error — kept even while it is running again. */
  last_failure: string | null;
  last_failure_at: string | null;
  restarts: number;
}

export async function getSidecars(token: string): Promise<SidecarState[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/sidecars`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as SidecarState[];
  } catch {
    return null;
  }
}

/** The email pillar's settings. No password: it never leaves Credential Manager and the sidecar. */
export interface EmailConfig {
  enabled: boolean;
  /** True only once the hook barrier was proven at startup. Enabled but unarmed is a real state. */
  armed: boolean;
  host: string;
  username: string;
  mailbox: string;
  sent_mailbox: string | null;
  poll_interval_secs: number;
  notify_classes: string[];
  digest_hour_utc: number;
  retain_bodies_days: number;
  local_triage_disabled: string | null;
}

export async function getEmailConfig(token: string): Promise<EmailConfig | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/config/email`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as EmailConfig;
  } catch {
    return null;
  }
}

// ── What a project does on its own ──────────────────────────────────────────

/** One scheduled rule, with what the daemon knows about it having run. */
export interface ScheduleView {
  name: string;
  cron: string;
  prompt: string;
  cwd: string | null;
  /** The IANA zone the cron is read in. null means UTC, which is what an absent field has always meant. */
  timezone: string | null;
  /** When it fires next, counted from the last time it did. null when it never will. */
  next_fire_at: string | null;
  /** Why it will never fire — a cron that does not parse, or a zone the daemon does not know. */
  problem: string | null;
  last_fired_at: string | null;
  fires_today: number;
  daily_cap: number;
}

/** One repo trigger, with the commit it last saw. */
export interface RepoTriggerView {
  name: string;
  branch: string;
  prompt: string;
  /** null means armed but with no first commit to compare against yet, which fires nothing. */
  last_sha: string | null;
}

/** Everything a project will do unasked, and everything currently holding it back. */
export interface ProjectRules {
  project_id: string;
  project_root: string | null;
  /** The three states `.ai/autopilot.yaml` can be in. */
  rules_file: "present" | "absent" | "unreadable";
  rules_error: string | null;
  gate_command: string | null;
  schedules: ScheduleView[];
  repo_triggers: RepoTriggerView[];
  /** The effective open-proposal ceiling. null means the brake is off. */
  wip_limit: number | null;
  open_proposals: number;
  queue_full: boolean;
}

export async function getProjectRules(
  token: string,
  projectId: string,
): Promise<ProjectRules | null> {
  try {
    const res = await fetch(
      `${DAEMON_URL}/projects/${encodeURIComponent(projectId)}/rules`,
      { headers: { Authorization: `Bearer ${token}` } },
    );
    if (!res.ok) return null;
    return (await res.json()) as ProjectRules;
  } catch {
    return null;
  }
}

/**
 * Sets how much unreviewed work a project may leave waiting before it stops starting more.
 *
 * `null` switches the brake off. The daemon refuses a negative ceiling: the comparison is
 * `open >= limit`, so a negative one means "never start anything again" while reading like a number
 * somebody chose.
 */
export async function setProjectWipLimit(
  token: string,
  projectId: string,
  limit: number | null,
): Promise<ApiResult<null>> {
  try {
    const res = await fetch(
      `${DAEMON_URL}/projects/${encodeURIComponent(projectId)}/wip-limit`,
      {
        method: "POST",
        headers: {
          Authorization: `Bearer ${token}`,
          "Content-Type": "application/json",
        },
        body: JSON.stringify({ limit }),
      },
    );
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    return { ok: true, value: null };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

/** One correspondent, as the daemon has come to know them. */
export interface Correspondent {
  address: string;
  /** Which person this address belongs to. Two rows sharing one id were merged by a human. */
  contact_id: number;
  /** `"human"` when someone approved joining this address to its contact, `"implicit"` otherwise. */
  linked_by: string;
  display_name: string | null;
  messages_in: number;
  /** 1 if you have ever written to them — what `priority.rs` uses to tell a stranger apart. */
  outbound_ever: number;
  first_seen: string;
  last_seen: string;
  /** The standing decision: `"pin"`, `"mute"`, or null. */
  verdict: string | null;
}

/**
 * Who writes to you, busiest first.
 *
 * One row per ADDRESS, not per person. The daemon can propose merging two addresses into one
 * contact but nothing in it performs that merge, so reporting a merged view would be reporting a
 * judgement nobody has made.
 */
export async function getContacts(token: string): Promise<Correspondent[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/contacts`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as Correspondent[];
  } catch {
    return null;
  }
}

/** One side of a suggested merge, named the way a person can recognise. */
export interface MergeSide {
  contact_id: number;
  addresses: string[];
  display_name: string | null;
  messages_in: number;
  /** The standing decision, so a conflict is visible before the button is pressed, not after. */
  verdict: string | null;
}

/** A pending suggestion that two addresses belong to one person. */
export interface MergeSuggestion {
  proposal_id: number;
  reasoning: string;
  created_at: string;
  /** The contact that survives the join. */
  keep: MergeSide;
  absorb: MergeSide;
}

/**
 * Merges the daemon has suggested and nobody has answered.
 *
 * Its own route rather than a slice of `/proposals`: an action approval is a paused run waiting for
 * a signature, this is a question about a mailbox, and they share nothing but a table.
 */
export async function getContactMerges(token: string): Promise<MergeSuggestion[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/contacts/merges`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as MergeSuggestion[];
  } catch {
    return null;
  }
}

/**
 * Answers a merge suggestion.
 *
 * Separate from `approveProposal`, which returns a resume run id and collapses every failure to
 * null. Neither fits here: a merge resumes nothing, and the 409 it can answer is the one refusal
 * that names something the person can go and fix — two contradictory standing decisions — so the
 * status has to survive.
 */
export async function decideContactMerge(
  token: string,
  proposalId: number,
  accept: boolean,
): Promise<ApiResult<null>> {
  const verb = accept ? "approve" : "reject";
  try {
    const res = await fetch(`${DAEMON_URL}/proposals/${proposalId}/${verb}`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
    });
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    return { ok: true, value: null };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

/**
 * Splits an address back out into a person of its own — the undo for an approved merge.
 *
 * The join is a pointer move, so undoing it restores exactly what was there, counters included.
 * The split address keeps whatever standing decision was governing it a moment earlier.
 */
export async function unmergeContact(
  token: string,
  address: string,
): Promise<ApiResult<null>> {
  try {
    const res = await fetch(`${DAEMON_URL}/contacts/unmerge`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ address }),
    });
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    return { ok: true, value: null };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

/** The two standing decisions a person can record about a sender. `priority.rs` knows only these. */
export const SENDER_VERDICTS = ["pin", "mute"] as const;
export type SenderVerdict = (typeof SENDER_VERDICTS)[number];

/**
 * Records what you have decided about a sender, for all their future mail.
 *
 * `null` withdraws the decision. The daemon refuses a verdict its policy does not know — `pin` and
 * `mute` are the whole vocabulary — and answers 404 for an address it has never received mail from,
 * because a contact exists only because a message arrived.
 *
 * This changes what happens NEXT. Mail already classified keeps the class it was given; re-reading
 * a message is what applies a new decision to it, which is why the page says so next to the button.
 */
export async function setSenderVerdict(
  token: string,
  address: string,
  verdict: SenderVerdict | null,
): Promise<ApiResult<null>> {
  try {
    const res = await fetch(`${DAEMON_URL}/contacts/verdict`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ address, verdict }),
    });
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    return { ok: true, value: null };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
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

/**
 * How the feed is narrowed. Every field is optional and an omitted one is not sent.
 *
 * The daemon treats the presence of ANY of `q`, `kind`, `since`, `until` or `limit` as the switch
 * between listing a scope and searching it — so sending an empty string for one of them is not the
 * same as leaving it out, and this only sends what was actually filled in.
 */
export interface FeedFilter {
  scope?: "all";
  projectId?: string;
  /** Matched against the entry's summary. */
  q?: string;
  kind?: string;
  /** RFC 3339. The daemon rejects anything else with a 400. */
  since?: string;
  until?: string;
  limit?: number;
}

function feedQuery(filter: FeedFilter): string {
  const params = new URLSearchParams();
  if (filter.scope === "all") params.set("scope", "all");
  else if (filter.projectId) params.set("project_id", filter.projectId);
  if (filter.q) params.set("q", filter.q);
  if (filter.kind) params.set("kind", filter.kind);
  if (filter.since) params.set("since", filter.since);
  if (filter.until) params.set("until", filter.until);
  if (filter.limit !== undefined) params.set("limit", String(filter.limit));
  const query = params.toString();
  return query === "" ? "" : `?${query}`;
}

export async function getFeed(
  token: string,
  opts: FeedFilter = {},
): Promise<FeedEntry[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/feed${feedQuery(opts)}`, {
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

/**
 * The items a job put down instead of finishing, waiting to be read.
 *
 * Shares `Proposal` with `getProposals` because the daemon hands back the same rows from the same
 * table — but the two lists are answered differently and must not be merged into one. A pending
 * `action-approval` is a run holding still until someone answers it; a skipped item is work that
 * already stopped, hours ago, with nothing waiting on the reply. Showing them together would put a
 * clock on half the list that does not apply to the other half.
 */
export async function getSkippedItems(
  token: string,
): Promise<Proposal[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/proposals/skipped-items`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return await res.json();
  } catch {
    return null;
  }
}

/**
 * Puts a read skipped item away.
 *
 * Deliberately NOT `rejectProposal` with a different label. `reject_proposal` guards on
 * `kind = 'action-approval'` and answers 409 for anything else, so pointing a "dismiss" button at
 * `/reject` would fail on exactly the rows it was drawn for.
 */
export async function dismissSkippedItem(
  token: string,
  id: number,
): Promise<boolean> {
  try {
    const res = await fetch(`${DAEMON_URL}/proposals/${id}/dismiss`, {
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

/**
 * One row of the git queue, as a listing shows it.
 *
 * `repo_key` travels alongside `project_id` rather than instead of it, and the shell shows both for
 * the reason `vcs.rs` gives: the project is the label a reader recognises, the key is the only thing
 * that says whether two differently-labelled rows were queued behind the same refs. A listing that
 * contains another project's work reads as a bug in the listing without it.
 *
 * `op` and `status` are the columns verbatim — the daemon does not parse them before handing them
 * over, precisely so a row written by an older version cannot fail the whole listing, and the shell
 * keeps that property by rendering whatever string arrives.
 */
export interface VcsRequestSummary {
  id: number;
  op: string;
  project_id: string;
  repo_key: string;
  origin: string;
  status: string;
  created_at: string;
}

/**
 * The whole git queue, newest first, optionally narrowed to one project.
 *
 * Note what this list is: nothing prunes `vcs_requests`, so it is the permanent history of every
 * git operation the daemon has queued, not a snapshot of what is pending. The daemon caps it at 200
 * rows. Hitting that cap means the daemon has been running a while — it is not a finding, and the
 * shell must not present it as one.
 */
export async function listVcsRequests(
  token: string,
  projectId?: string,
): Promise<VcsRequestSummary[] | null> {
  const path = projectId === undefined
    ? "/vcs/requests"
    : `/vcs/requests?project_id=${encodeURIComponent(projectId)}`;
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

/** How one request ended: the sha it produced, or why it did not. */
export interface VcsTicket {
  id: number;
  status: string;
  result_sha: string | null;
  failure_reason: string | null;
}

/**
 * One request's outcome, read with a zero deadline.
 *
 * The daemon also offers `/vcs/requests/{id}/wait`, which holds the connection open until the row
 * settles. The shell uses this one: it already polls, and a blocking read would tie a fetch up for
 * the whole of `vcs::DEFAULT_WAIT` while the rest of the page has nothing to show for it.
 */
export async function getVcsRequest(
  token: string,
  id: number,
): Promise<VcsTicket | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/vcs/requests/${id}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return await res.json();
  } catch {
    return null;
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

/** One entry in the files folder. */
export interface FileEntry {
  name: string;
  is_dir: boolean;
  /** Zero for a folder — its size is a different question, answered by walking it. */
  size_bytes: number;
  modified: string | null;
}

/**
 * What is in a folder under the files root. `path` empty means the root itself.
 *
 * Every call in this group reports the status rather than collapsing to null, because the Files tab
 * is a place where a refusal has to be readable: 409 after a delete means "this folder is not
 * empty" and 409 after a rename means "that name is taken", and a page that only knew "it failed"
 * would have to guess which.
 */
export async function listFiles(token: string, path = ""): Promise<ApiResult<FileEntry[]>> {
  try {
    const res = await fetch(`${DAEMON_URL}/files?path=${encodeURIComponent(path)}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return { ok: false, fault: faultForStatus(res.status), status: res.status };
    return { ok: true, value: (await res.json()) as FileEntry[] };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

/** One search hit: an entry, plus where it sits relative to the ROOT so a click can act on it. */
export interface FileHit extends FileEntry {
  path: string;
}

export interface FileSearch {
  hits: FileHit[];
  /** True when a ceiling cut the walk short. Shown, never swallowed. */
  truncated: boolean;
}

/** Finds entries by name in one folder and every folder under it. */
export async function searchFiles(
  token: string,
  path: string,
  q: string,
): Promise<ApiResult<FileSearch>> {
  try {
    const res = await fetch(
      `${DAEMON_URL}/files/search?path=${encodeURIComponent(path)}&q=${encodeURIComponent(q)}`,
      { headers: { Authorization: `Bearer ${token}` } },
    );
    if (!res.ok) return { ok: false, fault: faultForStatus(res.status), status: res.status };
    return { ok: true, value: (await res.json()) as FileSearch };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

export async function createFolder(token: string, path: string): Promise<ApiResult<null>> {
  try {
    const res = await fetch(`${DAEMON_URL}/files/folder`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ path }),
    });
    if (!res.ok) return { ok: false, fault: faultForStatus(res.status), status: res.status };
    return { ok: true, value: null };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

/**
 * Copies one of the user's own files into the folder, and reports the name it landed under.
 *
 * The name is not assumed for the same reason filing an attachment does not assume it: it is made
 * safe on the way to disk, and a collision is numbered rather than allowed to overwrite.
 */
export async function uploadFile(
  token: string,
  folder: string,
  file: File,
): Promise<ApiResult<string>> {
  try {
    const res = await fetch(
      `${DAEMON_URL}/files/upload?folder=${encodeURIComponent(folder)}&filename=${encodeURIComponent(file.name)}`,
      {
        method: "POST",
        headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/octet-stream" },
        body: file,
      },
    );
    if (!res.ok) return { ok: false, fault: faultForStatus(res.status), status: res.status };
    return { ok: true, value: ((await res.json()) as { filename: string }).filename };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

/** One file's bytes, for handing to the browser's own download. */
export async function downloadFile(token: string, path: string): Promise<Blob | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/files/download?path=${encodeURIComponent(path)}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return await res.blob();
  } catch {
    return null;
  }
}

/** Renames or moves one entry. Both are the same request with a different parent in `to`. */
export async function moveEntry(
  token: string,
  from: string,
  to: string,
): Promise<ApiResult<null>> {
  try {
    const res = await fetch(`${DAEMON_URL}/files/move`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ from, to }),
    });
    if (!res.ok) return { ok: false, fault: faultForStatus(res.status), status: res.status };
    return { ok: true, value: null };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

/**
 * Deletes a file, or a folder.
 *
 * `recursive` is the second word the daemon asks for before removing a folder that still has
 * something in it: without it that call answers 409, which is what lets the page say what is about
 * to go before asking again.
 */
export async function deleteEntry(
  token: string,
  path: string,
  recursive = false,
): Promise<ApiResult<null>> {
  try {
    const res = await fetch(
      `${DAEMON_URL}/files?path=${encodeURIComponent(path)}&recursive=${recursive}`,
      { method: "DELETE", headers: { Authorization: `Bearer ${token}` } },
    );
    if (!res.ok) return { ok: false, fault: faultForStatus(res.status), status: res.status };
    return { ok: true, value: null };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

/**
 * Files an attachment into the files folder.
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
  /**
   * How much of the model's context window the run has filled, in tokens.
   *
   * The daemon hands a run off to a successor at four fifths of its window (`handoff.rs`), so this
   * is the number that says a long run is approaching the point where it splits — the one fact
   * about a live run that predicts what it is about to do rather than reporting what it did.
   */
  context_fill: number | null;
  /**
   * Whether this run accepts `POST /runs/{id}/message`.
   *
   * Decided when the run was created and never afterwards, so it is a fact about the run rather
   * than about whether a channel currently happens to exist. A `false` here is why the composer is
   * absent, not merely disabled: nothing the person can do would make this run listen.
   */
  steerable: boolean;
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
  /**
   * Ask for a run that can be spoken to after it starts.
   *
   * Opt-in, and deliberately so on the daemon's side too: `steerable` defaults to false for every
   * caller that omits it, so a run only listens because someone asked for one that would. The
   * daemon refuses the combination outright for a mode that launches without tools.
   */
  steerable: boolean;
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

/**
 * Says one more turn to a run that is already working.
 *
 * The status is kept because the refusals are not interchangeable and the person can act on the
 * difference: 409 is "this run isn't listening — either it never opted in, or it has already
 * finished", 403 is "this run may never be spoken to", and both are ordinary answers rather than
 * faults. The daemon replies 202, not 200: the text reached the run's channel, and the run reads it
 * when it next reads stdin.
 */
export async function steerRun(
  token: string,
  id: number,
  message: string,
): Promise<ApiResult<null>> {
  try {
    const res = await fetch(`${DAEMON_URL}/runs/${id}/message`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ message }),
    });
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    return { ok: true, value: null };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

/**
 * Tells a steerable run that no more turns are coming, which is what lets it finish.
 *
 * A run launched to listen reads until its stdin closes, and the daemon holds that stdin open for as
 * long as it holds the run's channel — so without this a conversation could only end by going quiet
 * long enough to trip the progress deadline, and would be recorded `timed_out` for having waited.
 *
 * `true` for any run that exists, whether or not it was still listening: the daemon treats an
 * already-closed channel as the state the caller asked for, and so does this.
 */
export async function endRunTurns(token: string, id: number): Promise<boolean> {
  try {
    const res = await fetch(`${DAEMON_URL}/runs/${id}/message`, {
      method: "DELETE",
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

/** One exchange as the daemon remembers it, which is what makes a conversation outlive the window. */
export interface AssistantTurnRow {
  id: number;
  asked: string;
  /** The reply, or null while the turn is still running or if it produced nothing. */
  answer: string | null;
  /** What it failed with, when it failed. Shown rather than left as an empty bubble. */
  error: string | null;
  status: string;
  cost_usd: number | null;
  created_at: string;
}

/**
 * A chat's turns, oldest first.
 *
 * The daemon has always kept these — a turn is a run — but until `chat_id` was recorded on the row
 * there was no way to ask for one conversation's worth. `/runs?mode=assistant` is not a substitute:
 * that is every chat at once, the Telegram sidecar's turns included.
 */
export async function getAssistantChat(
  token: string,
  chatId: string,
): Promise<AssistantTurnRow[] | null> {
  try {
    const res = await fetch(
      `${DAEMON_URL}/assistant/chats/${encodeURIComponent(chatId)}`,
      { headers: { Authorization: `Bearer ${token}` } },
    );
    if (!res.ok) return null;
    return (await res.json()) as AssistantTurnRow[];
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

/** One outgoing message. Three fields, because the daemon accepts three — see `mailsend.rs`. */
export interface SendEmailInput {
  to: string;
  subject: string;
  body: string;
}

/**
 * Why a message did not go.
 *
 * Four answers rather than one `false`, and the split that matters most is not between the fixable
 * ones — it is **whether anything was attempted**. `invalid` and `unconfigured` and `unreachable`
 * all mean the bytes never left this process, so "it was not sent" is a fact. `undelivered` means
 * the sidecar was asked, and this side cannot honestly promise anything about what happened next.
 * For the one act in this daemon that cannot be undone, collapsing those two would be telling the
 * person a thing we do not know.
 *
 * `mailsend.rs` draws the same line, in the same place, for the same reason.
 */
export type SendFailure = {
  kind: "invalid" | "unconfigured" | "unreachable" | "undelivered";
  /** The daemon's own sentence, which names the field or the file. Never invented here. */
  reason: string;
};

/**
 * Sends one message under the mailbox owner's own address.
 *
 * The only call in this client that cannot be undone, and the shape of its failures is copied
 * deliberately from `mailsend.rs` rather than collapsed: a 503 means the bytes never left this
 * process, while a 502 means the sidecar was asked and something went wrong there. Telling the
 * caller "it did not go" in both cases would be a guess in the second one.
 */
export async function sendEmail(
  token: string,
  message: SendEmailInput,
): Promise<true | SendFailure> {
  try {
    const res = await fetch(`${DAEMON_URL}/email/send`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify(message),
    });
    if (res.ok) return true;
    const reason = (await res.text()) || `HTTP ${res.status}`;
    if (res.status === 400) return { kind: "invalid", reason };
    if (res.status === 503) return { kind: "unconfigured", reason };
    // Everything else is reported as attempted, including a 500 that in fact means the daemon never
    // built a client. Erring towards "this may have gone" is the safe direction for a send and the
    // unsafe one for nothing else here.
    return { kind: "undelivered", reason };
  } catch {
    // No response at all, so the request never reached the daemon: nothing was attempted, and that
    // is a stronger and more useful statement than the 502 above.
    return { kind: "unreachable", reason: "the daemon is not reachable" };
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
  /**
   * The two chords to register, as configured in `.ai/voice.yaml`.
   *
   * They arrive from the daemon rather than being decided here because that file is where they are
   * set and the shell may not read it. An empty string means the key is unset and nothing is
   * registered for it.
   */
  hotkey: string;
  memo_hotkey: string;
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

export interface VoiceCaptureResult {
  /** `0` means the text is good but no row was written, so there is nothing to fetch later. */
  id: number;
  text: string;
  state: VoiceCleanupState;
}

/**
 * Sends a finished recording as raw WAV bytes.
 *
 * Posted from here rather than from the shell's Rust side, and not by preference: capture has to
 * happen in the webview, and Tauri's IPC serialises arguments as JSON — handing twenty minutes of
 * samples over would mean roughly 58 million JSON numbers. The daemon is on localhost either way.
 *
 * `"silent"` is a distinct answer, not a failure. The daemon replies 204 when the transcriber heard
 * nothing, and telling someone their microphone picked up nothing is different from telling them
 * dictation is broken.
 */
export async function postVoiceCapture(
  token: string,
  kind: VoiceKind,
  durationMs: number,
  wav: Uint8Array,
): Promise<VoiceCaptureResult | "silent" | "failed"> {
  try {
    const res = await fetch(
      `${DAEMON_URL}/voice/capture?kind=${kind}&duration_ms=${durationMs}`,
      {
        method: "POST",
        headers: {
          Authorization: `Bearer ${token}`,
          "Content-Type": "application/octet-stream",
        },
        body: new Uint8Array(wav) as BodyInit,
      },
    );
    if (res.status === 204) return "silent";
    if (!res.ok) return "failed";
    return (await res.json()) as VoiceCaptureResult;
  } catch {
    return "failed";
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
 * The dictations, which are the other half of what the microphone produced.
 *
 * Same row shape as a memo and a separate list, because the two are separate surfaces in the daemon
 * too: memos and dictations draw from one id sequence, and `/voice/memos/{id}` answers 404 for a
 * dictation's id on purpose. There is deliberately no delete here — `delete_memo` guards on
 * `Kind::Memo`, so a delete button on this list would 404 on every row.
 *
 * `voice.rs` calls this route one read "by hand for prompt tuning, not by the shell". That was true
 * while nothing rendered it; what a dictation shows is how the transcript was cleaned up, which is
 * the only place the cleanup model's work is visible at all.
 */
export async function listVoiceDictations(token: string): Promise<VoiceCapture[] | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/voice/dictations`, {
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

// ── Calendar ──────────────────────────────────────────────────────────────────
//
// The daemon expands recurrence itself, so the shell asks for a WINDOW and gets
// flat occurrences back. It never sees a rule, which is deliberate: "the third
// Thursday" is a question with two daylight-saving answers, and there must be
// exactly one place in the system that decides it.

export interface CalendarOccurrence {
  event_id: number;
  title: string;
  /** `human` for something you created, `proposal` for something you approved. */
  source: string;
  /** The original local start — the handle used to cancel or move this one occurrence. */
  occurrence_local: string;
  starts_at: string;
  ends_at: string;
}

export interface CalendarConfigView {
  default_tz: string;
  working_hours_start: string;
  working_hours_end: string;
  working_weekdays: string[];
}

export interface PendingNotification {
  id: number;
  kind: string;
  summary: string;
  queued_at: string;
  /** Null while still held. Delivered rows are kept so "did it ever arrive?" is answerable. */
  delivered_at: string | null;
}

export async function getCalendarConfig(token: string): Promise<CalendarConfigView | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/calendar/config`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as CalendarConfigView;
  } catch {
    return null;
  }
}

export async function getCalendarEvents(
  token: string,
  from: Date,
  to: Date,
): Promise<CalendarOccurrence[]> {
  try {
    const query = new URLSearchParams({
      from: from.toISOString(),
      to: to.toISOString(),
    });
    const res = await fetch(`${DAEMON_URL}/calendar/events?${query}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return [];
    return (await res.json()) as CalendarOccurrence[];
  } catch {
    return [];
  }
}

export async function getCalendarBusy(token: string): Promise<boolean | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/calendar/busy`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return ((await res.json()) as { busy: boolean }).busy;
  } catch {
    return null;
  }
}

export interface NewCalendarEvent {
  title: string;
  /** Local wall clock, no offset: `2026-08-03T09:00:00`. */
  starts_at_local: string;
  duration_minutes: number;
  tz?: string;
  freq?: "daily" | "weekly" | "monthly";
  interval?: number;
  byday?: string;
  count?: number;
}

/** Returns the reason on refusal, so a 400 can say which field the daemon rejected. */
export async function createCalendarEvent(
  token: string,
  event: NewCalendarEvent,
): Promise<{ ok: true; id: number } | { ok: false; reason: string }> {
  try {
    const res = await fetch(`${DAEMON_URL}/calendar/events`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify(event),
    });
    if (!res.ok) return { ok: false, reason: (await res.text()) || `HTTP ${res.status}` };
    return { ok: true, id: ((await res.json()) as { id: number }).id };
  } catch {
    return { ok: false, reason: "the daemon is not reachable" };
  }
}

/** Deletes the whole series. One occurrence is `cancelCalendarOccurrence`. */
export async function deleteCalendarEvent(token: string, id: number): Promise<boolean> {
  try {
    const res = await fetch(`${DAEMON_URL}/calendar/events/${id}`, {
      method: "DELETE",
      headers: { Authorization: `Bearer ${token}` },
    });
    return res.ok;
  } catch {
    return false;
  }
}

export async function cancelCalendarOccurrence(
  token: string,
  id: number,
  occurrenceLocal: string,
): Promise<boolean> {
  try {
    const res = await fetch(`${DAEMON_URL}/calendar/events/${id}/cancel`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ occurrence_local: occurrenceLocal }),
    });
    return res.ok;
  } catch {
    return false;
  }
}

/**
 * Relocates one occurrence, leaving the rest of the series where it was.
 *
 * `durationMinutes` is required by the daemon, which rejects a non-positive value — a moved
 * occurrence writes a whole exception row, and an exception with no length is not a shorter event
 * but an unreadable one. The caller passes the occurrence's current length, so "move" means move
 * and nothing else.
 */
export async function moveCalendarOccurrence(
  token: string,
  id: number,
  occurrenceLocal: string,
  toLocal: string,
  durationMinutes: number,
): Promise<boolean> {
  try {
    const res = await fetch(`${DAEMON_URL}/calendar/events/${id}/move`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify({
        occurrence_local: occurrenceLocal,
        to_local: toLocal,
        duration_minutes: durationMinutes,
      }),
    });
    return res.ok;
  } catch {
    return false;
  }
}

export async function listPendingNotifications(token: string): Promise<PendingNotification[]> {
  try {
    const res = await fetch(`${DAEMON_URL}/notifications/pending`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return [];
    return (await res.json()) as PendingNotification[];
  } catch {
    return [];
  }
}

/** One page this machine has already read. */
export interface WebHit {
  id: number;
  final_url: string;
  host: string;
  title: string | null;
  snippet: string;
  /**
   * `raw` or `quarantined` — the trust this page was fetched UNDER.
   *
   * It is a record and not a permission: the daemon remakes the decision on every read, so a badge
   * here says what happened once, never what an agent would get now.
   */
  trust_at_fetch: string;
  fetched_at: string;
}

/**
 * The archive of what NucleOS has read, or a search of it.
 *
 * `q` searches the FTS5 index; absent, it lists newest-first.
 */
export async function listWebPages(
  token: string,
  q = "",
  limit = 50,
): Promise<WebHit[] | null> {
  const query = q.trim().length > 0 ? `?q=${encodeURIComponent(q)}&limit=${limit}` : `?limit=${limit}`;
  try {
    const res = await fetch(`${DAEMON_URL}/web/pages${query}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as WebHit[];
  } catch {
    return null;
  }
}

/** One stored page, in full. */
export interface WebPage {
  id: number;
  requested_url: string;
  final_url: string;
  host: string;
  title: string | null;
  byline: string | null;
  content_md: string;
  extract_status: string;
  trust_at_fetch: string;
  trust_rule: string;
  bytes: number;
  fetched_at: string;
}

export async function getWebPage(token: string, id: number): Promise<WebPage | null> {
  try {
    const res = await fetch(`${DAEMON_URL}/web/pages/${id}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (!res.ok) return null;
    return (await res.json()) as WebPage;
  } catch {
    return null;
  }
}

/** One destination the provider offered. Nothing has been fetched — these are links, not reads. */
export interface WebSearchResult {
  title: string;
  url: string;
  snippet: string;
}

/**
 * `cached` first, then the internet — the order the pillar is meant to be used in, and the order
 * the daemon's own response shape puts them in.
 *
 * `cached` is `WebHit`, the same rows `listWebPages` returns, because it is the same `web::Hit` from
 * the same index on the daemon's side. A parallel type would let the two drift apart while the
 * server kept sending one shape.
 *
 * `provider` is worth showing rather than swallowing: the local index answers even when no provider
 * is configured, so hits with an empty `results` is a working search on an installation with no API
 * key, not a failure.
 */
export interface WebSearchView {
  cached: WebHit[];
  provider: string;
  results: WebSearchResult[];
}

/**
 * Searches what has been read, and the internet beside it.
 *
 * `ApiResult` rather than `null`, because the pillar being switched off is the answer this call
 * gets most often on a fresh install and it is not an error — `/web/search` answers 503 when
 * `.ai/web.yaml` has not enabled it, and "turn the pillar on" is a different sentence from "the
 * search failed".
 */
export async function searchWeb(
  token: string,
  query: string,
  limit?: number,
): Promise<ApiResult<WebSearchView>> {
  try {
    const res = await fetch(`${DAEMON_URL}/web/search`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ query, limit: limit ?? null }),
    });
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    return { ok: true, value: (await res.json()) as WebSearchView };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}

/**
 * What a read hands back. Close to `WebPage` and deliberately not the same type: this one carries
 * `from_cache`, which only a read can answer, and lacks `bytes` and `byline`, which only the stored
 * row has. Aliasing them would put fields on screen that are absent from the payload.
 */
export interface WebReadView {
  id: number;
  requested_url: string;
  final_url: string;
  host: string;
  title: string | null;
  trust: string;
  trust_rule: string;
  extract_status: string;
  content_md: string;
  from_cache: boolean;
  fetched_at: string;
}

/**
 * Fetches one page by URL and files it in the archive.
 *
 * Admin-scoped in the daemon, unlike `/web/search` — see `auth.rs`. The shell holds the control
 * token so it is allowed, but the 403 is worth surfacing rather than flattening: an installation
 * driving the shell with a lesser API token would otherwise see a read silently do nothing.
 *
 * Who is asking is derived by the daemon from owner presence and never sent from here. A
 * `requester` field on the wire would be a permission the caller grants itself.
 */
export async function readWebPage(
  token: string,
  url: string,
): Promise<ApiResult<WebReadView>> {
  try {
    const res = await fetch(`${DAEMON_URL}/web/read`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ url }),
    });
    if (!res.ok) {
      return { ok: false, fault: faultForStatus(res.status), status: res.status };
    }
    return { ok: true, value: (await res.json()) as WebReadView };
  } catch {
    return { ok: false, fault: "unreachable", status: 0 };
  }
}
