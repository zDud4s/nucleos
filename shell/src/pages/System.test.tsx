import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import {
  Outlet,
  RouterProvider,
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { System } from "./System";
import { createAppQueryClient } from "../app/queryClient";
import { ApiRefusal } from "../data/client";
import type {
  ApiTokenLevel,
  ApiTokenSummary,
  BackupInfo,
  BudgetView,
  CalendarConfig,
  CreatedApiToken,
  EmailConfig,
  HealthReadout,
  PiiTallyRow,
  ProjectSummary,
  SidecarState,
  VoiceConfig,
} from "../data/system";
import type { ScopedKill } from "../data/autopilot";
import { daemonFetch, daemonState, project, renderWithRouter } from "../test/harness";

/** The dwell `ConfirmButton` needs between arming and confirming — a fast `findBy*` resolves inside it. */
async function afterDwell(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 350));
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
});

/* ------------------------------------------------------------- fixtures -- */

/** The daemon's own subsystem order — `health.rs:142-183`. Render, never sort. */
const DAEMON_ORDER = [
  "sqlite_pool",
  "cli_binary",
  "credential_manager",
  "worktree_disk",
  "echo_sidecar",
  "telegram_sidecar",
  "email_sidecar",
  "web_sidecar",
  "browser_sidecar",
  "voice_transcriber",
] as const;

/** The three config fixtures — one shape each, none of them optional on the wire. */
const DEFAULT_EMAIL_CONFIG: EmailConfig = {
  enabled: true,
  armed: true,
  host: "imap.example.com",
  username: "duarte@example.com",
  mailbox: "INBOX",
  sent_mailbox: null,
  poll_interval_secs: 300,
  notify_classes: ["urgent", "action"],
  digest_hour_utc: 7,
  retain_bodies_days: 30,
  local_triage_disabled: null,
};

const DEFAULT_VOICE_CONFIG: VoiceConfig = {
  armed: true,
  hints: [],
  cleanup_prompt: "tidy this up",
  cleanup_model: null,
  retain_dictations_days: 14,
  hotkey: "Ctrl+Shift+V",
  memo_hotkey: "Ctrl+Shift+M",
  max_capture_seconds: 120,
  max_body_bytes: 1_000_000,
};

const DEFAULT_CALENDAR_CONFIG: CalendarConfig = {
  default_tz: "Europe/Lisbon",
  working_hours_start: "09:00",
  working_hours_end: "18:00",
  working_weekdays: ["mon", "tue", "wed", "thu", "fri"],
};

interface SystemWorld {
  readout: HealthReadout;
  sidecars: SidecarState[];
  projects: ProjectSummary[];
  kills: ScopedKill[];
  budget: BudgetView;
  backups: BackupInfo[];
  pii: PiiTallyRow[];
  tokens: ApiTokenSummary[];
  emailConfig: EmailConfig;
  voiceConfig: VoiceConfig;
  calendarConfig: CalendarConfig;
}

function systemWorld(overrides: Partial<SystemWorld> = {}): SystemWorld {
  return {
    readout: { status: "ok", subsystems: DAEMON_ORDER.map((name) => ({ name, status: "ok" })) },
    sidecars: [],
    projects: [],
    kills: [],
    budget: daemonState().budget,
    backups: [],
    pii: [],
    tokens: [],
    emailConfig: DEFAULT_EMAIL_CONFIG,
    voiceConfig: DEFAULT_VOICE_CONFIG,
    calendarConfig: DEFAULT_CALENDAR_CONFIG,
    ...overrides,
  };
}

/**
 * The pillar's own routes, over the foundation's responder — `Browser.test.tsx`'s
 * pattern. Anything this does not know falls through to the shared fixture, so
 * `/proposals` and the global `/autopilot/kill` still answer without this file
 * teaching them.
 *
 * `opts.onRestore`, when given, replaces the default restore handling for
 * `POST /backups/{name}/restore` — the seam a test uses to make that route
 * throw a refusal instead of staging. `opts.onMint` does the same for
 * `POST /api-tokens`.
 */
