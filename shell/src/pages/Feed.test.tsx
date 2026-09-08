import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Feed } from "./Feed";
import { feedIsSearching, feedQueryString, type FeedEntry } from "../data/feed";
import { daemonFetch, daemonState, renderApp, renderWithRouter } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();

  // Up and authorising, for the one case below that mounts the whole app and so
  // has to get past the connection gate.
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  localStorage.clear();
});

/**
 * The page inside a real router, and nothing else.
 *
 * `renderApp` would mount the gate, the rail and its live queries around every
 * one of these assertions — a cost per test that buys nothing here. The single
 * case that genuinely needs the whole app is the one that proves `/feed` is in
 * the real tree, and it says so where it does it.
 */
function renderFeed(path = "/feed") {
  return renderWithRouter(<Feed />, { initialPath: path });
}

/* ------------------------------------------------------------- fixtures -- */

function entry(overrides: Partial<FeedEntry> = {}): FeedEntry {
  return {
    id: 1,
    project_id: "alpha",
    kind: "job_started",
    summary: "job 1 started",
    run_id: null,
    errand_id: null,
    created_at: "2026-08-17T09:00:00Z",
    ...overrides,
  };
}

/** The feed route over the foundation's responder; everything else falls through. */
function feedFetch(rows: FeedEntry[]): (path: string, init?: RequestInit) => Promise<unknown> {
  const shared = daemonFetch(daemonState());
  return async (path, init) => {
    if (path.startsWith("/feed")) return rows;
    return await shared(path, init);
  };
}

/** Every `/feed` path the page has asked for, in order. */
function feedCalls(): string[] {
  return daemon.apiFetch.mock.calls
    .map(([path]) => String(path))
    .filter((path) => path.startsWith("/feed"));
}

/* ------------------------------- A15: one kind, four different situations -- */

describe("Feed - a waiting line says what it is waiting for", () => {
  it("reads budget, slot and exclusion apart, from the same kind", async () => {
    /*
      All three are `job_waiting`. The daemon puts the reason nowhere but the
      summary — `park` writes `job {id} is waiting: {detail}` — so this is the
      test that the page reads the detail rather than badging the kind once and
      calling three different situations the same thing.

      The exclusion row is the trap, and it is here on purpose: its detail is
      "job 9 holds a slot and the two are excluded", so anything matching on the
      word "slot" reads it as slot contention and sends somebody looking for
      capacity that is already there.
    */
    const rows = [
      entry({
        id: 2,
        kind: "job_waiting",
        summary:
          "job 2 is waiting: hourly spend $4.90 + $0.25 reserve would exceed the $5.00 hourly limit",
      }),
      entry({
        id: 3,
        kind: "job_waiting",
        summary: "job 3 is waiting: another run holds the project's worktree slot",
      }),
      entry({
        id: 4,
        kind: "job_waiting",
        summary: "job 4 is waiting: job 9 holds a slot and the two are excluded",
      }),
    ];
    daemon.apiFetch.mockImplementation(feedFetch(rows));

    await renderFeed();

    const list = await screen.findByRole("list", { name: "Feed" });
    const items = within(list).getAllByRole("listitem");
    expect(items.length).toBe(3);

    // Each row carries its own reading, and only its own.
    expect(within(items[0]).getByText("held by budget")).toBeDefined();
    expect(within(items[0]).queryByText("waiting for a slot")).toBeNull();

    expect(within(items[1]).getByText("waiting for a slot")).toBeDefined();
    expect(within(items[1]).queryByText("held by budget")).toBeNull();

    expect(within(items[2]).getByText("held by an exclusion")).toBeDefined();
    expect(within(items[2]).queryByText("waiting for a slot")).toBeNull();
  });

  it("says nothing extra about a waiting line it cannot read", async () => {
    // The control on the test above: the reading is derived, not decorative, so
    // a reason this shell has no map for adds no badge rather than a plausible
    // one. `attention` is a real `Brake::Park` reason with no `wait_reason` entry.
    const rows = [
      entry({
        id: 5,
        kind: "job_waiting",
        summary: "job 5 is waiting: somebody is at the keyboard on alpha",
      }),
    ];
    daemon.apiFetch.mockImplementation(feedFetch(rows));

    await renderFeed();

    const list = await screen.findByRole("list", { name: "Feed" });
    expect(within(list).getByText("job waiting")).toBeDefined();
    expect(within(list).queryByText("held by budget")).toBeNull();
    expect(within(list).queryByText("waiting for a slot")).toBeNull();
    expect(within(list).queryByText("held by an exclusion")).toBeNull();
  });
});

