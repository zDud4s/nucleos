import { beforeEach, describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import { daemonWith, known } from "./test-helpers";
import { ScopedMemory } from "./ScopedMemory";
import { renderWithRouter } from "../../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../../data/client", async (original) => ({
  ...(await original<typeof import("../../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("ScopedMemory - own memory", () => {
  it("own memory lists only active rows of its scope and counts the waiting ones", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, scope_kind: "agent", scope_id: "copywriter", title: "Copy is short" }),
        known({ id: 2, scope_kind: "agent", scope_id: "other", title: "Someone else's habit" }),
        known({ id: 3, scope_kind: "team", scope_id: "copywriter", title: "A team with that id" }),
        known({
          id: 4,
          scope_kind: "agent",
          scope_id: "copywriter",
          status: "proposed",
          title: "Not yet approved",
        }),
        known({
          id: 5,
          scope_kind: "agent",
          scope_id: "copywriter",
          status: "proposed",
          title: "Also not yet approved",
        }),
      ]),
    );

    await renderWithRouter(<ScopedMemory scopeKind="agent" scopeId="copywriter" />);

    const region = await screen.findByRole("region", { name: "Memory" });
    expect(await screen.findByText("Copy is short")).toBeDefined();
    expect(region.textContent).not.toContain("Someone else's habit");
    expect(region.textContent).not.toContain("A team with that id");
    expect(region.textContent).not.toContain("Not yet approved");
    expect(region.textContent).toContain("2 more waiting for you in the Brain");
  });

  it("own memory asks the núcleo for its own scope only", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, scope_kind: "agent", scope_id: "copywriter", title: "Copy is short" }),
      ]),
    );

    await renderWithRouter(<ScopedMemory scopeKind="agent" scopeId="copywriter" />);
    await screen.findByText("Copy is short");

    const paths = daemon.apiFetch.mock.calls.map(([path]) => path);
    expect(paths).toContain("/knowledge?scope_kind=agent&scope_id=copywriter");
    expect(paths).not.toContain("/knowledge");
  });

  it("own memory says so when the scope holds nothing", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 2, scope_kind: "agent", scope_id: "other", title: "Someone else's habit" }),
      ]),
    );

    await renderWithRouter(<ScopedMemory scopeKind="team" scopeId="financas" />);

    expect(await screen.findByText("Nothing is known in this scope yet.")).toBeDefined();
    expect(screen.queryByText("Someone else's habit")).toBeNull();
    expect(screen.queryByText(/more waiting for you/)).toBeNull();
  });
});
