import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import {
  RouterProvider,
  createMemoryHistory,
  createRootRoute,
  createRouter,
} from "@tanstack/react-router";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { OnItsOwn } from "./OnItsOwn";
import { createAppQueryClient } from "../app/queryClient";
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
    ...overrides,
  };
}

/**
 * The component on its own, under a router that knows nothing about the inspector.
 *
 * That is the whole claim this file tests: the view is going to be mounted in the project
 * workspace, which passes a project id and nothing else. If it only worked beneath the inspector —
 * because the page read the rules for it, or threaded a prop through — this is where that shows.
 */
async function mount(served: ProjectRules) {
  daemon.apiFetch.mockImplementation(async (path: string) => {
    if (path === "/projects/alpha/rules") return served;
    if (path === "/assistant/models") return { choices: [], configured: null, efforts: [] };
    return undefined;
  });
  const rootRoute = createRootRoute({ component: () => <OnItsOwn projectId="alpha" /> });
  const router = createRouter({
    routeTree: rootRoute,
    history: createMemoryHistory({ initialEntries: ["/"] }),
    defaultPreload: false,
  });
  await router.load();
  return render(
    <QueryClientProvider client={createAppQueryClient()}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

describe("OnItsOwn", () => {
  it("reads the rules itself from nothing but a project id", async () => {
    await mount(rules());

    // Named where the daemon says it is, and not in the project's `.ai/`, where it no longer lives.
    expect(await screen.findByText("~/.nucleos/projects/alpha/autopilot.yaml")).toBeDefined();
    expect(await screen.findByRole("heading", { name: "Judge" })).toBeDefined();
    expect(screen.getByRole("heading", { name: "Work-in-progress ceiling" })).toBeDefined();
    expect(daemon.apiFetch).toHaveBeenCalledWith("/projects/alpha/rules");
  });

  /* Every finding it shows is put right in one file, and the file's editor is in the workspace.
     The unparseable-file alert used to leave the person to find that on their own. */
  it("leads an unreadable rules file to its editor", async () => {
    await mount(rules({ rules_file: "unreadable", rules_error: "unknown field `schedule`" }));

    const alert = await screen.findByRole("alert");
    const edit = within(alert).getByRole("link", { name: /Edit ~\/\.nucleos\/projects\/alpha\/autopilot\.yaml/ });
    expect(edit.getAttribute("href")).toBe("/projects/alpha/state");
  });

  /* One line, not a lesson: `Teach` is for when the emptiness is the screen, and this panel has
     three more below it. The reasoning is behind "why?", the way to fill it stays in sight. */
  it("says nothing runs here in one line, with the way to add a rule", async () => {
    await mount(rules());

    expect(await screen.findByText("Nothing starts work here by itself.")).toBeDefined();
    expect(screen.queryByText(/only ever does what somebody asks it to/)).toBeNull();
    expect(
      screen.getByRole("link", { name: "Add a schedule or a trigger" }).getAttribute("href"),
    ).toBe("/projects/alpha/state");
  });

  /* The gate's contradiction is an error box with the key in it and the way to set one — not a
     sentence set entirely in mono, which claimed the daemon wrote it. */
  it("puts the way to set a gate inside the alert that says one is missing", async () => {
    await mount(rules({ gate_before_publish: true, gate_command: null }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/gate_before_publish is on and no gate command is set/);
    expect(within(alert).getByText("gate_before_publish").tagName).toBe("CODE");
    expect(within(alert).getByRole("link", { name: "Set a gate command" })).toBeDefined();
  });
});
