import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ResolvePanel } from "./AutopilotResolve";
import { RESOLVE_DATA_WARNING } from "../data/autopilot";
import { renderWithRouter } from "../test/harness";

const waitPastTheDwell = () => new Promise((resolve) => setTimeout(resolve, 350));

beforeEach(() => daemon.apiFetch.mockReset());

describe("the resolver's switch", () => {
  it("says what leaves the machine before it can be turned on, and turns observe on", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) =>
      path.startsWith("/autopilot/judge-resolve?")
        ? { project_id: "alpha", judge_resolve: "off" }
        : { project_id: "alpha", judge_resolve: "observe" },
    );
    renderWithRouter(<ResolvePanel projectId="alpha" />, { initialPath: "/autopilot" });

    expect(await screen.findByText(RESOLVE_DATA_WARNING)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Observe how blocks would be resolved" }));
    await waitPastTheDwell();
    fireEvent.click(screen.getByRole("button", { name: /Send blocked commands and gate output to TypeSafe/ }));

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/autopilot/judge-resolve", {
        method: "POST",
        body: JSON.stringify({ project_id: "alpha", judge_resolve: "observe" }),
      }),
    );
  });

  it("asks for a project before it offers anything, and never asks the núcleo about none", () => {
    renderWithRouter(<ResolvePanel projectId={null} />, { initialPath: "/autopilot" });
    expect(screen.getByText("choose a project above.")).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });
});
