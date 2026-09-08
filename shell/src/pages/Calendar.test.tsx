import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, renderHook, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ApiRefusal } from "../data/client";
import {
  BusyIndicator,
  Calendar,
  HeldNotifications,
  headline,
  useMinuteClock,
  visibleWindow,
  type CalendarSearch,
} from "./Calendar";
import type { CalendarConfigView, EventOccurrence } from "../data/calendar";
import type { PendingNotification } from "../data/feed";
import { UI_LOCALE } from "../lib/locale";
import { renderWithQuery, renderWithRouter } from "../test/harness";

/**
 * The page inside a real router, because two of the things it does are part of
 * the location: `?view=` and `?on=`. `renderWithRouter` mounts the component in
 * a tree built from the app's own `NAV_PATHS`, so a navigation this page makes
 * resolves against a route that really exists — the same reason `Feed.test.tsx`
 * uses it rather than `renderApp`, which would drag the gate, the rail and
 * their live queries around every assertion.
 */
function page(path = "/calendar") {
  return renderWithRouter(<Calendar />, { initialPath: path });
}

beforeAll(() => {
  vi.stubEnv("TZ", "Europe/Lisbon");
});

afterAll(() => {
  vi.unstubAllEnvs();
});

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
});

const CONFIG: CalendarConfigView = {
  default_tz: "Europe/Lisbon",
  working_hours_start: "09:00",
  working_hours_end: "18:00",
  working_weekdays: ["mon", "tue", "wed", "thu", "fri"],
};

function occurrence(overrides: Partial<EventOccurrence> = {}): EventOccurrence {
  return {
    event_id: 1,
    title: "Standup",
    source: "human",
    occurrence_local: "2026-08-20T09:00:00",
    starts_at: "2026-08-20T08:00:00Z",
    ends_at: "2026-08-20T08:30:00Z",
    ...overrides,
  };
}

/* ------------------------------------------------------------- the clock -- */

describe("useMinuteClock", () => {
  /**
   * The page had no clock of its own: `now` was a bare `new Date()` evaluated
   * during render, so the "now" reading advanced only when something else
   * re-rendered the page — in practice the busy poll, which ran at 3 s and was
   * therefore acting as an undeclared clock. Design §6.14 asks for 60 s.
   */
  it("advances on its own, without anything else re-rendering the page", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 7, 20, 9, 0, 0));

    const { result } = renderHook(() => useMinuteClock());
    expect(result.current.getMinutes()).toBe(0);

    // Advancing the fake timers advances the mocked clock with them, so the
    // tick and the time it reads move together — setting the system time as
    // well would move it twice.
    act(() => {
      vi.advanceTimersByTime(60_000);
    });

    expect(result.current.getMinutes()).toBe(1);
    vi.useRealTimers();
  });

  it("stops when the page goes away", () => {
    vi.useFakeTimers();
    const cleared = vi.spyOn(globalThis, "clearInterval");

    const { unmount } = renderHook(() => useMinuteClock());
    unmount();

    expect(cleared).toHaveBeenCalled();
    cleared.mockRestore();
    vi.useRealTimers();
  });
});

/* ------------------------------------------------------------ the window -- */

describe("visibleWindow", () => {
  /** The month asks for the six weeks it DRAWS, not the calendar month. */
  it("covers the whole six-week month grid", () => {
    const { from, to } = visibleWindow(new Date(2026, 7, 1), "month");
    // August 2026 starts on a Saturday, so the grid opens on Monday 27 July.
    expect(new Date(from).getDate()).toBe(27);
    expect(new Date(from).getMonth()).toBe(6);
    // Six weeks later, ending at the midnight after Sunday 6 September.
    expect(new Date(to).getMonth()).toBe(8);
  });

  it("covers exactly the seven days of a week", () => {
    const { from, to } = visibleWindow(new Date(2026, 7, 20), "week");
    expect(new Date(from).getDate()).toBe(17);
    expect(new Date(to).getDate()).toBe(24);
    // Seven days, in whatever the local offsets of the two ends are.
    expect(new Date(to).getTime() - new Date(from).getTime()).toBe(7 * 86_400_000);
  });
});

/* ---------------------------------------------------------- the headline -- */

describe("headline", () => {
  /**
   * It said "this month" and counted the six-week window — up to twelve days
   * of neighbouring months folded into the number, on every page load.
   */
  it("counts only the month it names", () => {
    const rows = [
      occurrence({ event_id: 1 }),
      // 30 July, which the August grid draws but August does not contain.
      occurrence({
        event_id: 2,
        occurrence_local: "2026-07-30T09:00:00",
        starts_at: "2026-07-30T08:00:00Z",
        ends_at: "2026-07-30T08:30:00Z",
      }),
    ];

    expect(headline(rows, new Date(2026, 7, 1), "month", CONFIG)).toContain("1 occurrence this month");
  });

  it("counts the whole window when the window IS the week", () => {
    expect(headline([occurrence(), occurrence({ event_id: 2 })], new Date(2026, 7, 20), "week", CONFIG))
      .toContain("2 occurrences this week");
  });

  it("says nothing at all until the config has answered", () => {
    expect(headline([occurrence()], new Date(2026, 7, 1), "month", undefined)).toBeUndefined();
  });
});