/* --------------------------------------------- A15: the listing/search switch -- */

describe("Feed - a search freezes the list and Back to live resumes it", () => {
  it("is the daemon's own rule about which five fields turn a listing into a search", () => {
    // Pure, and asserted directly because it is the rule the freeze is: the
    // five are `has_search_filters` in `core/src/http.rs`, field for field.
    expect(feedIsSearching({})).toBe(false);
    expect(feedIsSearching({ project: "alpha" })).toBe(false);
    expect(feedIsSearching({ errand: "7" })).toBe(false);
    for (const filters of [
      { q: "gate" },
      { kind: "job_failed" },
      { since: "2026-08-17T00:00:00.000Z" },
      { until: "2026-08-17T00:00:00.000Z" },
      { limit: "10" },
    ]) {
      expect(feedIsSearching(filters)).toBe(true);
    }

    // And a blank is not a filter: `?kind=` would ask for rows whose kind is the
    // empty string AND flip the route into searching on the way.
    expect(feedIsSearching({ kind: "   " })).toBe(false);
    expect(feedQueryString({ kind: "   " })).toBe("?scope=all");
  });

  it("freezes on a search, then goes back to live without losing the project", async () => {
    daemon.apiFetch.mockImplementation(feedFetch([entry({ id: 6, kind: "job_gate_failed" })]));

    await renderFeed("/feed?project=alpha&q=gate");

    // The page says it has stopped refreshing. A feed that silently stopped
    // polling is indistinguishable from a machine that stopped doing anything.
    expect(await screen.findByText(/the page has stopped refreshing/)).toBeDefined();

    // The search really went to the daemon, narrowed to the project — and with
    // no `scope=all`, which would have short-circuited the project away.
    await waitFor(() => {
      expect(feedCalls().some((path) => path.includes("q=gate"))).toBe(true);
    });
    const searched = feedCalls().find((path) => path.includes("q=gate")) ?? "";
    expect(searched).toContain("project_id=alpha");
    expect(searched).not.toContain("scope=all");

    fireEvent.click(screen.getByRole("button", { name: "Back to live" }));

    // Back to a listing: the notice is gone and the project survived, because
    // narrowing to an owner never froze anything in the first place.
    await waitFor(() => {
      expect(screen.queryByText(/the page has stopped refreshing/)).toBeNull();
    });
    const live = await waitFor(() => {
      const found = feedCalls().find((path) => !path.includes("q=gate"));
      expect(found).toBeDefined();
      return found ?? "";
    });
    expect(live).toContain("project_id=alpha");
    expect(live).not.toContain("q=");
  });
});

/* ------------------------------------------------------ A15: unmapped kinds -- */

describe("Feed - a kind this shell has no reading for", () => {
  it("renders the literal rather than a guessed label", async () => {
    // `email_<class>` is built from configuration (`notify_classes`), so the
    // shell cannot have a table entry for everybody's classes. Showing the
    // literal admits ignorance; a plausible label would be a claim.
    daemon.apiFetch.mockImplementation(
      feedFetch([entry({ id: 7, kind: "email_shopping", summary: "3 messages about deliveries" })]),
    );

    await renderFeed();

    const list = await screen.findByRole("list", { name: "Feed" });
    expect(within(list).getByText("email_shopping")).toBeDefined();
  });
});

/* --------------------------------------------------------------- the route -- */

describe("Feed - the route", () => {
  it("is registered, so the rail reaches the page and not the placeholder", async () => {
    daemon.apiFetch.mockImplementation(feedFetch([]));

    // The whole app here, and only here: a stubbed destination would prove
    // nothing about whether `/feed` is in the real tree.
    const { router } = await renderApp({ initialPath: "/feed" });

    expect(await screen.findByRole("heading", { level: 1, name: "Feed" })).toBeDefined();
    expect(router.state.location.pathname).toBe("/feed");
    expect(screen.queryByText("Feed is not built yet")).toBeNull();
  });
});

describe("Feed - date bounds", () => {
  it("the date bounds declare the shell's locale", async () => {
    daemon.apiFetch.mockImplementation(feedFetch([]));
    renderFeed();

    expect((await screen.findByLabelText("Only lines after")).getAttribute("lang")).toBe("en-GB");
    expect(screen.getByLabelText("Only lines before").getAttribute("lang")).toBe("en-GB");
  });
});
