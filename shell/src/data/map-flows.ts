// §spec atlas-sequence
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/** One declared step of a flow, as the núcleo verified it against the code. */
export interface FlowStep {
  participant: string;
  path: string;
  fn: string;
  /** `null` is a direct call; `"<METHOD> /path"` an HTTP hop; `"async ..."` a queue hop. */
  via: string | null;
  /** Index of the calling step when it is not the previous one. */
  from: number | null;
  line: number | null;
  ok: boolean;
  reason: string | null;
}

export interface FlowCheck {
  name: string;
  stale: boolean;
  broken_step: number | null;
  steps: FlowStep[];
}

export interface MapFlows {
  file: string;
  declared: boolean;
  root: string;
  flows: FlowCheck[];
  error: string | null;
}

const NO_FLOWS: MapFlows = {
  file: ".nucleos/flows.yaml",
  declared: false,
  root: "",
  flows: [],
  error: null,
};

/**
 * Read the answer defensively. `apiFetch<T>` is only a cast, and the preview daemon and the
 * tests answer unknown GETs with `[]`, so anything that is not an object with a `flows` array is
 * read as "no flows" and never thrown.
 */
export function readFlows(raw: unknown): MapFlows {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return { ...NO_FLOWS };
  const answer = raw as Partial<MapFlows>;
  if (!Array.isArray(answer.flows)) return { ...NO_FLOWS };
  return answer as MapFlows;
}

export function useMapFlows(projectId: string | null) {
  return useQuery({
    queryKey: [...keys.projects.map(projectId ?? ""), "flows"] as const,
    queryFn: async () =>
      readFlows(
        await apiFetch<unknown>(`/projects/${encodeURIComponent(projectId ?? "")}/map/flows`),
      ),
    enabled: projectId !== null && projectId !== "",
  });
}

export type ArrowKind = "call" | "http" | "async";

export interface SequenceArrow {
  step: number;
  from: number;
  to: number;
  label: string;
  kind: ArrowKind;
  broken: boolean;
  /** After the broken step: never checked. */
  unchecked: boolean;
}

export interface SequenceLayout {
  participants: string[];
  arrows: SequenceArrow[];
}

/** Lifelines in first-appearance order, one arrow per step after the first. */
export function layoutSequence(flow: FlowCheck): SequenceLayout {
  const participants: string[] = [];
  for (const step of flow.steps) {
    if (!participants.includes(step.participant)) participants.push(step.participant);
  }
  const arrows: SequenceArrow[] = [];
  flow.steps.forEach((step, i) => {
    if (i === 0) return;
    const caller = flow.steps[step.from ?? i - 1] ?? flow.steps[i - 1];
    const via = step.via;
    arrows.push({
      step: i,
      from: participants.indexOf(caller.participant),
      to: participants.indexOf(step.participant),
      label: step.fn,
      kind: via === null ? "call" : via.startsWith("async") ? "async" : "http",
      broken: i === flow.broken_step,
      unchecked: flow.broken_step !== null && i > flow.broken_step,
    });
  });
  return { participants, arrows };
}
