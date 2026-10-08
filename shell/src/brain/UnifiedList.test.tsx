import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { OwnerNote } from "../data/owner-notes";
import type { Known } from "../data/knowledge";
import { renderWithRouter } from "../test/harness";
import { daemonWith, known } from "./knowledge/test-helpers";
import { UnifiedList } from "./UnifiedList";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

function note(over: Partial<OwnerNote> = {}): OwnerNote {
  return {
    id: 1,
    text: "Rust owns the state\nsecond line",
    origin: "shell",
    state: "active",
    created_at: "2026-09-01T09:00:00+00:00",
    updated_at: "2026-09-01T09:00:00+00:00",
    ...over,
  };
}

function daemonHolding(notes: OwnerNote[], rows: Known[]) {
  const knowledge = daemonWith(rows);
  daemon.apiFetch.mockImplementation((path: string) => {
    if (path.startsWith("/owner-notes")) return Promise.resolve(notes);
    return knowledge(path);
  });
}

describe("UnifiedList", () => {
  it("renders notes and knowledge, and a click reports the item address", async () => {
    daemonHolding([note({ id: 1 })], [known({ id: 5, title: "Suite needs PATH" })]);
    const onSelect = vi.fn();
    await renderWithRouter(<UnifiedList onSelect={onSelect} />);

    fireEvent.click(await screen.findByRole("button", { name: /Suite needs PATH/ }));
    expect(onSelect).toHaveBeenCalledWith("knowledge:5");
    fireEvent.click(screen.getByRole("button", { name: /Rust owns the state/ }));
    expect(onSelect).toHaveBeenCalledWith("note:1");
  });

  it("shows the waiting panel for proposed rows, and quick decisions in the list under Any state", async () => {
    daemonHolding([], [
      known({ id: 4, title: "In force one" }),
      known({ id: 9, status: "proposed", proposal_id: 3, title: "Proposed thing" }),
    ]);
    await renderWithRouter(<UnifiedList onSelect={vi.fn()} />);

    expect(await screen.findByRole("heading", { level: 2, name: "Waiting for you" })).toBeTruthy();
    // The default state keeps proposed rows out of the list itself.
    await screen.findByRole("button", { name: /In force one/ });
    expect(screen.queryByRole("button", { name: "Approve Proposed thing" })).toBeNull();

    const state = screen.getByRole("group", { name: "State" });
    fireEvent.click(within(state).getByRole("button", { name: "Any state" }));
    const approvals = await screen.findAllByRole("button", { name: "Approve Proposed thing" });
    // One in the waiting panel is labelled plain "Approve"; the quick one carries the title.
    expect(approvals).toHaveLength(1);
    fireEvent.click(approvals[0] as HTMLElement);
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/proposals/3/approve", { method: "POST" }),
    );
  });

  it("the Type filter hides notes", async () => {
    daemonHolding([note({ id: 1 })], [known({ id: 5, title: "Suite needs PATH" })]);
    await renderWithRouter(<UnifiedList onSelect={vi.fn()} />);
    await screen.findByRole("button", { name: /Suite needs PATH/ });

    const type = screen.getByRole("group", { name: "Type" });
    fireEvent.click(within(type).getByRole("button", { name: "Knowledge" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: /Rust owns the state/ })).toBeNull());
    expect(screen.getByRole("button", { name: /Suite needs PATH/ })).toBeTruthy();
  });

  it("shows the Teach callout when there is nothing at all", async () => {
    daemonHolding([], []);
    await renderWithRouter(<UnifiedList onSelect={vi.fn()} />);
    expect(await screen.findByText("Nothing has been learned yet")).toBeTruthy();
  });
});
