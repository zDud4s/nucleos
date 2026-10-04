import { describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import {
  daemonFetch,
  daemonState,
  daemonText,
  project,
  renderApp,
  type DaemonState,
} from "../test/harness";
import { ApiRefusal } from "../data/client";
import { keys } from "../data/keys";
import type { ProjectSummary } from "../data/system";

function fine(id: string, overrides: Partial<ProjectSummary> = {}): ProjectSummary {
  return project({
    project_id: id,
    mode: "shadow",
    project_root: `C:/Projects/${id}`,
    root_exists: true,
    ...overrides,
  });
}

/** The State mode of one project, with the live listings the roster reads answered empty. */
async function openState(
  id: string,
  projects: ProjectSummary[],
  overrides: Partial<DaemonState> = {},
  fetch?: (path: string, init?: RequestInit) => unknown,
) {
  const state = daemonState({ projects, ...overrides });
  const base = daemonFetch(state);
  daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
    const answered = fetch?.(path, init);
    if (answered !== undefined) return await answered;
    if (path === "/jobs?live=true" || path.startsWith("/runs?live=true")) return [];
    return await base(path, init);
  });
  daemon.apiText.mockImplementation(daemonText(state));
  daemon.probeHealth.mockResolvedValue(true);
  const rendered = await renderApp({ initialPath: `/projects/${id}/state` });
  return { state, rendered };
}

/** The Leaving section's remove control, opened. */
async function openRemove(id: string) {
  const leaving = await screen.findByRole("region", { name: "Leaving" });
  fireEvent.click(await within(leaving).findByRole("button", { name: "remove from NucleOS…" }));
  return within(await screen.findByRole("group", { name: `Remove ${id} from NucleOS` }));
}

/** Past `ConfirmButton`'s dwell, so the second press is a decision and not a double-click. */
async function pastTheDwell(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 350));
  });
}