function systemFetch(
  world: SystemWorld,
  opts: { onRestore?: (name: string) => unknown; onMint?: (mint: { name: string; level: ApiTokenLevel }) => unknown } = {},
): (path: string, init?: RequestInit) => Promise<unknown> {
  const shared = daemonFetch(daemonState({ projects: world.projects }));
  return async (path, init) => {
    if (init?.method === "POST") {
      if (path === "/autopilot/kill/scoped" && typeof init.body === "string") {
        const change = JSON.parse(init.body) as ScopedKill;
        const known = world.kills.some(
          (row) => row.scope_type === change.scope_type && row.scope_id === change.scope_id,
        );
        world.kills = known
          ? world.kills.map((row) =>
              row.scope_type === change.scope_type && row.scope_id === change.scope_id ? change : row,
            )
          : [...world.kills, change];
        return undefined;
      }
      if (path === "/autopilot/budget" && typeof init.body === "string") {
        const change = JSON.parse(init.body) as Record<string, unknown>;
        world.budget = { ...world.budget, ...change } as BudgetView;
        return world.budget;
      }
      if (path === "/backup") {
        const created: BackupInfo = {
          name: `snap-${String(world.backups.length + 1)}`,
          migration_version: 60,
          size_bytes: 1024,
        };
        world.backups = [...world.backups, created];
        return created;
      }
      const restoreMatch = /^\/backups\/([^/]+)\/restore$/.exec(path);
      if (restoreMatch !== null) {
        const name = decodeURIComponent(restoreMatch[1]);
        if (opts.onRestore !== undefined) return opts.onRestore(name);
        const backup = world.backups.find((row) => row.name === name);
        return {
          name,
          migration_version: backup?.migration_version ?? 0,
          applies: `applies on the núcleo's next start, replacing everything written after ${name}`,
        };
      }
      if (path === "/api-tokens" && typeof init.body === "string") {
        const mint = JSON.parse(init.body) as { name: string; level: ApiTokenLevel };
        if (opts.onMint !== undefined) return opts.onMint(mint);
        if (world.tokens.some((row) => row.name === mint.name)) {
          throw new ApiRefusal(409, "conflict", "conflict");
        }
        const created: CreatedApiToken = {
          name: mint.name,
          level: mint.level,
          created_at: "2026-08-18T09:00:00Z",
          token: `secret-${mint.name}`,
        };
        world.tokens = [...world.tokens, { name: created.name, level: created.level, created_at: created.created_at }];
        return created;
      }
      return await shared(path, init);
    }

    if (init?.method === "DELETE") {
      const revokeMatch = /^\/api-tokens\/([^/]+)$/.exec(path);
      if (revokeMatch !== null) {
        const name = decodeURIComponent(revokeMatch[1]);
        const known = world.tokens.some((row) => row.name === name);
        if (!known) throw new ApiRefusal(404, "not_found", "not_found");
        world.tokens = world.tokens.filter((row) => row.name !== name);
        return undefined;
      }
      return await shared(path, init);
    }

    switch (path) {
      case "/health/readout":
        return world.readout;
      case "/sidecars":
        return world.sidecars;
      case "/autopilot/kill/scoped":
        return world.kills;
      case "/autopilot/budget":
        return world.budget;
      case "/backups":
        return world.backups;
      case "/pii/observations":
        return world.pii;
      case "/api-tokens":
        return world.tokens;
      case "/config/email":
        return world.emailConfig;
      case "/voice/config":
        return world.voiceConfig;
      case "/calendar/config":
        return world.calendarConfig;
      default:
        return await shared(path, init);
    }
  };
}

function renderSystem() {
  return renderWithRouter(<System />, { initialPath: "/system" });
}

/**
 * The `$view` route, built locally — `renderWithRouter`'s tree comes from
 * `NAV_PATHS`, which has `/system` and not `/system/$view`. Same shape as
 * `Projects.test.tsx`'s `renderProjects`.
 */
async function renderSystemAt(initialPath: string) {
  const queryClient = createAppQueryClient();
  const rootRoute = createRootRoute({ component: () => <Outlet /> });
  const routes = [
    createRoute({ getParentRoute: () => rootRoute, path: "/system", component: System }),
    createRoute({ getParentRoute: () => rootRoute, path: "/system/$view", component: System }),
  ];
  const router = createRouter({
    routeTree: rootRoute.addChildren(routes),
    history: createMemoryHistory({ initialEntries: [initialPath] }),
    defaultPreload: false,
  });

  await router.load();
  const result = render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { ...result, router, queryClient };
}

function rowFor(list: HTMLElement, name: string): HTMLElement {
  const row = within(list).getByText(name).closest("li");
  if (row === null) throw new Error(`no row for ${name}`);
  return row as HTMLElement;
}

