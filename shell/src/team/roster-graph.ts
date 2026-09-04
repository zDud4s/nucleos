import { CHAR_W, GAP_X, MIN_W, PAD_X, wrap } from "../canvas/layered";
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

/* ------------------------------------------------------------------ layout -- */

/**
 * One height per rank. A person's box holds a name and a line under it; a piece of work holds a
 * description over two lines, the task it belongs to, and its state.
 */
export const LAYER_H: Record<RosterLayer, number> = { 0: 56, 1: 54, 2: 72 };

/** The white band between two ranks. Wide enough that an edge reads as a line and not a join. */
const ROW_GAP = 46;

/** Above this many characters a line is folded. Two lines at most, then it is clipped. */
const MAX_CHARS = 30;

export interface RosterBox {
  id: string;
  parent: string | null;
  x: number;
  y: number;
  width: number;
  height: number;
  lines: string[];
}

export interface RosterEdge {
  from: string;
  to: string;
  crosses: boolean;
}

export interface RosterLayout {
  boxes: RosterBox[];
  edges: RosterEdge[];
  width: number;
  height: number;
}

/** A label over at most two lines, clipped rather than allowed to grow the box. */
export function fold(label: string, max = MAX_CHARS): string[] {
  if (label.length <= max) return [label];
  // No spaces to break on: this is an identifier, and `layered.ts` already knows where those split.
  if (!label.includes(" ")) return wrap(label);

  const lines: string[] = [];
  let line = "";
  for (const word of label.split(/\s+/)) {
    if (line === "") line = word;
    else if (`${line} ${word}`.length <= max) line = `${line} ${word}`;
    else {
      lines.push(line);
      line = word;
    }
  }
  if (line !== "") lines.push(line);

  if (lines.length <= 2) return lines;
  return [lines[0], `${lines[1].slice(0, max - 1)}…`];
}

/** One line trimmed to fit rather than folded — the second line of a box is never a third. */
export function clip(text: string, max = MAX_CHARS): string {
  return text.length <= max ? text : `${text.slice(0, max - 1)}…`;
}

const rowY = (layer: RosterLayer): number =>
  layer === 0 ? 0 : layer === 1 ? LAYER_H[0] + ROW_GAP : LAYER_H[0] + LAYER_H[1] + 2 * ROW_GAP;

/** As wide as the widest thing written in it — `layered.ts`' rule, over this box's own two lines. */
function boxWidth(lines: string[], said: string): number {
  const longest = Math.max(...lines.map((line) => line.length), clip(said).length);
  return Math.max(MIN_W, Math.round(longest * CHAR_W + PAD_X));
}

/**
 * Nodes into coordinates: a tree walked down, then across.
 *
 * **Not `layered.ts`'s `layout()`**, for the two reasons the spec gives at §5. `rankNodes` would
 * rank the director's own item onto the specialists' row, because rank there is distance from a
 * source and this item's parent is the director; and `NODE_H` is one height for every rank, while
 * a person's box and a piece of work's box are not the same size.
 *
 * Two passes, which is all a tree needs. Bottom up, a node's span is the wider of its own box and
 * its children laid side by side. Top down, each node is handed its span and centred in it, and
 * its children divide it in order — so a parent always sits over the middle of its children and
 * nothing can overlap, because no two spans intersect.
 */
export function placeRoster(nodes: RosterNode[]): RosterLayout {
  if (nodes.length === 0) return { boxes: [], edges: [], width: 0, height: 0 };

  const byId = new Map(nodes.map((node) => [node.id, node]));
  const children = new Map<string, RosterNode[]>();
  const roots: RosterNode[] = [];
  for (const node of nodes) {
    // A parent nobody kept is a root: the picture still draws, rather than losing the box.
    if (node.parent === null || !byId.has(node.parent)) {
      roots.push(node);
      continue;
    }
    const kin = children.get(node.parent);
    if (kin === undefined) children.set(node.parent, [node]);
    else kin.push(node);
  }

  const lines = new Map<string, string[]>();
  const own = new Map<string, number>();
  for (const node of nodes) {
    const folded = fold(node.label);
    lines.set(node.id, folded);
    own.set(node.id, boxWidth(folded, node.said));
  }

  const span = new Map<string, number>();
  const measure = (node: RosterNode): number => {
    const kin = children.get(node.id) ?? [];
    const across =
      kin.reduce((total, kid) => total + measure(kid), 0) + GAP_X * Math.max(0, kin.length - 1);
    const width = Math.max(own.get(node.id) ?? MIN_W, across);
    span.set(node.id, width);
    return width;
  };
  for (const root of roots) measure(root);

  const boxes: RosterBox[] = [];
  const edges: RosterEdge[] = [];

  const place = (node: RosterNode, left: number): void => {
    const slot = span.get(node.id) ?? MIN_W;
    const width = own.get(node.id) ?? MIN_W;
    boxes.push({
      id: node.id,
      parent: node.parent,
      x: left + (slot - width) / 2,
      y: rowY(node.layer),
      width,
      height: LAYER_H[node.layer],
      lines: lines.get(node.id) ?? [node.label],
    });

    const kin = children.get(node.id) ?? [];
    const across =
      kin.reduce((total, kid) => total + (span.get(kid.id) ?? MIN_W), 0) +
      GAP_X * Math.max(0, kin.length - 1);
    let x = left + (slot - across) / 2;
    for (const kid of kin) {
      edges.push({ from: node.id, to: kid.id, crosses: kid.crosses });
      place(kid, x);
      x += (span.get(kid.id) ?? MIN_W) + GAP_X;
    }
  };

  let x = 0;
  for (const root of roots) {
    place(root, x);
    x += (span.get(root.id) ?? MIN_W) + GAP_X;
  }

  return {
    boxes,
    edges,
    width: Math.max(...boxes.map((box) => box.x + box.width)),
    height: Math.max(...boxes.map((box) => box.y + box.height)),
  };
}
