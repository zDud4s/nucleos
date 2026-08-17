/**
 * Query keys, built in one place.
 *
 * Two call sites that spell the same key differently are two caches, and the
 * bug that follows is a mutation invalidating a list nobody is reading while
 * the list on screen keeps its stale row. A factory makes the key a value with
 * one definition, so `invalidateQueries` and `useQuery` cannot drift apart.
 *
 * Keys are namespaced by domain and ordered general-to-specific, which is what
 * makes a prefix match meaningful: invalidating `["autopilot"]` reaches the
 * kill switch and the budget, and nothing else.
 *
 * `as const` throughout — react-query keys are structurally compared, and a
 * widened `string[]` loses the literal types that let TypeScript catch a
 * typo'd key at the call site.
 */
export const keys = {
  /** Unauthenticated liveness. Its own root: it answers before a token exists. */
  health: ["health"] as const,
  /** The first authenticated call of a round, and so the proof the token still works. */
  status: ["status"] as const,

  autopilot: {
    kill: ["autopilot", "kill"] as const,
    budget: ["autopilot", "budget"] as const,
  },

  projects: {
    all: ["projects"] as const,
  },

  proposals: {
    all: ["proposals"] as const,
  },
} as const;