/* ---------------------------------------------------------------- health -- */

describe("System - health readout", () => {
  it("renders the daemon's subsystems in order with reason and keeps disabled apart from down", async () => {
    const world = systemWorld({
      readout: {
        status: "degraded",
        subsystems: [
          { name: "sqlite_pool", status: "ok" },
          { name: "cli_binary", status: "ok" },
          { name: "credential_manager", status: "disabled" },
          { name: "worktree_disk", status: "degraded", reason: "low-disk-space" },
          { name: "echo_sidecar", status: "ok" },
          { name: "telegram_sidecar", status: "disabled" },
          { name: "email_sidecar", status: "down", reason: "not-running" },
          { name: "web_sidecar", status: "ok" },
          { name: "browser_sidecar", status: "down", reason: "unreachable" },
          { name: "voice_transcriber", status: "ok" },
        ],
      },
    });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystem();

    const list = await screen.findByRole("list", { name: "Subsystems" });

    // Rendered in the daemon's own order, not re-sorted.
    const names = within(list)
      .getAllByRole("listitem")
      .map((row) => row.querySelector(".sy-subsystem-name")?.textContent);
    expect(names).toEqual([...DAEMON_ORDER]);

    // Reasons render for the rows that have one.
    expect(within(list).getByText(/low-disk-space/)).toBeDefined();
    expect(within(list).getByText(/not-running/)).toBeDefined();
    expect(within(list).getByText(/unreachable/)).toBeDefined();

    // A disabled subsystem reads as "not configured", never as "down" — and a
    // down one reads as "down", never as merely "not configured".
    const credentialManager = rowFor(list, "credential_manager");
    expect(credentialManager.textContent).toMatch(/not configured/i);
    expect(credentialManager.textContent).not.toMatch(/\bdown\b/i);

    // No whitespace separates the name span from the badge in the DOM, so a
    // `\b`-anchored match would fail at the seam — match the word, not its
    // boundary.
    const emailSidecar = rowFor(list, "email_sidecar");
    expect(emailSidecar.textContent).toMatch(/down/i);
  });

  it("renders an aggregate timeout as one collapsed readout, not nine missing subsystems", async () => {
    const world = systemWorld({
      readout: { status: "down", subsystems: [{ name: "aggregate", status: "down", reason: "timeout" }] },
    });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystem();

    // Scoped to the panel's own status text: the page headline also says
    // "timed out", and a bare `findByText` throws on the two matches.
    const notice = await screen.findByRole("status");
    expect(notice.textContent).toMatch(/timed out/i);
    expect(screen.queryByText(/missing/i)).toBeNull();

    // None of the ten real subsystem names appear — the readout says it never
    // reached them, rather than drawing nine rows for subsystems it never
    // measured.
    for (const name of DAEMON_ORDER) {
      expect(screen.queryByText(name)).toBeNull();
    }
    expect(screen.queryByRole("list", { name: "Subsystems" })).toBeNull();
  });
});

/* --------------------------------------------------------------- sidecars -- */

describe("System - sidecar cards", () => {
  it("shows sidecar uptime, restarts, last failure and last line, omitting what is absent", async () => {
    const world = systemWorld({
      sidecars: [
        {
          name: "browser",
          state: "down",
          started_at: null,
          last_failure: "exited: exit code: 1",
          last_failure_at: "2026-08-18T08:00:00Z",
          restarts: 3,
          last_line: "panic: no display",
          last_line_at: "2026-08-18T08:00:01Z",
        },
        {
          name: "web",
          state: "running",
          started_at: "2026-08-18T07:00:00Z",
          last_failure: null,
          last_failure_at: null,
          restarts: 0,
          last_line: null,
          last_line_at: null,
        },
      ],
    });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystem();

    const list = await screen.findByRole("list", { name: "Sidecars" });
    const browserCard = rowFor(list, "browser");
    const webCard = rowFor(list, "web");

    // browser: no recorded start, so no "started" fact — but the failure, the
    // last line and the restart count all show.
    expect(within(browserCard).queryByText("started")).toBeNull();
    expect(browserCard.textContent).toMatch(/exited: exit code: 1/);
    expect(browserCard.textContent).toMatch(/panic: no display/);
    expect(browserCard.textContent).toMatch(/restarts/i);
    expect(browserCard.textContent).toMatch(/3/);

    // web: a real start time shows as "started" — and with nothing to report,
    // neither a failure line nor a last-line fact renders at all.
    expect(within(webCard).getByText("started")).toBeDefined();
    expect(webCard.textContent).not.toMatch(/exited/);
    expect(within(webCard).queryByRole("alert")).toBeNull();
  });
});

