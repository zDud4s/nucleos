import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Feed } from "./Feed";
import { feedIsSearching, feedQueryString, type FeedEntry } from "../data/feed";
import { daemonFetch, daemonState, renderApp, renderWithRouter, type DaemonState } from "../test/harness";

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

const MINUTE = 60_000;

/** A line `minutesAgo` before now — inside every window the page offers. */
function entry(overrides: Partial<FeedEntry> & { minutesAgo?: number } = {}): FeedEntry {
  const { minutesAgo = 10, ...rest } = overrides;
  return {
    id: 1,
    project_id: "alpha",
    kind: "job_started",
    summary: "job 1 started",
    run_id: null,
    errand_id: null,
    subject: null,
    created_at: new Date(Date.now() - minutesAgo * MINUTE).toISOString(),
    ...rest,
  };
}

/**
 * The foundation's responder, with the feed's lines in it; `/feed?` — the listing and the search —
 * answers `listing`, which defaults to the same lines.
 */
function feedFetch(
  lines: FeedEntry[],
  overrides: Partial<DaemonState> = {},
  listing: FeedEntry[] = lines,
): { state: DaemonState; fetch: (path: string, init?: RequestInit) => Promise<unknown> } {
  const state = daemonState({ feedLines: lines, ...overrides });
  const shared = daemonFetch(state);
  return {
    state,
    fetch: async (path, init) => {
      if (path.startsWith("/feed?")) return listing;
      return await shared(path, init);
    },
  };
}

function useLines(lines: FeedEntry[], overrides: Partial<DaemonState> = {}, listing?: FeedEntry[]): DaemonState {
  const { state, fetch } = feedFetch(lines, overrides, listing);
  daemon.apiFetch.mockImplementation(fetch);
  return state;
}

/** Every `/feed` path the page has asked for, in order. */
function feedCalls(): string[] {
  return daemon.apiFetch.mock.calls
    .map(([path]) => String(path))
    .filter((path) => path.startsWith("/feed"));
}

async function lines(): Promise<HTMLElement> {
  return await screen.findByRole("region", { name: "Lines in this window" });
}

