import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));
vi.mock("../lib/vscode", () => ({ openInVscode: vi.fn(() => Promise.resolve()) }));

import { StatesView } from "./StatesView";
import { ModeMap } from "./ModeMap";
import { createAppQueryClient } from "../app/queryClient";
import { openInVscode } from "../lib/vscode";
import type { ProjectStates } from "../data/project-states";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  vi.mocked(openInVscode).mockClear();
});

const ANSWER: ProjectStates = {
  root: "C:/p",
  files_read: 12,
  machines: [
    {
      table: "vcs_requests",
      column: "status",
      states: ["queued", "running"],
      transitions: [
        {
          from: "queued",
          to: "running",
          function: "claim_next",
          path: "core/src/execution/vcs/queue.rs",
          line: 356,
        },
      ],
    },
  ],
};

function withClient(ui: React.ReactElement) {
  return render(<QueryClientProvider client={createAppQueryClient()}>{ui}</QueryClientProvider>);
}

describe("the States view", () => {
  it("draws one titled graph per machine with the function on its edge", async () => {
    daemon.apiFetch.mockResolvedValue(ANSWER);
    withClient(<StatesView projectId="alpha" />);
    expect(await screen.findByRole("heading", { name: "vcs_requests.status" })).toBeTruthy();
    expect(screen.getByRole("region", { name: "vcs_requests.status" })).toBeTruthy();
    expect(screen.getByRole("img")).toBeTruthy();
    expect(screen.getAllByText(/claim_next/).length).toBeGreaterThan(0);
  });

  it("opens VS Code at the transition's file and line", async () => {
    daemon.apiFetch.mockResolvedValue(ANSWER);
    withClient(<StatesView projectId="alpha" />);
    fireEvent.click(
      await screen.findByRole("button", { name: "core/src/execution/vcs/queue.rs:356" }),
    );
    await waitFor(() =>
      expect(openInVscode).toHaveBeenCalledWith("C:/p/core/src/execution/vcs/queue.rs", 356),
    );
  });

  it("says so when the project has no state machine", async () => {
    daemon.apiFetch.mockResolvedValue({ root: "C:/p", files_read: 3, machines: [] });
    withClient(<StatesView projectId="alpha" />);
    expect(await screen.findByText(/No state machine found/)).toBeTruthy();
  });

  it("opens from the map's States door even while the map is unanswered", async () => {
    daemon.apiFetch.mockImplementation((path: string) => {
      if (path === "/projects/alpha/map/states") return Promise.resolve(ANSWER);
      if (path === "/projects/alpha/map") return new Promise(() => {});
      return Promise.resolve([]);
    });
    withClient(<ModeMap projectId="alpha" />);
    fireEvent.click(await screen.findByRole("button", { name: /^States/ }));
    expect(await screen.findByRole("heading", { name: "vcs_requests.status" })).toBeTruthy();
    expect(daemon.apiFetch).toHaveBeenCalledWith("/projects/alpha/map/states");
  });
});
