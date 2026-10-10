import { GAP_X, MIN_W, wrap } from "../canvas/layered";
import type { Agent } from "../data/agents";
import type { TeamRunView, TeamView } from "../data/teams";

/**
 * A department read as an org chart: what it is, who runs it, who is on it, and — when there is
 * any — what each of them is holding right now.
 *
 * Pure — no React, no DOM, no fetch. The tab that draws it (`Roster.tsx`) is SVG over what this
 * returns, which is the only way the interesting half of the feature is testable without a browser.
 *
 * **The structure is the subject; the work is an extra.** The first version of this had three
 * ranks — director, roster, work in flight — and the work rank was what made it a graph. Measured
 * against a real idle department (`Vendas`, nothing running) it drew three boxes and the sentence
 * "nothing in flight", and said nothing at all about how the department is put together. So the
 * standing facts are ranks of their own and are always drawn: the department with the limits it
 * runs under, then its director, then its roster. Work in flight is a fourth rank that exists
 * when there is work and is simply absent when there is not.
 *
 * **Why a graph at all, when `RosterMatrix.tsx` decided a roster is a matrix.** That decision was
 * about the whole house — nine specialists against six departments, a membership, which reads
 * better as a grid. One department is a containment and not a membership: the department holds a
 * director, the director holds a roster, a specialist holds work. Containment is a tree, and a
 * tree is a graph.
 */

/** 0 the department · 1 its director · 2 its roster · 3 the work in flight. */
export type RosterLayer = 0 | 1 | 2 | 3;

/**
 * One box.
 *
 * `state` is a `string` and not a union on purpose, for the reason `Work.tsx:253` is written the
 * way it is: the daemon's four item states (`done`, `running`, `pending`, `failed`) are
 * what the drawing has tones and glyphs for, plus the three this module synthesises — `holds` for
 * the department, `directs` for the one at the top and `idle` for everybody else. Anything the
 * daemon adds later arrives here as its own word, renders untoned, and is not silently reported as
 * something else.
 *
 * `said` is the one line under the name. `facts` are the lines under THAT, and only the department
 * box has any: the limits it runs under are four numbers, and four numbers do not fit on the line
 * that says how many people are on it.
 *
 * `crosses` is work that hangs off something other than the specialist who holds it — the
 * director's own item, or an item given to somebody who is on no roster. Its edge skips a rank, so
 * the drawing routes it rather than curving it.
 */
