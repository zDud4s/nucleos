// §spec mapa-do-projeto
import type { Edge, Node } from "@xyflow/react";
import type { ForeignFile, MapImport, MapModule } from "../data/project-map";

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
/** How much of a project has said which document its `§` numbers belong to. */
export interface DeclaredCoverage {
  /** Files naming at least one section — the only files the question applies to. */
  citing: number;
  /** Of those, the ones carrying a `§spec` header. */
  saying: number;
}

/**
 * The §8 progress of one project, counted over the files the question is about.
 *
 * **The denominator is files that cite a section, and not every file.** A module with no `§`
 * anywhere has nothing to disambiguate, so counting it as undeclared would report a debt that
 * does not exist and would make the number fall every time somebody adds an unrelated file.
 *
 * **Foreign files are counted with modules here, and that is deliberate even though nothing else
 * on this screen counts them together.** The question is *how much of this project has declared*,
 * which is about files rather than about what this reader can follow — and the Go sidecars are
 * where a large share of the headers are. Counting only modules would report a project as far less
 * declared than it is, which is the one direction this number must not err in: it exists to say
 * how much the map's confirmations are worth.
 */
export function declaredCoverage(
  modules: MapModule[],
  foreign: ForeignFile[],
): DeclaredCoverage {
  const citingModules = modules.filter((module) => module.cites.length > 0);
  return {
    citing: citingModules.length + foreign.length,
    saying:
      citingModules.filter((module) => declaredIn(module) !== null).length +
      foreign.filter((file) => declaredIn(file) !== null).length,
  };
}

/**
 * The document a file declares, from a payload that may not carry the field at all.
 *
 * **A núcleo older than `Module::spec` sends no `spec`, and `apiFetch<T>` is a cast** — the same
 * property `ForeignFile` records having been silently absent from this file for a whole slice. A
 * cast cannot notice a missing field, so `module.spec` is then `undefined` rather than `null`, and
 * every `=== null` in this module answers *no, it declared something* about a file that declared
 * nothing. The visible result is one box named `undefined` holding the entire project: a confident
 * wrong answer, which is the failure this map exists to refuse, reached through a version skew
 * nobody would think to look for.
 *
 * One place, so the two readers below cannot drift apart on it.
 */
function declaredIn(file: { spec?: string | null }): string | null {
  return file.spec ?? null;
}

/**
 * How tall a folder's column is allowed to get before it spills into the next one.
 *
 * **A column as tall as its folder is unreadable at the size this is drawn.** Unbounded, the
 * núcleo is one column of about 150 files, and one document's files are little better —
 * `pilar-de-browser` puts 53 into `sidecars` alone. `fitView` then scales the picture to about a
 * fifth and the labels go with it. Wrapping keeps the folder grouping, which is the only thing
 * this layout was ever saying, and bounds the height so the words survive.
 */
export const ROWS_PER_COLUMN = 12;

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
 * **It reads `cites` and never `declares`, and the two really do disagree.** `declares` is
 * `source.contains('§')` — the file *gestures* at a section, a bare `§` naming no number
 * included. `cites` is what it actually names, and for a TypeScript module that includes its
 * sibling test's citations, because a Rust module gets its `#[cfg(test)]` citations for free and
 * one language answering differently from the other is not a distinction anybody chose. Four
 * modules today — `Fleet.tsx`, `Home.tsx`, `Workspace.tsx`, `priority.ts` — are `declares: false`
 * with a non-empty `cites`: their tests name what they prove, so somebody did ask for them.
 * Colouring them as nobody's would put a second answer on screen to a question
 * `map_join::Junction` already owns, and two panels of one mode disagreeing by four is precisely
 * the confusion this mode exists to remove.
 *
 * **Not a row in `state-map.ts`, and the difference is worth naming.** That table maps state
 * literals the núcleo writes — strings like `errored` or `cancelled` — so that fourteen pages
 * cannot disagree about what one word means. There is no literal here: this is a derivation over
 * a list and a boolean, not a translation of anything. What `state-map.ts` is protecting is
 * honoured all the same, by landing inside the same seven tones rather than inventing an eighth.
 *
 * **A fourth state is folded into the third here, and on purpose.** A module that names nothing
 * reads as `pending` whether or not somebody happened to test it, because the axis this mode
 * reports is intent and not proof: nobody having asked for it is the fact, and a test written
 * anyway does not change it. A later slice wanting to tell *abandoned but proven* from *never
 * asked for* will need a signal of its own rather than a fourth colour.
 */
