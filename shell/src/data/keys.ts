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
 *
 * **The whole Operate namespace lands at once**, ahead of the pages that read
 * most of it. Not speculation: the alternative is each page of the slice
 * editing this file as it arrives, and a file six pages edit in sequence is a
 * file where the seventh quietly spells `["jobs","live"]` as `["live-jobs"]`.
 * Declaring the namespace once, from the routes `core/src/http.rs` actually
 * mounts, makes the shape a decision rather than an accumulation.
 */
export const keys = {
  /** Unauthenticated liveness. Its own root: it answers before a token exists. */
  health: ["health"] as const,
  /** The first authenticated call of a round, and so the proof the token still works. */
  status: ["status"] as const,

  /**
   * How much work fits and what is inside it — `GET /concurrency`.
   *
   * A root of its own rather than a child of `fleet`, because it is the house's
   * capacity and not the fleet page's data: Autopilot and the job forms read it
   * too, and a page that invalidates `["fleet"]` after drawing an edge must not
   * also throw away the capacity reading it is showing.
   */
  concurrency: ["concurrency"] as const,

  autopilot: {
    /** The prefix, for the rare caller that means *everything autopilot*. */
    all: ["autopilot"] as const,
    kill: ["autopilot", "kill"] as const,
    /**
     * The per-scope switches, deliberately NOT under `kill`: a prefix match on
     * the global switch would drag these along, and the two are answered by
     * different routes with different meanings.
     */
    scopedKills: ["autopilot", "scoped-kills"] as const,
    budget: ["autopilot", "budget"] as const,
    scoreboard: (projectId: string) => ["autopilot", "scoreboard", projectId] as const,
    shadowDecisions: ["autopilot", "shadow-decisions"] as const,
  },

  /**
   * The exclusions — the rules in force and the requests still waiting.
   *
   * Two keys and not one, because they are two different facts and the canvas
   * draws them differently: a rule is changing how the fleet schedules, a
   * request is changing nothing at all until somebody answers it.
   */
  fleet: {
    all: ["fleet"] as const,
    exclusions: ["fleet", "exclusions"] as const,
    exclusionRequests: ["fleet", "exclusions", "requests"] as const,
  },

  jobs: {
    all: ["jobs"] as const,
    /** `GET /jobs?live=true` — the work in flight, without the listing ceiling. */
    live: ["jobs", "live"] as const,
    detail: (id: number) => ["jobs", "detail", id] as const,
  },

  runs: {
    all: ["runs"] as const,
    /** `GET /runs?live=true` — only the runs still holding a slot. */
    live: ["runs", "live"] as const,
    /**
     * A filtered listing. The filters are part of the key on purpose: two
     * different filter sets are two different answers, and sharing one entry
     * between them is how a list shows the previous filter's rows.
     */
    search: (filters: Record<string, string | undefined>) => ["runs", "search", filters] as const,
    detail: (id: number) => ["runs", "detail", id] as const,
    /** The byte cursor lives in component state; the key only separates the tails. */
    tail: (id: number) => ["runs", "tail", id] as const,
    awaitingApproval: ["runs", "awaiting-approval"] as const,
  },

  presets: {
    all: ["presets"] as const,
  },

  /**
   * The single decision queue, by the lists it is assembled from.
   *
   * `proposals` stays at its own root below: it predates this namespace, it is
   * also the sidebar's badge, and moving it under `waiting` would make the rail
   * depend on a page's key.
   */
  waiting: {
    all: ["waiting"] as const,
    vcsRequests: ["waiting", "vcs-requests"] as const,
    skippedItems: ["waiting", "skipped-items"] as const,
    refusedActions: ["waiting", "refused-actions"] as const,
  },

  feed: {
    all: ["feed"] as const,
    search: (filters: Record<string, string | undefined>) => ["feed", "search", filters] as const,
  },

  notifications: {
    pending: ["notifications", "pending"] as const,
  },

  projects: {
    all: ["projects"] as const,
    rules: (projectId: string) => ["projects", projectId, "rules"] as const,
    ls: (projectId: string, path: string) => ["projects", projectId, "ls", path] as const,
    cat: (projectId: string, path: string) => ["projects", projectId, "cat", path] as const,
    grep: (projectId: string, q: string, path: string) =>
      ["projects", projectId, "grep", q, path] as const,
    diff: (projectId: string, path: string) => ["projects", projectId, "diff", path] as const,
  },

  proposals: {
    all: ["proposals"] as const,
  },
} as const;
