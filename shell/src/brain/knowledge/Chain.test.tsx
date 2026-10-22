import { beforeEach, describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import { Chain } from "./Chain";
import { daemonWith, known } from "./test-helpers";
import { renderWithRouter } from "../../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../../data/client", async (original) => ({
  ...(await original<typeof import("../../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("Chain", () => {
  it("says a row that replaced nothing replaced nothing", async () => {
    daemon.apiFetch.mockImplementation(daemonWith([known({ id: 4 })]));

    await renderWithRouter(<Chain id={4} />);

    expect(
      await screen.findByText("This one replaced nothing and nothing has replaced it."),
    ).toBeDefined();
  });

  it("lists what the row replaced", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([known({ id: 4 })], { replaced: [known({ id: 3, title: "the older text" })] }),
    );

    await renderWithRouter(<Chain id={4} />);

    expect(await screen.findByText("the older text")).toBeDefined();
  });

  describe("quoted_owner_text reason line", () => {
    const REASON = "Proposed, not recorded: it quoted your own text, so it waits for you to decide.";
    const event = (note: string, status: "proposed" | "active") => ({
      id: 1,
      from_status: status,
      to_status: status,
      note,
      at: "2026-08-19T09:00:00+00:00",
    });

    it("explains a proposed row that quoted the owner's text", async () => {
      const row = known({ id: 4, status: "proposed" });
      daemon.apiFetch.mockImplementation(
        daemonWith([row], { events: [event("quoted_owner_text", "proposed")] }),
      );

      await renderWithRouter(<Chain id={4} />);

      expect(await screen.findByText(REASON)).toBeDefined();
      // The raw events list still prints the note.
      expect(screen.getByText("quoted_owner_text")).toBeDefined();
    });

    it("stays silent for a proposed row with another note", async () => {
      const row = known({ id: 4, status: "proposed" });
      daemon.apiFetch.mockImplementation(
        daemonWith([row], { events: [event("near_duplicate", "proposed")] }),
      );

      await renderWithRouter(<Chain id={4} />);

      expect(await screen.findByText("near_duplicate")).toBeDefined();
      expect(screen.queryByText(REASON)).toBeNull();
    });

    it("stays silent once the row is no longer proposed", async () => {
      const row = known({ id: 4, status: "active" });
      daemon.apiFetch.mockImplementation(
        daemonWith([row], { events: [event("quoted_owner_text", "active")] }),
      );

      await renderWithRouter(<Chain id={4} />);

      expect(await screen.findByText("quoted_owner_text")).toBeDefined();
      expect(screen.queryByText(REASON)).toBeNull();
    });
  });
});