/* ------------------------------------------------------------------ busy -- */

describe("BusyIndicator", () => {
  it("says which of the two states this machine is in", () => {
    const { rerender } = render(<BusyIndicator busy={true} />);
    expect(screen.getByText("busy right now")).toBeDefined();

    rerender(<BusyIndicator busy={false} />);
    expect(screen.getByText("free right now")).toBeDefined();
  });

  /** `GET /calendar/busy` never refuses, so the only other state is "not answered yet". */
  it("draws nothing before the first answer", () => {
    const { container } = render(<BusyIndicator busy={undefined} />);
    expect(container.firstChild).toBeNull();
  });
});

/* --------------------------------------------------- held notifications -- */

function notification(overrides: Partial<PendingNotification> = {}): PendingNotification {
  return {
    id: 1,
    kind: "email_arrived",
    summary: "the accountant replied",
    queued_at: "2026-08-20T08:00:00Z",
    delivered_at: null,
    ...overrides,
  };
}

describe("HeldNotifications", () => {
  /**
   * Two lists and never one merged: the fear this feature earns is "did the
   * calendar swallow something", and a queue showing only what is still held
   * cannot answer it.
   */
  it("keeps what is held apart from what was let through", async () => {
    daemon.apiFetch.mockResolvedValue([
      notification({ id: 1, summary: "still waiting" }),
      notification({ id: 2, summary: "went out later", delivered_at: "2026-08-20T09:00:00Z" }),
    ]);

    renderWithQuery(<HeldNotifications />);

    const held = await screen.findByLabelText("Held notifications");
    const released = await screen.findByLabelText("Released notifications");
    expect(within(held).getByText("still waiting")).toBeDefined();
    expect(within(released).getByText("went out later")).toBeDefined();
    expect(within(held).queryByText("went out later")).toBeNull();
  });

  it("says plainly when nothing has been held", async () => {
    daemon.apiFetch.mockResolvedValue([]);
    renderWithQuery(<HeldNotifications />);
    expect(await screen.findByText("nothing has been held.")).toBeDefined();
  });

  it("does not pretend an unanswered route means nothing was held", async () => {
    daemon.apiFetch.mockRejectedValue(new Error("connection refused"));
    renderWithQuery(<HeldNotifications />);
    expect(await screen.findByText(/nothing is known about held notifications/)).toBeDefined();
  });
});

/* ------------------------------------------------------------- the page -- */

