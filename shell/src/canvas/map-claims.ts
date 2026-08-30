// §spec mapa-do-projeto

/**
 * Which decisions claim a file, and what the owner has said about each.
 *
 * **This is §16.4's L3, entered from the other end.** The spec describes a decision lighting up the
 * files below it; standing at a file and asking *which decisions claim this, and are they still
 * true?* is the same join read backwards, and it is the direction somebody has when they are
 * looking at code rather than at a plan. Both need the identical data, so neither needs new
 * machinery: `GET /map` already carries `junction.decisions[].modules` and the standing of each.
 *
 * **The two questions here are two, and §5 is why they never become one.** *Nothing claims this
 * file* is derived — it is `map_join` failing to find a `§` naming it, and no human said it. *This
 * decision is stamped* is the owner's verdict and nothing else can produce it. A single badge
 * blending them would put the triager and the owner in one voice, which §5 spends a section
 * refusing, so they are counted apart and drawn apart.
 */

import type { Anchored, Junction, Standing } from "../data/project-map";

/** Every file some approved decision names. The complement is §5.1's *code nobody asked for*. */
export function claimedFiles(junction: Junction): Set<string> {
  const claimed = new Set<string>();
  for (const decision of junction.decisions) {
    for (const path of decision.modules) claimed.add(path);
  }
  return claimed;
}

/**
 * The decisions that name one file, in the order a reader should meet them.
 *
 * Sorted by document and then by the ordinal the owner approved them in, and never by standing:
 * ordering by verdict would put the same decision in a different place on two visits, and the
 * position of a row is the one thing a reader uses to find it again.
 */
export function claimsFor(junction: Junction, path: string): Anchored[] {
  return junction.decisions
    .filter((decision) => decision.modules.includes(path))
    .sort((a, b) =>
      a.spec_slug === b.spec_slug
        ? a.ordinal - b.ordinal
        : a.spec_slug.localeCompare(b.spec_slug),
    );
}

/**
 * What the owner has said about a decision, in the words the stamp panel already uses.
 *
 * **Taken from `Carimbos` rather than invented**, because two screens naming one state differently
 * is the same decision wearing two faces — and this map exists to catch exactly that kind of quiet
 * disagreement, not to add one.
 */
export function standingLabel(standing: Standing | undefined): string {
  switch (standing?.state) {
    case "settled":
      return "stamped";
    case "partial":
      return "part-way, and you know it";
    case "lapsed":
      return "lapsed — the code moved since";
    case "withdrawn":
      return "withdrawn";
    case "never":
    default:
      return "nobody has looked";
  }
}

/**
 * Whether a standing is the owner's word or the absence of it.
 *
 * The green/not-green split the file panel leans on, kept as a function so the two places that ask
 * cannot drift. `lapsed` is deliberately NOT settled: it was stamped and the code has moved since,
 * which is §7's whole point — a verdict with an expiry is not a verdict that never expires.
 */
export function isSettled(standing: Standing | undefined): boolean {
  return standing?.state === "settled";
}