/* ---------------------------------------------------------- project brakes -- */

describe("System - project brakes", () => {
  it("holds and releases one project's brake without touching the trigger brakes", async () => {
    const world = systemWorld({
      projects: [project({ project_id: "alpha" }), project({ project_id: "beta" })],
    });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystem();

    const list = await screen.findByRole("list", { name: "Project brakes" });
    const alphaRow = rowFor(list, "alpha");

    fireEvent.click(within(alphaRow).getByRole("button", { name: "Hold alpha" }));

    await waitFor(() => {
      expect(within(alphaRow).queryByRole("button", { name: "Release alpha" })).not.toBeNull();
    });

    const holdCall = daemon.apiFetch.mock.calls.find(
      ([path, init]) =>
        path === "/autopilot/kill/scoped" && (init as RequestInit | undefined)?.method === "POST",
    );
    expect(holdCall).toBeDefined();
    const body = JSON.parse((holdCall?.[1] as RequestInit).body as string) as Record<string, unknown>;
    expect(body).toEqual({ scope_type: "project", scope_id: "alpha", engaged: true });

    // No call this test made ever named a trigger scope — the panel writes
    // project scopes only.
    const triggerCalls = daemon.apiFetch.mock.calls.filter(([path, init]) => {
      if (path !== "/autopilot/kill/scoped" || (init as RequestInit | undefined)?.method !== "POST") {
        return false;
      }
      const parsed = JSON.parse((init as RequestInit).body as string) as { scope_type: string };
      return parsed.scope_type === "trigger";
    });
    expect(triggerCalls).toHaveLength(0);

    fireEvent.click(within(alphaRow).getByRole("button", { name: "Release alpha" }));

    await waitFor(() => {
      expect(within(alphaRow).queryByRole("button", { name: "Hold alpha" })).not.toBeNull();
    });
  });
});

/* ------------------------------------------------------------------ budget -- */