describe("the page", () => {
  beforeEach(() => {
    // Pinned, or the assertions below pass in August and fail in October — and
    // `shouldAdvanceTime` so react-query's own waits still resolve.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(new Date(2026, 7, 20, 12, 0, 0));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  function answering(rows: EventOccurrence[]) {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path.startsWith("/calendar/events")) return rows;
      if (path === "/calendar/busy") return { busy: true };
      if (path === "/calendar/config") return CONFIG;
      if (path === "/notifications/pending") return [];
      return undefined;
    });
  }

  it("opens on this month, with today selected and its day in the sheet", async () => {
    answering([occurrence()]);
    await page();

    // The month's own name, and the honest count beside it.
    expect(await screen.findByText(/1 occurrence this month/)).toBeDefined();
    // The sheet opens on today rather than on nothing.
    const sheet = await screen.findByRole("heading", { level: 3 });
    expect(sheet.textContent).toContain("20");
    expect(await screen.findByRole("button", { name: "Skip this occurrence" })).toBeDefined();
  });

  it("renders the period label in the shell's own locale", async () => {
    answering([]);
    await page();

    const expected = new Date(2026, 7, 20).toLocaleDateString(UI_LOCALE, {
      month: "long",
      year: "numeric",
    });
    expect(await screen.findByRole("heading", { level: 1, name: expected })).toBeDefined();
  });

  it("switches to the week and keeps the day it was showing", async () => {
    answering([occurrence()]);
    const { container } = await page();

    fireEvent.click(await screen.findByRole("button", { name: "Week" }));

    await waitFor(() => expect(screen.getByText(/this week/)).toBeDefined());

    // The week containing Thursday 20 August opens on Monday the 17th and ends
    // on Sunday the 23rd. Read off the heading row rather than searched for as
    // text: the hour gutter is full of two-digit numbers too.
    const heads = [...container.querySelectorAll(".calendar-week-head-number")].map(
      (node) => node.textContent,
    );
    expect(heads).toEqual(["17", "18", "19", "20", "21", "22", "23"]);
    // And the day that was selected in the month is still the selected one.
    expect(screen.getByRole("heading", { level: 3 }).textContent).toContain("20");
  });

  it("pages to the next month and takes the selection with it", async () => {
    answering([]);
    await page();

    fireEvent.click(await screen.findByRole("button", { name: "Next ›" }));

    /*
      The selection follows, which is the defect this avoids: paging forward
      used to leave the sheet on a day in the month you had just left, with a
      heading that disagreed with the whole screen.
    */
    await waitFor(() => {
      const sheet = screen.getByRole("heading", { level: 3 });
      expect(sheet.textContent).toContain("2026");
      expect(sheet.textContent).not.toContain("20 de agosto");
    });
  });

  /**
   * The deep link. Worth having on its own — a calendar you can point at — and
   * the only way the screenshot harness can reach the week of a clock change,
   * which is otherwise several clicks deep in state no URL could express.
   */
  it("opens on the view and the day the location names", async () => {
    answering([]);
    const { container } = await page("/calendar?view=week&on=2026-03-29");

    await waitFor(() => {
      const heads = [...container.querySelectorAll(".calendar-week-head-number")].map(
        (node) => node.textContent,
      );
      // The week containing Sunday 29 March 2026 — Lisbon's 23-hour day.
      expect(heads).toEqual(["23", "24", "25", "26", "27", "28", "29"]);
    });
    expect(screen.getAllByText("23h")).toHaveLength(1);
  });

  /**
   * Paging published and selecting did not, which left `?on=` naming a day the
   * page had stopped showing the moment anybody clicked a cell — so copying
   * the link handed somebody a different day from the one on screen.
   */
  it("takes the location with it when a day is selected", async () => {
    answering([]);
    const { router, container } = await page("/calendar?on=2026-08-20");

    await waitFor(() => expect(container.querySelector(".calendar-grid")).not.toBeNull());
    const grid = container.querySelector(".calendar-grid") as HTMLElement;
    fireEvent.keyDown(grid, { key: "ArrowRight" });

    await waitFor(() =>
      expect((router.state.location.search as CalendarSearch).on).toBe("2026-08-21"),
    );
  });

  it("ignores a location that names something it cannot use", async () => {
    answering([]);
    // Neither of these is a view or a date, so the page opens where it always does.
    await page("/calendar?view=fortnight&on=yesterday");
    const month = await screen.findByRole("button", { name: "Month" });
    expect(month.getAttribute("aria-pressed")).toBe("true");
  });

  /**
   * The whole gesture, end to end, and the one thing about it that can go
   * quietly wrong: a drop must address the occurrence by its ORIGINAL local
   * start. Sending where it landed writes a second exception row instead of
   * relocating the first — the defect the `datetime-local` path has its own
   * double-move test for, arriving here by a different door.
   */
  it("a drag sends the original local start, not where it was dropped", async () => {
    const seen: Record<string, unknown> = {};
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (init?.method === "POST" && typeof init.body === "string") {
        seen[path] = JSON.parse(init.body);
      }
      if (path.startsWith("/calendar/events?")) return [occurrence()];
      if (path === "/calendar/busy") return { busy: true };
      if (path === "/calendar/config") return CONFIG;
      return [];
    });

    const { container } = await page("/calendar?on=2026-08-20");

    /*
      Scoped to the grid. The title is on screen twice by design — once as a
      chip in the cell and once in the sheet below, which is the whole point of
      the two halves — so a bare `findByText` finds both and refuses.
    */
    await waitFor(() => expect(container.querySelector(".calendar-chip")).not.toBeNull());
    const chip = container.querySelector(".calendar-chip") as HTMLElement;

    fireEvent.dragStart(chip, {
      dataTransfer: { setData: () => {}, effectAllowed: "" },
    });

    // The 27th: a different day, so a real move.
    const target = [...container.querySelectorAll(".calendar-day")].find((cell) =>
      (cell.getAttribute("data-day") ?? "") === "2026-08-27",
    ) as HTMLElement;
    fireEvent.dragOver(target);
    fireEvent.drop(target);

    await waitFor(() => expect(seen["/calendar/events/1/move"]).toBeDefined());
    expect(seen["/calendar/events/1/move"]).toEqual({
      // The identity, untouched — never the destination.
      occurrence_local: "2026-08-20T09:00:00",
      // The day changed and the time did not: a month cell cannot name an hour.
      to_local: "2026-08-27T09:00:00",
      duration_minutes: 30,
    });
  });

  it("says the núcleo did not answer rather than drawing an empty month", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path.startsWith("/calendar/events")) throw new Error("connection refused");
      if (path === "/calendar/config") return CONFIG;
      return [];
    });
    await page();

    expect(await screen.findByText(/nothing is known about this month/)).toBeDefined();
  });

  it("quotes a refusal on the events route instead of replacing it with generic copy", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path.startsWith("/calendar/events")) {
        throw new ApiRefusal(400, "bad_window", "from and to are both required");
      }
      if (path === "/calendar/config") return CONFIG;
      return [];
    });
    await page();

    expect(await screen.findByText(/bad_window/)).toBeDefined();
  });
});