describe("Feed - filter layout", () => {
  it("the Search button is a member of the filter row, not a loose child of it", async () => {
    useLines([entry()]);
    await renderFeed();

    const search = await screen.findByRole("button", { name: "Search", hidden: true });
    expect(search.parentElement?.className).toBe("feed-filter-submit");
    expect(search.parentElement?.parentElement?.className).toContain("feed-filters");
  });

  it("keeps the filters behind More, and opens them by itself when the route carries one", async () => {
    useLines([entry()]);
    const { unmount } = await renderFeed();
    const more = await screen.findByRole("button", { name: "More" });
    expect(more.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(more);
    expect(more.getAttribute("aria-expanded")).toBe("true");
    unmount();

    await renderFeed("/feed?kind=job_failed");
    expect((await screen.findByRole("button", { name: "More" })).getAttribute("aria-expanded")).toBe("true");
  });
});

/* ------------------------------------------------------------- verdict -- */

describe("Feed - the verdict names the window and what in it wants a look", () => {
  it("counts went wrong, held, asks for you and routine, and never borrows Waiting's phrase", async () => {
    useLines([
      entry({ id: 1, kind: "job_gate_failed", minutesAgo: 50 }),
      entry({ id: 2, kind: "run_failed_final", minutesAgo: 45 }),
      entry({ id: 3, kind: "job_item_conflicted", minutesAgo: 40 }),
      entry({ id: 4, kind: "email_urgent", project_id: null, minutesAgo: 35 }),
      entry({ id: 5, kind: "job_started", minutesAgo: 30 }),
      entry({ id: 6, kind: "job_planned", minutesAgo: 25 }),
    ]);
    await renderFeed();

    const header = await screen.findByRole("banner");
    await waitFor(() => expect(within(header).getByRole("button", { name: /2 went wrong/ })).toBeDefined());
    expect(within(header).getByRole("button", { name: /1 held/ })).toBeDefined();
    expect(within(header).getByRole("button", { name: /1 asks for you/ })).toBeDefined();
    expect(header.textContent).toMatch(/2 routine/);
    // No marker yet, so the window is a day and says why.
    expect(header.textContent).toMatch(/Last 24 h · nothing marked seen yet/);
    expect(header.textContent).not.toMatch(/waiting on you/i);
  });

  it("says nothing went wrong in words when nothing did", async () => {
    useLines([entry({ id: 1, kind: "job_finished" })]);
    await renderFeed();
    const header = await screen.findByRole("banner");
    await waitFor(() => expect(header.textContent).toMatch(/nothing went wrong\W*1 routine/));
    expect(within(header).queryByRole("button")).toBeNull();
  });

  it("a count opens the newest sequence it counts", async () => {
    useLines([
      entry({ id: 1, kind: "job_gate_failed", subject: "job:1", summary: "the older failure", minutesAgo: 50 }),
      entry({ id: 2, kind: "job_gate_failed", subject: "job:2", summary: "the newer failure", minutesAgo: 20 }),
      entry({ id: 3, kind: "email_urgent", project_id: null, summary: "the newest ask", minutesAgo: 10 }),
    ]);
    await renderFeed();
    // At rest the page opens on the newest exception of any gravity — the e-mail.
    expect(await screen.findByRole("heading", { level: 2, name: /^urgent e-mail/ })).toBeDefined();
    fireEvent.click(await screen.findByRole("button", { name: /2 went wrong/ }));
    expect(await screen.findByRole("heading", { level: 2, name: /^job 2/ })).toBeDefined();
  });
});

/* ---------------------------------------------------------------- trace -- */

describe("Feed - the trace draws sequences, not lines", () => {
  const night = [
    entry({ id: 1, kind: "job_started", subject: "job:57", summary: "job 57 started on job/57-importer from the rule nightly reconciliation", minutesAgo: 95 }),
    entry({ id: 2, kind: "job_gate_failed", subject: "job:57", summary: "job 57 gate failed", minutesAgo: 90 }),
    entry({ id: 3, kind: "vcs_request_finished", subject: "vcs:44", summary: "vcs request 44 landed", minutesAgo: 80 }),
    entry({ id: 4, kind: "email_urgent", project_id: null, summary: "the accountant asked twice", minutesAgo: 70 }),
    entry({ id: 5, kind: "run_retry", subject: "run:900598", project_id: "delta", run_id: 900598, summary: "run 900598 attempt 1 failed, retrying", minutesAgo: 66 }),
    entry({ id: 6, kind: "run_failed_final", subject: "run:900598", project_id: "delta", run_id: 900598, summary: "run 900598 failed after 2 attempts", minutesAgo: 60 }),
    entry({ id: 7, kind: "job_item_conflicted", subject: "job:57", summary: "job 57 item 3 did not merge", minutesAgo: 40 }),
    entry({ id: 8, kind: "job_waiting", subject: "job:58", project_id: "bravo", summary: "job 58 is waiting: another run holds the project's worktree slot", minutesAgo: 20 }),
  ];

  async function trace(): Promise<HTMLElement> {
    return await screen.findByRole("list", { name: /^Sequences/ });
  }

  it("folds a job's lines into one row that says how it ended, under its lane", async () => {
    useLines(night);
    await renderFeed();
    const rows = await trace();

    const job = within(rows).getByRole("button", { name: /^job 57 · nightly reconciliation, alpha, job item did not merge/ });
    expect(job.closest("li")?.className).toContain("feed-trace-shade-held");
    expect(within(rows).getAllByRole("button", { name: /^job 57/ })).toHaveLength(1);
    // Retries are counted, and a lone line is named by its kind.
    expect(within(rows).getByRole("button", { name: /^run 900598, delta, run failed for good, attempt 2/ })).toBeDefined();
    expect(within(rows).getByRole("button", { name: /^urgent e-mail/ })).toBeDefined();
    // Lanes are headings whose buttons collapse them, with the lines they hold in the name.
    expect(within(rows).getByRole("button", { name: "Jobs, 4 lines" }).closest("h3")).not.toBeNull();
  });

  it("clicking a lane collapses its rows and clicking again opens them", async () => {
    useLines(night);
    await renderFeed();
    const rows = await trace();
    const jobs = within(rows).getByRole("button", { name: "Jobs, 4 lines" });
    expect(jobs.getAttribute("aria-expanded")).toBe("true");

    fireEvent.click(jobs);
    expect(jobs.getAttribute("aria-expanded")).toBe("false");
    expect(within(rows).queryByRole("button", { name: /^job 57/ })).toBeNull();
    expect(within(rows).getByRole("button", { name: /^vcs request 44/ })).toBeDefined();

    fireEvent.click(jobs);
    expect(within(rows).getByRole("button", { name: /^job 57/ })).toBeDefined();
  });

  it("opens on the newest sequence that wants a look, and a click opens another", async () => {
    useLines(night);
    await renderFeed();
    const rows = await trace();

    // The accountant's e-mail is newer than the run, but job 57's unmerged item is newer still.
    const detail = await screen.findByRole("heading", { level: 2, name: /^job 57 · nightly reconciliation/ });
    const section = detail.closest("section") as HTMLElement;
    expect(within(section).getByText("job 57 gate failed")).toBeDefined();
    expect(within(section).getByText("job 57 item 3 did not merge")).toBeDefined();
    expect(within(rows).getByRole("button", { name: /^job 57/ }).getAttribute("aria-pressed")).toBe("true");

    const run = within(rows).getByRole("button", { name: /^run 900598/ });
    fireEvent.click(run);
    const opened = await screen.findByRole("heading", { level: 2, name: /^run 900598/ });
    const facts = within(opened.closest("section") as HTMLElement).getByRole("complementary");
    expect(facts.textContent).toMatch(/Attempts\s*2/);
    expect(within(facts).getByRole("link", { name: "run 900598" }).getAttribute("href")).toBe("/runs/900598");

    // Pressed again, it closes, and nothing stands in its place.
    fireEvent.click(run);
    await waitFor(() => expect(screen.queryByRole("heading", { level: 2, name: /^run 900598/ })).toBeNull());
    expect(screen.queryByRole("complementary")).toBeNull();
  });

  it("rests on now with the open sequences counted, and draws what is still open dashed", async () => {
    useLines(night);
    await renderFeed();
    const rows = await trace();
    expect(screen.getByText("now · 1 still open")).toBeDefined();
    const parked = within(rows).getByRole("button", { name: /^job 58, bravo, waiting for a slot, still open/ }).closest("li") as HTMLElement;
    expect(parked.getAttribute("data-state")).toBe("running");
    expect(parked.querySelector(".feed-trace-ghost-open")).not.toBeNull();
    const settled = within(rows).getByRole("button", { name: /^vcs request 44/ }).closest("li") as HTMLElement;
    expect(settled.querySelector(".feed-trace-ghost-open")).toBeNull();
  });

  it("the replay rail moves time from the keyboard, and the rows and the header follow", async () => {
    useLines(night);
    await renderFeed();
    const rows = await trace();
    const slider = screen.getByRole("slider", { name: "Replay time" });
    expect(slider.getAttribute("aria-valuetext")).toMatch(/, now$/);
    expect(slider.getAttribute("aria-valuenow")).toBe(slider.getAttribute("aria-valuemax"));

    fireEvent.keyDown(slider, { key: "Home" });
    expect(slider.getAttribute("aria-valuenow")).toBe("0");
    expect(slider.getAttribute("aria-valuetext")).not.toMatch(/now/);
    const job = within(rows).getByRole("button", { name: /^job 57/ }).closest("li") as HTMLElement;
    expect(job.getAttribute("data-state")).toBe("queued");
    expect(screen.getByText(/· nothing in progress$/)).toBeDefined();

    // Forward past job 57's first line and into its gate: in progress, not yet how it ended.
    const late = Date.now() - 88 * MINUTE;
    while (Number(slider.getAttribute("aria-valuenow")) * MINUTE + (Date.now() - 24 * 60 * MINUTE) < late) {
      fireEvent.keyDown(slider, { key: "ArrowRight" });
    }
    expect(job.getAttribute("data-state")).toBe("running");
    expect(job.textContent).toMatch(/in progress/);
    expect(screen.getByText(/· 1 in progress$/)).toBeDefined();

    fireEvent.click(screen.getByRole("button", { name: "Now" }));
    expect(slider.getAttribute("aria-valuetext")).toMatch(/, now$/);
    expect(job.getAttribute("data-state")).toBe("settled");
  });

  it("a count in the verdict dims every row without a line of its gravity, and pressed again lets go", async () => {
    useLines(night);
    await renderFeed();
    const rows = await trace();
    const header = await screen.findByRole("banner");
    const wrong = await within(header).findByRole("button", { name: /2 went wrong/ });

    fireEvent.click(wrong);
    expect(wrong.getAttribute("aria-pressed")).toBe("true");
    const row = (name: RegExp) => within(rows).getByRole("button", { name }).closest("li") as HTMLElement;
    expect(row(/^vcs request 44/).hasAttribute("data-dim")).toBe(true);
    expect(row(/^urgent e-mail/).hasAttribute("data-dim")).toBe(true);
    expect(row(/^job 57/).hasAttribute("data-dim")).toBe(false);
    expect(row(/^run 900598/).hasAttribute("data-dim")).toBe(false);
    // And it opens the newest sequence that went wrong — job 57, whose gate failed before its item.
    expect(await screen.findByRole("heading", { level: 2, name: /^job 57/ })).toBeDefined();

    fireEvent.click(wrong);
    expect(wrong.getAttribute("aria-pressed")).toBe("false");
    expect(row(/^vcs request 44/).hasAttribute("data-dim")).toBe(false);
  });

  it("a lane with more sequences than fit folds its routine ones into one row, and keeps the failure", async () => {
    const jobs = Array.from({ length: 14 }, (_, i) =>
      entry({
        id: i + 1,
        kind: i === 6 ? "job_failed" : "job_finished",
        subject: `job:${i + 1}`,
        summary: `job ${i + 1} settled`,
        minutesAgo: 200 - i * 10,
      }),
    );
    useLines(jobs);
    await renderFeed();
    const rows = await trace();
    expect(within(rows).getByRole("button", { name: /^job 7, alpha, job failed/ })).toBeDefined();
    expect(within(rows).queryByRole("button", { name: /^job 1,/ })).toBeNull();

    const fold = within(rows).getByRole("button", { name: "13 routine sequences in Jobs" });
    expect(fold.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(fold);
    expect(fold.getAttribute("aria-expanded")).toBe("true");
    expect(within(rows).getByRole("button", { name: /^job 1,/ })).toBeDefined();
    expect(within(rows).getAllByRole("button", { name: /^job \d+,/ })).toHaveLength(14);
  });

  it("lines about nothing that lasts group by kind into one series row, with no bar and nothing open", async () => {
    useLines([
      ...[300, 200, 100].map((minutesAgo, i) =>
        entry({ id: i + 1, kind: "email_digest", project_id: null, summary: `digest ${i + 1}: 23 e-mails triaged`, minutesAgo }),
      ),
      entry({ id: 9, kind: "job_finished", subject: "job:9", summary: "job 9 finished", minutesAgo: 50 }),
    ]);
    await renderFeed();
    const rows = await trace();
    const series = within(rows).getByRole("button", { name: /^e-mail digest, 3 lines/ }).closest("li") as HTMLElement;
    expect(within(rows).getAllByRole("button", { name: /^e-mail digest/ })).toHaveLength(1);
    expect(series.textContent).toMatch(/3 lines/);
    expect(series.querySelectorAll("circle")).toHaveLength(3);
    expect(series.querySelector("rect, line")).toBeNull();
    expect(screen.getByText("now · nothing still open")).toBeDefined();
  });

  it("the replay never narrows the list under it", async () => {
    useLines(night);
    await renderFeed();
    const list = await lines();
    expect(screen.getByRole("heading", { level: 2, name: "Every line" })).toBeDefined();
    fireEvent.keyDown(screen.getByRole("slider", { name: "Replay time" }), { key: "Home" });
    expect(within(list).getByText("job 57 item 3 did not merge")).toBeDefined();
    expect(within(list).getByText("the accountant asked twice")).toBeDefined();
  });
});

/* -------------------------------------------------------------- routine -- */

describe("Feed - routine folds", () => {
  it("folds consecutive routine lines into one row that opens", async () => {
    useLines([
      entry({ id: 1, kind: "job_started", summary: "job 8 started on job/8-tidy", minutesAgo: 30 }),
      entry({ id: 2, kind: "worktree_released", summary: "released worktree run-8", minutesAgo: 29 }),
      entry({ id: 3, kind: "job_finished", summary: "job 8 finished", minutesAgo: 28 }),
    ]);
    await renderFeed();
    const list = await lines();
    const fold = within(list).getByRole("button", { name: /3 routine/ });
    expect(fold.getAttribute("aria-expanded")).toBe("false");
    expect(fold.textContent).toMatch(/job finished, worktree released, job started/);
    expect(within(list).queryByText("job 8 finished")).toBeNull();

    fireEvent.click(fold);
    expect(fold.getAttribute("aria-expanded")).toBe("true");
    const finished = within(list).getByText("job finished");
    expect(finished.className).toContain("ui-badge-info");
    expect(finished.getAttribute("title")).toBe("job_finished");
  });
});

/* ------------------------------- A15: one kind, five different situations -- */

describe("Feed - a waiting line says what it is waiting for", () => {
  it("reads budget, slot, exclusion and disk apart, from the same kind", async () => {
    /*
      All three are `job_waiting`. The daemon puts the reason nowhere but the
      summary — `park` writes `job {id} is waiting: {detail}` — so this is the
      test that the page reads the detail rather than badging the kind once and
      calling three different situations the same thing.

      The exclusion row is the trap, and it is here on purpose: its detail is
      "job 9 holds a slot and the two are excluded", so anything matching on the
      word "slot" reads it as slot contention and sends somebody looking for
      capacity that is already there.

      The disk row is the other one. Its detail names "this project's
      worktrees", and on 2026-09-14 the daemon still reported it as slot
      contention — so it must read as the disk, and never as a slot.
    */
    useLines([
      entry({ id: 4, kind: "job_waiting", minutesAgo: 30, summary: "job 4 is waiting: job 9 holds a slot and the two are excluded" }),
      entry({ id: 3, kind: "job_waiting", minutesAgo: 31, summary: "job 3 is waiting: another run holds the project's worktree slot" }),
      entry({ id: 2, kind: "job_waiting", minutesAgo: 32, summary: "job 2 is waiting: hourly spend $4.90 + $0.25 reserve would exceed the $5.00 hourly limit" }),
      entry({ id: 6, kind: "job_waiting", minutesAgo: 33, summary: "job 6 is waiting: the disk is too full for another checkout: only 37559 MiB free where this project's worktrees live, and a new checkout needs at least 102400 MiB" }),
    ]);
    await renderFeed();

    // A parked job is routine — no park reason asks something of the reader — so the four fold.
    const list = await lines();
    fireEvent.click(within(list).getByRole("button", { name: /4 routine/ }));
    const items = [...list.querySelectorAll("li.feed-line")] as HTMLElement[];
    expect(items.length).toBe(4);

    // Newest first: exclusion, slot, budget, disk. Each row carries its own reading, and only its own.
    expect(within(items[0]).getByText("held by an exclusion")).toBeDefined();
    expect(within(items[0]).queryByText("waiting for a slot")).toBeNull();

    expect(within(items[1]).getByText("waiting for a slot")).toBeDefined();
    expect(within(items[1]).queryByText("held by budget")).toBeNull();
    // Slot contention frees itself: a fact, never the amber that summons somebody.
    expect(within(items[1]).getByText("waiting for a slot").className).not.toContain("ui-badge-pending");
    expect(within(items[1]).getByText("job waiting").className).not.toContain("ui-badge-pending");

    expect(within(items[2]).getByText("held by budget")).toBeDefined();
    expect(within(items[2]).queryByText("waiting for a slot")).toBeNull();

    expect(within(items[3]).getByText("held by a full disk")).toBeDefined();
    expect(within(items[3]).queryByText("waiting for a slot")).toBeNull();
  });

  it("says nothing extra about a waiting line it cannot read", async () => {
    // The control on the test above: the reading is derived, not decorative, so
    // a reason this shell has no map for adds no badge rather than a plausible
    // one. `attention` is a real `Brake::Park` reason with no `wait_reason` entry.
    useLines([entry({ id: 5, kind: "job_waiting", summary: "job 5 is waiting: somebody is at the keyboard on alpha" })]);
    await renderFeed();

    const list = await lines();
    expect(within(list).getByText("job waiting")).toBeDefined();
    expect(within(list).queryByText("held by budget")).toBeNull();
    expect(within(list).queryByText("waiting for a slot")).toBeNull();
    expect(within(list).queryByText("held by an exclusion")).toBeNull();
    expect(within(list).queryByText("held by a full disk")).toBeNull();
  });
});

/* --------------------------------------------------------- the seen marker -- */

describe("Feed - what you have seen moves when you leave", () => {
  const five = [1, 2, 3, 4, 5].map((id) =>
    entry({ id, kind: id === 4 ? "job_gate_failed" : "job_started", summary: `line ${id}`, minutesAgo: 60 - id * 5 }),
  );
  const markedAt = (id: number) => ({
    through: id,
    through_created_at: five[id - 1].created_at,
    seen_at: new Date(Date.parse(five[id - 1].created_at) + MINUTE).toISOString(),
  });

  it("shades from the marker it found on arrival, and marks through the newest line it showed on leaving", async () => {
    const state = useLines(five, { feedSeen: markedAt(2) });
    const { unmount } = await renderFeed();
    const list = await lines();

    // The rule sits between what you had seen and what you had not.
    const rule = within(list).getByText(/you looked here/);
    const order = [...list.querySelectorAll("li")].map((li) => li.textContent ?? "");
    const at = order.findIndex((text) => /you looked here/.test(text));
    expect(order.slice(at + 1).join(" ")).toMatch(/line 2|2 routine/);
    expect(rule).toBeDefined();

    // Nothing moves while you are reading.
    expect(state.feedSeenPosts).toEqual([]);

    unmount();
    await waitFor(() => expect(state.feedSeenPosts).toEqual([5]));
    expect(state.feedSeen.through).toBe(5);
  });

  it("marks through what was shown when the window is hidden, and not again for the same lines", async () => {
    const state = useLines(five, { feedSeen: markedAt(2) });
    const { unmount } = await renderFeed();
    await lines();

    const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await waitFor(() => expect(state.feedSeenPosts).toEqual([5]));
    visibility.mockRestore();

    unmount();
    // The visit's shading did not move, and leaving did not post the same line twice.
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(state.feedSeenPosts).toEqual([5]);
  });

  it("a search moves nothing, because it is a question about the past", async () => {
    const state = useLines(five, { feedSeen: markedAt(2) });
    const { unmount } = await renderFeed("/feed?q=line");
    expect(await screen.findByText(/the page has stopped refreshing/)).toBeDefined();
    unmount();
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(state.feedSeenPosts).toEqual([]);
  });

  it("Mark everything seen moves it now, and the shading follows", async () => {
    const state = useLines(five, { feedSeen: markedAt(2) });
    await renderFeed();
    await lines();
    const mark = screen.getByRole("button", { name: "Mark everything seen" });

    fireEvent.click(mark);
    await waitFor(() => expect(state.feedSeenPosts).toEqual([5]));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Mark everything seen" })).toBeNull());
    // The rule now opens the list: nothing below it is new.
    await waitFor(() => {
      const first = screen.getByRole("region", { name: "Lines in this window" }).querySelector("ol > li");
      expect(first?.textContent).toMatch(/you looked here/);
    });
  });
});

/* ---------------------------------------------------------------- states -- */

describe("Feed - the window's states", () => {
  it("says plainly when the window holds more than the route will send", async () => {
    useLines([entry({ id: 1, minutesAgo: 30 }), entry({ id: 2, minutesAgo: 20 }), entry({ id: 3, minutesAgo: 10 })], {
      feedTimelineCap: 2,
    });
    await renderFeed();
    expect(await screen.findByText(/holds more than 5,000 lines/)).toBeDefined();
    expect(screen.getByText(/That is the newest 5,000 lines in this window/)).toBeDefined();
  });

  it("an empty window is a quiet machine, with the last line named and a way to widen", async () => {
    const old = entry({ id: 1, minutesAgo: 3 * 24 * 60, summary: "an old line" });
    useLines([old], {}, [old]);
    await renderFeed();
    expect(await screen.findByText(/The machine was quiet in this window/)).toBeDefined();
    expect(screen.getByText(/The last line was written/)).toBeDefined();

    fireEvent.click(screen.getByRole("button", { name: "Widen to 7 days" }));
    expect(await screen.findByText("an old line", {}, { timeout: 3000 })).toBeDefined();
    expect(screen.getByRole("button", { name: "7 days" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("a feed that has never had a line explains itself and offers a way to start", async () => {
    useLines([], {}, []);
    await renderFeed();
    expect(await screen.findByText("Nothing has happened yet")).toBeDefined();
    expect(screen.getByRole("link", { name: "Start a run" }).getAttribute("href")).toBe("/runs");
    expect(screen.getByRole("link", { name: "Turn on autopilot" }).getAttribute("href")).toBe("/autopilot");
  });

  it("a poll brings only what is new, and says how many arrived", async () => {
    const state = useLines([entry({ id: 1, minutesAgo: 20 })]);
    const { queryClient } = await renderFeed();
    await lines();

    state.feedLines = [...state.feedLines, entry({ id: 2, kind: "job_gate_failed", summary: "a fresh failure", minutesAgo: 1 })];
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: ["feed", "timeline"] });
    });

    expect(await within(await lines()).findByText("a fresh failure")).toBeDefined();
    expect(feedCalls().some((path) => path.startsWith("/feed/timeline") && path.includes("after_id=1"))).toBe(true);
    await waitFor(() => expect(screen.getByText("1 new line")).toBeDefined());
  });

  it("the lines that ask for you carry the door to where they are answered", async () => {
    useLines([
      entry({ id: 1, kind: "promotion_ready", project_id: "charlie", summary: "charlie has 5 of 5 classes ready", minutesAgo: 30 }),
      entry({ id: 2, kind: "email_urgent", project_id: null, summary: "the accountant asked twice", minutesAgo: 20 }),
      entry({ id: 3, kind: "run_failed_final", run_id: 900598, summary: "run 900598 failed", minutesAgo: 10 }),
    ]);
    await renderFeed();
    const list = await lines();
    expect(within(list).getByRole("link", { name: "Review in Autopilot" }).getAttribute("href")).toBe("/autopilot");
    expect(within(list).getByRole("link", { name: "Open Mail" }).getAttribute("href")).toBe("/mail");
    expect(within(list).getByRole("link", { name: "run 900598" }).getAttribute("href")).toBe("/runs/900598");
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
    const lines = [
      entry({ id: 6, kind: "job_gate_failed", summary: "alpha's gate failed", minutesAgo: 20 }),
      entry({ id: 7, kind: "job_gate_failed", project_id: "bravo", summary: "bravo's gate failed", minutesAgo: 10 }),
    ];
    useLines(lines);

    await renderFeed("/feed?project=alpha&q=gate");

    // The page says it has stopped refreshing. A feed that silently stopped
    // polling is indistinguishable from a machine that stopped doing anything.
    expect(await screen.findByText(/the page has stopped refreshing/)).toBeDefined();
    expect(screen.getByRole("region", { name: "Search results" })).toBeDefined();
    // No trace over a search: its answer is a list of matches, not a stretch of time.
    expect(screen.queryByRole("slider", { name: "Replay time" })).toBeNull();

    // The search really went to the daemon, narrowed to the project — and with
    // no `scope=all`, which would have short-circuited the project away.
    await waitFor(() => {
      expect(feedCalls().some((path) => path.includes("q=gate"))).toBe(true);
    });
    const searched = feedCalls().find((path) => path.includes("q=gate")) ?? "";
    expect(searched).toContain("project_id=alpha");
    expect(searched).not.toContain("scope=all");
    expect(feedCalls().some((path) => path.startsWith("/feed/timeline"))).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: "Back to live" }));

    // Back to live: the notice is gone and the project survived, because
    // narrowing to an owner never froze anything in the first place.
    await waitFor(() => {
      expect(screen.queryByText(/the page has stopped refreshing/)).toBeNull();
    });
    const live = await screen.findByRole("region", { name: "Lines in this window" });
    expect(within(live).getByText("alpha's gate failed")).toBeDefined();
    expect(within(live).queryByText("bravo's gate failed")).toBeNull();
    expect(screen.getByText(/only alpha/)).toBeDefined();
    expect(feedCalls().some((path) => path.startsWith("/feed/timeline"))).toBe(true);
  });
});

