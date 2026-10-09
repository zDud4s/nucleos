import type { ReactNode } from "react";
import { screen } from "@testing-library/react";
import type { Known, KnowledgeHistory } from "../../data/knowledge";
import { KnownRow } from "./KnownRow";
import { WaitingPanel } from "./WaitingPanel";
import { MeasuredSummary } from "./MeasuredSummary";
import { renderWithRouter } from "../../test/harness";

/** The rows the last `daemonWith` call was given, so a test renders what the daemon holds. */
const lastRows: { rows: Known[] } = { rows: [] };

export function known(over: Partial<Known> = {}): Known {
  return {
    id: 1,
    layer: "semantic",
    scope_kind: "project",
    scope_id: "nucleos",
    source: "run",
    generator: null,
    kind: "memory",
    title: "The suite needs Git's usr/bin on PATH",
    body: "Nine tests spawn echo as a program.",
    status: "active",
    proposal_id: 7,
    supersedes: null,
    origin_run_id: 900001,
    evidence: null,
    observations: null,
    fingerprint: null,
    expires_after_runs: null,
    last_confirmed_at: null,
    shown_count: 0,
    outcome_count: 0,
    green_count: 0,
    last_shown_at: null,
    created_at: "2026-08-19T09:00:00+00:00",
    activated_at: "2026-08-19T09:05:00+00:00",
    ended_at: null,
    ...over,
  };
}

/** A daemon holding exactly this much, and answering the detail route from it. */
export function daemonWith(
  rows: Known[],
  history?: Partial<KnowledgeHistory>,
  causes: { id: number; cause: string }[] = [],
  duplicates: { id: number; of_id: number }[] = [],
) {
  lastRows.rows = rows;
  return (path: string) => {
    if (path === "/distill/duplicates") return Promise.resolve(duplicates);
    if (path === "/distill/causes") return Promise.resolve(causes);
    const listed = knowledgeAnswer(path, rows);
    if (listed !== undefined) return Promise.resolve(listed);
    if (path.startsWith("/knowledge/")) {
      const id = Number(path.split("/")[2]);
      return Promise.resolve({
        known: rows.find((row) => row.id === id) ?? rows[0],
        events: [],
        replaced: [],
        replaced_by: null,
        ...history,
      });
    }
    return Promise.resolve(undefined);
  };
}

/** The `<section>` a panel's own heading belongs to, so an assertion can be scoped to one. */
export async function panelFor(headingText: string | RegExp): Promise<HTMLElement> {
  const heading = await screen.findByRole("heading", { level: 2, name: headingText });
  const panel = heading.closest("section");
  if (panel === null) throw new Error(`no panel section found for heading "${String(headingText)}"`);
  return panel as HTMLElement;
}

/** Every row the daemon holds, each as a bare KnownRow. */
export function renderRows(): ReturnType<typeof renderWithRouter> {
  const list: ReactNode = (
    <>
      {lastRows.rows.map((row) => (
        <KnownRow key={row.id} row={row} />
      ))}
    </>
  );
  return renderWithRouter(list);
}

/** The proposed rows the daemon holds, in a WaitingPanel. */
export function renderWaiting(): ReturnType<typeof renderWithRouter> {
  return renderWithRouter(
    <WaitingPanel rows={lastRows.rows.filter((row) => row.status === "proposed")} />,
  );
}

/** The measured summary of everything the daemon holds. */
export function renderMeasured(): ReturnType<typeof renderWithRouter> {
  return renderWithRouter(<MeasuredSummary rows={lastRows.rows} />);
}

/**
 * What `GET /knowledge` answers for `path`: every row, or — narrowed with
 * `?scope_kind=&scope_id=` — that one scope's rows, the way the núcleo filters
 * them. `undefined` when `path` is not a knowledge listing at all.
 */
export function knowledgeAnswer(path: string, rows: Known[]): Known[] | undefined {
  if (path === "/knowledge") return rows;
  if (!path.startsWith("/knowledge?")) return undefined;
  const query = new URLSearchParams(path.slice("/knowledge?".length));
  return rows.filter(
    (row) => row.scope_kind === query.get("scope_kind") && row.scope_id === query.get("scope_id"),
  );
}
