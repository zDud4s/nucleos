// §spec mapa-do-projeto
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

  /** How much of each assistant's usage window is gone — `GET /quota`. */
  quota: ["quota"] as const,

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
    quotaBrake: ["autopilot", "quota-brake"] as const,
    scoreboard: (projectId: string) => ["autopilot", "scoreboard", projectId] as const,
    shadowDecisions: ["autopilot", "shadow-decisions"] as const,
    /** One project's judge setting and readiness (`GET /autopilot/judge`). */
    judge: (projectId: string) => ["autopilot", "judge", projectId] as const,
    /** The judge's own review queue — NOT under `shadowDecisions`: they are two queues. */
    judgeVerdictsAll: ["autopilot", "judge-verdicts"] as const,
    judgeVerdicts: (projectId: string) => ["autopilot", "judge-verdicts", projectId] as const,
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
    /**
     * Why the run stopped. Its own entry rather than a slice of `detail`: the
     * report reads `shadow_decisions` as well as the run row, so it is a second
     * answer about one run and not a projection of the first.
     */
    stop: (id: number) => ["runs", "stop", id] as const,
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
    /**
     * One window of the time axis. `until` is `null` for a live window, which is every window the
     * page offers today — and the key a poll merges into rather than replaces.
     */
    timeline: (since: string, until: string | null) => ["feed", "timeline", since, until] as const,
    /** The daemon's seen marker. One per machine, so no parameters. */
    seen: ["feed", "seen"] as const,
  },

  notifications: {
    pending: ["notifications", "pending"] as const,
  },

  projects: {
    all: ["projects"] as const,
    rules: (projectId: string) => ["projects", projectId, "rules"] as const,
    /**
     * What onboarding would find and propose — `GET /projects/{id}/onboard`. Keyed by the folder
     * too, because a project with no root on record is read at the folder somebody typed.
     */
    onboarding: (projectId: string, root: string) =>
      ["projects", projectId, "onboarding", root] as const,
    ls: (projectId: string, path: string) => ["projects", projectId, "ls", path] as const,
    cat: (projectId: string, path: string) => ["projects", projectId, "cat", path] as const,
    grep: (projectId: string, q: string, path: string) =>
      ["projects", projectId, "grep", q, path] as const,
    diff: (projectId: string, path: string) => ["projects", projectId, "diff", path] as const,
    readings: (projectId: string) => ["projects", projectId, "readings"] as const,
    /**
     * What this project would lose by leaving, and what is holding it here.
     *
     * Under the roster prefix like everything else about a project, which means the removal itself
     * invalidates it along with the row it just deleted — and a record left in the cache for a
     * project that is gone is a number a re-opened control would show about nothing.
     */
    record: (projectId: string) => ["projects", projectId, "record"] as const,
    /**
     * What deleting this project's folder would take, and whether it would be allowed.
     *
     * Two git subprocesses and a `stat` behind it, so it is fetched only when the control that acts
     * on it is open — never on the roster, and never on a timer.
     */
    folder: (projectId: string) => ["projects", projectId, "folder"] as const,
    /** The project's structure layer, derived off disk on every read. */
    map: (projectId: string) => ["projects", projectId, "map"] as const,
    /** The documents this project keeps, by the name the owner reads. */
    mapSpecs: (projectId: string) => ["projects", projectId, "map", "specs"] as const,
    /**
     * What ONE file declares — the step below the file, fetched only when somebody opens one.
     *
     * Keyed by the path, so opening a second file does not evict the first: a reader climbing back
     * up the ladder and down another branch gets the drawing they already had rather than a
     * spinner over a file whose contents cannot have changed in between.
     */
    mapItems: (projectId: string, path: string) =>
      ["projects", projectId, "map", "items", path] as const,
    /** The decisions waiting to be read. A table, unlike `map`, which is derived. */
    mapDecisions: (projectId: string) => ["projects", projectId, "map", "decisions"] as const,
    /**
     * Everything this project's triager has ever silenced (§6.2).
     *
     * Its own key because it is its own route, and its own route because §6.2 says the pile is
     * *sempre acessível* — a pile reachable only as a slice of the map is one that disappears
     * whenever the map's own reading fails. It is a table, like the pile above and unlike `map`.
     */
    mapSilenced: (projectId: string) => ["projects", projectId, "map", "silenced"] as const,
    /** The write boundary. Under the roster prefix, so one write invalidates it with everything else. */
    ownership: (projectId: string) => ["projects", projectId, "ownership"] as const,
    /**
     * The text of one file inside that boundary, read from wherever the daemon keeps it. Under the
     * same prefix, so the write that changes it invalidates it.
     */
    owned: (projectId: string, path: string) => ["projects", projectId, "owned", path] as const,
    /** What this project can be asked to do to itself, and what each of them last said. */
    commands: (projectId: string) => ["projects", projectId, "commands"] as const,
    /**
     * The reach this project declares for itself — three lists, three keys.
     *
     * Three and not one, because they are three tables answering three questions and a page may
     * well show one of them without the others. Under the roster prefix like everything else about
     * a project, which is what lets a declaration write invalidate all three by naming `all` —
     * these are neighbours often edited in the same sitting.
     */
    shellRules: (projectId: string) => ["projects", projectId, "shell-rules"] as const,
    githubOps: (projectId: string) => ["projects", projectId, "github-ops"] as const,
    gitOps: (projectId: string) => ["projects", projectId, "git-ops"] as const,
    landTargets: (projectId: string) => ["projects", projectId, "land-targets"] as const,
    /**
     * Which repository on GitHub this project is — `GET /projects/{id}/github-repo`.
     *
     * Under the project prefix and beside the three above, because it is a fact about THIS project
     * and not about the machine: it is read off the project's own root. It is not one of the three
     * declarations — nothing declares it, git does — which is why it is its own key rather than a
     * field of one of theirs.
     */
    githubRepo: (projectId: string) => ["projects", projectId, "github-repo"] as const,
    /**
     * Which workflows this project uses, measured against the library right now.
     *
     * Under the project prefix and NOT under `keys.workflows` below, because it is a fact about
     * this project rather than about the machine — and because installing one also changes what
     * `ownership` answers, which is its neighbour here. The library itself is the machine's and
     * has its own root.
     */
    workflows: (projectId: string) => ["projects", projectId, "workflows"] as const,
    /** One workflow's divergence from its origin, asked for only when somebody opens it. */
    workflowDiff: (projectId: string, name: string) =>
      ["projects", projectId, "workflows", name, "diff"] as const,
    /** One workflow's graph, overlay already painted on by the núcleo. */
    workflowGraph: (projectId: string, name: string) =>
      ["projects", projectId, "workflows", name, "graph"] as const,
    /**
     * What is in a folder nobody has registered yet — `GET /projects/detect`.
     *
     * Keyed by the path, because two folders are two different answers and a cache that collapsed
     * them would show the previous folder's findings under the new one's name. Under the roster
     * prefix even though there is no project: registering one invalidates it, which is right — the
     * `taken_by` field becomes true the moment somebody finishes the wizard.
     */
    detect: (path: string) => ["projects", "detect", path] as const,
    branches: (projectId: string) => ["projects", projectId, "branches"] as const,
    log: (projectId: string, path: string) => ["projects", projectId, "log", path] as const,
    /**
     * Reads scoped to one run's worktree.
     *
     * The run is in the key and not only in the URL, because the same project and the same path
     * mean a different file in a different run's checkout — and a cache that collapsed them would
     * show one run's work while reviewing another's.
     */
    changed: (projectId: string, run: number) => ["projects", projectId, "changed", run] as const,
    runDiff: (projectId: string, run: number, path: string) =>
      ["projects", projectId, "run-diff", run, path] as const,
    runFile: (projectId: string, run: number, path: string) =>
      ["projects", projectId, "run-file", run, path] as const,
    runBlame: (projectId: string, run: number, path: string) =>
      ["projects", projectId, "run-blame", run, path] as const,
    worktree: (projectId: string, run: number) => ["projects", projectId, "worktree", run] as const,
  },

  proposals: {
    all: ["proposals"] as const,
  },

  /**
   * The bundles on this machine — `GET /workflows/library`.
   *
   * Its own root, and not a child of `projects`: one folder, shared by everything, and a page that
   * invalidates one project's pins must not throw away a listing that every project reads.
   */
  workflows: {
    all: ["workflows"] as const,
    library: ["workflows", "library"] as const,
  },

  /**
   * What any project MAY declare about GitHub — `GET /github/declarable-ops`.
   *
   * Its own root, and pointedly NOT under `projects`, for `workflows` above's reason carried one
   * step further: this is not merely shared between projects, it is the same answer for all of them
   * and cannot change while the daemon runs. It is derived from two ceilings compiled into the
   * binary, so a declaration write — which invalidates `keys.projects.all` — must not throw it away.
   */
  github: {
    all: ["github"] as const,
    declarableOps: ["github", "declarable-ops"] as const,
    /**
     * What one listing read said about one repository — `POST /github/requests`.
     *
     * **Keyed by the repository and not by the project**, which is the whole reason it is here
     * rather than under `projects`. The answer is a fact about a repository on GitHub, so two
     * projects rooted at two worktrees of the same repository ask one question and share one
     * answer — and, more to the point, a declaration write invalidating `keys.projects.all` must
     * not throw away a listing that cost a network call and cannot have changed because somebody
     * ticked a checkbox.
     */
    listing: (op: string, repo: string) => ["github", "listing", op, repo] as const,
  },

  /**
   * What any project MAY declare about the shared git queue — `GET /vcs/declarable-ops`.
   *
   * Its own root, and pointedly NOT under `projects`, for the same reason as the GitHub catalogue:
   * this is the machine's answer, identical for every project and fixed for the life of the daemon.
   * A declaration write must not throw away a compiled catalogue that it cannot have changed.
   */
  vcs: {
    all: ["vcs"] as const,
    declarableOps: ["vcs", "declarable-ops"] as const,
  },

  /**
   * What the agent has been told, and what it asked to be told — `GET
   * /knowledge`.
   *
   * Its own root and not a child of `proposals`, although something waiting
   * for an answer IS a proposal: `GET /proposals` filters `kind =
   * 'action-approval'` (`core/src/proposals.rs`), so the two lists never
   * overlap, and a page invalidating one would refetch a list that cannot have
   * changed. `detail` carries the chain, which is the half the listing has no
   * room for.
   */
  knowledge: {
    all: ["knowledge"] as const,
    detail: (id: number) => ["knowledge", "detail", id] as const,
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
    /**
     * The download of a local model this machine does not have —
     * `GET /assistant/local-model/pull`.
     *
     * A sibling of `localModel` and deliberately NOT `[...localModel, "pull"]`,
     * though the routes are nested that way: react-query matches by prefix, so
     * the nested key would be swept by every invalidation of `localModel`. The
     * two answer questions with opposite cadences — that one is what STARTUP
     * resolved and cannot change while the daemon runs, this one moves every
     * second while a download is going — and sharing a prefix would tie the
     * settled one to the moving one.
     */
    localPull: ["chats", "local-pull"] as const,
    /**
     * What a model weighs and whether this machine can carry it —
     * `GET /assistant/local-model/size?model=…`.
     *
     * A sibling of the two above for the same prefix reason, and keyed by model
     * because the answer is per model: two absent rows in one menu ask this at
     * the same time and must not overwrite each other. It is the most settled of
     * the three — a model's size and this machine's memory do not move — so it
     * is also the one that should never be swept by a download's invalidations.
     */
    localSize: (model: string) => ["chats", "local-size", model] as const,
    /**
     * The models a conversation may be moved to — `GET /assistant/models`.
     *
     * Its own key and not a child of `detail`, because it is the daemon-wide
     * answer. A conversation picker uses `modelsFor` so its menu can differ.
     */
    models: ["chats", "models"] as const,
    modelsFor: (chatId: string) => ["chats", "models", chatId] as const,
    /**
     * The tools a conversation may be told not to reach for — `GET /assistant/tools`.
     *
     * Beside `models` and for the same reason: one answer for every conversation,
     * so one fetch feeds every picker on the page.
     */
    tools: ["chats", "tools"] as const,
    /** The commands the front door offers, before a conversation exists to scope them. */
    frontCommands: (query: string) => ["chats", "front-commands", query] as const,
    project: (chatId: string) => ["chats", "project", chatId] as const,
    diff: (chatId: string) => ["chats", "diff", chatId] as const,
    ideSessions: ["chats", "ide-sessions"] as const,
    ideSession: (sessionId: string) => ["chats", "ide-session", sessionId] as const,
    live: (turnId: number) => ["chats", "live", turnId] as const,
    /** Keyed by the query too: each keystroke is a different question, and its own cached answer. */
    files: (chatId: string, query: string) => ["chats", "files", chatId, query] as const,
    commands: (chatId: string, query: string) => ["chats", "commands", chatId, query] as const,
    /**
     * What one turn's tools answered — `GET /assistant/turns/{id}/tools`.
     *
     * Keyed by the turn and NOT a child of `detail`, deliberately: invalidating a conversation
     * happens on every poll tick, and these are settled facts about a run that has ended. A turn's
     * answers do not change, and re-reading two thousand characters per tool because a different
     * turn landed would be work for nothing.
     */
    turnTools: (turnId: number) => ["chats", "turn-tools", turnId] as const,
    /** Something that was said, across every conversation. Keyed by the query, like `files`. */
    said: (query: string) => ["chats", "said", query] as const,
    /**
     * The whole path one relayed turn travelled.
     *
     * Keyed by the turn and not by the chat: a chain is a property of the turn,
     * and two relayed turns of one conversation came from different places.
     */
    relayChain: (chatId: string, turnId: number) =>
      ["chats", "relay-chain", chatId, turnId] as const,
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
     * `GET /browser/writes/{project_id}` — what agents have submitted in
     * this project's profile. Its own entry and not folded into `sites`,
     * because the two are read together on one screen and refetched by
     * different things: a grant changes when a person answers a login, and
     * this changes every time an agent presses Send.
     */
    writes: (projectId: string) => ["browser", "writes", projectId] as const,
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
    /**
     * Which feed kinds still reach Telegram — `GET /notifications/policy` — and
     * which kinds this machine has actually written — `GET /notifications/kinds`.
     *
     * Two keys and not one, because they are two different things that happen to
     * be drawn on one screen: the first is a PREFERENCE the owner edits and a
     * save invalidates, the second an OBSERVATION of the feed that a save cannot
     * change. Sharing a key would make every save refetch the kind list for
     * nothing.
     */
    notifyPolicy: ["system", "notify-policy"] as const,
    notifyKinds: ["system", "notify-kinds"] as const,
    config: (area: string) => ["system", "config", area] as const,
    /**
     * This machine's settings files — `GET /config/machine`.
     *
     * A sibling of `config` above rather than a child of it, and the difference
     * is not cosmetic: that one keys the daemon's PARSED view of a pillar — what
     * it is actually doing, clamps applied — and this one keys the files on
     * disk. The two disagree exactly when somebody has edited a file and not
     * restarted, which is the state the settings page exists to show. A write
     * invalidating this must not also throw away the running readout it is
     * about to be compared against.
     */
    machine: ["system", "machine"] as const,
    /** The credentials this machine holds, by presence only -- `GET /config/secrets`. */
    secrets: ["system", "secrets"] as const,
  },
} as const;
