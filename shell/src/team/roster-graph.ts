import type { Agent } from "../data/agents";
import type { TeamRunView, TeamView } from "../data/teams";

/**
 * A department read as an org chart: who directs, who is on the roster, and what each of them is
 * holding right now.
 *
 * Pure — no React, no DOM, no fetch. The tab that draws it (`Roster.tsx`) is a hundred lines of
 * SVG over what this returns, which is the only way the interesting half of the feature is
 * testable without a browser.
 *
 * **Three layers and not two.** The roster on its own is a membership, and `RosterMatrix.tsx`
 * already settled that a membership reads better as a matrix than as a graph. What earns a graph
 * here is the third layer: work in flight hangs off the person holding it, and that is a relation
 * a matrix has no cell for.
 */

export type RosterLayer = 0 | 1 | 2;

/**
 * One box.
 *
 * `state` is a `string` and not a union on purpose, for the reason `Work.tsx:253` is written the
 * way it is: the daemon's five item states (`done`, `working`, `planned`, `failed`, `skipped`) are
 * what the drawing has tones and glyphs for, plus the two this module synthesises for people —
 * `directs` for the one at the top and `idle` for everybody else. Anything the daemon adds later
 * arrives here as its own word, renders untoned, and is not silently reported as something else.
 *
 * `crosses` is the director's own work: parented on layer 0 and drawn on layer 2, so its edge is
 * the one that skips a rank. It is flagged rather than routed differently — the drawing dashes it.
 */
export interface RosterNode {
  id: string;
  layer: RosterLayer;
  label: string;
  said: string;
  state: string;
  parent: string | null;
  href: string | null;
  missing: boolean;
  crosses: boolean;
}

export interface RosterInput {
  team: TeamView;
  agents: Agent[];
  /** Every department, so a specialist can be told which others it serves. */
  teams: TeamView[];
  /** Only the LIVE runs of this department. A finished run is history, not work in flight. */
  runs: TeamRunView[];
}

export interface RosterModel {
  nodes: RosterNode[];
}

/** What the one at the top is doing, rather than only that it is at the top. */
function directorSaid(newest: TeamRunView | undefined): string {
  if (newest === undefined) return "no task in flight";
  // `none` is a real value on the wire and reads as nothing at all in a box.
  return newest.director_node === "none" ? "between rounds" : newest.director_node;
}

/**
 * The one line under a specialist's name.
 *
 * Order matters: a specialist the catalogue no longer has is the fact that outranks every other,
 * because everything else said about them — including which departments they are still listed in
 * — is describing somebody who is gone.
 */
function memberSaid(id: string, who: Agent | undefined, team: TeamView, teams: TeamView[]): string {
  if (who === undefined) return "deleted from the catalogue";

  const elsewhere = teams
    .filter((other) => other.id !== team.id)
    .filter((other) => other.members.includes(id) || other.director_agent_id === id)
    .map((other) => other.name);
  if (elsewhere.length > 0) return `also in ${elsewhere.join(", ")}`;

  return who.speciality.trim() === "" ? "no speciality recorded" : who.speciality;
}

/**
 * Facts into nodes, in an order that does not depend on the order they arrived in.
 *
 * The list is rebuilt on every ten-second poll. An order that followed the wire's would reshuffle
 * the picture under whoever is reading it, so members sort by label and work sorts by (run,
 * ordinal) — both total, both stable.
 */
export function buildRoster(input: RosterInput): RosterModel {
  const { team, agents, teams, runs } = input;

  const catalogue = new Map(agents.map((who) => [who.id, who]));
  const directorId = team.director_agent_id;
  const directorNode = `director:${directorId}`;
  const director = catalogue.get(directorId);

  const newest = [...runs].sort((a, b) => b.created_at.localeCompare(a.created_at))[0];

  const nodes: RosterNode[] = [
    {
      id: directorNode,
      layer: 0,
      label: director?.name ?? directorId,
      said: director === undefined ? "deleted from the catalogue" : directorSaid(newest),
      state: "directs",
      parent: null,
      href: null,
      missing: director === undefined,
      crosses: false,
    },
  ];

  const roster = new Set(team.members);
  const members: RosterNode[] = team.members
    .filter((id) => id !== directorId)
    .map((id) => {
      const who = catalogue.get(id);
      return {
        id: `member:${id}`,
        layer: 1 as const,
        label: who?.name ?? id,
        said: memberSaid(id, who, team, teams),
        state: "idle",
        parent: directorNode,
        href: null,
        missing: who === undefined,
        crosses: false,
      };
    })
    .sort((a, b) => a.label.localeCompare(b.label));
  nodes.push(...members);

  const work = runs
    .flatMap((run) => run.items.map((item) => ({ run, item })))
    .sort((a, b) => a.run.id.localeCompare(b.run.id) || a.item.ordinal - b.item.ordinal);

  for (const { run, item } of work) {
    // An item held by somebody who is not on this roster hangs off the director for the same
    // reason the director's own does: the chart has one place to put work with no column of its
    // own, and pretending it belongs to a specialist who never had it is the worse answer.
    const onRoster = roster.has(item.agent_id) && item.agent_id !== directorId;
    nodes.push({
      id: `item:${run.id}#${item.ordinal}`,
      layer: 2,
      label: item.description,
      said: `round ${item.round} · ${run.request}`,
      state: item.state,
      parent: onRoster ? `member:${item.agent_id}` : directorNode,
      href: `/team-runs/${run.id}`,
      missing: false,
      crosses: !onRoster,
    });
  }

  return { nodes };
}
