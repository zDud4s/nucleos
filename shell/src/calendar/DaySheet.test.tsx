import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import type { ReactNode } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { ApiRefusal } from "../data/client";
import { DaySheet, DraftEventForm, OccurrenceActions } from "./DaySheet";
import type { CalendarConfigView, EventOccurrence } from "../data/calendar";
import { renderWithQuery } from "../test/harness";

/**
 * Mount, and be able to re-render against the SAME cache.
 *
 * Testing Library's `rerender` replaces the whole tree with what it is given,
 * which drops the provider `renderWithQuery` wrapped around the first render —
 * "No QueryClient set". Re-wrapping keeps the component's identity, and with
 * it the state a re-render is supposed to preserve, which is the whole point
 * of the two tests that use this.
 */
function mount(ui: ReactNode) {
  const { rerender, queryClient, ...rest } = renderWithQuery(ui);
  return {
    ...rest,
    queryClient,
    rerender: (next: ReactNode) =>
      rerender(<QueryClientProvider client={queryClient}>{next}</QueryClientProvider>),
  };
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

/** The dwell `ConfirmButton` needs between arming and confirming — a real gap. */
function afterDwell(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 350));
}

const CONFIG: CalendarConfigView = {
  default_tz: "Europe/Lisbon",
  working_hours_start: "09:00",
  working_hours_end: "18:00",
  working_weekdays: ["mon", "tue", "wed", "thu", "fri"],
};

function occurrence(overrides: Partial<EventOccurrence> = {}): EventOccurrence {
  return {
    event_id: 4,
    title: "Weekly check-in",
    source: "human",
    occurrence_local: "2026-08-20T09:00:00",
    starts_at: "2026-08-20T08:00:00Z",
    ends_at: "2026-08-20T08:30:00Z",
    ...overrides,
  };
}

/** Every write the shell made, by `"<METHOD> <path>"`, with its parsed body. */
function recorder(): Record<string, unknown> {
  const seen: Record<string, unknown> = {};
  daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
    if (init?.method !== undefined && init.method !== "GET") {
      seen[`${init.method} ${path}`] = typeof init.body === "string" ? JSON.parse(init.body) : null;
    }
    if (path === "/calendar/events" && init?.method === "POST") return { id: 9 };
    return undefined;
  });
  return seen;
}

/* ------------------------------------------------------------------ moving -- */