describe("System - budget", () => {
  it("sends all five budget fields and keeps no ceiling apart from a ceiling of zero", async () => {
    const world = systemWorld({
      budget: {
        limit_usd: 5,
        period: "daily",
        hourly_limit_usd: null,
        per_run_reserve_usd: 0.25,
        time_cost_per_hour_usd: 0.1,
        window_spend_usd: 1.2,
        hourly_spend_usd: 0.05,
        paused: false,
        reason: null,
      },
    });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystem();

    const windowInput = await screen.findByLabelText("Window limit (USD, blank = no ceiling)");
    fireEvent.change(windowInput, { target: { value: "" } });

    fireEvent.click(screen.getByRole("button", { name: "Save budget" }));
    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: "Send these five fields to the daemon" }));

    await waitFor(() => {
      const calls = daemon.apiFetch.mock.calls.filter(
        ([path, init]) => path === "/autopilot/budget" && (init as RequestInit | undefined)?.method === "POST",
      );
      expect(calls).toHaveLength(1);
    });

    const budgetCalls = () =>
      daemon.apiFetch.mock.calls.filter(
        ([path, init]) => path === "/autopilot/budget" && (init as RequestInit | undefined)?.method === "POST",
      );

    const firstBody = JSON.parse((budgetCalls()[0][1] as RequestInit).body as string) as Record<
      string,
      unknown
    >;
    expect(Object.keys(firstBody).sort()).toEqual(
      ["hourly_limit_usd", "limit_usd", "per_run_reserve_usd", "period", "time_cost_per_hour_usd"].sort(),
    );
    expect(firstBody.limit_usd).toBeNull();
    expect(firstBody.period).toBe("daily");
    expect(firstBody.hourly_limit_usd).toBeNull();
    expect(firstBody.per_run_reserve_usd).toBe(0.25);
    expect(firstBody.time_cost_per_hour_usd).toBe(0.1);

    // Typing an actual zero is a different fact from leaving the box blank.
    fireEvent.change(windowInput, { target: { value: "0" } });
    fireEvent.click(screen.getByRole("button", { name: "Save budget" }));
    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: "Send these five fields to the daemon" }));

    await waitFor(() => {
      expect(budgetCalls()).toHaveLength(2);
    });

    const secondBody = JSON.parse((budgetCalls()[1][1] as RequestInit).body as string) as Record<
      string,
      unknown
    >;
    expect(secondBody.limit_usd).toBe(0);
  });

  it.each(["abc", "Infinity"])(
    "refuses to save when a ceiling box holds text that is not a number (%s)",
    async (garbage) => {
      const world = systemWorld({
        budget: {
          limit_usd: 5,
          period: "daily",
          hourly_limit_usd: null,
          per_run_reserve_usd: 0.25,
          time_cost_per_hour_usd: 0.1,
          window_spend_usd: 1.2,
          hourly_spend_usd: 0.05,
          paused: false,
          reason: null,
        },
      });
      daemon.apiFetch.mockImplementation(systemFetch(world));

      await renderSystem();

      const windowInput = await screen.findByLabelText("Window limit (USD, blank = no ceiling)");
      fireEvent.change(windowInput, { target: { value: garbage } });

      fireEvent.click(screen.getByRole("button", { name: "Save budget" }));
      await afterDwell();
      fireEvent.click(screen.getByRole("button", { name: "Send these five fields to the daemon" }));

      const alert = await screen.findByRole("alert");
      expect(alert.textContent).toMatch(/the window limit/);

      const calls = daemon.apiFetch.mock.calls.filter(
        ([path, init]) => path === "/autopilot/budget" && (init as RequestInit | undefined)?.method === "POST",
      );
      expect(calls).toHaveLength(0);
    },
  );

  it("names the hourly limit when that is the box that will not parse", async () => {
    const world = systemWorld({
      budget: {
        limit_usd: 5,
        period: "daily",
        hourly_limit_usd: null,
        per_run_reserve_usd: 0.25,
        time_cost_per_hour_usd: 0.1,
        window_spend_usd: 1.2,
        hourly_spend_usd: 0.05,
        paused: false,
        reason: null,
      },
    });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystem();

    const hourlyInput = await screen.findByLabelText("Hourly limit (USD, blank = no ceiling)");
    fireEvent.change(hourlyInput, { target: { value: "abc" } });

    fireEvent.click(screen.getByRole("button", { name: "Save budget" }));
    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: "Send these five fields to the daemon" }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/the hourly limit/);
    expect(alert.textContent).not.toMatch(/the window limit/);

    const calls = daemon.apiFetch.mock.calls.filter(
      ([path, init]) => path === "/autopilot/budget" && (init as RequestInit | undefined)?.method === "POST",
    );
    expect(calls).toHaveLength(0);
  });
});

/* ----------------------------------------------------------------- backups -- */

