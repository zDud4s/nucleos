import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Contacts } from "./Contacts";
import type { Correspondent, MergeSuggestion } from "../data/contacts";
import { renderWithQuery } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
});

/* ------------------------------------------------------------- fixtures -- */

function correspondent(overrides: Partial<Correspondent> = {}): Correspondent {
  return {
    address: "ana@example.com",
    contact_id: 1,
    linked_by: "implicit",
    display_name: "Ana",
    messages_in: 4,
    outbound_ever: 0,
    first_seen: "2026-08-01T09:00:00Z",
    last_seen: "2026-08-17T09:00:00Z",
    verdict: null,
    ...overrides,
  };
}

function merge(overrides: Partial<MergeSuggestion> = {}): MergeSuggestion {
  return {
    proposal_id: 21,
    reasoning: "the same display name against two addresses",
    created_at: "2026-08-17T09:00:00Z",
    keep: {
      contact_id: 1,
      addresses: ["ana@example.com"],
      display_name: "Ana",
      messages_in: 12,
      verdict: null,
    },
    absorb: {
      contact_id: 2,
      addresses: ["ana.silva@example.com"],
      display_name: "Ana Silva",
      messages_in: 3,
      verdict: null,
    },
    ...overrides,
  };
}

/** The dwell `ConfirmButton` needs between arming and confirming — a real gap. */
function afterDwell(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 350));
}

/* --------------------------------------------------------------- roster -- */

describe("Contacts — the roster", () => {
  it("renders empty panels as one line under their headings", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/contacts" || path === "/contacts/merges") return [];
      return undefined;
    });

    renderWithQuery(<Contacts />);

    for (const name of ["Identity questions", "People"]) {
      const section = await screen.findByRole("region", { name });
      expect(section.querySelectorAll(".ui-quiet")).toHaveLength(1);
      expect(section.querySelector(".ui-panel")).toBeNull();
    }
  });

  it("shows one row per address and offers unmerge only for a human link", async () => {
    const rows = [
      // Merged with the next row (shared contact_id 1) — this address's OWN
      // link is the heuristic's own guess, not a person's decision.
      correspondent({ address: "ana@example.com", contact_id: 1, linked_by: "implicit" }),
      // The merge partner — a person explicitly attached this address.
      correspondent({ address: "ana.silva@example.com", contact_id: 1, linked_by: "human", display_name: "Ana Silva" }),
      // Unrelated, unmerged, and not human-linked either.
      correspondent({ address: "bob@example.com", contact_id: 3, linked_by: "implicit", display_name: "Bob" }),
    ];
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/contacts") return rows;
      if (path === "/contacts/merges") return [];
      return undefined;
    });

    renderWithQuery(<Contacts />);

    // One row per address — all three, not folded into two "contacts".
    expect(await screen.findByText("ana@example.com")).toBeDefined();
    expect(screen.getByText("ana.silva@example.com")).toBeDefined();
    expect(screen.getByText("bob@example.com")).toBeDefined();

    // The gate is `linked_by === "human"` alone — the daemon does not check it
    // itself, so exactly one row offers the button, and it is the human one.
    const buttons = screen.getAllByRole("button", { name: "Not the same person" });
    expect(buttons).toHaveLength(1);

    const humanRow = screen.getByText("ana.silva@example.com").closest("li");
    expect(humanRow).not.toBeNull();
    expect(within(humanRow as HTMLElement).getByRole("button", { name: "Not the same person" })).toBeDefined();

    const implicitRow = screen.getByText("ana@example.com").closest("li");
    const otherRow = screen.getByText("bob@example.com").closest("li");
    expect(within(implicitRow as HTMLElement).queryByRole("button", { name: "Not the same person" })).toBeNull();
    expect(within(otherRow as HTMLElement).queryByRole("button", { name: "Not the same person" })).toBeNull();
  });
});