describe("moving an occurrence", () => {
  /**
   * **The invariant the whole data model rests on, and it had no test at all.**
   *
   * `occurrence_local` is the ORIGINAL local start and is the identity an
   * exception row is keyed by (`recurrence.rs`: "This is the identity an
   * exception is keyed by, so it must survive being moved"). Address the
   * second move by where the occurrence now IS and the daemon writes a SECOND
   * exception instead of relocating the first — the series grows a duplicate
   * and the original slot comes back.
   *
   * A single move cannot catch that: the two candidate values are equal until
   * something has moved. So this moves twice, and re-renders in between with
   * the occurrence exactly as the daemon reports it afterwards — identity
   * unchanged, instants updated.
   */
  it("addresses a SECOND move by the original local start, not by where it now sits", async () => {
    const seen = recorder();
    const { rerender } = mount(<OccurrenceActions occurrence={occurrence()} />);

    fireEvent.change(screen.getByLabelText("Move Weekly check-in to"), {
      target: { value: "2026-08-20T15:00" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Move" }));

    await waitFor(() => expect(seen["POST /calendar/events/4/move"]).toBeDefined());
    expect(seen["POST /calendar/events/4/move"]).toEqual({
      occurrence_local: "2026-08-20T09:00:00",
      to_local: "2026-08-20T15:00:00",
      duration_minutes: 30,
    });

    // What `GET /calendar/events` now answers: the identity is untouched and
    // only the resolved instants have moved. 15:00 Lisbon in August is 14:00Z.
    const moved = occurrence({
      starts_at: "2026-08-20T14:00:00Z",
      ends_at: "2026-08-20T14:30:00Z",
    });
    delete seen["POST /calendar/events/4/move"];
    rerender(<OccurrenceActions occurrence={moved} />);

    fireEvent.change(screen.getByLabelText("Move Weekly check-in to"), {
      target: { value: "2026-08-21T11:00" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Move" }));

    await waitFor(() => expect(seen["POST /calendar/events/4/move"]).toBeDefined());
    expect(seen["POST /calendar/events/4/move"]).toEqual({
      // STILL the original 09:00, and not the 15:00 it currently occupies.
      occurrence_local: "2026-08-20T09:00:00",
      to_local: "2026-08-21T11:00:00",
      duration_minutes: 30,
    });
  });

  /**
   * The control is seeded from where the occurrence IS, while the request
   * addresses where it WAS. Seeding it from `occurrence_local` would have made
   * the second move default to undoing the first.
   */
  it("opens the move control at the time the occurrence currently occupies", () => {
    renderWithQuery(
      <OccurrenceActions
        occurrence={occurrence({
          occurrence_local: "2026-08-20T09:00:00",
          starts_at: "2026-08-20T14:00:00Z",
          ends_at: "2026-08-20T14:30:00Z",
        })}
      />,
    );

    expect((screen.getByLabelText("Move Weekly check-in to") as HTMLInputElement).value).toBe(
      "2026-08-20T15:00",
    );
  });

  it("says where a moved occurrence came from", () => {
    renderWithQuery(
      <OccurrenceActions
        occurrence={occurrence({
          occurrence_local: "2026-08-04T09:00:00",
          starts_at: "2026-08-20T14:00:00Z",
          ends_at: "2026-08-20T14:30:00Z",
        })}
      />,
    );

    expect(screen.getByText("moved from 2026-08-04 09:00")).toBeDefined();
  });
});

/* ------------------------------------------------------- the other writes -- */

describe("skipping and deleting", () => {
  it("makes the extra actions a word rather than a glyph", () => {
    renderWithQuery(<OccurrenceActions occurrence={occurrence()} />);

    expect(screen.getByText("Delete series", { selector: "summary" }).tagName).toBe("SUMMARY");
    expect(screen.queryByText("…")).toBeNull();
  });

  it("addresses a skip by occurrence_local — there is no occurrence id to send", async () => {
    const seen = recorder();
    renderWithQuery(<OccurrenceActions occurrence={occurrence()} />);

    // Arm, then confirm — genuinely apart in time so the second click does not
    // land inside the interlock's dwell.
    fireEvent.click(screen.getByRole("button", { name: "Skip this occurrence" }));
    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: "Skip it" }));

    await waitFor(() =>
      expect(seen["POST /calendar/events/4/cancel"]).toEqual({
        occurrence_local: "2026-08-20T09:00:00",
      }),
    );
  });

  /** The most destructive control on the page, and it had never been exercised. */
  it("deletes the whole series by id, and only behind the interlock", async () => {
    const seen = recorder();
    renderWithQuery(<OccurrenceActions occurrence={occurrence()} />);

    fireEvent.click(screen.getByRole("button", { name: "Delete whole series" }));
    // Armed but not fired: nothing has been sent yet.
    expect(seen["DELETE /calendar/events/4"]).toBeUndefined();

    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: "Delete every occurrence" }));

    await waitFor(() => expect(seen["DELETE /calendar/events/4"]).toBeDefined());
  });
});

/* ------------------------------------------------------------ the draft -- */

describe("the draft form", () => {
  it("sends no freq at all for a one-off", async () => {
    const seen = recorder();
    renderWithQuery(<DraftEventForm slot={{ day: new Date(2026, 7, 20), hour: null }} config={CONFIG} />);

    fireEvent.change(screen.getByLabelText("Title"), { target: { value: "Coffee" } });
    fireEvent.click(screen.getByRole("button", { name: "Add to calendar" }));

    await waitFor(() => expect(seen["POST /calendar/events"]).toBeDefined());
    const created = seen["POST /calendar/events"] as Record<string, unknown>;

    // Absent, never `null` — `JSON.stringify` drops the key, and the daemon
    // reads the difference.
    expect(Object.prototype.hasOwnProperty.call(created, "freq")).toBe(false);
    expect(created.title).toBe("Coffee");
  });

  /** The other half of that branch, which was never asserted: a series that repeats. */
  it("sends the chosen freq for a repeating series", async () => {
    const seen = recorder();
    renderWithQuery(<DraftEventForm slot={{ day: new Date(2026, 7, 20), hour: null }} config={CONFIG} />);

    fireEvent.change(screen.getByLabelText("Title"), { target: { value: "Standup" } });
    fireEvent.click(screen.getByRole("checkbox"));
    fireEvent.change(screen.getByLabelText("Repeat frequency"), { target: { value: "daily" } });
    fireEvent.click(screen.getByRole("button", { name: "Add to calendar" }));

    await waitFor(() => expect(seen["POST /calendar/events"]).toBeDefined());
    expect((seen["POST /calendar/events"] as Record<string, unknown>).freq).toBe("daily");
  });

  /**
   * Seeded from the selection, which is the point of moving the form next to
   * the grid: it used to be an empty control a person filled in by hand while
   * looking at a grid that already said which day they meant.
   */
  it("opens at the selected day and the working hour when no hour was named", () => {
    renderWithQuery(<DraftEventForm slot={{ day: new Date(2026, 7, 20), hour: null }} config={CONFIG} />);
    expect((screen.getByLabelText("Starts") as HTMLInputElement).value).toBe("2026-08-20T09:00");
  });

  it("opens at the clicked hour when the week named one", () => {
    renderWithQuery(<DraftEventForm slot={{ day: new Date(2026, 7, 20), hour: 14 }} config={CONFIG} />);
    expect((screen.getByLabelText("Starts") as HTMLInputElement).value).toBe("2026-08-20T14:00");
  });

  it("follows the selection when it moves, without throwing away a half-typed title", () => {
    const { rerender } = mount(
      <DraftEventForm slot={{ day: new Date(2026, 7, 20), hour: null }} config={CONFIG} />,
    );
    fireEvent.change(screen.getByLabelText("Title"), { target: { value: "Half typed" } });

    rerender(<DraftEventForm slot={{ day: new Date(2026, 7, 25), hour: 11 }} config={CONFIG} />);

    expect((screen.getByLabelText("Starts") as HTMLInputElement).value).toBe("2026-08-25T11:00");
    expect((screen.getByLabelText("Title") as HTMLInputElement).value).toBe("Half typed");
  });

  it("falls back to nine when the config has not answered yet", () => {
    renderWithQuery(<DraftEventForm slot={{ day: new Date(2026, 7, 20), hour: null }} config={undefined} />);
    expect((screen.getByLabelText("Starts") as HTMLInputElement).value).toBe("2026-08-20T09:00");
  });
});