describe("leaving the roster, from inside the project", () => {
  /**
   * **The way out moved inside the project.** It sits in State's *Leaving* section, above the
   * folder deletion and quieter than it, and the reassurance about the folder arrives before the
   * button does — `remove` on a page full of paths reads as *delete that* until something says
   * otherwise.
   */
  it("removes the project, goes to the roster, and says there what left", async () => {
    const { state, rendered } = await openState("spent", [fine("spent"), fine("kept")]);

    const panel = await openRemove("spent");
    expect(panel.getByText(/stays exactly where it is/)).toBeTruthy();
    expect(panel.getByText("C:/Projects/spent")).toBeTruthy();

    fireEvent.click(panel.getByRole("button", { name: "remove" }));
    await waitFor(() => expect(state.removed.length).toBe(1));
    expect(state.removed[0]).toEqual({ projectId: "spent", forgetHistory: false });

    await waitFor(() => expect(rendered.router.state.location.pathname).toBe("/projects"));
    const line = await screen.findByText(
      "spent left the roster — its folder is still at C:/Projects/spent, and its history is kept.",
    );
    const quiet = line.closest(".ui-quiet") as HTMLElement;
    expect(quiet.getAttribute("role")).toBe("status");
    expect(within(quiet).getByRole("link", { name: "add it back" }).getAttribute("href")).toBe(
      "/projects/new",
    );
    await waitFor(() => expect(document.activeElement?.contains(quiet)).toBe(true));
    // And the roster follows the daemon: the project is gone because the query was refetched.
    expect(document.querySelector('a[href="/projects/spent/state"].rs-quiet-item')).toBeNull();
  });

  /** The roster says nothing about a removal when it was reached any other way. */
  it("says nothing on the roster when nothing was removed", async () => {
    const state = daemonState({ projects: [fine("kept")] });
    const base = daemonFetch(state);
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) =>
      path === "/jobs?live=true" || path.startsWith("/runs?live=true") ? [] : await base(path, init),
    );
    daemon.apiText.mockImplementation(daemonText(state));
    daemon.probeHealth.mockResolvedValue(true);
    await renderApp({ initialPath: "/projects" });
    await screen.findByRole("region", { name: "Quiet" });
    expect(screen.queryByText(/left the roster/)).toBeNull();
  });

  /**
   * **The history is kept unless somebody says otherwise, at the moment they say it**, and the
   * number is beside the checkbox because that is what makes it a decision.
   */
  it("forgets only when the box is ticked and the second press confirms", async () => {
    const { state } = await openState("spent", [fine("spent")], {
      record: {
        forgets: { runs: 312, jobs: 0, proposals: 8, decisions: 0, stamps: 40, commands: 0, feed: 0 },
        holds: { slots: 0, worktrees: 0 },
      },
    });

    const panel = await openRemove("spent");
    expect(await panel.findByText(/312 runs, 8 proposals and 40 stamps on record/)).toBeTruthy();
    fireEvent.click(panel.getByRole("checkbox"));
    expect(panel.getByText(/will be deleted from the núcleo and cannot be brought back/)).toBeTruthy();

    fireEvent.click(panel.getByRole("button", { name: "remove and forget" }));
    expect(state.removed).toEqual([]);
    await pastTheDwell();
    fireEvent.click(
      panel.getByRole("button", { name: "delete 312 runs, 8 proposals and 40 stamps for good · spent" }),
    );
    await waitFor(() => expect(state.removed[0]).toEqual({ projectId: "spent", forgetHistory: true }));
    expect(
      await screen.findByText(
        "spent left the roster and its history was deleted — its folder is still at C:/Projects/spent.",
      ),
    ).toBeTruthy();
  });

  it("says what is holding a project rather than letting somebody press remove", async () => {
    const { state } = await openState("busy", [fine("busy")], {
      record: {
        forgets: { runs: 4, jobs: 0, proposals: 0, decisions: 0, stamps: 0, commands: 0, feed: 0 },
        holds: { slots: 1, worktrees: 2 },
      },
    });

    const panel = await openRemove("busy");
    expect(await panel.findByText(/1 slot in flight and 2 worktrees checked out/)).toBeTruthy();
    expect(panel.getByRole("button", { name: "remove" })).toHaveProperty("disabled", true);
    fireEvent.click(panel.getByRole("button", { name: "remove" }));
    expect(state.removed).toEqual([]);
  });

  /** A refusal leaves the panel — and the page — where they are, with the refusal's own words. */
  it("stays on the project and says why when the núcleo refuses", async () => {
    const { rendered } = await openState("busy", [fine("busy")], {
      removeRefusal: { status: 409, code: "in_flight", detail: "1 slot still working here" },
    });

    const panel = await openRemove("busy");
    fireEvent.click(panel.getByRole("button", { name: "remove" }));
    expect(await panel.findByText(/work is still in flight here/)).toBeTruthy();
    expect(rendered.router.state.location.pathname).toBe("/projects/busy/state");
  });

  /**
   * The toggle keeps its name, points at the panel it opened, leaves one `cancel` on screen, and gets focus back
   * when the panel closes by `cancel` or Escape — a panel that unmounts with focus inside drops it
   * to the body at the foot of the page.
   */
  it("keeps one cancel on screen, and hands focus back on cancel and Escape", async () => {
    await openState("one", [fine("one")]);
    const leaving = await screen.findByRole("region", { name: "Leaving" });

    await openRemove("one");
    const toggle = within(leaving).getByRole("button", { name: "remove from NucleOS…", expanded: true });
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    const panel = screen.getByRole("group", { name: "Remove one from NucleOS" });
    expect(toggle.getAttribute("aria-controls")).toBe(panel.id);
    // Exactly one `cancel` on screen, and it is the panel's.
    expect(screen.getAllByRole("button", { name: "cancel" })).toEqual([
      within(panel).getByRole("button", { name: "cancel" }),
    ]);
    // Exactly one `remove` on screen, and it is the one that removes.
    expect(screen.getAllByRole("button", { name: "remove" })).toEqual([
      within(panel).getByRole("button", { name: "remove" }),
    ]);

    fireEvent.click(within(panel).getByRole("button", { name: "cancel" }));
    expect(screen.queryByRole("group", { name: "Remove one from NucleOS" })).toBeNull();
    await waitFor(() =>
      expect(document.activeElement).toBe(
        within(leaving).getByRole("button", { name: "remove from NucleOS…" }),
      ),
    );

    const again = await openRemove("one");
    fireEvent.keyDown(again.getByRole("button", { name: "cancel" }), { key: "Escape" });
    expect(screen.queryByRole("group", { name: "Remove one from NucleOS" })).toBeNull();
    await waitFor(() =>
      expect(document.activeElement).toBe(
        within(leaving).getByRole("button", { name: "remove from NucleOS…" }),
      ),
    );
  });

  /** Gone, not disabled, over a roster nobody can vouch for. */
  it("takes the control away while the roster is stale", async () => {
    let answering = true;
    const { rendered } = await openState("one", [fine("one")], {}, (path, init) => {
      if (!answering && path === "/projects" && init?.method === undefined) {
        return Promise.reject(new ApiRefusal(503, "unavailable", ""));
      }
      return undefined;
    });
    const leaving = await screen.findByRole("region", { name: "Leaving" });
    await within(leaving).findByRole("button", { name: "remove from NucleOS…" });

    answering = false;
    await act(async () => {
      await rendered.queryClient.refetchQueries({ queryKey: keys.projects.all, exact: true });
    });
    await waitFor(() =>
      expect(within(leaving).queryByRole("button", { name: "remove from NucleOS…" })).toBeNull(),
    );
    // The folder deletion is its own control and keeps its own rules.
    expect(within(leaving).getByRole("button", { name: "delete this folder…" })).toBeTruthy();
  });
});