/* ------------------------------------------------------ A15: unmapped kinds -- */

describe("Feed - a kind this shell has no reading for", () => {
  it("a mapped kind's reading comes from the map, and an unmapped one still shows its own literal", async () => {
    useLines([
      entry({ id: 8, kind: "job_gate_failed", summary: "job 8 gate failed", minutesAgo: 20 }),
      entry({ id: 9, kind: "nonesuch_kind", summary: "a kind without a map reading", minutesAgo: 10 }),
    ]);

    await renderFeed();
    const list = await lines();

    const mapped = within(list).getByText("job gate failed");
    expect(mapped.className).toContain("ui-badge-danger");
    expect(mapped.getAttribute("title")).toBe("job_gate_failed");
    const unknown = within(list).getByText("nonesuch_kind");
    expect(unknown.className).toContain("ui-state-unmapped");
    expect(unknown.className).toContain("ui-badge-off");
  });

  it("an unknown feed kind wears the same ignorance device as an unknown state", async () => {
    // `email_<class>` is built from configuration (`notify_classes`), so the
    // shell cannot have a table entry for everybody's classes. Showing the
    // literal admits ignorance; a plausible label would be a claim.
    useLines([entry({ id: 7, kind: "email_shopping", summary: "3 messages about deliveries" })]);

    await renderFeed();

    const list = await lines();
    const unknown = within(list).getByText("email_shopping");
    expect(unknown.className).toContain("ui-state-unmapped");
    expect(unknown.className).toContain("ui-badge-off");
    // Still drawn in the mail lane: the family is known even where the class is not.
    expect(screen.getByRole("button", { name: "Errands & mail, 1 line" })).toBeDefined();
  });

  it("reads the kinds the Teams pillar writes most, instead of showing them as words it has never heard", async () => {
    // Twelve real kinds rendered `.ui-state-unmapped` until the completeness test enumerated the
    // writers; `team_run_finished` and `team_action` are the two the pillar writes most.
    useLines([
      entry({ id: 11, kind: "team_run_finished", project_id: null, summary: "a team run failed: no specialist answered", minutesAgo: 30 }),
      entry({ id: 12, kind: "team_trigger_skipped", project_id: null, summary: "`morning digest` did not start support: a run is already in flight", minutesAgo: 20 }),
      entry({ id: 13, kind: "team_action", project_id: null, summary: "a department's `email_send` failed: the mailbox refused it", minutesAgo: 10 }),
    ]);

    await renderFeed();
    const list = await lines();

    const skipped = within(list).getByText("team trigger did not fire");
    expect(skipped.className).toContain("ui-badge-paused");
    expect(skipped.className).not.toContain("ui-state-unmapped");
    for (const label of ["team run settled", "team action settled"]) {
      const badge = within(list).getByText(label);
      expect(badge.className).toContain("ui-badge-info");
      expect(badge.className).not.toContain("ui-state-unmapped");
    }
  });
});

/* --------------------------------------------------------------- the route -- */

describe("Feed - the route", () => {
  it("is registered, so the rail reaches the page and not the placeholder", async () => {
    useLines([]);

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
    useLines([]);
    void renderFeed();

    expect((await screen.findByLabelText("Only lines after")).getAttribute("lang")).toBe("en-GB");
    expect(screen.getByLabelText("Only lines before").getAttribute("lang")).toBe("en-GB");
  });
});
