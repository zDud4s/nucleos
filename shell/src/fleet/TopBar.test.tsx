import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { FleetStoreContext } from "./hooks";
import { FleetStore } from "./store";
import { node, snapshot } from "./test-fixtures";
import { TopBar, type TopBarProps } from "./TopBar";
import { ApiRefusal } from "../data/client";
import { renderWithRouter } from "../test/harness";

const base = (): TopBarProps => ({
  project: null, onProject: vi.fn(), onlyActive: false, onOnlyActive: vi.fn(), follow: false, onFollow: vi.fn(), onFit: vi.fn(),
});

async function show(store: FleetStore, over: Partial<TopBarProps> = {}) {
  return await renderWithRouter(
    <FleetStoreContext.Provider value={store}>
      <TopBar {...base()} {...over} />
    </FleetStoreContext.Provider>,
  );
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockResolvedValue({ house: { limit: 8, held: 3 }, projects: [] });
});

describe("TopBar", () => {
  it("filters by project, from the projects that have a tree", async () => {
    const store = new FleetStore();
    store.apply(snapshot([node("a", { project: "nucleos" }), node("b", { project: "deal_sniper" })]));
    const onProject = vi.fn();
    await show(store, { onProject });
    const select = screen.getByLabelText("Project") as HTMLSelectElement;
    expect([...select.options].map((o) => o.textContent)).toEqual(["All projects", "deal_sniper", "nucleos"]);
    fireEvent.change(select, { target: { value: "nucleos" } });
    expect(onProject).toHaveBeenCalledWith("nucleos");
    fireEvent.change(select, { target: { value: "" } });
    expect(onProject).toHaveBeenLastCalledWith(null);
  });

  it("has Active only, Follow (a toggle) and Fit", async () => {
    const store = new FleetStore();
    const onOnlyActive = vi.fn(), onFollow = vi.fn(), onFit = vi.fn();
    await show(store, { onOnlyActive, onFollow, onFit, follow: true });
    fireEvent.click(screen.getByLabelText("Active only"));
    expect(onOnlyActive).toHaveBeenCalledWith(true);
    const follow = screen.getByRole("button", { name: "Follow" });
    expect(follow.getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(follow);
    expect(onFollow).toHaveBeenCalledWith(false);
    fireEvent.click(screen.getByRole("button", { name: "Fit" }));
    expect(onFit).toHaveBeenCalledTimes(1);
  });

  it("shows slots n/limit from the house capacity, and nothing when it cannot be read", async () => {
    const store = new FleetStore();
    const view = await show(store);
    expect(await screen.findByText("slots 3/8")).toBeDefined();
    view.unmount();
    daemon.apiFetch.mockRejectedValue(new ApiRefusal(503, "unavailable", ""));
    await show(store);
    await act(async () => {});
    expect(screen.queryByText(/^slots /)).toBeNull();
  });

  it("says the connection state", async () => {
    const store = new FleetStore();
    await show(store);
    const status = screen.getByRole("status", { name: "Connection" });
    expect(status.textContent).toBe("◌ connecting…");
    act(() => store.setConnection("live"));
    expect(status.textContent).toBe("● live");
    act(() => store.setConnection("reconnecting"));
    expect(status.textContent).toBe("◌ reconnecting…");
    expect(within(status).queryByRole("button")).toBeNull();
  });
});
