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
import type { MachineConfig, MachineSecret } from "../data/machine-config";
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

/** The fixture's daemon order — the page sorts by health, worst first, and stays in daemon order inside each group. */
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

interface QuotaBrakeView {
  enabled: boolean;
  pause_above_percent_5h: number;
  pause_above_percent_7d: number;
  provider: string;
}

interface SystemWorld {
  readout: HealthReadout;
  sidecars: SidecarState[];
  projects: ProjectSummary[];
  kills: ScopedKill[];
  budget: BudgetView;
  quotaBrake: QuotaBrakeView;
  backups: BackupInfo[];
  pii: PiiTallyRow[];
  tokens: ApiTokenSummary[];
  emailConfig: EmailConfig;
  voiceConfig: VoiceConfig;
  calendarConfig: CalendarConfig;
  machine: MachineConfig;
  secrets: MachineSecret[];
}

/**
 * Every area the núcleo serves a settings row for, in the order it serves them.
 *
 * The four at the end are the ones the removed "Not exposed by the núcleo"
 * panel used to name. They are in this list rather than in a comment because
 * that is the fact the tests below now assert.
 */
const MACHINE_AREAS = [
  "email",
  "voice",
  "calendar",
  "web",
  "browser",
  "telegram",
  "github",
  "council",
  "models",
] as const;

function machineWorld(): MachineConfig {
  return {
    root: "C:/Projects/nucleos",
    settings: MACHINE_AREAS.map((area) => ({
      path: `.ai/${area}.yaml`,
      area,
      what: `what ${area} does`,
      takes_effect: "when the daemon restarts",
      exists: false,
      contents: null,
      resolved: `C:/Projects/nucleos/.ai/${area}.yaml`,
    })),
  };
}

