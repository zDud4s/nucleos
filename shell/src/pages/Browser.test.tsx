import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Browser } from "./Browser";
import { ApiRefusal } from "../data/client";
import type { BrowserSession, SidecarState, SubsystemReadout } from "../data/browser";
import { daemonFetch, daemonState, project, renderWithRouter } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
});

/* ------------------------------------------------------------- fixtures -- */

function session(overrides: Partial<BrowserSession> = {}): BrowserSession {
  return {
    id: 5,
    sidecar_id: "sc-1",
    run_id: null,
    project_id: "alpha",
    profile_kind: "project",
    profile_id: "alpha",
    requested_url: "https://example.com/login",
    final_url: "https://example.com/login",
    rule: "ask",
    mode: "human",
    refusal: null,
    proposal_id: null,
    chain: null,
    chain_decided_at: null,
    opened_at: "2026-08-17T09:00:00Z",
    closed_at: null,
    ...overrides,
  };
}

interface BrowserWorld {
  sessions: BrowserSession[];
  readout: { status: string; subsystems: SubsystemReadout[] };
  sidecars: SidecarState[];
  projects: ReturnType<typeof project>[];
  /** What `POST /browser/window` answers. Throw an `ApiRefusal` to refuse. */
  onWindow: (body: string) => unknown;
}

function browserWorld(overrides: Partial<BrowserWorld> = {}): BrowserWorld {
  return {
    sessions: [],
    readout: { status: "ok", subsystems: [{ name: "browser_sidecar", status: "ok" }] },
    sidecars: [],
    projects: [],
    onWindow: () => session({ id: 9 }),
    ...overrides,
  };
}

/**
 * The pillar's own routes, over the foundation's responder — `Waiting.test.tsx`'s
 * pattern. Anything this does not know falls through to the shared fixture, so
 * `/projects` and `/autopilot/*` still answer without this file teaching them.
 */
function browserFetch(world: BrowserWorld): (path: string, init?: RequestInit) => Promise<unknown> {
  const shared = daemonFetch(daemonState({ projects: world.projects }));
  return async (path, init) => {
    if (init?.method !== undefined && init.method !== "GET") {
      if (path === "/browser/window" && init.method === "POST") {
        return world.onWindow(String(init.body));
      }
      return await shared(path, init);
    }
    switch (path) {
      case "/browser/sessions":
        return world.sessions;
      case "/health/readout":
        return world.readout;
      case "/sidecars":
        return world.sidecars;
      default:
        // Site grants render as soon as a project exists; answer them so that panel
        // does not error into this file's output.
        if (path.startsWith("/browser/sites/")) return [];
        return await shared(path, init);
    }
  };
}

function renderBrowser() {
  return renderWithRouter(<Browser />, { initialPath: "/browser" });
}

/* ---------------------------------------------------------- live sessions -- */

