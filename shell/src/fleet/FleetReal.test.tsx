import { beforeAll, describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { FleetCanvas } from "./Surface";
import { FleetStoreContext } from "./hooks";
import { FleetStore } from "./store";
import { child, node, snapshot } from "./test-fixtures";
import { FleetUiContext } from "./ui-context";
import { renderWithRouter } from "../test/harness";

// jsdom/happy-dom ship neither; xyflow needs both (same stubs the old Fleet.test used).
beforeAll(() => {
  const scope = globalThis as unknown as Record<string, unknown>;
  scope.ResizeObserver ??= class { observe() {} unobserve() {} disconnect() {} };
  scope.DOMMatrixReadOnly ??= class { m22 = 1; constructor(_transform?: string) {} };
});

const ALL = { stop: true, open_run: true, full_transcript: true, pin: true };
const edges = (container: HTMLElement) => container.querySelectorAll("path.fleet-edge");

async function mount(store: FleetStore, openPanel = vi.fn()) {
  const view = await renderWithRouter(
    <FleetStoreContext.Provider value={store}>
      <FleetUiContext.Provider value={{ path: new Set(), openPanel, toggleGroup: vi.fn(), select: vi.fn() }}>
        <FleetCanvas project={null} onlyActive={false} expanded={new Set()} follow={false} fitSignal={0} />
      </FleetUiContext.Provider>
    </FleetStoreContext.Provider>,
  );
  return view;
}

describe("the real xyflow", () => {
  it("renders sized nodes and draws the spawn edge", async () => {
    const store = new FleetStore();
    const root = node("r", { label: "root" });
    store.apply(snapshot([root, child("k", root, { label: "kid" })]));
    const view = await mount(store);
    expect(await screen.findByRole("article", { name: /^root/ })).toBeDefined();
    expect(screen.getByRole("article", { name: /^kid/ })).toBeDefined();
    await waitFor(() => expect(edges(view.container)).toHaveLength(1));
  });

  it("keeps drawing every edge after a second event adds a node", async () => {
    const store = new FleetStore();
    const root = node("r", { label: "root" });
    store.apply(snapshot([root, child("k", root, { label: "kid" })]));
    const view = await mount(store);
    await waitFor(() => expect(edges(view.container)).toHaveLength(1));
    act(() => store.apply({ t: "node", node: child("k2", root, { label: "second", started_at: "2026-10-20T10:04:00Z" }) }));
    expect(await screen.findByRole("article", { name: /^second/ })).toBeDefined();
    await waitFor(() => expect(edges(view.container)).toHaveLength(2));
  });

  it("delivers a click on a button inside a node", async () => {
    const store = new FleetStore();
    store.apply(snapshot([node("r", { label: "root", capabilities: ALL, run_id: 3 })]));
    const openPanel = vi.fn();
    await mount(store, openPanel);
    fireEvent.click(await screen.findByRole("button", { name: "Open the full transcript" }));
    expect(openPanel).toHaveBeenCalledWith("r");
  });
});