/* ----------------------------------------------------------- the refusals -- */

describe("what the daemon refuses", () => {
  /**
   * Every refusal `POST /calendar/events` makes is bare prose written on
   * purpose, so it is quoted rather than replaced with generic copy. None of
   * these four branches had a test.
   */
  it("quotes the daemon's own sentence when it sent one", async () => {
    daemon.apiFetch.mockImplementation(async () => {
      throw new ApiRefusal(400, "bad_recurrence", "a series cannot end before it begins");
    });
    renderWithQuery(<DraftEventForm slot={{ day: new Date(2026, 7, 20), hour: null }} config={CONFIG} />);

    fireEvent.change(screen.getByLabelText("Title"), { target: { value: "Coffee" } });
    fireEvent.click(screen.getByRole("button", { name: "Add to calendar" }));

    expect(await screen.findByText(/a series cannot end before it begins/)).toBeDefined();
  });

  /** A detail that is only the code again is not prose, and must not be quoted as if it were. */
  it("says its own sentence when the daemon did not answer at all", async () => {
    daemon.apiFetch.mockImplementation(async () => {
      throw new Error("connection refused");
    });
    renderWithQuery(<DraftEventForm slot={{ day: new Date(2026, 7, 20), hour: null }} config={CONFIG} />);

    fireEvent.change(screen.getByLabelText("Title"), { target: { value: "Coffee" } });
    fireEvent.click(screen.getByRole("button", { name: "Add to calendar" }));

    expect(await screen.findByText(/nothing was added/)).toBeDefined();
  });

  it("says which write failed when a move is refused", async () => {
    daemon.apiFetch.mockImplementation(async () => {
      throw new Error("connection refused");
    });
    renderWithQuery(<OccurrenceActions occurrence={occurrence()} />);

    fireEvent.click(screen.getByRole("button", { name: "Move" }));

    expect(await screen.findByText(/not moved/)).toBeDefined();
  });
});

/* ------------------------------------------------------------- the sheet -- */

describe("the sheet itself", () => {
  it("names the day and what is unusual about it", () => {
    renderWithQuery(
      <DaySheet
        slot={{ day: new Date(2026, 9, 25), hour: null }}
        occurrences={[]}
        now={new Date(2026, 9, 20)}
        config={CONFIG}
      />,
    );

    // 25 October 2026 is Lisbon's long day AND a Sunday — two facts, said separately.
    expect(screen.getByText("25h — long day")).toBeDefined();
    expect(screen.getByText("not a working day")).toBeDefined();
    expect(screen.getByText("nothing on this day.")).toBeDefined();
  });

  it("lists the day's occurrences with their controls", () => {
    renderWithQuery(
      <DaySheet
        slot={{ day: new Date(2026, 7, 20), hour: null }}
        occurrences={[occurrence(), occurrence({ event_id: 5, title: "Retro" })]}
        now={new Date(2026, 7, 20)}
        config={CONFIG}
      />,
    );

    expect(screen.getAllByRole("button", { name: "Skip this occurrence" })).toHaveLength(2);
    expect(screen.getByText("Retro")).toBeDefined();
  });
});
