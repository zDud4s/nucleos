import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { NodeProps } from "@xyflow/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));
// No layout engine in happy-dom: the zoom is a variable and handles draw nothing.
const zoom = vi.hoisted(() => ({ value: 1 }));
vi.mock("@xyflow/react", async (original) => ({
  ...(await original<typeof import("@xyflow/react")>()),
  useStore: (selector: (s: { transform: [number, number, number] }) => unknown) =>
    selector({ transform: [0, 0, zoom.value] }),
  Handle: () => null,
}));

import { FleetNode, type FlowAgent } from "./FleetNode";
import { FleetStoreContext } from "./hooks";
import { FleetStore } from "./store";
import { child, line, metricsOf, node, snapshot } from "./test-fixtures";
import { renderWithRouter } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.apiFetch.mockResolvedValue(undefined);
  daemon.apiText.mockResolvedValue("");
  zoom.value = 1;
});

const props = (id: string) => ({ data: { id } }) as unknown as NodeProps<FlowAgent>;

async function show(store: FleetStore, ...ids: string[]) {
  return await renderWithRouter(
    <FleetStoreContext.Provider value={store}>
      {ids.map((id) => (
        <FleetNode key={id} {...props(id)} />
      ))}
    </FleetStoreContext.Provider>,
  );
}

const ALL = { stop: true, open_run: true, full_transcript: true, pin: true };
const NONE = { stop: false, open_run: false, full_transcript: false, pin: false };

