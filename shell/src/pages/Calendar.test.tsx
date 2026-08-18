import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { CalendarGrid, DraftEventForm, OccurrenceActions } from "./Calendar";
import type { EventOccurrence } from "../data/calendar";
import { renderWithQuery } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
});

/** The dwell `ConfirmButton` needs between arming and confirming — a real gap. */
function afterDwell(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 350));
}

/* --------------------------------------------------------------- the grid -- */

describe("Calendar — the month grid", () => {
  // `dayHours` reads a real local day span, which only differs from 24 on the
  // two real transition days of a real zone's year — so this suite pins the
  // process to one, through vitest's own env stub rather than `process.env`
  // (this project's `lib` carries no Node types), and only for its own
  // duration.
  beforeAll(() => {
    // Lisbon, 29 March 2026: clocks jump 01:00 WET to 02:00 WEST, so the
    // local calendar day is 23 hours, not 24.
    vi.stubEnv("TZ", "Europe/Lisbon");
  });

  afterAll(() => {
    vi.unstubAllEnvs();
  });

  it("draws expanded occurrences in the month grid with a short-day badge on a DST day", () => {
    const occurrence: EventOccurrence = {
      event_id: 1,
      title: "Spring sync",
      source: "human",
      // The daemon's own local wall clock — this is what places the box, not
      // `starts_at`.
      occurrence_local: "2026-03-29T10:00:00",
      starts_at: "2026-03-29T09:00:00Z",
      ends_at: "2026-03-29T09:30:00Z",
    };

    render(<CalendarGrid anchor={new Date(2026, 2, 1)} occurrences={[occurrence]} now={new Date(2026, 2, 15)} />);

    // The occurrence really expanded into its own day cell.
    expect(screen.getByText("Spring sync")).toBeDefined();

    // And only the short day itself carries the badge.
    const cells = screen.getAllByRole("gridcell");
    const shortCell = cells.find((cell) => within(cell).queryByText(/short day/) !== null);
    expect(shortCell).toBeDefined();
    expect(within(shortCell as HTMLElement).getByText("29")).toBeDefined();
    expect(within(shortCell as HTMLElement).getByText("Spring sync")).toBeDefined();

    // No other cell in this month claims the badge — Lisbon has one spring
    // transition, not several.
    const otherBadged = cells.filter((cell) => cell !== shortCell && within(cell).queryByText(/short day|long day/) !== null);
    expect(otherBadged).toEqual([]);
  });
});

/* --------------------------------------------------------- writing to it -- */

describe("Calendar — writing to the daemon", () => {
  it("sends no freq for a one-off draft and addresses a skip by occurrence_local", async () => {
    const seen: Record<string, unknown> = {};
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (init?.method !== undefined && init.method !== "GET" && typeof init.body === "string") {
        seen[`${init.method} ${path}`] = JSON.parse(init.body);
      }
      if (path === "/calendar/events" && init?.method === "POST") return { id: 9 };
      return undefined;
    });

    renderWithQuery(<DraftEventForm />);

    fireEvent.change(screen.getByLabelText("Title"), { target: { value: "Coffee" } });
    fireEvent.change(screen.getByLabelText("Starts"), { target: { value: "2026-08-20T09:00" } });
    fireEvent.click(screen.getByRole("button", { name: "Add to calendar" }));

    await waitFor(() => expect(seen["POST /calendar/events"]).toBeDefined());
    const created = seen["POST /calendar/events"] as Record<string, unknown>;

    // A one-off omits the key entirely — never sends it as `null`.
    expect(Object.prototype.hasOwnProperty.call(created, "freq")).toBe(false);
    expect(created.title).toBe("Coffee");
    expect(created.starts_at_local).toBe("2026-08-20T09:00:00");

    const occurrence: EventOccurrence = {
      event_id: 4,
      title: "Weekly check-in",
      source: "human",
      occurrence_local: "2026-08-20T09:00:00",
      starts_at: "2026-08-20T08:00:00Z",
      ends_at: "2026-08-20T08:30:00Z",
    };
    renderWithQuery(<OccurrenceActions occurrence={occurrence} />);

    // Arm, then confirm — genuinely apart in time so the second click does
    // not land inside the interlock's dwell.
    fireEvent.click(screen.getByRole("button", { name: "Skip this occurrence" }));
    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: "Skip it" }));

    // Addressed by `occurrence_local` — there is no occurrence id to send.
    await waitFor(() =>
      expect(seen["POST /calendar/events/4/cancel"]).toEqual({ occurrence_local: "2026-08-20T09:00:00" }),
    );
  });
});