describe("Browser - live sessions", () => {
  it("lists every session mode and offers no wheel decision on this page", async () => {
    const world = browserWorld({
      sessions: [
        session({ id: 1, mode: "agent" }),
        session({ id: 2, mode: "wheel-requested", proposal_id: 91 }),
        session({ id: 3, mode: "human" }),
        session({ id: 4, mode: "delivery-failed" }),
      ],
    });
    daemon.apiFetch.mockImplementation(browserFetch(world));

    await renderBrowser();

    // Every mode's own copy, once each.
    expect(await screen.findByText("agent is driving")).toBeDefined();
    expect(screen.getByText("asking for the wheel")).toBeDefined();
    expect(screen.getByText("you are driving")).toBeDefined();
    expect(screen.getByText("the window would not open")).toBeDefined();

    // The wheel-requested row points at Waiting rather than offering a
    // decision of its own — the buttons that decide it appear exactly once
    // in the app, and this is not the page that has them.
    expect(screen.getByRole("link", { name: "answer it there" })).toBeDefined();
    expect(screen.queryByRole("button", { name: /Give wheel #\d+ the window/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /Refuse wheel/ })).toBeNull();

    // This page's own actions exist, worded differently so neither is ever
    // mistaken for the wheel decision above.
    expect(screen.getByRole("button", { name: "Give the wheel back" })).toBeDefined();
    expect(screen.getAllByRole("button", { name: "Close session" })).toHaveLength(2);

    // And nothing on this page decided the wheel behind the scenes either.
    const posted = daemon.apiFetch.mock.calls
      .filter(([, init]) => (init as RequestInit | undefined)?.method === "POST")
      .map(([path]) => String(path));
    expect(posted).toEqual([]);
  });
});

/* ---------------------------------------------------------------- health -- */

describe("Browser - health", () => {
  it("reports the one browser subsystem the daemon has", async () => {
    const world = browserWorld({
      readout: {
        status: "degraded",
        subsystems: [
          { name: "web_sidecar", status: "down", reason: "not-configured" },
          { name: "browser_sidecar", status: "down", reason: "not-running" },
          { name: "email_sidecar", status: "ok" },
        ],
      },
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
    daemon.apiFetch.mockImplementation(browserFetch(world));

    await renderBrowser();

    const heading = await screen.findByRole("heading", { level: 2, name: "Browser health" });
    const panel = heading.closest("section");
    expect(panel).not.toBeNull();

    // The daemon answered with THREE subsystems and this call site asked for
    // ONE — the browser's own reason made it onto the page...
    await waitFor(() => expect(panel?.textContent).toMatch(/not-running/));
    // ...and the other subsystem's reason did not, proving it was filtered
    // rather than merely drawn first.
    expect(panel?.textContent).not.toMatch(/not-configured/);

    // The sidecar's own prose — fetched only because the subsystem was not
    // `ok` — is the browser's row from `GET /sidecars`, matched by the
    // literal `"browser"`, not the health probe's own label `"browser_sidecar"`.
    expect(panel?.textContent).toMatch(/exited: exit code: 1/);
    expect(panel?.textContent).toMatch(/restarts\D*3/);

    // `GET /sidecars` answered with a second row, "web", and only one
    // "sidecar state" fact is on the page — the other row left no trace.
    expect(panel?.textContent?.match(/sidecar state/g)).toHaveLength(1);
  });
});

/* --------------------------------------------------- opening a window -- */

describe("Browser - opening a window yourself", () => {
  const withProject = (overrides: Partial<BrowserWorld> = {}) =>
    browserWorld({ projects: [project({ project_id: "alpha" })], ...overrides });

  it("opens a window on the chosen project's profile from a typed address and names the session it opened", async () => {
    const bodies: string[] = [];
    daemon.apiFetch.mockImplementation(
      browserFetch(
        withProject({
          onWindow: (body) => {
            bodies.push(body);
            return session({ id: 9, profile_kind: "project", profile_id: "alpha" });
          },
        }),
      ),
    );

    await renderBrowser();

    fireEvent.change(await screen.findByLabelText("Address to open"), {
      target: { value: "https://jira.example.com" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Open a window" }));

    expect(await screen.findByText(/opened session #9/i)).toBeTruthy();
    expect(JSON.parse(bodies[0])).toEqual({
      project_id: "alpha",
      url: "https://jira.example.com",
    });
  });

  it("tells a pillar that is off apart from nobody being present, by rendering the daemon's own sentence", async () => {
    // Both causes are 409 with only prose to separate them, so the page must not
    // substitute a sentence of its own for either.
    const refuse = (detail: string) => () => {
      throw new ApiRefusal(409, "conflict", detail);
    };

    daemon.apiFetch.mockImplementation(
      browserFetch(withProject({ onWindow: refuse("the browser pillar is off") })),
    );
    const off = await renderBrowser();
    fireEvent.change(await screen.findByLabelText("Address to open"), {
      target: { value: "https://jira.example.com" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Open a window" }));
    expect(await screen.findByText(/the browser pillar is off/i)).toBeTruthy();
    off.unmount();

    daemon.apiFetch.mockImplementation(
      browserFetch(
        withProject({
          onWindow: refuse(
            "a window is opened for somebody to sit at, and nobody is at this machine",
          ),
        }),
      ),
    );
    await renderBrowser();
    fireEvent.change(await screen.findByLabelText("Address to open"), {
      target: { value: "https://jira.example.com" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Open a window" }));
    expect(await screen.findByText(/nobody is at this machine/i)).toBeTruthy();
  });

  it("will not submit an empty address and asks for no confirmation once one is typed", async () => {
    let calls = 0;
    daemon.apiFetch.mockImplementation(
      browserFetch(
        withProject({
          onWindow: () => {
            calls += 1;
            return session({ id: 9 });
          },
        }),
      ),
    );

    await renderBrowser();

    const open = await screen.findByRole("button", { name: "Open a window" });
    expect(open.hasAttribute("disabled")).toBe(true);
    fireEvent.click(open);
    expect(calls).toBe(0);

    fireEvent.change(screen.getByLabelText("Address to open"), {
      target: { value: "https://jira.example.com" },
    });
    // One click and it opens — the label never changes, because nothing here is
    // destructive and there is no arming step to pass through.
    fireEvent.click(screen.getByRole("button", { name: "Open a window" }));
    expect(await screen.findByText(/opened session #9/i)).toBeTruthy();
    expect(calls).toBe(1);
  });
});
