// §spec mapa-do-projeto
import type { LayerViolation, Layering, MapImport, MapModule } from "../data/project-map";
import { blockKey } from "./atlas-model";

/** The files of one folder that sit in one layer. */
export interface LayerBlock {
  key: string;
  label: string;
  layer: number;
  files: string[];
  /** Upward imports whose importer is one of these files. */
  upward: number;
  /** The subset of {@link LayerBlock.upward} absent from the frozen baseline. */
  upwardNew: number;
}

/** One layer: its blocks and the counts over them. */
export interface Band {
  layer: number;
  blocks: LayerBlock[];
  files: number;
  upward: number;
  upwardNew: number;
}

/** A band seen alone, with the imports crossing its edge counted per neighbouring layer. */
export interface IsolatedBand {
  band: Band;
  /** Imports from another layer into this one, keyed by the importer's layer. */
  into: Map<number, number>;
  /** Imports from this layer out to another one, keyed by the imported layer. */
  outOf: Map<number, number>;
}

/** The highest layer any module holds (0 for an empty layering). */
export function maxLayer(layering: Layering): number {
  let max = 0;
  for (const layer of Object.values(layering.modules)) {
    if (layer > max) max = layer;
  }
  return max;
}

function compare(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

/** Bands from the highest layer down to 0, each grouping its files by folder block. */
export function buildBands(modules: MapModule[], layering: Layering): Band[] {
  const byLayer = new Map<number, Map<string, LayerBlock>>();
  const blockOf = new Map<string, LayerBlock>();

  for (const module of modules) {
    const layer = layering.modules[module.path];
    if (layer === undefined) continue;
    let blocks = byLayer.get(layer);
    if (blocks === undefined) {
      blocks = new Map();
      byLayer.set(layer, blocks);
    }
    const key = blockKey(module.path);
    let block = blocks.get(key);
    if (block === undefined) {
      block = { key, label: key, layer, files: [], upward: 0, upwardNew: 0 };
      blocks.set(key, block);
    }
    block.files.push(module.path);
    blockOf.set(module.path, block);
  }

  // A violation counts on the block of its importer.
  for (const violation of layering.violations) {
    const block = blockOf.get(violation.from);
    if (block === undefined) continue;
    block.upward += 1;
    if (violation.new) block.upwardNew += 1;
  }

  const bands: Band[] = [];
  for (const [layer, blocks] of byLayer) {
    const sorted = [...blocks.values()].sort((a, b) => compare(a.key, b.key));
    for (const block of sorted) block.files.sort(compare);
    bands.push({
      layer,
      blocks: sorted,
      files: sorted.reduce((sum, block) => sum + block.files.length, 0),
      upward: sorted.reduce((sum, block) => sum + block.upward, 0),
      upwardNew: sorted.reduce((sum, block) => sum + block.upwardNew, 0),
    });
  }
  return bands.sort((a, b) => b.layer - a.layer);
}

function bump(counts: Map<number, number>, layer: number) {
  counts.set(layer, (counts.get(layer) ?? 0) + 1);
}

/** Keep one band and aggregate the imports crossing its edge by neighbouring layer. */
export function isolate(
  bands: Band[],
  imports: MapImport[],
  layering: Layering,
  layer: number,
): IsolatedBand {
  const band = bands.find((candidate) => candidate.layer === layer) ?? {
    layer,
    blocks: [],
    files: 0,
    upward: 0,
    upwardNew: 0,
  };
  const into = new Map<number, number>();
  const outOf = new Map<number, number>();
  for (const edge of imports) {
    const from = layering.modules[edge.from];
    const to = layering.modules[edge.to];
    if (from === undefined || to === undefined || from === to) continue;
    if (to === layer) bump(into, from);
    if (from === layer) bump(outOf, to);
  }
  return { band, into, outOf };
}

/** One row of the ladder: a band, or a run of quiet bands folded into one. */
export interface LadderRow {
  /** Highest layer in the row. */
  top: number;
  /** Lowest layer in the row (equal to `top` for a single band). */
  bottom: number;
  files: number;
}

/** Upward imports joining two rows, counted together. `fromRow` is the importer's row. */
export interface LadderLine {
  fromRow: number;
  toRow: number;
  count: number;
  newCount: number;
}

export interface Ladder {
  rows: LadderRow[];
  lines: LadderLine[];
}

/**
 * The stack boiled down to what an upward import needs: one row per band, and a line for every
 * pair of rows an upward import joins. When there are more bands than `limit` the bands no
 * violation starts or ends at are folded into runs, so a deep graph keeps its lines readable;
 * with `limit` or fewer, nothing is folded.
 */
export function ladder(bands: Band[], violations: LayerViolation[], limit = 12): Ladder {
  const ends = new Set<number>();
  for (const violation of violations) {
    ends.add(violation.from_layer);
    ends.add(violation.to_layer);
  }
  const fold = bands.length > limit;

  const rows: LadderRow[] = [];
  const rowOf = new Map<number, number>();
  let open = false;
  for (const band of bands) {
    if (fold && !ends.has(band.layer) && open) {
      const row = rows[rows.length - 1];
      row.bottom = band.layer;
      row.files += band.files;
    } else {
      rows.push({ top: band.layer, bottom: band.layer, files: band.files });
      open = fold && !ends.has(band.layer);
    }
    rowOf.set(band.layer, rows.length - 1);
  }

  const joined = new Map<string, LadderLine>();
  for (const violation of violations) {
    const fromRow = rowOf.get(violation.from_layer);
    const toRow = rowOf.get(violation.to_layer);
    if (fromRow === undefined || toRow === undefined || fromRow === toRow) continue;
    const key = `${fromRow}:${toRow}`;
    let line = joined.get(key);
    if (line === undefined) {
      line = { fromRow, toRow, count: 0, newCount: 0 };
      joined.set(key, line);
    }
    line.count += 1;
    if (violation.new) line.newCount += 1;
  }
  const lines = [...joined.values()].sort((a, b) => a.fromRow - b.fromRow || a.toRow - b.toRow);
  return { rows, lines };
}