export interface RosterNode {
  id: string;
  layer: RosterLayer;
  label: string;
  said: string;
  facts: string[];
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

/**
 * The limits a department runs under, over two lines.
 *
 * These live in the Charter as form fields, which is where they are CHANGED. This is where they
 * are read, and a number somebody can only see by opening the form that edits it is a number
 * nobody checks. `budget_usd` is `null` for a department with no ceiling of its own, and null is
 * not zero — printing `$0.00` there would say the opposite of what it means.
 */
function limitsOf(team: TeamView): string[] {
  const ceiling =
    team.budget_usd === null ? "no ceiling of its own" : `$${team.budget_usd.toFixed(2)} ceiling`;
  return [
    `${team.max_rounds} rounds · ${team.max_parallel} at a time`,
    `${ceiling} · ${team.max_open_actions} open actions`,
  ];
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
  const teamNode = `team:${team.id}`;
  const newest = [...runs].sort((a, b) => b.created_at.localeCompare(a.created_at))[0];

  const roster = team.members.filter((id) => id !== team.director_agent_id);

  const nodes: RosterNode[] = [
    {
      id: teamNode,
      layer: 0,
      label: team.name,
      said: roster.length === 1 ? "1 on the roster" : `${roster.length} on the roster`,
      facts: limitsOf(team),
      state: "holds",
      parent: null,
      href: null,
      missing: false,
      crosses: false,
    },
  ];

  /*
    A department can have no director at all — `Operações` in the preview fixtures is one, created
    and never staffed. Emitting a nameless box for it was the first version's real defect: the
    chart drew an empty rectangle where a person should be, which reads as a rendering fault
    rather than as the fact that nobody has been put in charge. So the rank is simply absent, and
    the tab says so in words below the chart.
  */
  const directorId = team.director_agent_id;
  const director = directorId === "" ? undefined : catalogue.get(directorId);
  const directorNode = directorId === "" ? null : `director:${directorId}`;
  if (directorNode !== null) {
    nodes.push({
      id: directorNode,
      layer: 1,
      label: director?.name ?? directorId,
      said: director === undefined ? "deleted from the catalogue" : directorSaid(newest),
      facts: [],
      state: "directs",
      parent: teamNode,
      href: null,
      missing: director === undefined,
      crosses: false,
    });
  }

  const members: RosterNode[] = roster
    .map((id) => {
      const who = catalogue.get(id);
      return {
        id: `member:${id}`,
        layer: 2 as const,
        label: who?.name ?? id,
        said: memberSaid(id, who, team, teams),
        facts: [],
        state: "idle",
        parent: directorNode ?? teamNode,
        href: null,
        missing: who === undefined,
        crosses: false,
      };
    })
    .sort((a, b) => a.label.localeCompare(b.label));
  nodes.push(...members);

  const held = new Set(roster);
  const work = runs
    .flatMap((run) => run.items.map((item) => ({ run, item })))
    .sort((a, b) => a.run.id.localeCompare(b.run.id) || a.item.ordinal - b.item.ordinal);

  for (const { run, item } of work) {
    /*
      Work held by somebody who is not on this roster — the director's own, or an item given to an
      agent since taken off it — hangs off whatever rank above it does exist. The chart has one
      place to put work with no column of its own, and pretending it belongs to a specialist who
      never had it is the worse answer.
    */
    const onRoster = held.has(item.agent_id);
    nodes.push({
      id: `item:${run.id}#${item.ordinal}`,
      layer: 3,
      label: item.description,
      said: `round ${item.round} · ${run.request}`,
      facts: [],
      state: item.state,
      parent: onRoster ? `member:${item.agent_id}` : (directorNode ?? teamNode),
      href: `/team-runs/${run.id}`,
      missing: false,
      crosses: !onRoster,
    });
  }

  return { nodes };
}

/* ------------------------------------------------------------------ layout -- */

/**
 * One height per rank.
 *
 * The department carries four lines — its name, how many are on it, and its limits over two — and
 * is the only rank that does. A person carries a name and a line; a piece of work carries a
 * description over two lines, the task it belongs to, and its state.
 */
export const LAYER_H: Record<RosterLayer, number> = { 0: 88, 1: 56, 2: 54, 3: 72 };

/**
 * The white band between two ranks. Wide enough that an edge reads as a line and not a join, and
 * exported because the drawing routes a crossing edge along the middle of it.
 */
export const ROW_GAP = 46;

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

const ROWS: RosterLayer[] = [0, 1, 2, 3];

const rowY = (layer: RosterLayer): number =>
  ROWS.slice(0, layer).reduce<number>((y, rank) => y + LAYER_H[rank] + ROW_GAP, 0);

/**
 * As wide as the widest thing written in it — `layered.ts`' rule, over this box's own lines.
 *
 * **`facts` are measured whole and `said` is measured clipped**, and the asymmetry is the point.
 * `said` is somebody's prose — a speciality, a list of departments — and has no upper bound, so it
 * is trimmed to fit and the box is sized to the trim. `facts` are this module's own sentences
 * about numbers the daemon holds; measuring them clipped is how the department's limits came out
 * as `no ceiling of its own · 5 ope…` in a box that had refused to grow to hold them.
 */
/**
 * The chart's own measure. `layered.ts`' CHAR_W is tuned to its 12px job graph; this chart is drawn
 * at 13px (`.teams-org-svg`), and at the old measure the department's limits ran to its border.
 */
const CHAR_W = 7.4;
const PAD_X = 40;

/**
 * A floor per rank, so one department reads as one set of boxes and not a box per label length —
 * a two-letter specialist beside a long one drew as a stub next to a slab.
 */
export const RANK_MIN: Record<RosterLayer, number> = { 0: 300, 1: 200, 2: 200, 3: 220 };

function boxWidth(layer: RosterLayer, lines: string[], said: string, facts: string[]): number {
  const longest = Math.max(
    ...lines.map((line) => line.length),
    clip(said).length,
    ...facts.map((fact) => fact.length),
  );
  return Math.max(RANK_MIN[layer], Math.round(longest * CHAR_W + PAD_X));
}

/**
 * Nodes into coordinates: a tree walked down, then across.
 *
 * **Not `layered.ts`'s `layout()`**, for the two reasons the spec gives at §5. `rankNodes` would
 * rank the director's own item onto the specialists' row, because rank there is distance from a
 * source and this item's parent is the director; and `NODE_H` is one height for every rank, while
 * a department's box, a person's box and a piece of work's box are not the same size.
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
    own.set(node.id, boxWidth(node.layer, folded, node.said, node.facts));
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
