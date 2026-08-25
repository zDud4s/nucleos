import type { Edge, Node } from "@xyflow/react";
import type { MapImport, MapModule } from "../data/project-map";

/**
 * Where a project's modules sit, and what xyflow is handed.
 *
 * **Grouped, not laid out in sequence — and that is the difference from `workflow-model.ts`
 * next door.** A workflow is a sequence and the sequence is the entire content. A repository
 * is not: `agent.rs` does not come before `budget.rs`, and any ordering between them would be
 * invented. What does carry structure is the **folder** — somebody decided that and wrote it
 * to disk — so that is what this groups by.
 */

export const COLUMN = 260;
export const ROW = 44;

export interface Point {
  x: number;
  y: number;
}

/** The top folder of a path, which is how this repo already divides: core, shell, sidecars. */
export function topFolder(path: string): string {
  const cut = path.indexOf("/");
  return cut === -1 ? "" : path.slice(0, cut);
}

/**
 * The three readings a module can have, as a subset of the app's closed tone vocabulary.
 *
 * Narrower than `BadgeTone` on purpose: these three are the only ones this mode can produce,
 * and saying so here means a fourth would fail to compile rather than arrive on screen.
 */
export type ModuleTone = "pending" | "off" | "info";

/**
 * A module's tone, and the one question it answers without a word being read.
 *
 * *Did anybody ask for this?* A module that cites no section is `pending` — not because it is
 * wrong, but because it is the pile §5.1 calls *code nobody asked for*, and that pile is half
 * the reason this mode exists. Declared but unproven is `info`: somebody asked for it, and
 * nothing shows that it runs. The two are different facts and must not share a colour.
 *
 * **Not a row in `state-map.ts`, and the difference is worth naming.** That table maps state
 * literals the núcleo writes — strings like `errored` or `cancelled` — so that fourteen pages
 * cannot disagree about what one word means. There is no literal here: `declares` and `tested`
 * are two booleans, and this is a derivation over them, not a translation of anything. What
 * `state-map.ts` is protecting is honoured all the same, by landing inside the same seven
 * tones rather than inventing an eighth.
 *
 * **A fourth state is folded into the third here, and on purpose.** A module that declares
 * nothing reads as `pending` whether or not somebody happened to test it, because the axis this
 * mode reports is intent and not proof: nobody having asked for it is the fact, and a test
 * written anyway does not change it. A later slice wanting to tell *abandoned but proven* from
 * *never asked for* will need a signal of its own rather than a fourth colour.
 */
export function moduleTone(module: MapModule): ModuleTone {
  if (!module.declares) return "pending";
  return module.tested ? "off" : "info";
}

export interface MapNodeData extends Record<string, unknown> {
  module: MapModule;
}

export type MapFlowNode = Node<MapNodeData, "mapNode">;

export interface MapEdgeData extends Record<string, unknown> {
  import: MapImport;
}

export type MapFlowEdge = Edge<MapEdgeData, "mapEdge">;

export interface MapModel {
  nodes: MapFlowNode[];
  edges: MapFlowEdge[];
}

/**
 * The whole picture, from the daemon's answer.
 *
 * An edge whose ends are not both in the list is dropped. The núcleo does not return those,
 * but trusting blindly is how a canvas dies on `undefined` instead of drawing what it can.
 */
export function buildMap(modules: MapModule[], imports: MapImport[]): MapModel {
  const known = new Set(modules.map((module) => module.path));
  const filled: Record<string, number> = {};
  const folders: string[] = [];

  const nodes: MapFlowNode[] = modules.map((module) => {
    const folder = topFolder(module.path);
    if (!folders.includes(folder)) folders.push(folder);
    const row = filled[folder] ?? 0;
    filled[folder] = row + 1;
    return {
      id: module.path,
      type: "mapNode" as const,
      position: { x: folders.indexOf(folder) * COLUMN, y: row * ROW },
      data: { module },
    };
  });

  const edges: MapFlowEdge[] = imports
    .filter((line) => known.has(line.from) && known.has(line.to))
    .map((line) => ({
      id: `${line.from}->${line.to}`,
      source: line.from,
      target: line.to,
      type: "mapEdge" as const,
      data: { import: line },
    }));

  return { nodes, edges };
}
