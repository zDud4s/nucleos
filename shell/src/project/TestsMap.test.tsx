import { beforeEach, describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import type { TestsMapView } from "../data/project-tests-map";
import { renderWithQuery } from "../test/harness";
import { TestsMap } from "./TestsMap";

function view(overrides: Partial<TestsMapView> = {}): TestsMapView {
  return {
    state: "absent",
    errors: [],
    groups: [],
    proposal: { yaml: "groups:\n  core:\n    paths: [core/**]\n", sources: ["Cargo.toml"] },
    ...overrides,
  };
}

describe("the test map section", () => {
  beforeEach(() => {
    daemon.apiFetch.mockReset();
  });

  it("says there is no map yet, and opens the proposal", async () => {
    daemon.apiFetch.mockResolvedValue(view());

    renderWithQuery(<TestsMap projectId="p1" />);

    await screen.findByText(/Proposed map/);
    expect(document.body.textContent).toContain(
      "No nucleos.tests.yaml yet. Every change runs the whole gate.",
    );
    const details = document.querySelector("details");
    expect(details?.open).toBe(true);
    expect(document.querySelector("pre")?.textContent).toContain("core/**");
  });

  it("lists every error of an unusable map", async () => {
    daemon.apiFetch.mockResolvedValue(
      view({ state: "invalid", errors: ["group core: no paths", "unknown marker {modules}"] }),
    );

    renderWithQuery(<TestsMap projectId="p1" />);

    const items = await screen.findAllByRole("listitem");
    expect(items.map((item) => item.textContent)).toEqual([
      "group core: no paths",
      "unknown marker {modules}",
    ]);
    expect(document.querySelector("details")?.open).toBe(false);
  });

  it("counts the groups of a usable map", async () => {
    daemon.apiFetch.mockResolvedValue(view({ state: "valid", groups: ["core", "shell"] }));

    renderWithQuery(<TestsMap projectId="p1" />);

    expect(await screen.findByText(/2 groups/)).toBeTruthy();
    expect(document.body.textContent).toContain("core, shell");
  });

  it("asks the daemon for this project's map", async () => {
    daemon.apiFetch.mockResolvedValue(view());

    renderWithQuery(<TestsMap projectId="p1" />);

    await screen.findByText(/Proposed map/);
    expect(daemon.apiFetch).toHaveBeenCalledWith("/projects/p1/tests-map");
  });
});