describe("Contacts — finding people", () => {
  it("narrows the roster by search text and by verdict filter", async () => {
    const rows = [
      correspondent({ address: "ana@example.com", contact_id: 1, display_name: "Ana", verdict: "pin" }),
      correspondent({ address: "bob@example.com", contact_id: 3, display_name: "Bob" }),
    ];
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/contacts") return rows;
      if (path === "/contacts/merges") return [];
      return undefined;
    });

    renderWithQuery(<Contacts />);
    await screen.findByText("bob@example.com");

    fireEvent.change(screen.getByRole("searchbox", { name: "Search people" }), { target: { value: "bob" } });
    expect(screen.queryByText("ana@example.com")).toBeNull();
    expect(screen.getByText("bob@example.com")).toBeDefined();

    fireEvent.change(screen.getByRole("searchbox", { name: "Search people" }), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: /^Urgent/ }));
    expect(screen.queryByText("bob@example.com")).toBeNull();
    expect(screen.getByText("ana@example.com")).toBeDefined();
  });
});

describe("Contacts — verdict undo", () => {
  it("offers Undo after Mute and sends the previous verdict back", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/contacts") return [correspondent({ verdict: "pin" })];
      if (path === "/contacts/merges") return [];
      return undefined;
    });

    renderWithQuery(<Contacts />);
    fireEvent.click(await screen.findByRole("button", { name: "Mute" }));

    expect(await screen.findByText("Ana marked noise")).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));

    await vi.waitFor(() => {
      const sent = daemon.apiFetch.mock.calls.filter(([path]) => String(path).includes("verdict"));
      expect(sent).toHaveLength(2);
    });
  });
});

/* ---------------------------------------------------- identity questions -- */

describe("Contacts — identity questions", () => {
  it("names the pair by display name in the question's head", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/contacts") return [];
      if (path === "/contacts/merges") return [merge({ proposal_id: 21 })];
      return undefined;
    });

    renderWithQuery(<Contacts />);

    // The head asks the question the two buttons answer (display names, else the first
    // address) and no longer carries the proposal number.
    expect(await screen.findByText("Are Ana and Ana Silva one person?")).toBeDefined();
    expect(screen.queryByText("question #21")).toBeNull();
  });

  it("shows a predicted conflict before the button is pressed, and the button cannot send it", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/contacts") return [];
      if (path === "/contacts/merges") {
        // Two standing decisions that disagree: the daemon would answer 409 on approve.
        return [
          merge({
            proposal_id: 21,
            keep: { ...merge().keep, verdict: "pin" },
            absorb: { ...merge().absorb, verdict: "mute" },
          }),
        ];
      }
      return undefined;
    });

    renderWithQuery(<Contacts />);

    const note = await screen.findByText(/standing decisions that disagree/);
    const approve = screen.getByRole("button", { name: "Yes, one person" });

    // The conflict is said up front, and the approve control is unavailable (aria-disabled, still
    // focusable) and described by that note.
    expect(approve.getAttribute("aria-disabled")).toBe("true");
    expect(note.parentElement?.id).not.toBe("");
    expect(approve.getAttribute("aria-describedby")).toBe(note.parentElement?.id);

    // Pressing it, by pointer and by a bare activation, arms nothing and sends nothing — so the
    // 409 prose can no longer be reached from this screen.
    fireEvent.pointerDown(approve, { button: 0, isPrimary: true, pointerId: 1 });
    fireEvent.pointerUp(approve, { button: 0, isPrimary: true, pointerId: 1 });
    fireEvent.click(approve);
    await afterDwell();
    fireEvent.click(approve);
    await afterDwell();

    expect(screen.queryByRole("button", { name: "They are one person" })).toBeNull();
    expect(
      daemon.apiFetch.mock.calls.filter(([path]) => String(path).startsWith("/proposals/")),
    ).toHaveLength(0);
  });

  it("leaves the approve control available when the two sides do not disagree", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/contacts") return [];
      if (path === "/contacts/merges") return [merge({ proposal_id: 21 })];
      return undefined;
    });

    renderWithQuery(<Contacts />);

    const approve = await screen.findByRole("button", { name: "Yes, one person" });
    expect(approve.getAttribute("aria-disabled")).toBeNull();
    expect(approve.getAttribute("aria-describedby")).toBeNull();
    expect(screen.queryByText(/standing decisions that disagree/)).toBeNull();
  });
});