export function moduleTone(module: MapModule): ModuleTone {
  if (module.cites.length === 0) return "pending";
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
/**
 * The modules one document's box stands for.
 *
 * **The same membership rule {@link buildDocuments} counts with, written once.** A file naming no
 * section and declaring nothing is §5.1's *code nobody asked for* and belongs to no box, so
 * opening the undeclared pile must not show it either — two rules would let a box open onto a
 * different set from the one its own number counted, which is a surface disagreeing with itself.
 */
export function filesOf(modules: MapModule[], slug: string | null): MapModule[] {
  return modules.filter((module) => {
    const spec = declaredIn(module);
    if (spec === null && module.cites.length === 0) return false;
    return spec === slug;
  });
}

export function buildMap(modules: MapModule[], imports: MapImport[]): MapModel {
  const known = new Set(modules.map((module) => module.path));

  // Two passes, because where a folder's second column starts depends on how many the folders
  // before it needed. One pass could only know that by placing everything twice anyway.
  const size: Record<string, number> = {};
  const folders: string[] = [];
  for (const module of modules) {
    const folder = topFolder(module.path);
    if (!folders.includes(folder)) folders.push(folder);
    size[folder] = (size[folder] ?? 0) + 1;
  }
  const startsAt: Record<string, number> = {};
  let column = 0;
  for (const folder of folders) {
    startsAt[folder] = column;
    column += Math.ceil(size[folder] / ROWS_PER_COLUMN);
  }

  const filled: Record<string, number> = {};
  const nodes: MapFlowNode[] = modules.map((module) => {
    const folder = topFolder(module.path);
    const row = filled[folder] ?? 0;
    filled[folder] = row + 1;
    return {
      id: module.path,
      type: "mapNode" as const,
      position: {
        x: (startsAt[folder] + Math.floor(row / ROWS_PER_COLUMN)) * COLUMN,
        y: (row % ROWS_PER_COLUMN) * ROW,
      },
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

/**
 * Horizontal gap between document nodes, and the grid is five wide.
 *
 * **Sized against `fitView` and not by eye.** Twenty-one boxes at 300 apart are 1500 wide, which
 * a 420px-tall canvas fits by scaling to about 0.6 — and at 0.6 a 10px label is 6px. The numbers
 * here keep the whole grid near 1:1 in the space it is given, so the picture is readable without
 * anybody reaching for the zoom. Layout only: see {@link buildDocuments} on what position means,
 * which is nothing.
 */
export const DOC_COLUMN = 210;
/** Vertical gap between document nodes. Sized with {@link DOC_COLUMN}. */
export const DOC_ROW = 118;

/** The pile of files that name a section and say no document. */
export const UNDECLARED = null;

export interface DocumentNodeData extends Record<string, unknown> {
  /** The document, or `null` for the pile that declares none. */
  slug: string | null;
  /** Files carrying this header — modules and foreign files together. */
  files: number;
  /** Of those, how many name at least one section. */
  citing: number;
}

export type DocumentFlowNode = Node<DocumentNodeData, "documentNode">;

export interface DocumentEdgeData extends Record<string, unknown> {
  /** How many imports cross from one document's files into the other's. */
  weight: number;
}

export type DocumentFlowEdge = Edge<DocumentEdgeData, "documentEdge">;

export interface DocumentModel {
  nodes: DocumentFlowNode[];
  edges: DocumentFlowEdge[];
}

/**
 * The map one level above the file: **a node is a document, and its files are the ones that said
 * so.**
 *
 * **The boundary is declared and not invented, which is the whole reason this exists.** Grouping
 * by folder — what {@link buildMap} does — was measured against this repository and fails in both
 * directions: `core/src` is a single directory holding 94 files, so the whole núcleo collapses into
 * one node; and `shell/src/project/` holds 30 files of which 14 belong to one document and 16 to
 * others, so the folder puts `WorkflowGraph.tsx` inside the map. A hand-written list of subsystems
 * fixes neither and rots on the day somebody adds a file. The `§spec` header is a fact the owner
 * writes, so this grouping is derived, durable, and the same mechanism the rest of the feature
 * already turns on.
 *
 * **The undeclared pile is a node and not an omission.** It is exactly the §8 debt — files naming
 * a section without saying which document — and it shrinks as headers are written. Hiding it
 * would draw a project as more organised than it is, which is the failure this whole map is
 * against; drawing it as a document would claim somebody decided it.
 *
 * **Foreign files count toward a document's size and contribute no edges**, because nothing here
 * knows what a Go file imports. Leaving them out of the count would report the sidecars as
 * belonging to nothing when 68 of them declare; drawing an edge for them would invent one.
 *
 * **Position carries no meaning and the drawing must not imply otherwise.** Nodes are ordered by
 * size and laid on a grid so the picture is stable between reads; nothing about *where* a document
 * sits says anything about it. What is true here is the sizes and the edges.
 */
export function buildDocuments(
  modules: MapModule[],
  foreign: ForeignFile[],
  imports: MapImport[],
): DocumentModel {
  const tally = new Map<string | null, { files: number; citing: number }>();
  const bump = (slug: string | null, citing: boolean) => {
    const row = tally.get(slug) ?? { files: 0, citing: 0 };
    tally.set(slug, { files: row.files + 1, citing: row.citing + (citing ? 1 : 0) });
  };

  const documentOf = new Map<string, string | null>();
  for (const module of modules) {
    // A module naming no section and declaring nothing is `§5.1`'s *code nobody asked for*, and it
    // is not §8 debt: there is nothing in it to disambiguate. Putting it in the undeclared pile
    // would make that pile grow with files that were never the question.
    const spec = declaredIn(module);
    if (spec === null && module.cites.length === 0) continue;
    documentOf.set(module.path, spec);
    bump(spec, module.cites.length > 0);
  }
  // Foreign files are only ever sent when they cite something, so each one counts.
  for (const file of foreign) bump(declaredIn(file), true);

  const nodes: DocumentFlowNode[] = [...tally.entries()]
    .sort((a, b) => b[1].files - a[1].files || String(a[0]).localeCompare(String(b[0])))
    .map(([slug, row], index) => ({
      id: slug ?? "",
      type: "documentNode" as const,
      position: {
        x: (index % 5) * DOC_COLUMN,
        y: Math.floor(index / 5) * DOC_ROW,
      },
      data: { slug, files: row.files, citing: row.citing },
    }));

  const crossings = new Map<string, { from: string | null; to: string | null; weight: number }>();
  for (const line of imports) {
    if (!documentOf.has(line.from) || !documentOf.has(line.to)) continue;
    const from = documentOf.get(line.from) ?? null;
    const to = documentOf.get(line.to) ?? null;
    // An import inside one document is not a crossing. Drawing it as a self-loop would put a
    // number on the picture that says nothing about how the documents relate.
    if (from === to) continue;
    const id = `${from ?? ""}->${to ?? ""}`;
    const seen = crossings.get(id);
    crossings.set(id, { from, to, weight: (seen?.weight ?? 0) + 1 });
  }

  const edges: DocumentFlowEdge[] = [...crossings.entries()].map(([id, crossing]) => ({
    id,
    source: crossing.from ?? "",
    target: crossing.to ?? "",
    type: "documentEdge" as const,
    data: { weight: crossing.weight },
  }));

  return { nodes, edges };
}