function systemWorld(overrides: Partial<SystemWorld> = {}): SystemWorld {
  return {
    readout: { status: "ok", subsystems: DAEMON_ORDER.map((name) => ({ name, status: "ok" })) },
    sidecars: [],
    projects: [],
    kills: [],
    budget: daemonState().budget,
    quotaBrake: {
      enabled: false,
      pause_above_percent_5h: 85,
      pause_above_percent_7d: 90,
      provider: "claude",
    },
    backups: [],
    pii: [],
    tokens: [],
    emailConfig: DEFAULT_EMAIL_CONFIG,
    voiceConfig: DEFAULT_VOICE_CONFIG,
    calendarConfig: DEFAULT_CALENDAR_CONFIG,
    machine: machineWorld(),
    secrets: [
      { key: "github-token", area: "github", what: "the token gh is handed", present: false },
      { key: "web-search-api-key", area: "web", what: "the search provider's key", present: true },
    ],
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
  opts: {
    onRestore?: (name: string) => unknown;
    onRestart?: (name: string) => unknown;
    onMint?: (mint: { name: string; level: ApiTokenLevel }) => unknown;
  } = {},
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
      if (path === "/autopilot/quota-brake" && typeof init.body === "string") {
        const change = JSON.parse(init.body) as Omit<QuotaBrakeView, "provider">;
        world.quotaBrake = { ...change, provider: world.quotaBrake.provider };
        return world.quotaBrake;
      }
      if (path === "/config/machine" && typeof init.body === "string") {
        const write = JSON.parse(init.body) as { path: string; contents: string };
        const row = world.machine.settings.find((entry) => entry.path === write.path);
        if (row === undefined) throw new ApiRefusal(403, "not_ours", "not_ours");
        // The daemon validates before it writes; the double is only as strict as
        // it needs to be to keep that ordering observable from a test.
        if (write.contents.includes("!!bad")) {
          throw new ApiRefusal(422, "invalid", "did not find expected node content");
        }
        world.machine = {
          ...world.machine,
          settings: world.machine.settings.map((entry) =>
            entry.path === write.path
              ? { ...entry, exists: true, contents: write.contents }
              : entry,
          ),
        };
        return undefined;
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
      const restartMatch = /^\/sidecars\/([^/]+)\/restart$/.exec(path);
      if (restartMatch !== null) {
        const name = decodeURIComponent(restartMatch[1]);
        if (opts.onRestart !== undefined) return opts.onRestart(name);
        return { name, asked: true };
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

    if (init?.method === "PUT") {
      const putMatch = /^\/config\/secrets\/([^/]+)$/.exec(path);
      if (putMatch !== null && typeof init.body === "string") {
        const key = decodeURIComponent(putMatch[1]);
        const row = world.secrets.find((entry) => entry.key === key);
        if (row === undefined) throw new ApiRefusal(403, "not_ours", "not_ours");
        const { value } = JSON.parse(init.body) as { value: string };
        if (value === "") {
          throw new ApiRefusal(422, "invalid", "an empty value is not a credential");
        }
        world.secrets = world.secrets.map((entry) =>
          entry.key === key ? { ...entry, present: true } : entry,
        );
        return undefined;
      }
      return await shared(path, init);
    }

    if (init?.method === "DELETE") {
      const forgetMatch = /^\/config\/secrets\/([^/]+)$/.exec(path);
      if (forgetMatch !== null) {
        const key = decodeURIComponent(forgetMatch[1]);
        world.secrets = world.secrets.map((entry) =>
          entry.key === key ? { ...entry, present: false } : entry,
        );
        return undefined;
      }
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
      case "/autopilot/quota-brake":
        return world.quotaBrake;
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
      case "/config/machine":
        return world.machine;
      case "/config/secrets":
        return { secrets: world.secrets };
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
  it("the headline says what is wrong in the tone for wrong", async () => {
    const world = systemWorld({
      readout: {
        status: "degraded",
        subsystems: [
          { name: "sqlite_pool", status: "down" },
          { name: "cli_binary", status: "degraded" },
        ],
      },
    });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystem();

    expect((await screen.findByText("1 down, 1 degraded")).className).toContain("ui-wrong");
  });

  it("leaves a healthy headline untoned", async () => {
    daemon.apiFetch.mockImplementation(systemFetch(systemWorld()));

    await renderSystem();

    expect((await screen.findByText("every configured subsystem is healthy")).className).not.toContain(
      "ui-wrong",
    );
  });

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

    // Down comes first, then degraded, with daemon order preserved inside each group.
    const names = within(list)
      .getAllByRole("listitem")
      .map((row) => row.querySelector(".sy-subsystem-name")?.textContent);
    const expectedOrder = [
      ...world.readout.subsystems.filter((subsystem) => subsystem.status === "down"),
      ...world.readout.subsystems.filter((subsystem) => subsystem.status === "degraded"),
      ...world.readout.subsystems.filter(
        (subsystem) => subsystem.status !== "down" && subsystem.status !== "degraded",
      ),
    ].map((subsystem) => subsystem.name);
    expect(names).toEqual(expectedOrder);

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

    // Found by its own opening words, and then checked to BE a live region.
    //
    // It used to be `findByRole("status")`, on the argument that the page headline also says
    // "timed out" and a bare `findByText` would throw on two matches. Both halves still hold;
    // what changed is that `ConfirmButton` now renders a polite region of its own — empty at
    // rest — so "the only status on the page" is no longer a way to name anything.
    const notice = await screen.findByText(/^The readout timed out before it could measure/);
    expect(notice.getAttribute("role")).toBe("status");
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

  it("offers a restart only beside a down sidecar", async () => {
    const world = systemWorld({
      readout: {
        status: "degraded",
        subsystems: [
          { name: "sqlite_pool", status: "down", reason: "unreachable" },
          { name: "echo_sidecar", status: "ok" },
          { name: "browser_sidecar", status: "down", reason: "not-running" },
          { name: "telegram_sidecar", status: "disabled", reason: "not-configured" },
        ],
      },
    });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystem();

    const list = await screen.findByRole("list", { name: "Subsystems" });
    const restarts = within(list).getAllByRole("button", { name: "Restart" });
    expect(restarts).toHaveLength(1);
    expect(rowFor(list, "browser_sidecar").textContent).toContain("Restart");
    expect(rowFor(list, "sqlite_pool").textContent).not.toContain("Restart");
    expect(rowFor(list, "echo_sidecar").textContent).not.toContain("Restart");
    expect(rowFor(list, "telegram_sidecar").textContent).not.toContain("Restart");
  });

  it("presses twice and asks the núcleo to restart the sidecar the row names", async () => {
    const asked: string[] = [];
    const world = systemWorld({
      readout: {
        status: "down",
        subsystems: [{ name: "browser_sidecar", status: "down", reason: "not-running" }],
      },
    });
    daemon.apiFetch.mockImplementation(
      systemFetch(world, {
        onRestart: (name) => {
          asked.push(name);
          return { name, asked: true };
        },
      }),
    );

    await renderSystem();

    const list = await screen.findByRole("list", { name: "Subsystems" });
    fireEvent.click(within(list).getByRole("button", { name: "Restart" }));
    await afterDwell();
    fireEvent.click(
      await within(list).findByRole("button", { name: "Start it again · browser_sidecar" }),
    );

    await waitFor(() => {
      expect(asked).toEqual(["browser"]);
    });
    const note = await within(list).findByText("asked — the supervisor is trying now");
    expect(note.getAttribute("role")).toBe("status");
  });

  it("says why the núcleo refused a restart, in the row", async () => {
    const world = systemWorld({
      readout: {
        status: "down",
        subsystems: [{ name: "browser_sidecar", status: "down", reason: "not-running" }],
      },
    });
    daemon.apiFetch.mockImplementation(
      systemFetch(world, {
        onRestart: () => {
          throw new ApiRefusal(404, "not_supervised", "not_supervised");
        },
      }),
    );

    await renderSystem();

    const list = await screen.findByRole("list", { name: "Subsystems" });
    fireEvent.click(within(list).getByRole("button", { name: "Restart" }));
    await afterDwell();
    fireEvent.click(
      await within(list).findByRole("button", { name: "Start it again · browser_sidecar" }),
    );

    expect(await within(list).findByText(/nothing is supervising it/)).toBeDefined();
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

describe("System - quota brake", () => {
  it("quota brake fields show the daemon's values and save all three", async () => {
    const world = systemWorld({
      quotaBrake: {
        enabled: false,
        pause_above_percent_5h: 85,
        pause_above_percent_7d: 90,
        provider: "claude",
      },
    });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystem();

    const quotaBrake = await screen.findByRole("heading", { level: 3, name: "Quota brake" });
    const block = quotaBrake.closest("section");
    if (block === null) throw new Error("no quota brake block");
    const enabled = within(block).getByRole("checkbox", { name: "Enable quota brake" }) as HTMLInputElement;
    const fiveHour = within(block).getByLabelText("Pause above 5h usage (%)") as HTMLInputElement;
    const sevenDay = within(block).getByLabelText("Pause above 7d usage (%)") as HTMLInputElement;
    expect(enabled.checked).toBe(false);
    expect(fiveHour.value).toBe("85");
    expect(sevenDay.value).toBe("90");
    expect(within(block).getByText("only claude's windows count")).toBeDefined();

    fireEvent.click(enabled);
    fireEvent.change(fiveHour, { target: { value: "80" } });
    fireEvent.change(sevenDay, { target: { value: "95" } });
    fireEvent.click(within(block).getByRole("button", { name: "Save quota brake" }));
    await afterDwell();
    fireEvent.click(within(block).getByRole("button", { name: "Save these three quota brake settings to the daemon" }));

    const quotaBrakePosts = () =>
      daemon.apiFetch.mock.calls.filter(
        ([path, init]) =>
          path === "/autopilot/quota-brake" && (init as RequestInit | undefined)?.method === "POST",
      );
    await waitFor(() => {
      expect(quotaBrakePosts()).toHaveLength(1);
    });
    expect(JSON.parse((quotaBrakePosts()[0][1] as RequestInit).body as string)).toEqual({
      enabled: true,
      pause_above_percent_5h: 80,
      pause_above_percent_7d: 95,
    });

    fireEvent.change(fiveHour, { target: { value: "0" } });
    fireEvent.click(within(block).getByRole("button", { name: "Save quota brake" }));
    await afterDwell();
    fireEvent.click(within(block).getByRole("button", { name: "Save these three quota brake settings to the daemon" }));

    expect(await within(block).findByRole("alert")).toBeDefined();
    expect(quotaBrakePosts()).toHaveLength(1);
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

    // By its words, not by being the row's only live region: the row's own interlock
    // ("Stage a restore") now carries one too, so that arming it is announced.
    const notice = await within(snap1Row).findByText(/^Nothing has changed yet/);
    expect(notice.getAttribute("role")).toBe("status");
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
  it("still reads the three running readouts, and no longer claims four areas cannot be configured", async () => {
    const world = systemWorld();
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/tokens");

    // The three routes that serve the daemon's PARSED view are still read. They
    // are not what the settings tab shows, and neither replaces the other.
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/config/email");
      expect(daemon.apiFetch).toHaveBeenCalledWith("/voice/config");
      expect(daemon.apiFetch).toHaveBeenCalledWith("/calendar/config");
    });

    // The confession is gone, because it stopped being true.
    expect(screen.queryByRole("list", { name: "Areas with no configuration route" })).toBeNull();
  });
});

/* --------------------------------------------------------- machine settings -- */

describe("System - this machine's settings", () => {
  it("offers every area the núcleo serves, including the four that had no route", async () => {
    const world = systemWorld();
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/settings");

    // The four the removed panel used to name are now editable like the rest.
    for (const area of ["web", "browser", "council", "models"]) {
      expect(await screen.findByRole("heading", { name: area })).toBeDefined();
    }
    expect(daemon.apiFetch).toHaveBeenCalledWith("/config/machine");
  });

  it("names the absolute file each row would write, not the relative one", async () => {
    const world = systemWorld();
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/settings");

    // There are twenty-odd worktrees on this machine and every one has an
    // `.ai/`. The relative path alone would let somebody edit settings with
    // great confidence in the wrong checkout.
    expect(await screen.findByText("C:/Projects/nucleos/.ai/voice.yaml")).toBeDefined();
  });

  it("saves a file and shows what the daemon said about when it starts mattering", async () => {
    const world = systemWorld();
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/settings");

    const editor = await screen.findByLabelText(".ai/calendar.yaml");
    fireEvent.change(editor, { target: { value: 'working_hours_start: "10:00"\n' } });
    fireEvent.click(within(editor.closest("section") as HTMLElement).getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(
        world.machine.settings.find((row) => row.path === ".ai/calendar.yaml")?.contents,
      ).toContain("10:00");
    });
    // Saved is not the same as in effect, and the page says which it means. The
    // marker has to be the SAVED line specifically: every row on the page also
    // carries this sentence as a standing fact, so a bare match for it would
    // pass without anything having been saved at all.
    expect(await screen.findByText(/^saved .* when the daemon restarts/)).toBeDefined();
  });

  it("shows the parser's own words when a file is refused, and does not claim it saved", async () => {
    const world = systemWorld();
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/settings");

    const editor = await screen.findByLabelText(".ai/browser.yaml");
    fireEvent.change(editor, { target: { value: "!!bad\n" } });
    fireEvent.click(within(editor.closest("section") as HTMLElement).getByRole("button", { name: "Save" }));

    // The daemon's sentence, not a generic "invalid" that would send somebody
    // back to a file they cannot see to look for a line nobody named.
    expect(await screen.findByText(/did not find expected node content/)).toBeDefined();
    expect(world.machine.settings.find((row) => row.path === ".ai/browser.yaml")?.exists).toBe(false);
  });
});

describe("System - credentials", () => {
  it("puts a credential beside the file it belongs with, and never renders a value", async () => {
    const world = systemWorld();
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/settings");

    // The github credential renders inside the github panel, not in a list of
    // its own: a pillar and its key are one decision.
    const githubPanel = (await screen.findByRole("heading", { name: "github" })).closest(
      "section",
    ) as HTMLElement;
    expect(within(githubPanel).getByText("github-token")).toBeDefined();
    expect(within(githubPanel).getByText("not set")).toBeDefined();

    // The input is a password field and starts empty, whatever is stored.
    const input = within(githubPanel).getByLabelText("github-token");
    expect(input.getAttribute("type")).toBe("password");
    expect((input as HTMLInputElement).value).toBe("");
  });

  it("stores a credential, clears the box, and reports only that it is set", async () => {
    const world = systemWorld();
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/settings");

    const githubPanel = (await screen.findByRole("heading", { name: "github" })).closest(
      "section",
    ) as HTMLElement;
    const input = within(githubPanel).getByLabelText("github-token");
    fireEvent.change(input, { target: { value: "ghp_a_real_looking_token" } });
    fireEvent.click(within(githubPanel).getByRole("button", { name: "Set" }));

    await waitFor(() => {
      expect(world.secrets.find((row) => row.key === "github-token")?.present).toBe(true);
    });

    // The box is cleared and the token is nowhere on the page. A credential that
    // stayed on screen after being stored is a credential in a screenshot.
    await waitFor(() => {
      expect((input as HTMLInputElement).value).toBe("");
    });
    expect(document.body.textContent).not.toContain("ghp_a_real_looking_token");
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

describe("System - map-authored readings", () => {
  it("a project brake reads held or released, and neither is Acting Green", async () => {
    const world = systemWorld({ projects: [project({ project_id: "alpha" })] });
    daemon.apiFetch.mockImplementation(systemFetch(world));
    await renderSystem();
    const alphaRow = rowFor(await screen.findByRole("list", { name: "Project brakes" }), "alpha");
    const released = within(alphaRow).getByText("released");
    expect(released.className).toContain("ui-badge-off");
    expect(released.className).not.toContain("ui-badge-active");
    fireEvent.click(within(alphaRow).getByRole("button", { name: "Hold alpha" }));
    await waitFor(() => {
      const held = within(alphaRow).getByText("held");
      expect(held.className).toContain("ui-badge-paused");
      expect(held.className).not.toContain("ui-badge-active");
    });
  });

  it("an enabled mailbox and an armed one are facts, not work in flight", async () => {
    daemon.apiFetch.mockImplementation(systemFetch(systemWorld()));
    await renderSystemAt("/system/tokens");
    const panel = (await screen.findByRole("heading", { level: 2, name: "Email configuration" })).closest("section");
    if (panel === null) throw new Error("no email configuration panel");
    for (const label of ["enabled", "armed"]) {
      const badge = within(panel).getByText(label);
      expect(badge.className).toContain("ui-badge-info");
      expect(badge.className).not.toContain("ui-badge-active");
    }
  });
});
