// §spec atlas-sequence
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
const opener = vi.hoisted(() => ({ openUrl: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => opener);
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { SequenceView } from "./SequenceView";
import { createAppQueryClient } from "../app/queryClient";
import type { FlowCheck, FlowStep, MapFlows } from "../data/map-flows";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  opener.openUrl.mockReset();
  opener.openUrl.mockResolvedValue(undefined);
});

function step(participant: string, path: string, fn: string, over: Partial<FlowStep> = {}) {
  const base: FlowStep = {
    participant,
    path,
    fn,
    via: null,
    from: null,
    line: 12,
    ok: true,
    reason: null,
  };
  return { ...base, ...over };
}

function flow(name: string, steps: FlowStep[], over: Partial<FlowCheck> = {}): FlowCheck {
  return { name, stale: false, broken_step: null, steps, ...over };
}

/** Answer `GET /map/flows` with this and render the view over it. */
function open(answer: Partial<MapFlows>) {
  const whole: MapFlows = {
    file: ".nucleos/flows.yaml",
    declared: true,
    root: "C:/repo",
    flows: [],
    error: null,
    ...answer,
  };
  daemon.apiFetch.mockImplementation(async (path: string) => {
    if (path.endsWith("/map/flows")) return whole;
    return [];
  });
  return render(
    <QueryClientProvider client={createAppQueryClient()}>
      <SequenceView projectId="alpha" />
    </QueryClientProvider>,
  );
}

describe("the sequence view", () => {
  it("draws one lifeline per participant and one arrow per call", async () => {
    open({
      flows: [
        flow("land", [
          step("cli", "core/src/cli.rs", "land"),
          step("http", "core/src/http.rs", "land_worktree"),
          step("vcs", "core/src/vcs.rs", "submit"),
        ]),
      ],
    });

    const diagram = await screen.findByRole("img", { name: "Sequence of land" });
    // One header per participant, in the order the flow met them.
    for (const participant of ["cli", "http", "vcs"]) {
      expect(within(diagram).getByText(participant)).toBeTruthy();
    }
    // Lifelines are the dashed verticals; a flow of direct calls has no other dashed line.
    expect(diagram.querySelectorAll('line[stroke-dasharray="4 4"]')).toHaveLength(3);
    // Three steps, two calls, each labelled with the fn it calls.
    const arrows = diagram.querySelectorAll("[data-step]");
    expect(arrows).toHaveLength(2);
    expect(within(diagram).getByText("land_worktree")).toBeTruthy();
    expect(within(diagram).getByText("submit")).toBeTruthy();
    expect(screen.getByText(/Verified against the code/)).toBeTruthy();
  });

  it("says which step broke a stale flow", async () => {
    open({
      flows: [
        flow(
          "land",
          [
            step("cli", "core/src/cli.rs", "land"),
            step("http", "core/src/http.rs", "land_worktree"),
            step("vcs", "core/src/vcs.rs", "submit", {
              ok: false,
              line: null,
              reason: "http.rs never calls submit",
            }),
          ],
          { stale: true, broken_step: 2 },
        ),
      ],
    });

    const section = await screen.findByRole("region", { name: "Flow land" });
    expect(within(section).getByText(/Stale/)).toBeTruthy();
    // The step is named for a person, counting from one, with the fn and the reason.
    expect(within(section).getByText(/step 3 \(submit\)/)).toBeTruthy();
    expect(within(section).getByText(/http\.rs never calls submit/)).toBeTruthy();
    expect(within(section).queryByText(/Verified against the code/)).toBeNull();
  });

  it("opens a step at its file and line", async () => {
    open({
      flows: [
        flow("land", [
          step("cli", "core/src/cli.rs", "land", { line: 3 }),
          step("a", "core/src/a.rs", "run", { line: 12 }),
        ]),
      ],
    });

    const link = await screen.findByRole("link", { name: "core/src/a.rs:12" });
    expect(link.getAttribute("href")).toBe("vscode://file/C:/repo/core/src/a.rs:12");

    // A click goes through the opener, and the webview never follows the href itself.
    fireEvent.click(link);
    expect(opener.openUrl).toHaveBeenCalledWith("vscode://file/C:/repo/core/src/a.rs:12");
  });

  it("says how to declare flows when there are none", async () => {
    open({ declared: false, flows: [] });

    expect(await screen.findByText(/\.nucleos\/flows\.yaml/)).toBeTruthy();
    expect(screen.queryByRole("region", { name: /^Flow / })).toBeNull();
  });
});
