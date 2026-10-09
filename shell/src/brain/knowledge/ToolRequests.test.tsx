import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { ApiRefusal } from "../../data/client";
import { tool, toolsDaemon } from "./tools-test-helpers";
import { ToolRequests } from "./ToolRequests";
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

const proposed = (over = {}) =>
  tool({ id: 11, status: "proposed", decided_at: null, tool: "web_read", ...over });

describe("ToolRequests", () => {
  it("draws nothing when no tool is waiting", async () => {
    daemon.apiFetch.mockImplementation(toolsDaemon([tool({ status: "active" })]));

    const { container } = await renderWithRouter(<ToolRequests />);
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalled());

    expect(container.querySelector("section")).toBeNull();
  });

  it("lists each proposed request with who asked, the tool and the reason", async () => {
    daemon.apiFetch.mockImplementation(
      toolsDaemon([
        proposed({ id: 11, owner_id: "scout", tool: "web_read", reason: "read the vendor page" }),
        proposed({ id: 12, owner_id: "writer", tool: "email_queue", reason: null }),
        tool({ id: 13, tool: "web_search", status: "active" }),
      ]),
    );

    await renderWithRouter(<ToolRequests />);

    const panel = (await screen.findByRole("heading", { level: 2, name: "Tools to approve" })).closest(
      "section",
    ) as HTMLElement;
    expect(panel.textContent).toContain("scout");
    expect(panel.textContent).toContain("web_read");
    expect(panel.textContent).toContain("read the vendor page");
    expect(panel.textContent).toContain("writer");
    expect(panel.textContent).toContain("email_queue");
    expect(panel.textContent).not.toContain("web_search");
  });

  it("approve defaults to the agent and sends an empty body", async () => {
    daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) =>
      init?.method === "POST" ? Promise.resolve(tool({ id: 11 })) : toolsDaemon([proposed()], ["crew"])(path),
    );

    await renderWithRouter(<ToolRequests />);
    fireEvent.click(await screen.findByRole("button", { name: "Approve web_read" }));

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/loadout/tools/11/approve", {
        method: "POST",
        body: JSON.stringify({}),
      }),
    );
  });

  it("approve for a team re-targets the owner", async () => {
    daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) =>
      init?.method === "POST"
        ? Promise.resolve(tool({ id: 11, owner_kind: "team", owner_id: "crew" }))
        : toolsDaemon([proposed()], ["crew", "ops"])(path),
    );

    await renderWithRouter(<ToolRequests />);
    const choice = await screen.findByLabelText("Approve web_read for");
    await screen.findByRole("option", { name: "team crew" });
    fireEvent.change(choice, { target: { value: "crew" } });
    fireEvent.click(screen.getByRole("button", { name: "Approve web_read" }));

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/loadout/tools/11/approve", {
        method: "POST",
        body: JSON.stringify({ owner: "team", team_id: "crew" }),
      }),
    );
  });

  it("refuse posts to the reject route", async () => {
    daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) =>
      init?.method === "POST" ? Promise.resolve(tool({ id: 11 })) : toolsDaemon([proposed()])(path),
    );

    await renderWithRouter(<ToolRequests />);
    fireEvent.click(await screen.findByRole("button", { name: "Refuse web_read" }));

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith(
        "/loadout/tools/11/reject",
        expect.objectContaining({ method: "POST" }),
      ),
    );
  });

  it("a decision somebody else already made is reported, not swallowed", async () => {
    daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) =>
      init?.method === "POST"
        ? Promise.reject(new ApiRefusal(409, "conflict", ""))
        : toolsDaemon([proposed()])(path),
    );

    await renderWithRouter(<ToolRequests />);
    fireEvent.click(await screen.findByRole("button", { name: "Approve web_read" }));

    expect(await screen.findByText(/already decided|no longer active/i)).toBeDefined();
  });
});
