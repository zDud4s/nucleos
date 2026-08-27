import type { ApiRefusal } from "../data/client";

/**
 * The daemon's own sentence, when it really sent one.
 *
 * `RefusalNote` prefers page copy over the shared floor over the daemon's
 * prose, in that order — which means a refusal whose code has a floor sentence
 * (`bad_request` is "the núcleo would not accept that request") drops whatever
 * the daemon actually said. For this pillar that is exactly backwards: the 400s
 * on `POST /teams/{id}/runs` name the missing specialist, the empty roster, the
 * deleted director or the local model this machine has not got, and the generic
 * floor throws all four away. Passing `{[code]: detail}` as page copy is how
 * the daemon's sentence wins.
 *
 * The four-word floor is the guard. `client.ts` falls back to `statusText` for
 * a refusal with an empty body, so a bare status arrives carrying only the
 * status word — and promoting *that* over the shared sentence would be a
 * downgrade. Four words is the line between a status word and a sentence
 * somebody wrote on purpose.
 *
 * Shared by the console and every tab of the bench, because all of them talk to
 * the same routes and a second copy is how two of them start disagreeing.
 */
export function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const detail = refusal.detail.trim();
  if (detail === "" || detail === refusal.code) return {};
  if (detail.split(/\s+/).length < 4) return {};
  return { [refusal.code]: detail };
}
