import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";

import Projects from "./Projects";
import type { ProjectSummary } from "./api";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

function project(overrides: Partial<ProjectSummary>): ProjectSummary {
  return {
    project_id: "x", mode: "off", project_root: null, pending: 0,
    classes_ready: 0, classes_total: 0, promotable: false,
    open_proposals: 0, wip_limit: null, queue_full: false,
    ...overrides,
  };
}

/**
 * A daemon holding this roster.
 *
 * `onDisk` names the projects whose recorded root actually exists. Everything else gets the 404 the
 * daemon really answers when `canonicalize` fails on a root that has been cleaned up — which is the
 * whole case under test, so it is modelled rather than assumed away.
 */
function roster(projects: ProjectSummary[], onDisk: string[] = []) {
  fetchMock.mockImplementation(async (url: string) => {
    if (url.endsWith("/projects")) {
      return { ok: true, status: 200, json: async () => projects };
    }
    const match = /\/projects\/([^/]+)\//.exec(String(url));
    if (match !== null && onDisk.includes(decodeURIComponent(match[1] ?? ""))) {
      return { ok: true, status: 200, json: async () => [] };
    }
    return { ok: false, status: 404 };
  });
}

async function settle() {
  await act(async () => {});
}

function inspectCalls() {
  return fetchMock.mock.calls.filter(([url]) => /\/projects\/[^/]+\//.test(String(url)));
}

describe("the project inspector and a root it cannot read", () => {
  beforeEach(() => fetchMock.mockReset());
  afterEach(() => fetchMock.mockReset());

  it("defaults to a project it can actually read, not merely the first one", async () => {
    // The off project sorts first, which is exactly how the dead default happened.
    roster(
      [
        project({ project_id: "alpha", mode: "off", project_root: null }),
        project({ project_id: "beta", mode: "shadow", project_root: "C:/repos/beta" }),
      ],
      ["beta"],
    );

    render(<Projects token="t" connection="connected" />);
    await settle();

    expect(screen.getByRole("heading", { level: 1 }).textContent).toBe("Reading beta.");
    expect(screen.getByRole("button", { name: /beta/ }).getAttribute("aria-current")).toBe("true");
    expect(screen.getByRole("navigation", { name: "Project views" })).toBeTruthy();
  });

  /** The tabs that read the tree, as opposed to `rules`, which reads what the project does. */
  function inspectorTabsAreUsable() {
    // The crumbs only exist once the browser mounted, which is the thing the root check gates.
    return screen.queryByRole("navigation", { name: "Path" }) !== null;
  }

  it("explains an off project instead of firing a request that can only 404", async () => {
    roster([project({ project_id: "alpha", mode: "off", project_root: null })]);

    render(<Projects token="t" connection="connected" />);
    await settle();

    // The roster already said there is no root, so nothing is asked of the inspect routes.
    expect(inspectCalls()).toHaveLength(0);
    expect(screen.getByText("alpha has no root to read.")).toBeTruthy();
    // A state, not a failure: no error styling, and nothing of the tree to click into.
    expect(screen.queryByRole("alert")).toBeNull();
    expect(inspectorTabsAreUsable()).toBe(false);
    // The nav stays, though. An off project still has an approval-queue ceiling and still has a
    // rules file to be missing, and those are exactly what someone looking at an idle project came
    // to find out — hiding the tab behind the same check would hide the explanation behind the
    // failure it explains.
    expect(screen.getByRole("navigation", { name: "Project views" })).toBeTruthy();
  });

  it("names a recorded root that has vanished, and shows which path it was", async () => {
    // The real case: a root under a temp directory that an e2e run left behind and cleanup removed.
    const root = "C:/Users/x/AppData/Local/Temp/nucleos-c5t2-20260719-001/project";
    roster([project({ project_id: "gone-root", mode: "shadow", project_root: root })], []);

    render(<Projects token="t" connection="connected" />);
    await settle();

    expect(
      screen.getByText("The root recorded for this project is no longer on disk."),
    ).toBeTruthy();
    // The path is the only actionable thing on that screen, so it must be on it.
    expect(screen.getByText(root)).toBeTruthy();
    // The three tree views cannot work, so none of them is drawn — but `rules` still can, and the
    // nav that reaches it stays.
    expect(inspectorTabsAreUsable()).toBe(false);
    expect(screen.getByRole("navigation", { name: "Project views" })).toBeTruthy();
  });

  it("does not claim to be reading a project whose root is gone", async () => {
    roster([project({ project_id: "gone-root", mode: "shadow", project_root: "C:/nope" })], []);

    render(<Projects token="t" connection="connected" />);
    await settle();

    expect(screen.getByRole("heading", { level: 1 }).textContent)
      .toBe("gone-root points at a root that is gone.");
  });

  it("probes the root once, rather than once per view", async () => {
    roster([project({ project_id: "beta", mode: "shadow", project_root: "C:/repos/beta" })], ["beta"]);

    render(<Projects token="t" connection="connected" />);
    await settle();

    // One probe from the page, plus the browser's own listing of the root it then shows.
    expect(inspectCalls().length).toBeLessThanOrEqual(2);
  });

  it("counts how many projects are readable, so the picker's dashes are explained", async () => {
    roster(
      [
        project({ project_id: "alpha", mode: "off", project_root: null }),
        project({ project_id: "beta", mode: "shadow", project_root: "C:/repos/beta" }),
        project({ project_id: "gamma", mode: "active", project_root: "C:/repos/gamma" }),
      ],
      ["beta", "gamma"],
    );

    render(<Projects token="t" connection="connected" />);
    await settle();

    expect(screen.getByText("3 projects · 2 readable")).toBeTruthy();
  });
});
