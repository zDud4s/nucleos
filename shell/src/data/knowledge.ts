import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";

/**
 * What the agent knows, as hooks.
 *
 * **This store is invisible from every other page, by construction.** Something
 * waiting for an answer is a `kind = 'refinement'` proposal, and
 * `proposals::list_pending` filters `kind = 'action-approval'`
 * (`core/src/proposals.rs`) — so the Waiting queue, which is the page for
 * things that stopped to ask you something, cannot see one. The listing that
 * can is `GET /knowledge`, and it answers every status rather than only the
 * pending ones, because the reviewable history is the whole point of the table.
 *
 * **`refinement` is still the proposal's kind, and that is not an oversight.**
 * It is a value already written to rows on disk that `0143_knowledge.sql` does
 * not rewrite; renaming it would orphan every question still waiting for an
 * answer. The store is `knowledge`; the question about it kept its old word.
 *
 * Three routes, read off `core/src/http.rs` rather than inferred from the
 * names:
 *
 * | what                | route                          |
 * |---------------------|--------------------------------|
 * | the store, all of it| `GET /knowledge`               |
 * | one, with its chain | `GET /knowledge/{id}`          |
 * | take an active one back | `POST /knowledge/{id}/revert` |
 *
 * The two decisions — yes and no — are **not** here: they go through the
 * ordinary proposal doors (`POST /proposals/{id}/approve` and `/reject`), which
 * dispatch on the proposal's kind. That is why the row carries `proposal_id`.
 *
 * `POLL.queue`: a store only changes when a person decides something or a run
 * declares something, and five seconds is soon enough for a list worked through
 * by hand.
 */

/* ----------------------------------------------------------------- shapes -- */

/** The four kinds, in the order a node reads them — `knowledge::Kind`. */
export type KnownKind = "prompt" | "memory" | "skill" | "subagent";

/**
 * The nature of what is known — `knowledge::Layer`.
 *
 * The column `0143_knowledge.sql` added, and the one that makes this one store
 * rather than four: a fact about the project, a measurement of what happened,
 * how work is done here, and what one job knows while it runs.
 */
export type KnownLayer = "semantic" | "episodic" | "procedural" | "working";

/**
 * Whose it is. Two columns rather than one, because a `job` is not
 * a project and a single nullable `project_id` could not say so.
 */
export type KnownScope = "machine" | "project" | "job";

/**
 * A row's status.
 *
 * Nine and not five: `0088`'s five said everything a person decides, and the
 * four `0143` adds say what the store does to itself — merged into a lesson,
 * closed with its job, stopped being confirmed, or live inside one job's run.
 * `active` still means exactly one thing, which is why `live` exists at all.
 */
export type KnownStatus =
  | "proposed"
  | "active"
  | "rejected"
  | "reverted"
  | "superseded"
  | "archived"
  | "closed"
  | "expired"
  | "live";

export const EVIDENCE_TAGS = [
  "run",
  "job",
  "job_item",
  "proposal",
  "knowledge",
  "project",
  "command",
  "gate",
] as const;

export type EvidenceTag = (typeof EVIDENCE_TAGS)[number];

export interface EvidenceRef {
  t: EvidenceTag;
  id: number | string;
}

/** One thing the agent knows, as `knowledge::Known` serialises. */
export interface Known {
  id: number;
  layer: KnownLayer;
  scope_kind: KnownScope;
  /** `null` only for `machine`: a lesson about the house rather than about a repo. */
  scope_id: string | null;
  /** Who knocked at the door — never read from the body. */
  source: "owner" | "run" | "consolidator" | "distiller";
  generator: string | null;
  kind: KnownKind;
  title: string;
  body: string;
  status: KnownStatus;
  /** The question that let it in. The door a decision is sent through. */
  proposal_id: number | null;
  /** The row this one replaces, ended when this one was approved. */
  supersedes: number | null;
  origin_run_id: number | null;
  evidence: string | null;
  observations: number | null;
  fingerprint: string | null;
  expires_after_runs: number | null;
  last_confirmed_at: string | null;
  shown_count: number;
  outcome_count: number;
  green_count: number;
  last_shown_at: string | null;
  created_at: string;
  activated_at: string | null;
  ended_at: string | null;
}

/**
 * Read the tagged evidence JSON without letting a stale or future tag invent a
 * destination in the shell. Malformed JSON is absence; known entries survive
 * independently of malformed neighbours.
 */
