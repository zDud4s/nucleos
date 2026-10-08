import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { IdeVerifyPanel } from "./IdeVerify";
import { createAppQueryClient } from "../app/queryClient";
import { ApiRefusal } from "../data/client";
import type { ProjectRules } from "../data/projects";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
});

function rules(overrides: Partial<ProjectRules> = {}): ProjectRules {
  return {
    project_id: "alpha",
    project_root: "C:/repos/alpha",
    rules_file: "present",
    rules_path: "~/.nucleos/projects/alpha/autopilot.yaml",
    rules_error: null,
    gate_command: null,
    gate_before_publish: false,
    judge: { state: "default" },
    schedules: [],
    repo_triggers: [],
    wip_limit: null,
    open_review_items: 0,
    queue_full: false,
    ide_verify: false,
    ...overrides,
  };
}

function mount(served: ProjectRules, stale = false) {
  return render(
    <QueryClientProvider client={createAppQueryClient()}>
      <IdeVerifyPanel projectId="alpha" rules={served} stale={stale} />
    </QueryClientProvider>,
  );
}

/** Past `ConfirmButton`'s dwell, so the second press is a decision and not a double-click. */
async function pastTheDwell(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 350));
  });
}

/** The calls the panel made to the switch route, as `[path, parsed body]`. */
function posts(): Array<[string, unknown]> {
  return daemon.apiFetch.mock.calls
    .filter(([path]) => path === "/projects/alpha/ide-verify")
    .map(([path, init]) => [path as string, JSON.parse((init as RequestInit).body as string)]);
}

describe("IdeVerifyPanel", () => {
  it("shows the switch off and posts nothing on render", async () => {
    mount(rules({ ide_verify: false }));

    expect(screen.getByRole("heading", { name: "IDE verify" })).toBeDefined();
    expect(screen.getByText("off")).toBeDefined();
    expect(screen.getByRole("button", { name: "Switch on" })).toBeDefined();
    expect(posts()).toEqual([]);
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });

  it("switching on takes the interlock and posts enabled true", async () => {
    daemon.apiFetch.mockResolvedValue({ project: "alpha", enabled: true, worktrees: [] });
    mount(rules({ ide_verify: false }));

    fireEvent.click(screen.getByRole("button", { name: "Switch on" }));
    // The first press only arms: nothing is written into any worktree yet.
    expect(posts()).toEqual([]);
    await pastTheDwell();
    fireEvent.click(
      screen.getByRole("button", { name: "Switch on — write into every IDE worktree" }),
    );

    await waitFor(() => expect(posts().length).toBe(1));
    expect(posts()[0]).toEqual(["/projects/alpha/ide-verify", { enabled: true }]);
    expect(daemon.apiFetch).toHaveBeenCalledWith(
      "/projects/alpha/ide-verify",
      expect.objectContaining({ method: "POST" }),
    );
  });

  it("switching off posts enabled false in one click", async () => {
    daemon.apiFetch.mockResolvedValue({ project: "alpha", enabled: false, worktrees: [] });
    mount(rules({ ide_verify: true }));

    expect(screen.getByText("on")).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Switch off" }));

    await waitFor(() => expect(posts().length).toBe(1));
    expect(posts()[0]).toEqual(["/projects/alpha/ide-verify", { enabled: false }]);
  });

  it("lists each worktree the reconcile reported", async () => {
    daemon.apiFetch.mockResolvedValue({
      project: "alpha",
      enabled: true,
      worktrees: [
        { path: "C:/repos/alpha-one", state: "provisioned" },
        { path: "C:/repos/alpha-two", state: "not_provisioned", reason: "no .claude directory" },
      ],
    });
    mount(rules({ ide_verify: true }));

    fireEvent.click(screen.getByRole("button", { name: "Provision again" }));

    const report = await screen.findByRole("status");
    expect(report.textContent).toContain("C:/repos/alpha-one");
    expect(report.textContent).toContain("C:/repos/alpha-two");
    expect(report.textContent).toContain("provisioned");
    expect(report.textContent).toContain("not provisioned");
    expect(report.textContent).toContain("no .claude directory");
  });

  it("names a refusal and keeps the state unchanged", async () => {
    daemon.apiFetch.mockRejectedValue(
      new ApiRefusal(403, "forbidden", "only the owner's key can switch IDE verify"),
    );
    mount(rules({ ide_verify: true }));

    fireEvent.click(screen.getByRole("button", { name: "Switch off" }));

    expect(await screen.findByText(/only the owner's key can switch IDE verify/)).toBeDefined();
    // The state line still says what the rules read said: nothing is drawn ahead of the daemon.
    expect(screen.getByText("on")).toBeDefined();
    expect(screen.queryByText("off")).toBeNull();
  });

  it("offers no control when the switch is unreported or the read is stale", () => {
    const older = rules();
    delete (older as { ide_verify?: boolean }).ide_verify;
    const first = mount(older);
    expect(screen.queryByRole("button", { name: "Switch on" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Switch off" })).toBeNull();
    first.unmount();

    mount(rules({ ide_verify: true }), true);
    expect(screen.queryByRole("button", { name: "Switch on" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Switch off" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Provision again" })).toBeNull();
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });
});
