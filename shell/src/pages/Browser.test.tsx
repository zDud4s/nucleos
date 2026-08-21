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
import type { BrowserSession, Site, SidecarState, SubsystemReadout, Written } from "../data/browser";
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

function site(overrides: Partial<Site> = {}): Site {
  return {
    origin: "https://jira.example.org:443",
    kind: "destination",
    granted_at: "2026-08-17T09:00:00Z",
    granted_for: null,
    writable: false,
    ...overrides,
  };
}

function written(overrides: Partial<Written> = {}): Written {
  return {
    id: 1,
    session_id: 5,
    origin: "https://jira.example.org:443",
    action: "https://jira.example.org/browse/X-1/comment",
    method: "POST",
    fields: ["body", "password"],
    field_count: 2,
    element_ref: "e7",
    verb: "click",
    files: [],
    written_at: "2026-08-17T09:30:00Z",
    ...overrides,
  };
}

interface BrowserWorld {
  sessions: BrowserSession[];
  readout: { status: string; subsystems: SubsystemReadout[] };
  sidecars: SidecarState[];
  projects: ReturnType<typeof project>[];
  /** What `GET /browser/sites/{id}` and `GET /browser/writes/{id}` answer. */
  sites: Site[];
  writes: Written[];
  /** What `POST /browser/window` answers. Throw an `ApiRefusal` to refuse. */
  onWindow: (body: string) => unknown;
  /** Every non-GET body this page sent, by path, so a test can assert what left. */
  posted: { path: string; body: string }[];
}

function browserWorld(overrides: Partial<BrowserWorld> = {}): BrowserWorld {
  return {
    sessions: [],
    readout: { status: "ok", subsystems: [{ name: "browser_sidecar", status: "ok" }] },
    sidecars: [],
    projects: [],
    sites: [],
    writes: [],
    onWindow: () => session({ id: 9 }),
    posted: [],
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
      world.posted.push({ path, body: String(init.body ?? "") });
      if (path === "/browser/window" && init.method === "POST") {
        return world.onWindow(String(init.body));
      }
      if (path === "/browser/return") {
        return { chain: ["https://jira.example.org/login", "https://jira.example.org/browse/X-1"] };
      }
      if (path === "/browser/keep") {
        return { granted: ["https://jira.example.org:443"] };
      }
      if (path === "/browser/readonly") return undefined;
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
        if (path.startsWith("/browser/sites/")) return world.sites;
        if (path.startsWith("/browser/writes/")) return world.writes;
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

/** The dwell `ConfirmButton` needs between arming and confirming — a real gap. */
function afterDwell(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 350));
}

/* ------------------------------------------------------------- writing -- */