export function parseEvidence(raw: string | null): EvidenceRef[] {
  if (raw === null) return [];

  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];

  const tags: ReadonlySet<string> = new Set(EVIDENCE_TAGS);
  return parsed.flatMap((item): EvidenceRef[] => {
    if (typeof item !== "object" || item === null) return [];
    const candidate = item as { t?: unknown; id?: unknown };
    if (typeof candidate.t !== "string" || !tags.has(candidate.t)) return [];
    if (typeof candidate.id !== "string" && typeof candidate.id !== "number") return [];
    return [{ t: candidate.t as EvidenceTag, id: candidate.id }];
  });
}

/** Why the distiller wrote a row, in the words /learned shows — `distill::Cause`'s wire names. */
export const DISTILL_CAUSE_LABELS: Record<string, string> = {
  job_landed: "the job landed",
  job_failed: "the job failed",
  gate_recovered: "a red gate recovered",
  review_blocking: "a review blocked it",
  run_exhausted: "a run ran out of attempts",
};

/** Where a distilled row came from: its job, the runs behind it, and why it was written. */
export interface DistilledOrigin {
  job: number | string | null;
  runs: (number | string)[];
  /** The raw cause as the daemon answered it, `null` when none is on record. */
  cause: string | null;
  /** The cause in words; the raw cause itself when this shell does not know it. */
  causeLabel: string | null;
}

/**
 * The origin of a row the distiller wrote, `null` for every other row.
 *
 * Pure: the causes arrive as a map because `Known` does not carry `distill_cause`
 * (`GET /distill/causes` serves it apart). An unknown cause shows as itself, so a
 * cause a newer daemon invents is readable rather than hidden.
 */
export function distilledOrigin(
  row: Known,
  causes: ReadonlyMap<number, string>,
): DistilledOrigin | null {
  if (row.source !== "distiller") return null;

  const refs = parseEvidence(row.evidence);
  const cause = causes.get(row.id) ?? null;
  return {
    job: refs.find((ref) => ref.t === "job")?.id ?? null,
    runs: refs.filter((ref) => ref.t === "run").map((ref) => ref.id),
    cause,
    causeLabel: cause === null ? null : (DISTILL_CAUSE_LABELS[cause] ?? cause),
  };
}

export interface MeasuredScope {
  scope: string;
  total: number;
  byGenerator: { generator: string; count: number }[];
}

export interface WaitingGroup {
  key: string;
  scope: string;
  source: Known["source"];
  rows: Known[];
  proposalIds: number[];
}

/** Proposed rows, collected by the scope and source a person decides together. */
export function groupWaiting(rows: Known[]): WaitingGroup[] {
  const groups = new Map<string, WaitingGroup>();

  for (const row of rows) {
    if (row.status !== "proposed") continue;
    const key = `${row.scope_kind}|${row.scope_id ?? ""}|${row.source}`;
    const group = groups.get(key) ?? {
      key,
      scope: row.scope_id ?? "this machine",
      source: row.source,
      rows: [],
      proposalIds: [],
    };
    group.rows.push(row);
    groups.set(key, group);
  }

  return [...groups.values()]
    .map((group) => {
      const sorted = [...group.rows].sort((left, right) => left.id - right.id);
      return {
        ...group,
        rows: sorted,
        proposalIds: sorted.flatMap((row) =>
          row.proposal_id === null ? [] : [row.proposal_id],
        ),
      };
    })
    .sort((left, right) => left.rows[0].id - right.rows[0].id);
}

/** Active measurements, grouped into the number spec 13.1 asks a person to watch. */
export function measuredByGenerator(rows: Known[]): MeasuredScope[] {
  const scopes = new Map<string, Map<string, number>>();

  for (const row of rows) {
    if (
      row.status !== "active" ||
      row.layer !== "episodic" ||
      row.source !== "consolidator"
    ) {
      continue;
    }
    const scope = row.scope_id ?? "this machine";
    const generator = row.generator ?? "unknown";
    const counts = scopes.get(scope) ?? new Map<string, number>();
    counts.set(generator, (counts.get(generator) ?? 0) + 1);
    scopes.set(scope, counts);
  }

  return [...scopes.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([scope, counts]) => ({
      scope,
      total: [...counts.values()].reduce((sum, count) => sum + count, 0),
      byGenerator: [...counts.entries()]
        .sort(([left], [right]) => left.localeCompare(right))
        .map(([generator, count]) => ({ generator, count })),
    }));
}

/** One decision in a row's life — `knowledge::Event`. */
export interface KnowledgeEvent {
  id: number;
  from_status: KnownStatus | null;
  to_status: KnownStatus;
  note: string | null;
  at: string;
}

