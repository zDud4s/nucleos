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

import { System, takenAt, validateSystemSearch } from "./System";
import type { MachineConfig, MachineSecret } from "../data/machine-config";
import { createAppQueryClient } from "../app/queryClient";
import { ApiRefusal } from "../data/client";
import type {
  ApiTokenLevel,
  ApiTokenSummary,
  BackupInfo,
  StorageReport,
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
import type { RouteReport } from "../data/route";
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
  username: "helena@example.com",
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
  /** What `/system/storage` answers; `undefined` is a daemon that cannot read it. */
  storage?: StorageReport;
  pii: PiiTallyRow[];
  tokens: ApiTokenSummary[];
  emailConfig: EmailConfig;
  voiceConfig: VoiceConfig;
  calendarConfig: CalendarConfig;
  machine: MachineConfig;
  secrets: MachineSecret[];
  /** What `/route/report` answers; `undefined` is an older daemon's 404. */
  routeReport?: RouteReport;
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
    root: "C:/Users/someone/.nucleos",
    root_display: "~/.nucleos",
    settings: MACHINE_AREAS.map((area) => ({
      path: `${area}.yaml`,
      display: `~/.nucleos/${area}.yaml`,
      area,
      what: `what ${area} does`,
      takes_effect: "when the daemon restarts",
      exists: false,
      valid: true,
      problem: null,
      at_startup: { valid: true, problem: null },
      contents: null,
      resolved: `C:/Users/someone/.nucleos/${area}.yaml`,
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
    onCompact?: () => unknown;
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
          kind: "manual",
          migration_version: 60,
          size_bytes: 1024,
        };
        world.backups = [...world.backups, created];
        return created;
      }
      if (path === "/system/storage/compact") {
        if (opts.onCompact !== undefined) return opts.onCompact();
        if (world.storage !== undefined) world.storage = { ...world.storage, compaction_pending: true };
        return { applies: "on next daemon start" };
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
      case "/system/storage":
        if (world.storage === undefined) throw new ApiRefusal(404, "not_found", "no such route");
        return world.storage;
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
      case "/route/report?days=30":
        if (world.routeReport === undefined) throw new ApiRefusal(404, "not_found", "no such route");
        return world.routeReport;
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
    createRoute({ getParentRoute: () => rootRoute, path: "/system/$view",
      component: System,
      validateSearch: validateSystemSearch,
    }),
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

/** The settings tab shows one file at a time: pick it from the list first. */
async function openSettingsFile(area: string): Promise<void> {
  const list = await screen.findByRole("list", { name: "Settings files" });
  fireEvent.click(within(list).getByRole("button", { name: new RegExp(`^${area}\\b`) }));
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

  it("a subsystem with counts shows them beside its state", async () => {
    const world = systemWorld({
      readout: {
        status: "ok",
        subsystems: [
          { name: "sqlite_pool", status: "ok" },
          { name: "distiller", status: "ok", counts: { pending: 3 } },
        ],
      },
    });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystem();

    const list = await screen.findByRole("list", { name: "Subsystems" });
    // The tally sits inside the row it belongs to, and only that row carries one.
    expect(rowFor(list, "distiller").textContent).toContain("pending 3");
    expect(rowFor(list, "sqlite_pool").textContent).not.toContain("pending");
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
      await within(list).findByRole("button", { name: "Start it again" }),
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
      await within(list).findByRole("button", { name: "Start it again" }),
    );

    expect(await within(list).findByText(/nothing is supervising it/)).toBeDefined();
  });
});

/* ---------------------------------------------------------------- router -- */

describe("System - router report", () => {
  const report: RouteReport = {
    days: 30,
    runs: 12,
    shadow: 10,
    apply: 2,
    advised: 10,
    shadow_advised: 8,
    matched: 6,
    pairs: [
      {
        runner: "claude", model: "sonnet", effort: "high",
        advised_runner: "claude", advised_model: "haiku", advised_effort: "low",
        runs: 4, passed: 3, failed: 1,
      },
    ],
  };

  /** The value printed under a label of the totals list. */
  function totalOf(label: string): string | null {
    return screen.getByText(label).parentElement?.querySelector("dd")?.textContent ?? null;
  }

  it("renders the report's totals as label-above-value pairs and its pairs table", async () => {
    daemon.apiFetch.mockImplementation(systemFetch(systemWorld({ routeReport: report })));

    await renderSystem();

    expect(await screen.findByRole("heading", { name: "Router (last 30 days)" })).toBeTruthy();
    expect(totalOf("Routed runs")).toBe("12");
    expect(totalOf("Match rate")).toBe("75%");
    expect(screen.getByText("claude · sonnet · high → claude · haiku · low")).toBeTruthy();
  });

  it("names the router's modes so they are not mistaken for a run's shadow mode, raw words kept", async () => {
    daemon.apiFetch.mockImplementation(systemFetch(systemWorld({ routeReport: report })));

    await renderSystem();

    await screen.findByRole("heading", { name: "Router (last 30 days)" });
    expect(totalOf("Advice only (shadow)")).toBe("10");
    expect(totalOf("Advice applied (apply)")).toBe("2");
    expect(totalOf("With advice")).toBe("10");
  });

  it("renders nothing when no run was routed", async () => {
    daemon.apiFetch.mockImplementation(
      systemFetch(systemWorld({ routeReport: { ...report, runs: 0, pairs: [] } })),
    );

    await renderSystem();

    await screen.findByRole("list", { name: "Subsystems" });
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledWith("/route/report?days=30"));
    expect(screen.queryByText(/Router \(last/)).toBeNull();
  });

  it("an older daemon's 404 shows no panel and no error", async () => {
    daemon.apiFetch.mockImplementation(systemFetch(systemWorld()));

    await renderSystem();

    await screen.findByRole("list", { name: "Subsystems" });
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledWith("/route/report?days=30"));
    expect(screen.queryByText(/Router \(last/)).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
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

    // The liveness word comes from the sidecar domain of the state map.
    expect(within(webCard).getByText("running")).toBeDefined();
    expect(within(browserCard).getByText("down")).toBeDefined();
  });
});

describe("System - unsaved limits", () => {
  it("warns on unload only while a limit edit is unsaved", async () => {
    daemon.apiFetch.mockImplementation(systemFetch(systemWorld()));
    await renderSystemAt("/system/limits");

    const input = await screen.findByLabelText("Window limit (USD)");
    const unload = () => {
      const event = new Event("beforeunload", { cancelable: true });
      window.dispatchEvent(event);
      return event.defaultPrevented;
    };
    expect(unload()).toBe(false);

    fireEvent.change(input, { target: { value: "987" } });
    await waitFor(() => {
      expect(unload()).toBe(true);
    });
  });
});

describe("System - appearance", () => {
  it("keeps the quota notch preference under Appearance on the Machine tab", async () => {
    daemon.apiFetch.mockImplementation(systemFetch(systemWorld()));
    await renderSystemAt("/system/settings");

    expect(await screen.findByRole("region", { name: "Appearance" })).toBeDefined();
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
  it("sends all six budget fields and keeps no ceiling apart from a ceiling of zero", async () => {
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
        relay_chain_limit_usd: 5,
      },
    });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/limits");

    const windowInput = await screen.findByLabelText("Window limit (USD)");
    fireEvent.change(windowInput, { target: { value: "" } });

    fireEvent.click(screen.getByRole("button", { name: "Save budget" }));
    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: "Send these six fields to the daemon" }));

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
      [
        "hourly_limit_usd",
        "limit_usd",
        "per_run_reserve_usd",
        "period",
        "relay_chain_limit_usd",
        "time_cost_per_hour_usd",
      ].sort(),
    );
    expect(firstBody.limit_usd).toBeNull();
    expect(firstBody.period).toBe("daily");
    expect(firstBody.hourly_limit_usd).toBeNull();
    expect(firstBody.per_run_reserve_usd).toBe(0.25);
    expect(firstBody.time_cost_per_hour_usd).toBe(0.1);
    expect(firstBody.relay_chain_limit_usd).toBe(5);

    // Typing an actual zero is a different fact from leaving the box blank.
    fireEvent.change(windowInput, { target: { value: "0" } });
    fireEvent.click(screen.getByRole("button", { name: "Save budget" }));
    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: "Send these six fields to the daemon" }));

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
          relay_chain_limit_usd: 5,
        },
      });
      daemon.apiFetch.mockImplementation(systemFetch(world));

      await renderSystemAt("/system/limits");

      const windowInput = await screen.findByLabelText("Window limit (USD)");
      fireEvent.change(windowInput, { target: { value: garbage } });

      fireEvent.click(screen.getByRole("button", { name: "Save budget" }));
      await afterDwell();
      fireEvent.click(screen.getByRole("button", { name: "Send these six fields to the daemon" }));

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
        relay_chain_limit_usd: 5,
      },
    });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/limits");

    const hourlyInput = await screen.findByLabelText("Hourly limit (USD)");
    fireEvent.change(hourlyInput, { target: { value: "abc" } });

    fireEvent.click(screen.getByRole("button", { name: "Save budget" }));
    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: "Send these six fields to the daemon" }));

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

    await renderSystemAt("/system/limits");

    const quotaBrake = await screen.findByRole("heading", { level: 3, name: "Quota brake" });
    const block = quotaBrake.closest("section");
    if (block === null) throw new Error("no quota brake block");
    const enabled = within(block).getByRole("checkbox", { name: "Enable quota brake" }) as HTMLInputElement;
    const fiveHour = within(block).getByLabelText("Pause above 5h usage (%)") as HTMLInputElement;
    const sevenDay = within(block).getByLabelText("Pause above 7d usage (%)") as HTMLInputElement;
    expect(enabled.checked).toBe(false);
    expect(fiveHour.value).toBe("85");
    expect(sevenDay.value).toBe("90");
    expect(within(block).getByText(/Only claude's windows count\./)).toBeDefined();

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
  it("dates an automatic copy from its name like a manual one", () => {
    expect(takenAt("nucleos-20261019T094420.000000000Z-0000.db")).toBe("2026-10-19T09:44:20Z");
    expect(takenAt("auto-nucleos-20261019T094420.000000000Z-0000.db")).toBe("2026-10-19T09:44:20Z");
    expect(takenAt("snap-1")).toBeNull();
  });

  it("shows the space the database uses", async () => {
    const world = systemWorld({
      storage: {
        db_bytes: 440 * 1024 * 1024,
        wal_bytes: 4 * 1024 * 1024,
        page_size: 4096,
        page_count: 112_640,
        freelist_count: 4_864,
        reclaimable_bytes: 19 * 1024 * 1024,
        backups_bytes: 90 * 1024 * 1024,
        files_bytes: null,
        logs_bytes: 8 * 1024 * 1024,
        loose_bytes: 2 * 1024 * 1024,
        auto_backup_last: "2026-10-19T03:00:00Z",
        compaction_pending: false,
      },
    });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/backups");

    const database = (await screen.findByText("Database")).closest("article");
    expect(database?.textContent).toMatch(/440\.0 MB/);
    expect(database?.textContent).toMatch(/4\.0 MB write-ahead log/);
    // The reclaimable pages are drawn against the whole file, in words as well as width.
    expect(screen.getByRole("img", { name: /Reclaimable/ }).getAttribute("aria-label")).toMatch(/19\.0 MB/);
    expect(screen.getByText("Backups", { selector: ".ui-stat-label" }).closest("article")?.textContent).toMatch(
      /90\.0 MB/,
    );
    // A folder the daemon could not measure in time is not an empty one.
    const files = screen.getByText("Files").closest("article");
    expect(files?.textContent).toMatch(/not measured in time/);
    expect(files?.textContent).not.toMatch(/0 B/);
    expect(screen.getByText(/2\.0 MB of old database copies/)).toBeTruthy();
  });

  const storageReading: StorageReport = {
    db_bytes: 440 * 1024 * 1024,
    wal_bytes: 4 * 1024 * 1024,
    page_size: 4096,
    page_count: 112_640,
    freelist_count: 4_864,
    reclaimable_bytes: 19 * 1024 * 1024,
    backups_bytes: 90 * 1024 * 1024,
    files_bytes: 10 * 1024 * 1024,
    logs_bytes: 8 * 1024 * 1024,
    loose_bytes: 0,
    auto_backup_last: null,
    compaction_pending: false,
  };

  it("schedules a compaction for the next start and then says it is scheduled", async () => {
    const world = systemWorld({ storage: storageReading });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/backups");

    const button = await screen.findByRole("button", { name: "Compact on next start" });
    // The cost is said before the press, not after it: the time, and the safety copy that stays
    // (the compacted size, 440 MB less the 19 MB given back) until the next compaction.
    expect(screen.getByText(/Gives back about 19\.0 MB/)).toBeTruthy();
    expect(screen.getByText(/next start takes longer/)).toBeTruthy();
    expect(
      screen.getByText(/A safety copy of about 421\.0 MB stays in backups\/ until the next compaction/),
    ).toBeTruthy();
    expect(button.getAttribute("aria-disabled")).toBeNull();

    fireEvent.click(button);
    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: "Compact when it next starts" }));

    expect(await screen.findByText(/Compaction is scheduled/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Compact on next start" })).toBeNull();
    const posts = daemon.apiFetch.mock.calls.filter(([path]) => path === "/system/storage/compact");
    expect(posts).toHaveLength(1);
    expect((posts[0][1] as RequestInit).method).toBe("POST");
  });

  it("disables the compact button, with the reason, when nothing is reclaimable", async () => {
    const world = systemWorld({ storage: { ...storageReading, reclaimable_bytes: 0 } });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/backups");

    const button = await screen.findByRole("button", { name: "Compact on next start" });
    expect(button.getAttribute("aria-disabled")).toBe("true");
    const reason = screen.getByText("Nothing to give back: the database has no free pages.");
    expect(button.getAttribute("aria-describedby")).toBe(reason.id);
    expect(screen.queryByText(/Gives back about/)).toBeNull();

    fireEvent.click(button);
    await afterDwell();
    expect(screen.queryByRole("button", { name: "Compact when it next starts" })).toBeNull();
    expect(daemon.apiFetch.mock.calls.some(([path]) => path === "/system/storage/compact")).toBe(
      false,
    );
  });

  it("shows a compaction already scheduled without offering the button again", async () => {
    const world = systemWorld({ storage: { ...storageReading, compaction_pending: true } });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/backups");

    expect(await screen.findByText(/Compaction is scheduled/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Compact on next start" })).toBeNull();
  });

  it("says a restore is pending when the daemon refuses to compact with 409", async () => {
    const world = systemWorld({ storage: storageReading });
    daemon.apiFetch.mockImplementation(
      systemFetch(world, {
        onCompact: () => {
          throw new ApiRefusal(409, "conflict", "conflict");
        },
      }),
    );

    await renderSystemAt("/system/backups");

    fireEvent.click(await screen.findByRole("button", { name: "Compact on next start" }));
    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: "Compact when it next starts" }));

    expect(
      await screen.findByText("a restore is pending — compact after it has been applied"),
    ).toBeDefined();
    expect(screen.getByRole("button", { name: "Compact on next start" })).toBeDefined();
  });

  it("marks an automatic backup in the list", async () => {
    const world = systemWorld({
      backups: [
        { name: "auto-nucleos-20261019T030000.000000000Z-0000.db", kind: "auto", migration_version: 64, size_bytes: 2048 },
        { name: "nucleos-20261018T030000.000000000Z-0000.db", kind: "manual", migration_version: 64, size_bytes: 2048 },
      ],
    });
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/backups");

    const list = await screen.findByRole("list", { name: "Backups" });
    expect(rowFor(list, "auto-nucleos-20261019T030000.000000000Z-0000.db").textContent).toMatch(/automatic/);
    expect(rowFor(list, "nucleos-20261018T030000.000000000Z-0000.db").textContent).not.toMatch(/automatic/);
  });

  it("lists backups and says a restore is staged for the next start", async () => {
    const world = systemWorld({
      backups: [
        { name: "snap-1", kind: "manual", migration_version: 60, size_bytes: 2048 },
        { name: "snap-0", kind: "manual", migration_version: null, size_bytes: 512 },
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
    fireEvent.click(within(snap1Row).getByRole("button", { name: "Restore on next start" }));

    // By its words, not by being the row's only live region: the row's own interlock
    // ("Stage a restore") now carries one too, so that arming it is announced.
    const notice = await within(snap1Row).findByText(/^Nothing has changed yet/);
    expect(notice.getAttribute("role")).toBe("status");
    expect(notice.textContent).toMatch(/applies on the núcleo's next start/i);
    expect(notice.textContent).toMatch(/nothing has changed yet/i);
  });

  it("says a restore is already staged when the daemon answers 409", async () => {
    const world = systemWorld({
      backups: [{ name: "snap-1", kind: "manual", migration_version: 60, size_bytes: 2048 }],
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
    fireEvent.click(within(row).getByRole("button", { name: "Restore on next start" }));

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

    // The four the removed panel used to name are now editable like the rest — each in
    // the list, and each opening its own file.
    const list = await screen.findByRole("list", { name: "Settings files" });
    for (const area of ["web", "browser", "council", "models"]) {
      fireEvent.click(within(list).getByRole("button", { name: new RegExp(`^${area}\\b`) }));
      expect(await screen.findByRole("heading", { name: area })).toBeDefined();
    }
    expect(daemon.apiFetch).toHaveBeenCalledWith("/config/machine");
  });

  it("names the absolute file each row would write, beside the name a person is shown", async () => {
    const world = systemWorld();
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/settings");

    // "Which file is this" gets the real answer, and the editor is labelled with
    // the `~/.nucleos/` spelling rather than a bare file name.
    expect(await screen.findByText("C:/Users/someone/.nucleos/voice.yaml")).toBeDefined();
    expect(await screen.findByLabelText("~/.nucleos/voice.yaml")).toBeDefined();
  });

  it("saves a file and shows what the daemon said about when it starts mattering", async () => {
    const world = systemWorld();
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/settings");

    await openSettingsFile("calendar");
    const editor = await screen.findByLabelText("~/.nucleos/calendar.yaml");
    fireEvent.change(editor, { target: { value: 'working_hours_start: "10:00"\n' } });
    fireEvent.click(within(editor.closest("section") as HTMLElement).getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(
        world.machine.settings.find((row) => row.path === "calendar.yaml")?.contents,
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

    await openSettingsFile("browser");
    const editor = await screen.findByLabelText("~/.nucleos/browser.yaml");
    fireEvent.change(editor, { target: { value: "!!bad\n" } });
    fireEvent.click(within(editor.closest("section") as HTMLElement).getByRole("button", { name: "Save" }));

    // The daemon's sentence, not a generic "invalid" that would send somebody
    // back to a file they cannot see to look for a line nobody named.
    expect(await screen.findByText(/did not find expected node content/)).toBeDefined();
    expect(world.machine.settings.find((row) => row.path === "browser.yaml")?.exists).toBe(false);
  });
});

describe("System - a settings file that does not read", () => {
  it("a file that does not parse says so and shows why", async () => {
    const world = systemWorld();
    world.machine.settings = world.machine.settings.map((row) =>
      row.path === "email.yaml"
        ? {
            ...row,
            exists: true,
            contents: "enabled: [oops\n",
            valid: false,
            problem: "did not find expected ',' or ']' at line 2",
          }
        : row,
    );
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/settings");

    await openSettingsFile("email");
    const panel = (await screen.findByRole("heading", { name: "email" })).closest("section") as HTMLElement;
    expect(within(panel).getByText("does not parse")).toBeDefined();
    expect(within(panel).getByText(/did not find expected ',' or '\]' at line 2/)).toBeDefined();
    // A healthy file beside it carries neither the badge nor the note.
    await openSettingsFile("web");
    const web = (await screen.findByRole("heading", { name: "web" })).closest("section") as HTMLElement;
    expect(within(web).queryByText("does not parse")).toBeNull();
  });

  it("a file the daemon started without says a restart is needed", async () => {
    const world = systemWorld();
    // Broken when the daemon read it, fixed on disk since: valid now, defaults in force.
    world.machine.settings = world.machine.settings.map((row) =>
      row.path === "voice.yaml"
        ? {
            ...row,
            exists: true,
            contents: "enabled: false\n",
            at_startup: { valid: false, problem: "bad chord" },
          }
        : row,
    );
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/settings");

    await openSettingsFile("voice");
    const panel = (await screen.findByRole("heading", { name: "voice" })).closest("section") as HTMLElement;
    expect(
      within(panel).getByText("the daemon started on defaults — restart after fixing"),
    ).toBeDefined();
    expect(within(panel).queryByText("does not parse")).toBeNull();
  });
});

describe("System - credentials", () => {
  it("puts a credential beside the file it belongs with, and never renders a value", async () => {
    const world = systemWorld();
    daemon.apiFetch.mockImplementation(systemFetch(world));

    await renderSystemAt("/system/settings");

    // The github credential renders inside the github panel, not in a list of
    // its own: a pillar and its key are one decision.
    await openSettingsFile("github");
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

    await openSettingsFile("github");
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

describe("System - the channel search param", () => {
  const EMPTY = { families: [], kinds: [] };
  const channel = (id: string, label: string) => ({ id, label, configured: true });

  /** `systemFetch` plus the channel list and the per-channel policy the notifications tab reads. */
  function serveChannels() {
    const base = systemFetch(systemWorld());
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/channels") {
        return {
          contract_version: "1",
          primary: "telegram",
          routing: { state: "ok" },
          channels: [channel("telegram", "Telegram"), channel("discord", "Discord")],
        };
      }
      if (path === "/notifications/kinds") return [];
      if (path === "/notifications/policy") return EMPTY;
      if (path.startsWith("/notifications/policy?channel=")) {
        const id = path.slice(path.indexOf("=") + 1);
        return { channel: id, primary: id === "telegram", default: EMPTY, overrides: EMPTY, resolved: EMPTY };
      }
      return await base(path, init);
    });
  }

  const requested = () => daemon.apiFetch.mock.calls.map((call) => call[0] as string);

  it("reads the channel's policy when ?channel= names one", async () => {
    serveChannels();
    await renderSystemAt("/system/notifications?channel=telegram");
    await waitFor(() => {
      expect(requested()).toContain("/notifications/policy?channel=telegram");
    });
  });

  it("choosing another channel puts it in the URL, and Default takes it out", async () => {
    serveChannels();
    const { router } = await renderSystemAt("/system/notifications?channel=telegram");
    const select = await screen.findByLabelText("Channel");
    await waitFor(() => {
      expect(within(select).getAllByRole("option").length).toBe(3);
    });

    fireEvent.change(select, { target: { value: "discord" } });
    await waitFor(() => {
      expect(router.state.location.search).toEqual({ channel: "discord" });
    });
    await waitFor(() => {
      expect(requested()).toContain("/notifications/policy?channel=discord");
    });

    fireEvent.change(await screen.findByLabelText("Channel"), { target: { value: "" } });
    await waitFor(() => {
      expect(router.state.location.search).toEqual({});
    });
  });

  it("an empty or malformed ?channel= is the Default mode", async () => {
    serveChannels();
    await renderSystemAt("/system/notifications?channel=");
    await screen.findByLabelText("Channel");
    expect(requested().some((path) => path.startsWith("/notifications/policy?channel="))).toBe(false);
    expect(validateSystemSearch({ channel: "" })).toEqual({});
    expect(validateSystemSearch({ channel: "Bad Id" })).toEqual({});
    expect(validateSystemSearch({ channel: 7 })).toEqual({});
    expect(validateSystemSearch({ channel: "discord" })).toEqual({ channel: "discord" });
  });

  it("ignores the param on another tab", async () => {
    serveChannels();
    await renderSystemAt("/system/limits?channel=x");
    await screen.findByLabelText("Window limit (USD)");
    expect(requested().some((path) => path.startsWith("/notifications/policy"))).toBe(false);
  });
});
