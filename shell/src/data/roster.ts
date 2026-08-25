import type { ProjectSummary } from "./system";

/**
 * What the roster page has to decide before it can draw a row, kept pure so it can be argued with.
 *
 * The page's question is **"how are all of them doing"**, and with twenty-five projects the answer
 * is not a list of names — it is which ones need somebody. That is an ordering and a set of
 * readings, both of which are arithmetic, and arithmetic belongs where a test can reach it without
 * a browser.
 */

/**
 * What a project's folder is, in the three states the daemon can distinguish.
 *
 * **Never two.** `missing` is a folder that was named and is not there — something broke, and until
 * it is fixed nothing this project does can run. `unset` is a project nobody has pointed anywhere:
 * not broken, unfinished, and the fix is on the Autopilot page rather than on a disk. Collapsing
 * them puts a person hunting a moved folder into a settings screen, and vice versa.
 */
export type Folder = "ok" | "missing" | "unset";

export function folderOf(project: ProjectSummary): Folder {
  if (project.root_exists === true) return "ok";
  if (project.root_exists === false) return "missing";
  return "unset";
}

/**
 * What the last gate said, reduced to the four things it can mean.
 *
 * `none` is not a fourth kind of failure: `0032_run_gate.sql` spends its comment on exactly this —
 * a project with no gate command has no definition of green, so there is nothing to report and
 * saying "passed" would invent a check that never ran.
 */
export type Gate = "passed" | "failed" | "errored" | "none";

export function gateOf(project: ProjectSummary): Gate {
  switch (project.last_gate) {
    case "passed":
      return "passed";
    case "failed":
      return "failed";
    case "errored":
      return "errored";
    default:
      return "none";
  }
}

/**
 * How far up the page a project belongs.
 *
 * **The order IS the page's answer**, which is why it is fixed rather than left to whichever column
 * somebody clicked last. A roster sorted by name answers "where is X", and the sidebar already
 * answers that — alphabetically, one click away, on every page inside this area. What no other
 * surface answers is which of twenty-five projects is broken, and a table that hid that under `A`
 * for `ANSup` would be the wall of chips again with straighter edges.
 *
 * Four ranks, in the order somebody can act on them:
 *
 * 0. **The folder is a problem** — missing or never named. Nothing this project does can run, so
 *    every other reading about it is stale by definition.
 * 1. **The gate said no.** The code is broken; that is the one verdict that means it.
 * 2. **Something is waiting on a person.** Work that has stopped, and only a human restarts it.
 * 3. Everything else.
 */
export function rankOf(project: ProjectSummary): number {
  if (folderOf(project) !== "ok") return 0;
  if (gateOf(project) === "failed") return 1;
  if (project.open_proposals > 0) return 2;
  return 3;
}

/**
 * The roster in the order the page draws it.
 *
 * Rank first, then the amount waiting descending — within "somebody must decide", the one with 736
 * decisions outstanding is not the same size of problem as the one with 2 — then the name, so the
 * order is total and a re-render never reshuffles equal rows.
 *
 * Copies rather than sorting in place: the array belongs to react-query's cache, and sorting it
 * would mutate what every other reader of that query sees.
 */
export function inAttentionOrder(rows: ProjectSummary[]): ProjectSummary[] {
  return [...rows].sort((a, b) => {
    const rank = rankOf(a) - rankOf(b);
    if (rank !== 0) return rank;
    const waiting = b.open_proposals - a.open_proposals;
    if (waiting !== 0) return waiting;
    return a.project_id.localeCompare(b.project_id);
  });
}

/**
 * The line under the title: how many, and what is wrong with them.
 *
 * **It counts what the rows count.** The version this replaces counted projects with a *recorded
 * root* and called it "all with a folder", while every row beneath it probed the folder for real —
 * so the page could say "25 projects, all with a folder" above seven rows saying `folder gone`.
 * One source now answers both.
 *
 * Only non-zero facts are mentioned. "25 projects · 0 broken" makes somebody read a number to learn
 * nothing, and a page that reports its own good news gets skimmed.
 */
export function headline(rows: ProjectSummary[]): string {
  if (rows.length === 0) return "the núcleo knows of no project";

  const parts = [`${rows.length} ${rows.length === 1 ? "project" : "projects"}`];
  const active = rows.filter((row) => row.mode === "active").length;
  const missing = rows.filter((row) => folderOf(row) === "missing").length;
  const unset = rows.filter((row) => folderOf(row) === "unset").length;
  const failing = rows.filter((row) => gateOf(row) === "failed").length;
  const waiting = rows.reduce((total, row) => total + row.open_proposals, 0);

  if (active > 0) parts.push(`${active} acting`);
  if (missing > 0) parts.push(`${missing} with the folder gone`);
  if (unset > 0) parts.push(`${unset} with no folder named`);
  if (failing > 0) parts.push(`${failing} failing the gate`);
  if (waiting > 0) parts.push(`${waiting} waiting on you`);

  return parts.join(" · ");
}
