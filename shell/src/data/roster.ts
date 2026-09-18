import type { ProjectForgets, ProjectHolds } from "./projects";
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
 * Five ranks, in the order somebody can act on them:
 *
 * 0. **The folder is a problem** — missing or never named. Nothing this project does can run, so
 *    every other reading about it is stale by definition.
 * 1. **The gate said no.** The code is broken; that is the one verdict that means it.
 * 2. **Something is waiting on a person.** Work that has stopped, and only a human restarts it.
 * 3. Everything else.
 * 4. **It is switched off**, which is nobody asking it for anything.
 *
 * **The last rank is checked first, and that is the point.** Every rank above it says *this needs
 * somebody now*, and none of them can be true of a project the owner turned off. The version this
 * replaces put a dormant project at the very top of the page for having no folder — which is not a
 * fault in something switched off, it is what switched off looks like — and pushed the one that was
 * acting, failing its gate and holding two decisions underneath it. Nothing is hidden by the move:
 * the row still carries every reading it carried before, in the same columns. Only the shouting
 * stops.
 */
export function rankOf(project: ProjectSummary): number {
  if (project.mode === "off") return 4;
  if (folderOf(project) !== "ok") return 0;
  if (gateOf(project) === "failed") return 1;
  if (project.open_review_items > 0) return 2;
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
    const waiting = b.open_review_items - a.open_review_items;
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
 *
 * **`rankOf` growing a fifth rank deliberately did not change this.** Off having stopped being a
 * reason to shout does not make a switched-off project's absent folder untrue, and the line and the
 * order are answering two different questions — one tallies what is out there, the other says who
 * needs somebody first. Filtering the folder counts to live projects was tried and reverted: it
 * would have made this line stop counting what the rows count, which is the one property the
 * paragraph above exists to defend. What the row says about that folder is the row's to soften.
 */
export function headline(rows: ProjectSummary[]): string {
  if (rows.length === 0) return "the núcleo knows of no project";

  const parts = [`${rows.length} ${rows.length === 1 ? "project" : "projects"}`];
  const active = rows.filter((row) => row.mode === "active").length;
  const missing = rows.filter((row) => folderOf(row) === "missing").length;
  const unset = rows.filter((row) => folderOf(row) === "unset").length;
  const failing = rows.filter((row) => gateOf(row) === "failed").length;
  const waiting = rows.reduce((total, row) => total + row.open_review_items, 0);

  if (active > 0) parts.push(`${active} acting`);
  if (missing > 0) parts.push(`${missing} with the folder gone`);
  if (unset > 0) parts.push(`${unset} with no folder named`);
  if (failing > 0) parts.push(`${failing} failing the gate`);
  // The number is `open_review_items`, which is proposals and shadow decisions together, so the
  // word cannot be "proposal": master renamed the field precisely because the two are not the same.
  if (waiting > 0) parts.push(`${waiting} item${waiting === 1 ? "" : "s"} to review`);

  return parts.join(" · ");
}

/**
 * Where a project's waiting items actually are, as a phrase, or `null` when it cannot be said.
 *
 * **The total was the whole bug report.** A project whose brake was holding showed a full queue and
 * an empty proposals list, and both were right: every waiting item was an unreviewed shadow
 * decision, which lives on another screen. Measured on `nucleos` — 0 proposals, 13 shadow
 * decisions. The word "review" pointed at the one page that could never clear it.
 *
 * `null` when the daemon is older than this shell and served no split, so the caller keeps saying
 * only the total rather than claiming a zero it was not told.
 */
export function whereWaiting(project: {
  open_proposals?: number;
  open_shadow_decisions?: number;
}): string | null {
  const { open_proposals: proposals, open_shadow_decisions: shadow } = project;
  if (proposals === undefined || shadow === undefined) return null;
  const parts: string[] = [];
  if (proposals > 0) parts.push(`${proposals} ${proposals === 1 ? "proposal" : "proposals"}`);
  if (shadow > 0) parts.push(`${shadow} shadow ${shadow === 1 ? "decision" : "decisions"}`);
  if (parts.length === 0) return null;
  return parts.join(" and ");
}

/* ------------------------------------------------------------------ the exit -- */

/**
 * What a project has on record, as a phrase.
 *
 * **The checkbox is the reason this exists.** "Forget the history too" over no number is not a
 * decision somebody can take — it asks them to agree to lose an amount they cannot see. So the
 * counts come back from the daemon and are read out beside it, in the nouns the app's own screens
 * use.
 *
 * Only non-zero facts, for the reason {@link headline} gives about itself: a phrase that walked
 * through seven zeros to reach one number would bury the number. `null` when there is genuinely
 * nothing, which is a different answer again — a project with no record has nothing to forget, and
 * the checkbox should not be offered at all.
 */
const RECORD_NOUNS: [keyof ProjectForgets, string, string][] = [
  ["runs", "run", "runs"],
  ["jobs", "job", "jobs"],
  ["proposals", "proposal", "proposals"],
  ["decisions", "decision", "decisions"],
  ["stamps", "stamp", "stamps"],
  ["commands", "command", "commands"],
  ["feed", "feed entry", "feed entries"],
];

export function onRecord(forgets: ProjectForgets): string | null {
  const parts = RECORD_NOUNS.filter(([field]) => forgets[field] > 0).map(
    ([field, one, many]) => `${forgets[field]} ${forgets[field] === 1 ? one : many}`,
  );
  return words(parts);
}

/**
 * What is still going on here, as a phrase — or `null` when the answer is nothing.
 *
 * **Read before the button is pressed, and that is the point.** The daemon refuses a removal while
 * work is in flight and writes its own sentence about it, in the past tense: *nothing was removed*.
 * This is the other moment — the control is open, the person has not pressed anything yet, and what
 * they need is why the button is off. Two sentences because they are two tenses; a shell that only
 * had the daemon's would have to let somebody press a button in order to be told they could not.
 */
export function heldBy(holds: ProjectHolds): string | null {
  const parts: string[] = [];
  if (holds.slots > 0) parts.push(`${holds.slots} ${holds.slots === 1 ? "slot" : "slots"} in flight`);
  if (holds.worktrees > 0) {
    parts.push(`${holds.worktrees} ${holds.worktrees === 1 ? "worktree" : "worktrees"} checked out`);
  }
  return words(parts);
}

/** `a`, `a and b`, `a, b and c` — the last join is a word, because a list read aloud has one. */
function words(parts: string[]): string | null {
  if (parts.length === 0) return null;
  if (parts.length === 1) return parts[0];
  return `${parts.slice(0, -1).join(", ")} and ${parts[parts.length - 1]}`;
}
