// §spec mapa-do-projeto

/**
 * The sides of the product, and the seam between them — §16.4's L0.
 *
 * **A side is the pair `(which folder the file sits under, which reader understands it)`**, and
 * that pair is not a list somebody wrote down. It is read off the paths that are on disk, so the
 * day `sidecars/` holds Rust a fourth box appears without anybody editing this file. §16.2 spends
 * its length on why that matters: the thirteen subsystems the old diagrams drew came from a
 * hand-written array in a script, and a hand-written array rots in silence — a new
 * `core/src/telegram.rs` falls into a `rest` bucket and the picture still looks right.
 *
 * **The pair is also exactly the scope the núcleo's import resolver works in**, which is what makes
 * the boxes real rather than decorative: `project_map` keys Rust modules by `(top folder, stem)`
 * precisely so `crate::` cannot cross a crate. Measured over this repository, 2026-08-30:
 *
 * | side | files | imports inside | imports leaving |
 * |---|---:|---:|---:|
 * | `core · rust` | 105 | 590 | **0** |
 * | `shell · typescript` | 162 | 464 | **0** |
 * | `shell · rust` (the Tauri host) | 6 | 1 | **0** |
 *
 * **And the zero is the one number here that must not be read as a result.** No import crosses,
 * and no import *could*: a Rust file cannot import a TypeScript module in either direction, and
 * the resolver refuses to look outside a file's own folder. Drawing three boxes with no lines
 * between them and calling it independence would be this map telling its owner something nobody
 * measured — §1's false confidence, now in pixels. What the three islands say is narrower and
 * true: **these sides share no source.** What passes between them is HTTP, and {@link crossing}
 * is a tripwire for the day somebody wires them together in the file system instead.
 */

import type { ForeignFile, MapImport, MapModule, Reader } from "../data/project-map";
import { declaredIn } from "./map-model";

/** One side of the product: a folder, and a language something here can read. */
export interface Side {
  /** `core · rust`. The folder and the reader, joined, because `shell` alone names two boxes. */
  key: string;
  folder: string;
  reader: Reader;
  files: number;
  /** Imports with both ends inside this side. */
  imports: number;
  /**
   * Files naming at least one `§` — the only files §8's question applies to.
   *
   * **The same denominator {@link declaredCoverage} uses, and not this side's file count**, so the
   * boxes add up to the header above them instead of quietly disagreeing with it. A module with no
   * `§` anywhere has nothing to disambiguate, and counting it as undeclared would report a debt
   * that does not exist and make the number fall whenever somebody adds an unrelated file.
   */
  citing: number;
  /** Of those, the ones carrying a `§spec` header. */
  declared: number;
  /** Files with a test beside them. The nearest thing to evidence this map has today. */
  tested: number;
}

/**
 * A folder holding files no reader here understands.
 *
 * **Not a side, and never counted as one.** `sidecars/` is 168 Go files and is unmistakably part of
 * the product; `core/`'s 131 are its SQL migrations, which are part of a side that already has a
 * box. So this is reported per folder next to the sides rather than as a fourth box — a folder that
 * appears here *and* has no side is a piece of the product this map is blind to, and a folder that
 * appears in both is a side with something behind it.
 */
export interface Unread {
  folder: string;
  files: number;
  /** Of those, the ones that name a `§` at all. */
  citing: number;
  /** Of those, the ones that also say which document — the header §8 asks for. */
  declared: number;
}

export interface Seam {
  /** Largest first, so the box order is a fact about the project and not about the alphabet. */
  sides: Side[];
  unread: Unread[];
  /** Imports whose two ends land in different sides. Zero, and see this module's header for why. */
  crossing: number;
  /**
   * Imports with an end in no side at all.
   *
   * Cannot happen while the núcleo only emits edges between modules it listed, which is exactly
   * why it is counted: a number that must be zero and is not says the two halves have drifted, and
   * that is worth finding here rather than in a drawing that quietly lost a line.
   */
  loose: number;
}

/**
 * The folder a path sits in, which is as near to *which side of the product is this* as a path gets.
 *
 * A file at the repository root gets `(root)` rather than its own name, so five loose config files
 * do not become five sides.
 */
export function topFolder(path: string): string {
  const cut = path.indexOf("/");
  return cut === -1 ? "(root)" : path.slice(0, cut);
}

/** The side a readable module belongs to. */
export function sideOf(module: MapModule): string {
  return `${topFolder(module.path)} · ${module.reader}`;
}

/**
 * The sides, what each one hides, and how much passes between them.
 *
 * Everything here is counted off the answer the structure layer already sends. There is no second
 * walk and no second definition of a side — §16.3's fourth consequence, applied one level up.
 */
export function buildSeam(
  modules: MapModule[],
  imports: MapImport[],
  unread: string[],
  foreign: ForeignFile[],
): Seam {
  const sides = new Map<string, Side>();
  const of = new Map<string, string>();
  for (const module of modules) {
    const key = sideOf(module);
    of.set(module.path, key);
    const side = sides.get(key) ?? {
      key,
      folder: topFolder(module.path),
      reader: module.reader,
      files: 0,
      imports: 0,
      citing: 0,
      declared: 0,
      tested: 0,
    };
    side.files += 1;
    if (module.cites.length > 0) {
      side.citing += 1;
      // `declaredIn` and never `module.spec !== null`: a núcleo older than `Module::spec` sends no
      // field at all, `apiFetch` is a cast, and `undefined !== null` would answer *this declared*
      // about every file in the project.
      if (declaredIn(module) !== null) side.declared += 1;
    }
    if (module.tested) side.tested += 1;
    sides.set(key, side);
  }

  let crossing = 0;
  let loose = 0;
  for (const line of imports) {
    const from = of.get(line.from);
    const to = of.get(line.to);
    if (from === undefined || to === undefined) {
      loose += 1;
      continue;
    }
    if (from === to) sides.get(from)!.imports += 1;
    else crossing += 1;
  }

  const citing = new Map<string, ForeignFile[]>();
  for (const file of foreign) {
    const folder = topFolder(file.path);
    citing.set(folder, [...(citing.get(folder) ?? []), file]);
  }
  const unreadBy = new Map<string, number>();
  for (const path of unread) {
    const folder = topFolder(path);
    unreadBy.set(folder, (unreadBy.get(folder) ?? 0) + 1);
  }
  const quiet: Unread[] = [...unreadBy].map(([folder, files]) => {
    const named = citing.get(folder) ?? [];
    return {
      folder,
      files,
      citing: named.length,
      declared: named.filter((file) => declaredIn(file) !== null).length,
    };
  });

  const bySize = <T extends { files: number }>(a: T, b: T) => b.files - a.files;
  return {
    sides: [...sides.values()].sort(bySize),
    unread: quiet.sort(bySize),
    crossing,
    loose,
  };
}

/**
 * Whether a folder is a piece of the product nothing here can read.
 *
 * `sidecars/` answers yes: 168 files, not one of them a module, and 77 of them naming a `§`. That
 * combination is worth a sentence of its own, because the side this map is blindest to is also the
 * only one that has finished declaring: 77 citing files and 77 headers, where the núcleo and the
 * shell are still counting.
 */
export function isBlindSpot(seam: Seam, folder: string): boolean {
  return !seam.sides.some((side) => side.folder === folder);
}