describe("System - backups", () => {
  it("lists backups and says a restore is staged for the next start", async () => {
    const world = systemWorld({
      backups: [
        { name: "snap-1", migration_version: 60, size_bytes: 2048 },
        { name: "snap-0", migration_version: null, size_bytes: 512 },
      ],
    });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/backups");

    const list = await screen.findByRole("list", { name: "Backups" });
    const snap1Row = rowFor(list, "snap-1");
    const snap0Row = rowFor(list, "snap-0");

    // A missing migration version reads as "unknown", never as zero.
    expect(snap0Row.textContent).toMatch(/unknown/i);

    fireEvent.click(within(snap1Row).getByRole("button", { name: "Stage a restore" }));
    await afterDwell();
    fireEvent.click(within(snap1Row).getByRole("button", { name: "Restore snap-1 on next start" }));

    const notice = await within(snap1Row).findByRole("status");
    expect(notice.textContent).toMatch(/applies on the núcleo's next start/i);
    expect(notice.textContent).toMatch(/nothing has changed yet/i);
  });

  it("says a restore is already staged when the daemon answers 409", async () => {
    const world = systemWorld({
      backups: [{ name: "snap-1", migration_version: 60, size_bytes: 2048 }],
    });
    daemon.apiFetch.mockImplementation(
      systemFetch(world, {
        onRestore: () => {
          throw new ApiRefusal(409, "conflict", "conflict");
        },
      }),
    );

    await renderSystemAt("/system/backups");

    const list = await screen.findByRole("list", { name: "Backups" });
    const row = rowFor(list, "snap-1");

    fireEvent.click(within(row).getByRole("button", { name: "Stage a restore" }));
    await afterDwell();
    fireEvent.click(within(row).getByRole("button", { name: "Restore snap-1 on next start" }));

    expect(
      await within(row).findByText(
        "a restore is already staged, or that snapshot name is taken — only one can be pending",
      ),
    ).toBeDefined();
  });
});

/* -------------------------------------------------------------------- pii -- */

describe("System - PII observations", () => {
  it("renders the PII tally as column, class and count", async () => {
    const world = systemWorld({
      pii: [
        { column: "contacts.phone", class: "phone", count: 12 },
        { column: "mail.body_text", class: "email", count: 4 },
      ],
    });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/backups");

    const table = await screen.findByRole("table");
    expect(within(table).getByText("contacts.phone")).toBeDefined();
    expect(within(table).getByText("phone")).toBeDefined();
    expect(within(table).getByText("12")).toBeDefined();
    expect(within(table).getByText("mail.body_text")).toBeDefined();
    expect(within(table).getByText("email")).toBeDefined();
    expect(within(table).getByText("4")).toBeDefined();
  });
});

/* ----------------------------------------------------------------- tokens -- */

describe("System - tokens", () => {
  it("shows a minted token's secret once and not in the listing", async () => {
    const world = systemWorld();
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/tokens");

    const nameInput = await screen.findByLabelText("Name");
    fireEvent.change(nameInput, { target: { value: "ci-reader" } });
    fireEvent.click(screen.getByRole("button", { name: "Mint token" }));

    const secretField = await screen.findByLabelText("the new token for ci-reader");
    expect((secretField as HTMLInputElement).value).toBe("secret-ci-reader");

    // The listing refetches after the mint and shows the new row — but never
    // the secret, which lives only in the CopyOnce state above.
    const list = await screen.findByRole("list", { name: "API tokens" });
    const row = rowFor(list, "ci-reader");
    expect(row.textContent).not.toMatch(/secret-ci-reader/);

    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(screen.queryByLabelText("the new token for ci-reader")).toBeNull();
  });

  it("refuses a duplicate token name with the daemon's 409 as a value", async () => {
    const world = systemWorld({
      tokens: [{ name: "ci-reader", level: "read-only", created_at: "2026-08-17T09:00:00Z" }],
    });
    daemon.apiFetch.mockImplementation(
      systemFetch(world, {
        onMint: () => {
          throw new ApiRefusal(409, "conflict", "Conflict");
        },
      }),
    );

    await renderSystemAt("/system/tokens");

    const nameInput = await screen.findByLabelText("Name");
    fireEvent.change(nameInput, { target: { value: "ci-reader" } });
    fireEvent.click(screen.getByRole("button", { name: "Mint token" }));

    expect(await screen.findByText("there is already a token with that name")).toBeDefined();
    // No secret ever reached the screen — the refusal is the whole outcome.
    expect(screen.queryByRole("group", { name: "a secret shown once" })).toBeNull();
  });
});

/* ------------------------------------------------------------ config index -- */

describe("System - config index", () => {
  it("names the config areas that have no route instead of failing to read them", async () => {
    const world = systemWorld();
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/tokens");

    const areas = await screen.findByRole("list", { name: "Areas with no configuration route" });
    for (const area of ["web", "browser", "council", "models"]) {
      expect(within(areas).getByText(area)).toBeDefined();
    }

    // The three real routes were read...
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/config/email");
      expect(daemon.apiFetch).toHaveBeenCalledWith("/voice/config");
      expect(daemon.apiFetch).toHaveBeenCalledWith("/calendar/config");
    });

    // ...and nothing shaped like the four that do not exist was ever asked for.
    const badCall = daemon.apiFetch.mock.calls.find(([path]) => {
      const p = path as string;
      return /\/config\/(web|browser|council|models)\b/.test(p) || /^\/(web|browser|council|models)\/config$/.test(p);
    });
    expect(badCall).toBeUndefined();
  });
});

/* ------------------------------------------------------------------ route -- */

describe("System - the route", () => {
  it("falls back to the health view for an unrecognised view parameter", async () => {
    const world = systemWorld();
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/bogus");

    const healthTab = await screen.findByRole("link", { name: "Health" });
    expect(healthTab.getAttribute("aria-current")).toBe("page");
    expect(await screen.findByRole("heading", { level: 2, name: "Subsystems" })).toBeDefined();
  });
});