/** One row, its decisions, and the chain on both sides — `knowledge::History`. */
export interface KnowledgeHistory {
  known: Known;
  events: KnowledgeEvent[];
  /** Newest first: what this replaced, then what THAT replaced. */
  replaced: Known[];
  replaced_by: Known | null;
}

/* ------------------------------------------------------------------ reads -- */

/** Everything the store holds, in every status. */
export function useKnowledge() {
  return useQuery({
    queryKey: keys.knowledge.all,
    queryFn: () => apiFetch<Known[]>("/knowledge"),
    refetchInterval: POLL.queue,
  });
}

/** The cause of every distilled row, by row id — `GET /distill/causes`. */
export function useDistillCauses() {
  return useQuery({
    queryKey: keys.knowledge.distillCauses,
    queryFn: () => apiFetch<{ id: number; cause: string }[]>("/distill/causes"),
    refetchInterval: POLL.queue,
    // A daemon that does not serve the route answers nothing usable: no causes, not an error.
    select: (rows): ReadonlyMap<number, string> =>
      new Map(Array.isArray(rows) ? rows.map((row) => [row.id, row.cause] as const) : []),
  });
}

/**
 * One row's chain and decisions, fetched only when somebody opens it.
 *
 * `enabled` rather than a prefetch: the chain is a second request per row, and
 * a page that fetched one for every row would make reading a list of forty cost
 * forty-one requests to answer a question nobody asked yet.
 */
export function useKnowledgeHistory(id: number | null) {
  return useQuery({
    queryKey: keys.knowledge.detail(id ?? 0),
    queryFn: () => apiFetch<KnowledgeHistory>(`/knowledge/${id}`),
    enabled: id !== null,
  });
}

/* -------------------------------------------------------------- decisions -- */

/**
 * Every list a decision can move, invalidated together.
 *
 * The detail cache comes too: approving a successor changes the *predecessor's*
 * row — it ends it — so a chain left on screen from before the decision would
 * show a superseded text as still in force.
 */
function useKnowledgeDecision<Input, Result>(mutationFn: (input: Input) => Promise<Result>) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn,
    // A decision is settled. A retried approval is a second attempt at letting
    // something into every later prompt that a person allowed once.
    retry: false,
    // `onSettled`, not `onSuccess`: a 409 means somebody decided this while it
    // sat on screen, and the list is wrong either way.
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.knowledge.all });
      void queryClient.invalidateQueries({ queryKey: keys.proposals.all });
      // The Brain graph and list read notes and knowledge together.
      void queryClient.invalidateQueries({ queryKey: keys.ownerNotes.all });
    },
  });
}

/**
 * Yes — and only now does anything reach a prompt.
 *
 * Sent to the proposal, not to the row: `post_proposal_approve` dispatches on
 * kind and `knowledge::approve` activates the row, ends whatever it supersedes
 * and decides the question in one transaction.
 */
export function useApproveKnowledge() {
  return useKnowledgeDecision((proposalId: number) =>
    apiFetch<{ refinement_id: number }>(`/proposals/${proposalId}/approve`, { method: "POST" }),
  );
}

/** No — kept as a refusal rather than as an absence. 204, so no body to read. */
export function useRejectKnowledge() {
  return useKnowledgeDecision((proposalId: number) =>
    apiFetch<void>(`/proposals/${proposalId}/reject`, { method: "POST" }),
  );
}

export interface BatchResult {
  done: number;
  failed: number[];
}

/** Decide a visible group through the existing proposal doors, one row at a time. */
export function useDecideKnowledgeBatch() {
  return useKnowledgeDecision(
    async ({
      action,
      proposalIds,
    }: {
      action: "approve" | "reject";
      proposalIds: number[];
    }): Promise<BatchResult> => {
      let done = 0;
      const failed: number[] = [];

      for (const id of [...proposalIds].sort((left, right) => left - right)) {
        try {
          if (action === "approve") {
            await apiFetch<{ refinement_id: number }>(`/proposals/${id}/approve`, {
              method: "POST",
            });
          } else {
            await apiFetch<void>(`/proposals/${id}/reject`, { method: "POST" });
          }
          done += 1;
        } catch {
          failed.push(id);
        }
      }

      return { done, failed };
    },
  );
}

/**
 * Take back one that is in force.
 *
 * Aimed at the row and not at a proposal: the question was answered months ago,
 * and what is being changed now is the store rather than a decision. 409 when
 * the row is not active — already reverted, or superseded by a later text —
 * which is a different thing for a reader to do about than a 404.
 */
export function useRevertKnowledge() {
  return useKnowledgeDecision((id: number) =>
    apiFetch<void>(`/knowledge/${id}/revert`, { method: "POST", body: JSON.stringify({}) }),
  );
}
