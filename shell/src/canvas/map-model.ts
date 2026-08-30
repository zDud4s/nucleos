// §spec mapa-do-projeto
import type { ForeignFile, MapModule } from "../data/project-map";

/**
 * How much of a project has said which document its `§` numbers belong to.
 *
 * **All that is left of what this file used to hold.** It also carried a grid of every file grouped
 * by its top folder, and a graph whose boxes were documents. Both are gone, replaced by
 * `map-graphs.ts`, which groups by the dependencies themselves rather than by a folder somebody
 * happened to choose or a header somebody happened to write. The measurements that ended them:
 * grouping by folder puts the whole núcleo in one node, because `core/src` is a single directory of
 * 99 files; and the document graph drew 73% of its edges across boxes they had nothing to do with.
 *
 * This question survived because it is not a layout. It is the one number saying what every
 * confirmation elsewhere on this screen is worth.
 */
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
 * every `=== null` here would answer *no, it declared something* about a file that declared
 * nothing. The visible result was one box named `undefined` holding the entire project: a confident
 * wrong answer, which is the failure this map exists to refuse, reached through a version skew
 * nobody would think to look for.
 *
 * One place, so the readers above cannot drift apart on it — `map-sides.ts` asks the same question
 * a side at a time and must get the same answer, or the top of the screen contradicts its header.
 */
export function declaredIn(file: { spec?: string | null }): string | null {
  return file.spec ?? null;
}
