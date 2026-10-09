import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ResolvePanel } from "./AutopilotResolve";
import { RESOLVE_DATA_WARNING, RESOLVE_ENFORCE_RISK } from "../data/autopilot";
import { ApiRefusal } from "../data/client";
import { project, renderWithRouter } from "../test/harness";

const waitPastTheDwell = () => new Promise((resolve) => setTimeout(resolve, 350));

const noReadiness = { reviewed: 0, agree: 0, less_cautious: 0, ready: false };

// Braces matter: a beforeEach that returns a function has it run as cleanup, and mockReset returns the mock itself.
beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("the resolver's switch", () => {
  it("says what leaves the machine before it can be turned on, and turns observe on", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) =>
      path.startsWith("/autopilot/judge-resolve?")
        ? { project_id: "alpha", judge_resolve: "off", readiness: noReadiness }
        : { project_id: "alpha", judge_resolve: "observe", readiness: noReadiness },
    );
    await renderWithRouter(<ResolvePanel projectId="alpha" project={project({ mode: "active" })} />, { initialPath: "/autopilot" });

    expect(await screen.findByText(RESOLVE_DATA_WARNING)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Observe how blocks would be resolved" }));
    await waitPastTheDwell();
    fireEvent.click(screen.getByRole("button", { name: "Start observing" }));

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/autopilot/judge-resolve", {
        method: "POST",
        body: JSON.stringify({ project_id: "alpha", judge_resolve: "observe" }),
      }),
    );
  });

  it("asks for a project before it offers anything, and never asks the núcleo about none", async () => {
    await renderWithRouter(<ResolvePanel projectId={null} project={undefined} />, { initialPath: "/autopilot" });
    expect(screen.getByText("Choose a project above.")).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });
});

describe("the resolver's readiness, queue and enforce", () => {
  const ready = { reviewed: 10, agree: 10, less_cautious: 0, ready: true };
  const observing = (readiness = ready) => ({ project_id: "alpha", judge_resolve: "observe", readiness });
  const panel = (mode: "active" | "shadow") => (
    <ResolvePanel projectId="alpha" project={project({ mode })} />
  );

  it("shows the risk, and lets an active project that cleared the bar enforce after a confirm", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) =>
      path.startsWith("/judge-resolutions/") ? [] : observing(),
    );
    await renderWithRouter(panel("active"), { initialPath: "/autopilot" });

    expect(await screen.findByText(RESOLVE_ENFORCE_RISK)).toBeTruthy();
    expect(screen.getByText("10 reviewed, 10 agree — ready")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Let the resolver decide" }));
    await waitPastTheDwell();
    fireEvent.click(screen.getByRole("button", { name: /I accept the risk above/ }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/autopilot/judge-resolve", {
        method: "POST",
        body: JSON.stringify({ project_id: "alpha", judge_resolve: "enforce" }),
      }),
    );
  });

  it("keeps enforce shut off Active and under the bar", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) =>
      path.startsWith("/judge-resolutions/") ? [] : observing(),
    );
    const { unmount } = await renderWithRouter(panel("shadow"), { initialPath: "/autopilot" });
    await screen.findByText(RESOLVE_ENFORCE_RISK);
    expect((screen.getByRole("button", { name: "Let the resolver decide" }) as HTMLButtonElement).disabled).toBe(true);
    unmount();
    daemon.apiFetch.mockImplementation(async (path: string) =>
      path.startsWith("/judge-resolutions/")
        ? []
        : observing({ reviewed: 10, agree: 8, less_cautious: 0, ready: false }),
    );
    await renderWithRouter(panel("active"), { initialPath: "/autopilot" });
    expect(await screen.findByText("8 of 10 agree — under 90%")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Let the resolver decide" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("says why the núcleo refused enforce", async () => {
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (init?.method === "POST") throw new ApiRefusal(409, "not_active", "not_active");
      return path.startsWith("/judge-resolutions/") ? [] : observing();
    });
    await renderWithRouter(panel("active"), { initialPath: "/autopilot" });

    fireEvent.click(await screen.findByRole("button", { name: "Let the resolver decide" }));
    await waitPastTheDwell();
    fireEvent.click(screen.getByRole("button", { name: /I accept the risk above/ }));
    expect(await screen.findByText(/the resolver decides only on top of an active project/)).toBeTruthy();
  });

  it("offers each queued block only its event's outcomes, with its numbers, and sends the one chosen", async () => {
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (init?.method === "POST") return undefined;
      if (path.startsWith("/judge-resolutions/unreviewed")) {
        return [
          {
            id: 7, run_id: 3, lineage_root_id: 3, event: "gate_failed", tool_name: null, tool_input: null,
            gate_output: "FAILED core::x", p_off_task: null, p_needed: null, p_avoidable: null, p_fixable: 0.93,
            default_outcome: "owner", judge_outcome: "correction", final_outcome: "owner", enforced: false,
            created_at: "2026-09-27T10:00:00Z",
          },
        ];
      }
      return observing({ reviewed: 0, agree: 0, less_cautious: 0, ready: false });
    });
    await renderWithRouter(panel("active"), { initialPath: "/autopilot" });

    expect(await screen.findByText("FAILED core::x")).toBeTruthy();
    expect(screen.getByText("fixable")).toBeTruthy();
    expect(screen.getByRole("link", { name: "run 3" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "carry on without it" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "hand it to me" }));
    await waitPastTheDwell();
    fireEvent.click(screen.getByRole("button", { name: "Yes — hand it to me" }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/judge-resolutions/7/outcome", {
        method: "POST",
        body: JSON.stringify({ outcome: "owner" }),
      }),
    );
  });
});
