import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
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

import { PostMergePanel } from "./PostMerge";
import { OnItsOwn } from "./OnItsOwn";
import { createAppQueryClient } from "../app/queryClient";
import type { PostgateState, ProjectRules } from "../data/projects";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
});

const RED_SHA = "1111111111111111111111111111111111111111";
const SINCE_SHA = "2222222222222222222222222222222222222222";
const BASE_SHA = "3333333333333333333333333333333333333333";
const CULPRIT_SHA = "4444444444444444444444444444444444444444";
const GREEN_SHA = "5555555555555555555555555555555555555555";

function postgate(overrides: Partial<PostgateState> = {}): PostgateState {
  return {
    target: "main",
    last_green: null,
    running: null,
    red_groups: [],
    red_since: null,
    red_sha: null,
    red_base: null,
    phase: null,
    culprit: null,
    candidates: [],
    also_suspect: [],
    ...overrides,
  };
}

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
    postgate: null,
    ...overrides,
  };
}

describe("PostMergePanel", () => {
  it("draws nothing when the núcleo reports no post-merge state", () => {
    const { container } = render(<PostMergePanel rules={rules({ postgate: null })} />);
    expect(container.textContent).toBe("");

    // An older daemon omits the field altogether.
    const older = rules();
    delete older.postgate;
    const { container: other } = render(<PostMergePanel rules={older} />);
    expect(other.textContent).toBe("");
    expect(screen.queryByRole("heading", { name: "Post-merge gate" })).toBeNull();
  });

  it("says the post-merge state could not be read, with the error, instead of drawing nothing", () => {
    render(
      <PostMergePanel
        rules={rules({
          postgate: null,
          postgate_error: "the post-merge state could not be read; see the daemon log",
        })}
      />,
    );

    expect(screen.getByRole("heading", { name: "Post-merge gate" })).toBeDefined();
    expect(screen.getByText("The post-merge state could not be read.")).toBeDefined();
    expect(screen.getByText(/see the daemon log/)).toBeDefined();
    expect(screen.queryByText(/No result on/)).toBeNull();
    expect(screen.queryByText(/is green/)).toBeNull();
  });

  it("shows a red target with its groups, since, culprit and range, and offers no control", () => {
    render(
      <PostMergePanel
        rules={rules({
          postgate: postgate({
            red_groups: ["core", "py"],
            red_sha: RED_SHA,
            red_since: SINCE_SHA,
            red_base: BASE_SHA,
            culprit: CULPRIT_SHA,
            phase: null,
          }),
        })}
      />,
    );

    expect(screen.getByRole("heading", { name: "Post-merge gate" })).toBeDefined();
    expect(screen.getByText("main")).toBeDefined();
    expect(screen.getByText(/core, py/)).toBeDefined();
    expect(screen.getByText(RED_SHA.slice(0, 12))).toBeDefined();
    expect(screen.getByText(SINCE_SHA.slice(0, 12))).toBeDefined();
    expect(screen.getByText(CULPRIT_SHA.slice(0, 12))).toBeDefined();
    expect(screen.getByText(BASE_SHA.slice(0, 12))).toBeDefined();
    expect(screen.queryAllByRole("button")).toEqual([]);
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });

  it("shows an inconclusive red's candidates instead of a culprit", () => {
    render(
      <PostMergePanel
        rules={rules({
          postgate: postgate({
            red_groups: ["core"],
            red_sha: RED_SHA,
            red_since: SINCE_SHA,
            red_base: BASE_SHA,
            culprit: null,
            candidates: [CULPRIT_SHA, GREEN_SHA],
          }),
        })}
      />,
    );

    expect(screen.getByText(/Inconclusive between/)).toBeDefined();
    expect(screen.getByText(CULPRIT_SHA.slice(0, 12))).toBeDefined();
    expect(screen.getByText(GREEN_SHA.slice(0, 12))).toBeDefined();
    expect(screen.queryByText(/Culprit/)).toBeNull();
  });

  it("shows a green target's last green commit and no red", () => {
    render(<PostMergePanel rules={rules({ postgate: postgate({ last_green: GREEN_SHA }) })} />);

    expect(screen.getByRole("heading", { name: "Post-merge gate" })).toBeDefined();
    expect(screen.getByText(GREEN_SHA.slice(0, 12))).toBeDefined();
    expect(screen.getByText(/is green at/)).toBeDefined();
    expect(screen.queryByText(/is red at/)).toBeNull();
    expect(screen.queryByText(/Failing/)).toBeNull();
  });

  it("says there is no result yet when a first gate is running and nothing is recorded", () => {
    render(<PostMergePanel rules={rules({ postgate: postgate({ running: RED_SHA }) })} />);

    expect(screen.getByText(/No result on/)).toBeDefined();
    expect(screen.getByText(/A gate is running on/)).toBeDefined();
    expect(screen.queryByText(/is green/)).toBeNull();
  });
});

describe("OnItsOwn", () => {
  it("OnItsOwn mounts the post-merge panel from the rules read", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/projects/alpha/rules") {
        return rules({
          postgate: postgate({ red_groups: ["core"], red_sha: RED_SHA, red_since: SINCE_SHA }),
        });
      }
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
    render(
      <QueryClientProvider client={createAppQueryClient()}>
        <RouterProvider router={router} />
      </QueryClientProvider>,
    );

    expect(await screen.findByRole("heading", { name: "Post-merge gate" })).toBeDefined();
  });
});