describe("Browser - the write grant", () => {
  const withProject = (overrides: Partial<BrowserWorld> = {}) =>
    browserWorld({ projects: [project({ project_id: "alpha" })], ...overrides });

  /**
   * The permissive answer is the one a person has to choose.
   *
   * A box that arrived ticked would make writing something granted by not
   * reading the screen, which is the shape of consent that is not consent. The
   * assertion is on the BODY that leaves, not on the checkbox: what the daemon
   * receives is the permission, and a box wired to nothing would look identical
   * on screen.
   */
  it("keeps the chain without granting writing until somebody ticks the box", async () => {
    const world = withProject({ sessions: [session({ id: 3, mode: "human" })] });
    daemon.apiFetch.mockImplementation(browserFetch(world));

    await renderBrowser();
    fireEvent.click(await screen.findByRole("button", { name: "Give the wheel back" }));
    await afterDwell();
    fireEvent.click(await screen.findByRole("button", { name: "Close the window and bring the chain back" }));

    fireEvent.click(await screen.findByRole("button", { name: "Keep them" }));
    await afterDwell();
    fireEvent.click(await screen.findByRole("button", { name: "Grant these origins" }));

    await waitFor(() => {
      expect(world.posted.some((one) => one.path === "/browser/keep")).toBe(true);
    });
    const kept = world.posted.find((one) => one.path === "/browser/keep");
    expect(JSON.parse(kept?.body ?? "{}")).toEqual({
      session_id: 3,
      keep: true,
      writable: false,
    });
  });

  /** And the same act with the box ticked, which is the other half of the pair. */
  it("grants writing when the box is ticked, in the same answer", async () => {
    const world = withProject({ sessions: [session({ id: 3, mode: "human" })] });
    daemon.apiFetch.mockImplementation(browserFetch(world));

    await renderBrowser();
    fireEvent.click(await screen.findByRole("button", { name: "Give the wheel back" }));
    await afterDwell();
    fireEvent.click(await screen.findByRole("button", { name: "Close the window and bring the chain back" }));

    fireEvent.click(await screen.findByLabelText(/submit forms here/i));
    fireEvent.click(await screen.findByRole("button", { name: "Keep them" }));
    await afterDwell();
    fireEvent.click(await screen.findByRole("button", { name: "Grant these origins" }));

    await waitFor(() => {
      expect(world.posted.some((one) => one.path === "/browser/keep")).toBe(true);
    });
    const kept = world.posted.find((one) => one.path === "/browser/keep");
    expect(JSON.parse(kept?.body ?? "{}").writable).toBe(true);
  });

  /**
   * Two permissions, two ways of taking one back.
   *
   * A person may want to end only the larger one — "this has been useful and I
   * would rather it stopped pressing Send" — and a screen where the only door
   * is Revoke turns that into a choice between keeping too much and losing the
   * site. The read-only door is offered ONLY where there is something to
   * narrow, which is the other half of this assertion.
   */
  it("offers to narrow a writable grant, and offers nothing to narrow on a read-only one", async () => {
    const world = withProject({
      sites: [
        site({ origin: "https://jira.example.org:443", writable: true }),
        site({ origin: "https://docs.example.org:443", writable: false }),
      ],
    });
    daemon.apiFetch.mockImplementation(browserFetch(world));

    await renderBrowser();

    // One badge and one door, for the one site that has something to narrow.
    expect(await screen.findByText("submits forms")).toBeDefined();
    expect(screen.getAllByRole("button", { name: "Read-only" })).toHaveLength(1);
    // Both sites can still be revoked outright; narrowing is the extra door.
    expect(screen.getAllByRole("button", { name: "Revoke" })).toHaveLength(2);

    fireEvent.click(screen.getByRole("button", { name: "Read-only" }));
    await afterDwell();
    fireEvent.click(await screen.findByRole("button", { name: "Stop agents submitting forms here" }));

    await waitFor(() => {
      expect(world.posted.some((one) => one.path === "/browser/readonly")).toBe(true);
    });
    const narrowed = world.posted.find((one) => one.path === "/browser/readonly");
    expect(JSON.parse(narrowed?.body ?? "{}")).toEqual({
      project_id: "alpha",
      origin: "https://jira.example.org:443",
    });
  });

  /**
   * The record, on the screen where the grant comes off.
   *
   * This is the whole argument for the feature being supervisable: the agent
   * works alone inside the grant, so the supervision is afterwards, and
   * supervision that lives on another page is supervision nobody performs.
   *
   * The password field is why the fixture has one. Its NAME belongs here — a
   * person reviewing this needs to know a form with a password in it was
   * submitted — and there is nowhere in the shape for its contents, which is
   * the price this design pays and states.
   */
  it("shows what was submitted beside the grant, by field name", async () => {
    const world = withProject({
      sites: [site({ writable: true })],
      writes: [written()],
    });
    daemon.apiFetch.mockImplementation(browserFetch(world));

    await renderBrowser();

    expect(await screen.findByText("https://jira.example.org/browse/X-1/comment")).toBeDefined();
    expect(screen.getByText(/2 fields: body, password/)).toBeDefined();
    expect(screen.getByText(/sent by a click on e7/)).toBeDefined();
  });

  /**
   * A submission that carried a document is not the same event as one that carried a comment, and
   * the record is the only place the owner ever sees either. The names show; what the file said is
   * deliberately not in the database at all (migration 0098), so there is nothing here to leak.
   */
  it("says when a submission carried a file, and says what it was called", async () => {
    const world = withProject({
      sites: [site({ writable: true })],
      writes: [written({ files: ["relatorio.txt"] })],
    });
    daemon.apiFetch.mockImplementation(browserFetch(world));

    await renderBrowser();

    expect(await screen.findByText(/with a file: relatorio.txt/)).toBeDefined();
  });

  /** And a submission that carried none says nothing about files, rather than "0 files". */
  it("stays quiet about files when none went", async () => {
    const world = withProject({
      sites: [site({ writable: true })],
      writes: [written()],
    });
    daemon.apiFetch.mockImplementation(browserFetch(world));

    await renderBrowser();

    expect(await screen.findByText(/2 fields: body, password/)).toBeDefined();
    expect(screen.queryByText(/with a file/)).toBeNull();
    expect(screen.queryByText(/with 0 files/)).toBeNull();
  });

  /** A profile that has written nothing says so, rather than showing an empty box. */
  it("says plainly when nothing has been submitted", async () => {
    daemon.apiFetch.mockImplementation(browserFetch(withProject({ sites: [site()] })));

    await renderBrowser();

    expect(await screen.findByText(/nothing has been submitted from this profile/i)).toBeDefined();
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