describe("FleetNode", () => {
  it("shows the header, the terminal and the footer", async () => {
    const store = new FleetStore();
    const root = node("cc:root", { label: "nucleos-37", role: "orchestrate-tdd", model: "opus-5.5" });
    store.apply(snapshot([root], { "cc:root": [line("prompt", "plan it")] }, [metricsOf("cc:root")]));
    await show(store, "cc:root");
    const card = screen.getByRole("article", { name: "nucleos-37, running" });
    expect(within(card).getByText("orchestrate-tdd")).toBeDefined();
    expect(within(card).getByText("claude-code · opus-5.5")).toBeDefined();
    expect(within(card).getByText("plan it")).toBeDefined();
    expect(within(card).getByText("ctx 96k")).toBeDefined();
    expect(within(card).getByText("$0.71")).toBeDefined();
  });

  it("says each state in words", async () => {
    const store = new FleetStore();
    store.apply(
      snapshot([
        node("a", { label: "a", state: "idle" }),
        node("b", { label: "b", state: "lost" }),
        node("c", { label: "c", state: "done", ended_at: "2026-10-20T10:05:00Z" }),
      ]),
    );
    await show(store, "a", "b", "c");
    expect(screen.getByText("waiting for you")).toBeDefined();
    expect(screen.getByText("ended without closing")).toBeDefined();
    expect(screen.getByText("done")).toBeDefined();
  });

  it("offers actions from capabilities only, whatever the harness is", async () => {
    const store = new FleetStore();
    const root = node("cc:root", { label: "full", capabilities: ALL, run_id: 7 });
    const plain = child("cc:kid", root, { label: "plain", capabilities: NONE, run_id: 7, harness: "codex-cli" });
    const odd = node("x:odd", { label: "odd", harness: "something-new", capabilities: { ...NONE, stop: true }, run_id: 9 });
    store.apply(snapshot([root, plain, odd]));
    await show(store, "cc:root", "cc:kid", "x:odd");

    const full = screen.getByRole("article", { name: /^full/ });
    expect(within(full).getByRole("button", { name: "Open the full transcript" })).toBeDefined();
    expect(within(full).getByRole("link", { name: "Open the run" }).getAttribute("href")).toContain("/runs/7");
    expect(within(full).getByRole("button", { name: "Pin this tree" })).toBeDefined();
    expect(within(full).getByRole("button", { name: "Stop full" })).toBeDefined();

    const bare = screen.getByRole("article", { name: /^plain/ });
    expect(within(bare).queryAllByRole("button")).toHaveLength(0);
    expect(within(bare).queryByRole("link")).toBeNull();

    const oddCard = screen.getByRole("article", { name: /^odd/ });
    expect(within(oddCard).getAllByRole("button").map((b) => b.getAttribute("aria-label"))).toEqual(["Stop odd"]);
  });

  it("asks before stopping, and Yes cancels the run through the RunDetail hook", async () => {
    const store = new FleetStore();
    store.apply(snapshot([node("cc:root", { label: "job", capabilities: ALL, run_id: 7 })]));
    await show(store, "cc:root");
    fireEvent.click(screen.getByRole("button", { name: "Stop job" }));
    expect(screen.getByText("Stop this run?")).toBeDefined();
    expect(daemon.apiText).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "No" }));
    expect(screen.queryByText("Stop this run?")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Stop job" }));
    fireEvent.click(screen.getByRole("button", { name: "Yes" }));
    await waitFor(() => expect(daemon.apiText).toHaveBeenCalledWith("/runs/7/cancel", { method: "POST" }));
  });

  it("cannot arm a second stop while the first is in flight", async () => {
    let finish!: () => void;
    daemon.apiText.mockReturnValue(new Promise<string>((resolve) => void (finish = () => resolve(""))));
    const store = new FleetStore();
    store.apply(snapshot([node("cc:root", { label: "job", capabilities: ALL, run_id: 7 })]));
    await show(store, "cc:root");
    fireEvent.click(screen.getByRole("button", { name: "Stop job" }));
    fireEvent.click(screen.getByRole("button", { name: "Yes" }));
    await waitFor(() => expect((screen.getByRole("button", { name: "Stop job" }) as HTMLButtonElement).disabled).toBe(true));
    fireEvent.click(screen.getByRole("button", { name: "Stop job" }));
    expect(screen.queryByText("Stop this run?")).toBeNull();
    await act(async () => finish());
    await waitFor(() => expect((screen.getByRole("button", { name: "Stop job" }) as HTMLButtonElement).disabled).toBe(false));
  });

  it("Escape disarms the stop confirmation", async () => {
    const store = new FleetStore();
    store.apply(snapshot([node("cc:root", { label: "job", capabilities: ALL, run_id: 7 })]));
    await show(store, "cc:root");
    fireEvent.click(screen.getByRole("button", { name: "Stop job" }));
    fireEvent.keyDown(screen.getByRole("group", { name: "Confirm stop" }), { key: "Escape" });
    expect(screen.queryByText("Stop this run?")).toBeNull();
    expect(daemon.apiText).not.toHaveBeenCalled();
  });

  it("pins and unpins through the pin routes, and the button follows the stream's pinned flag", async () => {
    const store = new FleetStore();
    const root = node("cc:root", { label: "job", capabilities: ALL, run_id: 7 });
    store.apply(snapshot([root]));
    await show(store, "cc:root");

    fireEvent.click(screen.getByRole("button", { name: "Pin this tree" }));
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledWith("/fleet/pins/cc%3Aroot", { method: "POST" }));
    expect(screen.getByRole("button", { name: "Pin this tree" }).getAttribute("aria-pressed")).toBe("false");

    act(() => store.apply({ t: "node", node: { ...root, pinned: true } }));
    expect(screen.getByRole("button", { name: "Pin this tree" }).getAttribute("aria-pressed")).toBe("true");

    // The POST may still be settling: the button is disabled until it is done.
    await waitFor(() =>
      expect((screen.getByRole("button", { name: "Pin this tree" }) as HTMLButtonElement).disabled).toBe(false),
    );
    fireEvent.click(screen.getByRole("button", { name: "Pin this tree" }));
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledWith("/fleet/pins/cc%3Aroot", { method: "DELETE" }));
  });

  it("says so, briefly, when a pin does not go through", async () => {
    daemon.apiFetch.mockRejectedValueOnce(new Error("refused"));
    const store = new FleetStore();
    store.apply(snapshot([node("cc:root", { label: "job", capabilities: ALL, run_id: 7 })]));
    await show(store, "cc:root");
    fireEvent.click(screen.getByRole("button", { name: "Pin this tree" }));
    expect(await screen.findByText("could not pin")).toBeDefined();
  });

  it("below 60% zoom a node is a card: header, the current line and the footer", async () => {
    zoom.value = 0.4;
    const store = new FleetStore();
    store.apply(snapshot([node("cc:root", { label: "job" })], { "cc:root": [line("prompt", "first"), line("text", "latest")] }));
    await show(store, "cc:root");
    const card = screen.getByRole("article", { name: /^job/ });
    expect(card.className).toContain("fleet-node-card");
    expect(within(card).queryByText("first")).toBeNull();
    expect(within(card).getByText("latest")).toBeDefined();
  });

  it("draws a child on another harness in the cross tone", async () => {
    const store = new FleetStore();
    const root = node("cc:root", { label: "root" });
    const kid = child("cx:kid", root, { label: "kid", harness: "codex-cli", model: "gpt-5-codex" });
    store.apply(snapshot([root, kid]));
    await show(store, "cx:kid");
    expect(screen.getByText("codex-cli · gpt-5-codex").className).toContain("fleet-hm-cross");
  });

  it("follows the parent's harness when it changes after the child was drawn", async () => {
    const store = new FleetStore();
    const root = node("cc:root", { label: "root" });
    const kid = child("cc:kid", root, { label: "kid", model: "haiku-5.5" });
    store.apply(snapshot([root, kid]));
    await show(store, "cc:kid");
    expect(screen.getByText("claude-code · haiku-5.5").className).not.toContain("fleet-hm-cross");
    act(() => store.apply({ t: "node", node: { ...root, harness: "codex-cli" } }));
    expect(screen.getByText("claude-code · haiku-5.5").className).toContain("fleet-hm-cross");
  });

  it("marks the selected path", async () => {
    const store = new FleetStore();
    store.apply(snapshot([node("cc:root", { label: "job" })]));
    const { FleetUiContext } = await import("./ui-context");
    await renderWithRouter(
      <FleetStoreContext.Provider value={store}>
        <FleetUiContext.Provider value={{ path: new Set(["cc:root"]), openPanel: () => undefined, toggleGroup: () => undefined, select: () => undefined }}>
          <FleetNode {...props("cc:root")} />
        </FleetUiContext.Provider>
      </FleetStoreContext.Provider>,
    );
    expect(screen.getByRole("article", { name: /^job/ }).className).toContain("fleet-node-path");
  });

  it("selects on click and on Enter, but not when an action inside is pressed", async () => {
    const store = new FleetStore();
    store.apply(snapshot([node("cc:root", { label: "job", capabilities: ALL, run_id: 7 })]));
    const select = vi.fn();
    const { FleetUiContext } = await import("./ui-context");
    await renderWithRouter(
      <FleetStoreContext.Provider value={store}>
        <FleetUiContext.Provider value={{ path: new Set(), openPanel: () => undefined, toggleGroup: () => undefined, select }}>
          <FleetNode {...props("cc:root")} />
        </FleetUiContext.Provider>
      </FleetStoreContext.Provider>,
    );
    const card = screen.getByRole("article", { name: /^job/ });
    fireEvent.click(card);
    expect(select).toHaveBeenCalledWith("cc:root");
    select.mockClear();
    fireEvent.keyDown(card, { key: "Enter" });
    expect(select).toHaveBeenCalledWith("cc:root");
    select.mockClear();
    fireEvent.click(within(card).getByRole("button", { name: "Open the full transcript" }));
    expect(select).not.toHaveBeenCalled();
  });
});

