import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { NotificationsDrawer } from "./NotificationsDrawer";
import type { FeedEntry, PendingNotification } from "../data/feed";
import { daemonFetch, daemonState, renderApp, renderWithQuery } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();

  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  localStorage.clear();
});

/* ------------------------------------------------------------- fixtures -- */

function notification(overrides: Partial<PendingNotification> = {}): PendingNotification {
  return {
    id: 1,
    kind: "email_urgent",
    summary: "the roof is on fire",
    queued_at: "2026-08-17T09:00:00Z",
    delivered_at: null,
    ...overrides,
  };
}

function entry(overrides: Partial<FeedEntry> = {}): FeedEntry {
  return {
    id: 1,
    project_id: "alpha",
    kind: "job_started",
    summary: "job 1 started",
    run_id: null,
    errand_id: null,
    subject: null,
    created_at: "2026-08-17T09:00:00Z",
    ...overrides,
  };
}

interface DrawerWorld {
  pending: PendingNotification[];
  feed: FeedEntry[];
}

function drawerFetch(world: DrawerWorld): (path: string, init?: RequestInit) => Promise<unknown> {
  const shared = daemonFetch(daemonState());
  return async (path, init) => {
    if (path === "/notifications/pending") return world.pending;
    if (path.startsWith("/feed")) return world.feed;
    return await shared(path, init);
  };
}

function feedCalls(): string[] {
  return daemon.apiFetch.mock.calls
    .map(([path]) => String(path))
    .filter((path) => path.startsWith("/feed"));
}

/**
 * The drawer alone, with a cache and no router.
 *
 * It needs neither a route nor a location — it reads two queries and renders a
 * panel — so this is the cheapest mount that exercises it honestly. The one
 * assertion that genuinely needs the shell is where the trigger *lives*, and
 * that test pays for `renderApp` where it makes the claim.
 */
function renderDrawer(world: DrawerWorld) {
  daemon.apiFetch.mockImplementation(drawerFetch(world));
  return renderWithQuery(<NotificationsDrawer />);
}

/* ------------------------------------ A16: held and delivered stay apart -- */

describe("NotificationsDrawer - held is not delivered", () => {
  it("puts what the calendar is holding in a different list from what it let through", async () => {
    /*
      `delivered_at` is the whole distinction, and the núcleo keeps delivered
      rows on purpose: the fear this feature earns is "did the calendar swallow
      something?", and a queue that listed only what has not arrived yet cannot
      answer it. One list of both would throw that away on screen just as
      thoroughly as dropping the rows would.
    */
    renderDrawer({
      pending: [
        notification({ id: 1, summary: "the roof is on fire" }),
        notification({
          id: 2,
          kind: "token_efficiency",
          summary: "token efficiency (global|real): 4 runs in a row sent a prompt of 50000 tokens",
          queued_at: "2026-08-17T08:00:00Z",
          delivered_at: "2026-08-17T08:40:00Z",
        }),
      ],
      feed: [],
    });

    fireEvent.click(
      await screen.findByRole("button", { name: "Notifications, 1 held by the calendar" }),
    );

    const heldList = await screen.findByRole("list", { name: "Held notifications" });
    const deliveredList = screen.getByRole("list", { name: "Delivered notifications" });

    expect(within(heldList).getByText("the roof is on fire")).toBeDefined();
    expect(within(heldList).queryByText(/token efficiency/)).toBeNull();

    expect(within(deliveredList).getByText(/token efficiency/)).toBeDefined();
    expect(within(deliveredList).queryByText("the roof is on fire")).toBeNull();

    // A held row cannot say when it arrived, because it has not. The delivered
    // one carries both instants: queued alone cannot say whether it ever came,
    // delivered alone cannot say how long it waited.
    expect(within(heldList).queryByText(/delivered/)).toBeNull();
    expect(within(deliveredList).getByText(/delivered/)).toBeDefined();
    expect(within(deliveredList).getByText(/queued/)).toBeDefined();
  });

  it("counts only what is still held, and says the count out loud", async () => {
    renderDrawer({
      pending: [
        notification({ id: 1 }),
        notification({ id: 2 }),
        notification({ id: 3, delivered_at: "2026-08-17T08:40:00Z" }),
      ],
      feed: [],
    });

    // Two held, one delivered — and the number is in the accessible name, not
    // only in the pill: adjacent inline text concatenates with no separator, so
    // the name would otherwise be announced as "Notifications2".
    expect(await screen.findByRole("button", { name: "Notifications, 2 held by the calendar" }))
      .toBeDefined();
  });
});

/* -------------------------------------- A16: it opens, and only then reads -- */

describe("NotificationsDrawer - opening it", () => {
  it("shows the last feed lines, and does not ask for them until it is opened", async () => {
    renderDrawer({ pending: [], feed: [entry({ id: 9, summary: "job 9 started" })] });

    // Mounted on every page in the app: a hook that fetched while shut would
    // poll the feed for the whole session on behalf of a panel nobody opened.
    await screen.findByRole("button", { name: "Notifications, nothing held" });
    expect(feedCalls()).toEqual([]);

    fireEvent.click(screen.getByRole("button", { name: "Notifications, nothing held" }));

    const feedList = await screen.findByRole("list", { name: "Latest feed" });
    expect(within(feedList).getByText("job 9 started")).toBeDefined();

    // Twenty lines from every scope — `scope=all`, because `Global` in the
    // núcleo means the machine's own rows and not everything.
    await waitFor(() => {
      expect(feedCalls().length).toBeGreaterThan(0);
    });
    expect(feedCalls()[0]).toContain("scope=all");
    expect(feedCalls()[0]).toContain("limit=20");
  });

  it("closes again from its own control", async () => {
    renderDrawer({ pending: [], feed: [] });

    fireEvent.click(await screen.findByRole("button", { name: "Notifications, nothing held" }));
    expect(await screen.findByRole("complementary", { name: "Notifications" })).toBeDefined();

    fireEvent.click(screen.getByRole("button", { name: "Close notifications" }));
    await waitFor(() => {
      expect(screen.queryByRole("complementary", { name: "Notifications" })).toBeNull();
    });
  });
});

/* ------------------------------------------------ A16: where the trigger is -- */

describe("NotificationsDrawer - the control in the rail", () => {
  it("is pinned in the sidebar footer, and opens the drawer from there", async () => {
    daemon.apiFetch.mockImplementation(
      drawerFetch({ pending: [notification({ id: 1 })], feed: [] }),
    );

    // The whole app here, and only here: mounting the drawer on its own would
    // prove nothing about whether it is reachable from every page.
    await renderApp({ initialPath: "/" });

    const nav = await screen.findByRole("navigation", { name: "Sections" });
    const trigger = await within(nav).findByRole("button", {
      name: "Notifications, 1 held by the calendar",
    });

    // In the pinned footer, not in the scrolling item list — the footer is
    // where the state of the machine lives, and it is reachable at any scroll
    // position of the rail above it.
    expect(trigger.closest(".nav-footer")).not.toBeNull();

    fireEvent.click(trigger);
    expect(await screen.findByRole("complementary", { name: "Notifications" })).toBeDefined();
  });
});
