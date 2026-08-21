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

  /**
   * What the agent has been told, and what it asked to be told — `GET
   * /refinements`.
   *
   * Its own root and not a child of `proposals`, although a refinement waiting
   * for an answer IS a proposal: `GET /proposals` filters `kind =
   * 'action-approval'` (`core/src/proposals.rs`), so the two lists never
   * overlap, and a page invalidating one would refetch a list that cannot have
   * changed. `detail` carries the chain, which is the half the listing has no
   * room for.
   */
  refinements: {
    all: ["refinements"] as const,
    detail: (id: number) => ["refinements", "detail", id] as const,
  },

  /**
   * The Work namespace — chats, council, errands, agents — landing together
   * ahead of the pages that read most of it, for the reason at the top of this
   * file: a namespace four pages edit in sequence is a namespace where the
   * fifth quietly spells its own key.
   *
   * `chats` is filled in from the routes this slice verified against
   * `core/src/http.rs`. The other three carry only the root a later slice
   * invalidates by — the same minimal shape `presets` above uses — until the
   * routes that would fill them in further are verified in turn.
   */
  chats: {
    all: ["chats"] as const,
    detail: (chatId: string) => ["chats", "detail", chatId] as const,
    localModel: ["chats", "local-model"] as const,
    project: (chatId: string) => ["chats", "project", chatId] as const,
    diff: (chatId: string) => ["chats", "diff", chatId] as const,
    ideSessions: ["chats", "ide-sessions"] as const,
    ideSession: (sessionId: string) => ["chats", "ide-session", sessionId] as const,
    live: (turnId: number) => ["chats", "live", turnId] as const,
    /** Keyed by the query too: each keystroke is a different question, and its own cached answer. */
    files: (chatId: string, query: string) => ["chats", "files", chatId, query] as const,
    commands: (chatId: string, query: string) => ["chats", "commands", chatId, query] as const,
  },

  council: {
    all: ["council"] as const,
  },

  errands: {
    all: ["errands"] as const,
  },

  agents: {
    all: ["agents"] as const,
  },

  /**
   * The departments, their runs, the actions they ask for and the rules that
   * start them.
   *
   * Ordered general-to-specific so prefix invalidation means something:
   * `run(id)` is a prefix of `runActions(id)`, so invalidating a run also
   * re-reads what it asked for, which is right — an action landing is a change
   * to the run.
   *
   * Two of these are not team routes at all. `proposedActions` and `recruits`
   * read `/proposals/team-actions` and `/proposals/recruits`, because the
   * decision surface for both is the ordinary proposals door — there is no
   * `/team-actions/{id}/approve`. They live here rather than under `waiting`
   * because the domain is teams and the queue is only where they are answered.
   */
  teams: {
    all: ["teams"] as const,
    list: ["teams", "list"] as const,
    detail: (id: string) => ["teams", "detail", id] as const,
    runs: ["teams", "runs"] as const,
    run: (id: string) => ["teams", "run", id] as const,
    runActions: (id: string) => ["teams", "run", id, "actions"] as const,
    openActions: ["teams", "actions"] as const,
    triggers: ["teams", "triggers"] as const,
    triggerNext: (id: number) => ["teams", "triggers", id, "next"] as const,
    proposedActions: ["teams", "proposals", "actions"] as const,
    recruits: ["teams", "proposals", "recruits"] as const,
  },

  /**
   * The Pillars namespace — mail, contacts, calendar, voice, web, browser,
   * files — landing together for the reason the top of this file gives: seven
   * pillars arrive across seven slices, and a namespace built one page at a
   * time is a namespace where the seventh page spells its own key for a route
   * the first page's slice already named.
   *
   * Every child below was read off the route it will serve, whether or not a
   * hook reads it yet in this slice. `mail` is filled in against
   * `core/src/http.rs`'s `/email/*` routes for the slice that builds it here;
   * `contacts.merges` and `browser.sessions` are two keys that already had a
   * reader — `data/waiting.ts`'s local `WAITING_KEYS`, retired in this same
   * slice in favour of these. The rest carry the shape their own route
   * answers with and wait for the slice that reads them.
   */
  mail: {
    all: ["mail"] as const,
    /** `GET /email/queue?q=` — one optional free-text filter, nothing else (`http.rs:206`). */
    queue: (q?: string) => ["mail", "queue", q ?? null] as const,
    /** `GET /email/cursor?mailbox=` — `mailbox` is required, so there is no bare form. */
    cursor: (mailbox: string) => ["mail", "cursor", mailbox] as const,
    config: ["mail", "config"] as const,
    detail: (id: number) => ["mail", "detail", id] as const,
    attachments: (id: number) => ["mail", "attachments", id] as const,
  },

  contacts: {
    all: ["contacts"] as const,
    /**
     * `GET /contacts/merges` — already live, previously read through
     * `WAITING_KEYS.contactMerges`. That constant is gone as of this slice.
     */
    merges: ["contacts", "merges"] as const,
  },

  calendar: {
    all: ["calendar"] as const,
    events: (from: string, to: string) => ["calendar", "events", from, to] as const,
    busy: ["calendar", "busy"] as const,
    config: ["calendar", "config"] as const,
  },

  voice: {
    all: ["voice"] as const,
    config: ["voice", "config"] as const,
    memos: ["voice", "memos"] as const,
    dictations: ["voice", "dictations"] as const,
  },

  web: {
    all: ["web"] as const,
    pages: (q?: string) => ["web", "pages", q ?? null] as const,
    page: (id: number) => ["web", "page", id] as const,
  },

  browser: {
    all: ["browser"] as const,
    /**
     * `GET /browser/sessions` — already live, previously read through
     * `WAITING_KEYS.wheelRequests`. The route answers every open session and
     * the wheel-request filter still happens in `select`, at the call site.
     */
    sessions: ["browser", "sessions"] as const,
    sites: (projectId: string) => ["browser", "sites", projectId] as const,
    /**
     * Health used to live here as `browser.health`, its own cache entry. It is
     * `keys.system.health` now — the ONE health query in the app, not one per
     * pillar — the same retirement `WAITING_KEYS.contactMerges` and
     * `WAITING_KEYS.wheelRequests` went through when `contacts.merges` and
     * `sessions` above got their real homes: a route's local key retires in
     * the slice that gives it a real home.
     */
  },

  files: {
    all: ["files"] as const,
    list: (path: string) => ["files", "list", path] as const,
    search: (path: string, q: string) => ["files", "search", path, q] as const,
  },

  system: {
    all: ["system"] as const,
    /** `GET /health/readout` (+ `/sidecars`) — the ONE health query in the app. */
    health: ["system", "health"] as const,
    sidecars: ["system", "sidecars"] as const,
    backups: ["system", "backups"] as const,
    tokens: ["system", "tokens"] as const,
    pii: ["system", "pii"] as const,
    config: (area: string) => ["system", "config", area] as const,
  },
} as const;
