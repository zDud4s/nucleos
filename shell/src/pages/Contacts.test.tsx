import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Contacts } from "./Contacts";
import { ApiRefusal } from "../data/client";
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

/* ---------------------------------------------------- identity questions -- */

describe("Contacts — identity questions", () => {
  it("surfaces the conflicting-verdict refusal prose when a merge is approved", async () => {
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/contacts") return [];
      if (path === "/contacts/merges") return [merge({ proposal_id: 21 })];
      if (path === "/proposals/21/approve" && init?.method === "POST") {
        // The daemon's own bare prose — worth quoting verbatim, and long
        // enough (well over four words) to clear the shell's floor.
        throw new ApiRefusal(
          409,
          "conflict",
          "these two people carry standing decisions that disagree — Ana against Ana Silva; settle one of them and decide this again",
        );
      }
      return undefined;
    });

    renderWithQuery(<Contacts />);

    expect(await screen.findByText("question #21")).toBeDefined();

    // Arm, then confirm — two separate gestures, genuinely apart in time so
    // the second click does not land inside the interlock's dwell.
    fireEvent.click(screen.getByRole("button", { name: "Yes, one person" }));
    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: "They are one person" }));

    expect(
      await screen.findByText(
        "these two people carry standing decisions that disagree — Ana against Ana Silva; settle one of them and decide this again",
      ),
    ).toBeDefined();
  });
});
