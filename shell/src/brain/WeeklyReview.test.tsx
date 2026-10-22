import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import type { OwnerNote, WeeklyReview as Review } from "../data/owner-notes";
import { renderWithQuery } from "../test/harness";
import { WeeklyReview, weekRange } from "./WeeklyReview";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

function note(id: number, text: string): OwnerNote {
  return {
    id,
    text,
    origin: "shell",
    state: "active",
    created_at: "2026-10-20T09:00:00+00:00",
    updated_at: "2026-10-20T09:00:00+00:00",
  };
}

// Local midnight stamps, so the local-time range reads the same in every timezone.
function local(y: number, m: number, d: number): string {
  return new Date(y, m - 1, d, 0, 0, 0).toISOString();
}

function review(over: Partial<Review> = {}): Review {
  return {
    week_start: local(2026, 10, 19),
    week_end: local(2026, 10, 26),
    created: [note(1, "First thought\nsecond line"), note(2, "Second thought")],
    unlinked: [note(3, "Lonely note")],
    unlinked_total: 3,
    resurfaced: [note(4, "Old idea\nwith detail")],
    waiting_proposals: 1,
    ...over,
  };
}

function serve(data: Review) {
  daemon.apiFetch.mockImplementation((path: string) =>
    path === "/owner-notes/review" ? Promise.resolve(data) : Promise.reject(new Error(path)),
  );
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("weekRange", () => {
  it("formats the week with an exclusive end as its last day", () => {
    expect(weekRange(local(2026, 10, 19), local(2026, 10, 26))).toBe("Oct 19 - Oct 25");
  });
});

describe("WeeklyReview", () => {
  it("is collapsed to a one-line summary by default", async () => {
    serve(review());
    renderWithQuery(<WeeklyReview onItem={() => {}} />);
    expect(await screen.findByText("This week: 2 written, 3 unlinked, 1 to revisit, 1 proposal waiting")).toBeTruthy();
    expect(screen.getByText("Oct 19 - Oct 25")).toBeTruthy();
    const toggle = screen.getByRole("button", { name: "Show" });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByText("Written")).toBeNull();
  });

  it("opens into its sections and opens a note by its item ref", async () => {
    serve(review());
    const onItem = vi.fn();
    renderWithQuery(<WeeklyReview onItem={onItem} />);
    fireEvent.click(await screen.findByRole("button", { name: "Show" }));

    expect(screen.getByRole("button", { name: "Hide" }).getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByText("Written")).toBeTruthy();
    expect(screen.getByText("Unlinked")).toBeTruthy();
    expect(screen.getByText("Resurfaced")).toBeTruthy();
    // Rows show the plain first line only.
    expect(screen.getByRole("button", { name: "First thought" })).toBeTruthy();
    expect(screen.queryByText(/second line/)).toBeNull();
    expect(screen.getByText("showing 1 of 3")).toBeTruthy();
    expect(screen.getByText("1 proposal waiting")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Old idea" }));
    expect(onItem).toHaveBeenCalledWith("note:4");
  });

  it("omits the showing line when every unlinked note is listed", async () => {
    serve(review({ unlinked_total: 1, waiting_proposals: 2 }));
    renderWithQuery(<WeeklyReview onItem={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "Show" }));
    expect(screen.queryByText(/showing/)).toBeNull();
    expect(screen.getByText("2 proposals waiting")).toBeTruthy();
  });

  it("says so in a quiet line when a section is empty", async () => {
    serve(review({ created: [], unlinked: [], unlinked_total: 0, resurfaced: [] }));
    renderWithQuery(<WeeklyReview onItem={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "Show" }));
    expect(screen.getByText("Nothing written this week.")).toBeTruthy();
    expect(screen.getByText("Every note is linked.")).toBeTruthy();
    expect(screen.getByText("Nothing old to revisit.")).toBeTruthy();
  });

  it("shows a skeleton while loading and an error note on failure", async () => {
    daemon.apiFetch.mockReturnValue(new Promise(() => {}));
    const first = renderWithQuery(<WeeklyReview onItem={() => {}} />);
    expect(await screen.findByText("Loading this week")).toBeTruthy();
    first.unmount();

    daemon.apiFetch.mockRejectedValue(new Error("down"));
    renderWithQuery(<WeeklyReview onItem={() => {}} />);
    await waitFor(() => expect(screen.getByRole("alert")).toBeTruthy());
  });
});
