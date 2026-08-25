import type { TeamGrant, TeamView } from "../data/teams";

/**
 * The guard against firing somebody by saving a form.
 *
 * `PUT /teams/{id}` is a full replace: `replace_roster` does a `DELETE`
 * followed by re-insertion (`core/src/team.rs:427`), so the roster and the
 * grants go whole or they go away. And a department can grow its own roster
 * without anybody opening this form — approving an `agent-recruit` inserts
 * straight into `team_members` (`core/src/team.rs:1749`).
 *
 * Which makes this sequence lose data, silently, with nobody doing anything
 * wrong:
 *
 * | 09:41 | The Charter form is seeded with four members             |
 * | 09:52 | A recruit is hired on the Decisions tab; the núcleo makes it five |
 * | 09:58 | The form is saved and re-sends the four it was seeded with |
 * | 09:58 | `replace_roster` deletes five and re-inserts four — the new hire is gone |
 *
 * The fix is not to re-seed the form on every poll: that would overwrite a
 * half-typed edit, which is why the seeding guard exists in the first place.
 * The fix is to **re-read at submit and compare with the seed**, and to raise
 * only what changed underneath a field the person did not touch. If they edited
 * the roster on purpose, removing somebody is their decision and there is
 * nothing to ask about.
 *
 * Pure on purpose — `(seed, current, touched)` in, a list out. No React, no
 * fetch, no component to mount. The failure this prevents is a data loss nobody
 * would see happen, so it is the one part of the tab that must be provable
 * without a browser.
 */

/** The fields `PUT /teams/{id}` replaces, by the name the form knows them by. */
export type DriftField =
  | "name"
  | "mission"
  | "directorAgentId"
  | "maxRounds"
  | "maxParallel"
  | "budgetUsd"
  | "maxOpenActions"
  | "maxLiveRuns"
  | "members"
  | "grants";

/**
 * A department as the form holds it, with numbers as numbers.
 *
 * Not the form state itself: that keeps the ceiling boxes as strings, because a
 * half-typed number is a string and coercing it while somebody types is how a
 * field fights back. Comparison happens on the parsed values, so `"4"` and `4`
 * are the same reading and a trailing space is not a drift.
 */
export interface TeamSnapshot {
  name: string;
  mission: string;
  directorAgentId: string;
  maxRounds: number;
  maxParallel: number;
  budgetUsd: number | null;
  maxOpenActions: number;
  maxLiveRuns: number;
  members: string[];
  grants: TeamGrant[];
}

/** One field the daemon changed under a form that was not editing it. */
export interface Drift {
  field: DriftField;
  /** What the field is called on screen, so the notice can be written from this alone. */
  label: string;
  /** What the form was seeded with, in words. */
  was: string;
  /** What the daemon holds now, in words. */
  now: string;
}

const LABELS: Record<DriftField, string> = {
  name: "Name",
  mission: "Mission",
  directorAgentId: "Director",
  maxRounds: "Max rounds",
  maxParallel: "Max parallel",
  budgetUsd: "Budget ceiling",
  maxOpenActions: "Max open actions",
  maxLiveRuns: "Max live runs",
  members: "Staff",
  grants: "Powers",
};

/** The daemon's answer, in the shape the comparison works on. */
export function snapshotFromView(team: TeamView): TeamSnapshot {
  return {
    name: team.name,
    mission: team.mission,
    directorAgentId: team.director_agent_id,
    maxRounds: team.max_rounds,
    maxParallel: team.max_parallel,
    budgetUsd: team.budget_usd,
    maxOpenActions: team.max_open_actions,
    maxLiveRuns: team.max_live_runs,
    members: team.members,
    grants: team.grants,
  };
}

/**
 * The roster as an order-free reading.
 *
 * `replace_roster` re-inserts with `INSERT OR IGNORE` and the read comes back
 * ordered by the daemon, so the order of `members` is not a fact anybody set.
 * Comparing the arrays as written would report a drift every time two members
 * came back the other way round, and a guard that cries wolf is a guard people
 * click through.
 */
function rosterKey(members: readonly string[]): string {
  return [...new Set(members)].sort().join(", ");
}

/**
 * The grants as an order-free reading, one line per kind.
 *
 * Same argument as the roster, plus one more: the absence of a row IS the
 * denial — there is no `deny` mode — so a kind that is missing from both sides
 * must read the same as a kind that is missing from both sides, and never as
 * two different empty strings.
 */
function grantsKey(grants: readonly TeamGrant[]): string {
  const byKind = new Map<string, string>();
  for (const grant of grants) byKind.set(grant.kind, grant.mode);
  return [...byKind.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([kind, mode]) => `${kind}: ${mode}`)
    .join(", ");
}

/** How each field is written into the notice, and how it is compared. */
function reading(snapshot: TeamSnapshot, field: DriftField): string {
  switch (field) {
    case "members":
      return rosterKey(snapshot.members) === "" ? "nobody" : rosterKey(snapshot.members);
    case "grants":
      return grantsKey(snapshot.grants) === "" ? "nothing" : grantsKey(snapshot.grants);
    case "budgetUsd":
      // Absent is not zero, here as everywhere else in this pillar.
      return snapshot.budgetUsd === null ? "no ceiling" : `$${snapshot.budgetUsd.toFixed(2)}`;
    default: {
      const value = snapshot[field];
      return String(value);
    }
  }
}

const FIELDS: DriftField[] = [
  "name",
  "mission",
  "directorAgentId",
  "maxRounds",
  "maxParallel",
  "budgetUsd",
  "maxOpenActions",
  "maxLiveRuns",
  "members",
  "grants",
];

/**
 * What changed under the form, among the fields nobody was editing.
 *
 * Empty means save with a clear conscience. Non-empty means ask — and the three
 * answers the caller must offer are *take theirs*, *reload the form* and *save
 * mine anyway*. None of them is "save quietly", which is the state this
 * function exists to remove.
 *
 * `touched` is the set of fields the person changed. A field they touched is
 * theirs: the whole point of the form is that they get to decide, including
 * deciding to remove somebody who was added a minute ago.
 */
export function detectDrift(
  seed: TeamSnapshot,
  current: TeamSnapshot,
  touched: Iterable<DriftField>,
): Drift[] {
  const edited = new Set(touched);
  const drifted: Drift[] = [];

  for (const field of FIELDS) {
    if (edited.has(field)) continue;
    const was = reading(seed, field);
    const now = reading(current, field);
    if (was === now) continue;
    drifted.push({ field, label: LABELS[field], was, now });
  }

  return drifted;
}
