import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../remote/nativeTransport", () => ({
  probeMachine: vi.fn(async () => "ok"),
  nativeTransport: vi.fn(),
}));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Runs } from "./Runs";
import { daemonFetch, daemonState, project, renderWithRouter } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  localStorage.clear();
});

describe("Runs - on another machine", () => {
  it("a_run_started_elsewhere_says_where_it_runs", async () => {
    localStorage.setItem(
      "nucleos-machines",
      JSON.stringify([{ id: "tower", name: "Tower", origin: "https://tower.example" }]),
    );
    localStorage.setItem("nucleos-default-machine", "tower");
    const shared = daemonFetch(
      daemonState({ projects: [project({ project_id: "alpha", remote: "git@host:team/app.git" })] }),
    );
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit, machine?: string) => {
      // The tower holds the same repository, by another spelling: it is offered for the project.
      if (path === "/projects" && machine === "tower") return [{ remote: "https://host/team/app" }];
      if (path === "/runs" && init?.method === "POST") return { id: 91 };
      if (path.startsWith("/runs?")) return [];
      if (path === "/presets") return [];
      return await shared(path, init);
    });

    await renderWithRouter(<Runs />, { initialPath: "/runs" });
    const buttons = await screen.findAllByRole("button", { name: "New run" });
    fireEvent.click(buttons.find((button) => button.hasAttribute("aria-haspopup")) as HTMLElement);
    fireEvent.change(await screen.findByLabelText("Project for this run"), { target: { value: "alpha" } });
    // Before a project is chosen the tower is not offered (no remote to match); now it is.
    await screen.findByRole("option", { name: /Tower/ });
    fireEvent.change(screen.getByLabelText("Where it runs"), { target: { value: "tower" } });
    fireEvent.change(await screen.findByLabelText("Prompt"), { target: { value: "tidy the imports" } });
    fireEvent.click(screen.getByRole("button", { name: "Start run" }));

    expect(
      await screen.findByText("Started on Tower. Open NucleOS on that machine to follow it."),
    ).toBeTruthy();
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/runs", expect.objectContaining({ method: "POST" }), "tower"),
    );
    // No navigation to a local page that does not exist.
    expect(window.location.pathname).not.toMatch(/\/runs\/91/);
  });
});
