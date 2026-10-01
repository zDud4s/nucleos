import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { JudgePanel, JudgeReviewPanel } from "./AutopilotJudge";
import { JUDGE_RESIDUAL_RISK, type JudgeStatus, type JudgeVerdict } from "../data/autopilot";
import { project, renderWithRouter } from "../test/harness";

function status(overrides: Partial<JudgeStatus> = {}): JudgeStatus {
  return {
    project_id: "alpha",
    judge: "off",
    rules_error: null,
    readiness: { reviewed: 0, agree: 0, ready: false, by_class: [] },
    ...overrides,
  };
}

const waitPastTheDwell = () => new Promise((resolve) => setTimeout(resolve, 350));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("the judge's panel", () => {
  it("shows the mode, the readiness by class and the residual risk, and keeps enforce shut below the bar", async () => {
    daemon.apiFetch.mockResolvedValue(
      status({
        judge: "observe",
        readiness: {
          reviewed: 9,
          agree: 9,
          ready: false,
          by_class: [{ action_class: "unrecognized", reviewed: 9, agree: 9 }],
        },
      }),
    );
    renderWithRouter(<JudgePanel projectId="alpha" project={project({ mode: "active" })} />, {
      initialPath: "/autopilot",
    });

    expect(await screen.findByText(/9 distinct actions reviewed, 9 agreed/)).toBeDefined();
    expect(screen.getByText("unrecognized")).toBeDefined();
    expect(screen.getByText(JUDGE_RESIDUAL_RISK)).toBeDefined();
    expect((screen.getByRole("button", { name: "Enforce" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("says when the project's rules cannot be read, and that a change reaches only its next runs", async () => {
    daemon.apiFetch.mockResolvedValue(status({ judge: "observe", rules_error: "config: invalid type" }));
    renderWithRouter(<JudgePanel projectId="alpha" project={project({ mode: "active" })} />, {
      initialPath: "/autopilot",
    });

    expect(await screen.findByText(/rules cannot be read — the judge has no effect/)).toBeDefined();
    expect(screen.getByText(/runs already working keep the setting they started with/)).toBeDefined();
  });

  it("turns observation on with the project and the mode the núcleo expects", async () => {
    daemon.apiFetch.mockResolvedValue(status());
    renderWithRouter(<JudgePanel projectId="alpha" project={project({ mode: "active" })} />, {
      initialPath: "/autopilot",
    });

    fireEvent.click(await screen.findByRole("button", { name: "Observe" }));
    await waitPastTheDwell();
    fireEvent.click(screen.getByRole("button", { name: /Send each judged call to TypeSafe/ }));

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/autopilot/judge", {
        method: "POST",
        body: JSON.stringify({ project_id: "alpha", judge: "observe" }),
      }),
    );
  });
});

describe("the judge's review queue", () => {
  const entry: JudgeVerdict = {
    id: 5,
    run_id: 44,
    tool_name: "Bash",
    tool_input: JSON.stringify({ command: "cargo test --workspace | tee t.log" }),
    action_class: "unrecognized",
    classifier_decision: "pending_approval",
    judge: "observe",
    model: "jev-latest",
    p_in_scope: 0.97,
    p_safe: 0.94,
    p: 0.94,
    band: "allow",
    capped: false,
    final_decision: "pending_approval",
    enforced: false,
    created_at: "2026-09-27T10:00:00Z",
  };

  it("shows the probability and the band, and records a verdict", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) =>
      path.startsWith("/judge-verdicts/unreviewed") ? [entry] : undefined,
    );
    renderWithRouter(<JudgeReviewPanel projectId="alpha" />, { initialPath: "/autopilot" });

    expect(await screen.findByText("would allow")).toBeDefined();
    expect(screen.getByText("0.94")).toBeDefined();

    fireEvent.click(screen.getByRole("button", { name: "Agree #5" }));
    await waitPastTheDwell();
    fireEvent.click(screen.getByRole("button", { name: /The judge was right/ }));

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/judge-verdicts/5/verdict", {
        method: "POST",
        body: JSON.stringify({ verdict: "approve" }),
      }),
    );
  });
});
