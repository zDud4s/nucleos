import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { ApiRefusal } from "../../data/client";
import { tool, toolsDaemon } from "./tools-test-helpers";
import { ScopedTools } from "./ScopedTools";
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

describe("ScopedTools - own tools", () => {
  it("lists only the active tools of its owner with their origin, and counts the waiting ones", async () => {
    daemon.apiFetch.mockImplementation(
      toolsDaemon([
        tool({ id: 1, tool: "web_read", source: "request", run_id: "900001" }),
        tool({ id: 2, tool: "email_queue", source: "owner", run_id: null, reason: null }),
        tool({ id: 3, tool: "web_search", owner_id: "someone-else" }),
        tool({ id: 4, tool: "team_note", owner_kind: "team", owner_id: "scout" }),
        tool({ id: 5, tool: "team_report", status: "proposed" }),
        tool({ id: 6, tool: "team_action", status: "revoked" }),
      ]),
    );

    await renderWithRouter(<ScopedTools ownerKind="agent" ownerId="scout" />);

    const region = await screen.findByRole("region", { name: "Tools" });
    expect(await screen.findByText("web_read")).toBeDefined();
    expect(region.textContent).toContain("email_queue");
    expect(region.textContent).toContain("requested by run 900001");
    expect(region.textContent).toContain("added by you");
    expect(region.textContent).not.toContain("web_search");
    expect(region.textContent).not.toContain("team_note");
    expect(region.textContent).not.toContain("team_report");
    expect(region.textContent).not.toContain("team_action");
    expect(await screen.findByText("1 more waiting for you in the Brain")).toBeDefined();
    expect(screen.getAllByRole("button", { name: /^Revoke / })).toHaveLength(2);
  });

  it("asks the daemon for this owner's rows only", async () => {
    daemon.apiFetch.mockImplementation(toolsDaemon([]));

    await renderWithRouter(<ScopedTools ownerKind="team" ownerId="crew" />);

    await screen.findByText("No tool beyond the base has been approved here yet.");
    const paths = daemon.apiFetch.mock.calls.map((call) => call[0] as string);
    expect(paths).toContain("/loadout/tools?status=active&owner_kind=team&owner_id=crew");
  });

  it("revoke posts to the revoke route of that row", async () => {
    daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) =>
      init?.method === "POST" ? Promise.resolve(tool({ id: 2, status: "revoked" })) : toolsDaemon([tool({ id: 2 })])(path),
    );

    await renderWithRouter(<ScopedTools ownerKind="agent" ownerId="scout" />);
    fireEvent.click(await screen.findByRole("button", { name: "Revoke web_read" }));

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith(
        "/loadout/tools/2/revoke",
        expect.objectContaining({ method: "POST" }),
      ),
    );
  });

  it("a refused revoke says so", async () => {
    daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) =>
      init?.method === "POST"
        ? Promise.reject(new ApiRefusal(409, "conflict", ""))
        : toolsDaemon([tool({ id: 2 })])(path),
    );

    await renderWithRouter(<ScopedTools ownerKind="agent" ownerId="scout" />);
    fireEvent.click(await screen.findByRole("button", { name: "Revoke web_read" }));

    expect(await screen.findByText(/already decided|no longer active/i)).toBeDefined();
  });

  it("says the núcleo did not answer rather than showing an empty list", async () => {
    daemon.apiFetch.mockRejectedValue(new Error("down"));

    await renderWithRouter(<ScopedTools ownerKind="agent" ownerId="scout" />);

    expect(await screen.findByText(/did not answer/)).toBeDefined();
  });
});
