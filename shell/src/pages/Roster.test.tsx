import { describe, expect, it, vi } from "vitest";
import { screen, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { daemonFetch, daemonState, daemonText, project, renderApp } from "../test/harness";
import type { ProjectSummary } from "../data/system";

async function openRoster(projects: ProjectSummary[]) {
  const state = daemonState({ projects });
  daemon.apiFetch.mockImplementation(daemonFetch(state));
  daemon.apiText.mockImplementation(daemonText(state));
  daemon.probeHealth.mockResolvedValue(true);
  return renderApp({ initialPath: "/projects" });
}

/** A project whose folder is where it says it is — the uninteresting case, to vary from. */
function fine(id: string, overrides: Partial<ProjectSummary> = {}): ProjectSummary {
  return project({
    project_id: id,
    mode: "shadow",
    project_root: `C:/Projects/${id}`,
    root_exists: true,
    ...overrides,
  });
}

/** The rows, top to bottom, by the name in each row's header cell. */
function order(): string[] {
  return screen
    .getAllByRole("row")
    .slice(1) // the header
    .map((row) => within(row).getAllByRole("rowheader")[0].textContent ?? "");
}

describe("the roster", () => {
  /**
   * **The complaint this page was rebuilt to answer.** Twenty-five equal pills, each repeating the
   * `shadow` that twenty-four of them share, with the two readings that actually differ mixed in
   * among them. The order is the fix: what needs somebody is at the top, and the sidebar keeps
   * answering "where is X" alphabetically for anybody who wants that instead.
   */
  it("puts what needs somebody at the top, whatever it is called", async () => {
    await openRoster([
      fine("ANSup"),
      fine("zeta", { open_proposals: 4, last_gate: "passed" }),
      fine("beta", { last_gate: "failed" }),
      fine("gamma", { root_exists: false }),
    ]);

    await screen.findByRole("table");
    expect(order()).toEqual(["gamma", "beta", "zeta", "ANSup"]);
  });

  /**
   * A column of twenty-four identical badges teaches an eye to skip the column that holds the one
   * that is not. `shadow` is the default state of this app, so it is written plainly and only what
   * departs from it is coloured.
   */
  it("badges the mode only when it is not the shadow everything else is", async () => {
    await openRoster([fine("quiet"), fine("acting", { mode: "active" })]);

    await screen.findByRole("table");
    const shadowRow = screen.getByRole("rowheader", { name: "quiet" }).closest("tr") as HTMLElement;
    const activeRow = screen.getByRole("rowheader", { name: "acting" }).closest("tr") as HTMLElement;

    expect(within(shadowRow).getByText("shadow").className).not.toContain("ui-badge");
    expect(within(activeRow).getByText("active").className).toContain("ui-badge");
  });

  /**
   * **The headline used to contradict the rows it sat above.** It counted projects with a recorded
   * root and said "all with a folder"; the rows probed the folder and said `folder gone`. One
   * source answers both now, and the daemon is that source.
   */
  it("says the same thing about folders as its own rows do", async () => {
    await openRoster([fine("here"), fine("moved", { root_exists: false })]);

    expect(await screen.findByText(/1 with the folder gone/)).toBeTruthy();
    const movedRow = screen.getByRole("rowheader", { name: "moved" }).closest("tr") as HTMLElement;
    expect(within(movedRow).getByText("gone")).toBeTruthy();
  });

  /**
   * The three states §12 asks for, on the one reading this page owns. `not named` is a setting
   * nobody filled in and `gone` is a folder that moved — they send a person to two different
   * places, and the page that drew them the same way sent half of them to the wrong one.
   */
  it("distinguishes a folder that moved from one that was never named", async () => {
    await openRoster([
      fine("moved", { root_exists: false }),
      project({ project_id: "empty", mode: "shadow", project_root: null, root_exists: null }),
    ]);

    await screen.findByRole("table");
    const emptyRow = screen.getByRole("rowheader", { name: "empty" }).closest("tr") as HTMLElement;
    expect(within(emptyRow).getByText("not named")).toBeTruthy();
    expect(within(emptyRow).queryByText("gone")).toBeNull();
  });

  /**
   * Into the workspace and not back into a file tree: a roster row is a project, and the question
   * somebody arrives at a project with is what Estado answers. This is the whole reason the
   * inspector moved off this page.
   */
  it("opens a project in its workspace", async () => {
    const rendered = await openRoster([fine("nucleos")]);

    /*
      Scoped to the table, because the SIDEBAR also lists this project by name while you are in the
      projects area — which is the arrangement the rail was changed to on the same day. An unscoped
      query here would be ambiguous, and the ambiguity is the two surfaces agreeing.
    */
    const table = await screen.findByRole("table");
    const link = within(table).getByRole("link", { name: "nucleos" });
    expect(link.getAttribute("href")).toContain("/projects/nucleos/estado");
    expect(rendered.router.state.location.pathname).toBe("/projects");
  });

  /** A gate that could not RUN says nothing about the code, so it is not drawn as a failure. */
  it("keeps a gate that could not run apart from one that said no", async () => {
    await openRoster([fine("broken", { last_gate: "failed" }), fine("unrun", { last_gate: "errored" })]);

    await screen.findByRole("table");
    const brokenRow = screen.getByRole("rowheader", { name: "broken" }).closest("tr") as HTMLElement;
    const unrunRow = screen.getByRole("rowheader", { name: "unrun" }).closest("tr") as HTMLElement;

    expect(within(brokenRow).getByText("failed").className).toContain("ui-badge-danger");
    expect(within(unrunRow).getByText("errored").className).toContain("ui-badge-paused");
  });

  it("says so plainly when the núcleo knows of no project", async () => {
    await openRoster([]);

    expect(await screen.findByText(/no project has been registered/i)).toBeTruthy();
    expect(screen.queryByRole("table")).toBeNull();
  });
});
