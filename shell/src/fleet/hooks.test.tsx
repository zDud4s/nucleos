import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import { createElement } from "react";
import { FleetStoreContext, useFleetStream, useNodeEntry } from "./hooks";
import { FleetStore } from "./store";
import { child, line, metricsOf, node, snapshot } from "./test-fixtures";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(cleanup);

describe("useNodeEntry", () => {
  it("re-renders only the node an event belongs to", () => {
    const store = new FleetStore();
    const a = node("cc:a");
    store.apply(snapshot([a, child("cc:b", a)]));
    const renders: Record<string, number> = {};
    function Probe({ id }: { id: string }) {
      useNodeEntry(id);
      renders[id] = (renders[id] ?? 0) + 1;
      return null;
    }
    render(
      createElement(
        FleetStoreContext.Provider,
        { value: store },
        createElement(Probe, { id: "cc:a" }),
        createElement(Probe, { id: "cc:b" }),
      ),
    );
    const base = { ...renders };
    act(() => store.apply({ t: "lines", node_id: "cc:a", lines: [line("text", "hi")] }));
    act(() => store.apply({ t: "metrics", metrics: [metricsOf("cc:a")] }));
    expect(renders["cc:a"]).toBe(base["cc:a"] + 2);
    expect(renders["cc:b"]).toBe(base["cc:b"]);
  });
});

describe("useFleetStream", () => {
  function setVisibility(state: "visible" | "hidden") {
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => state });
    document.dispatchEvent(new Event("visibilitychange"));
  }

  it("opens the stream while visible, aborts it when the page is hidden, and again on return", async () => {
    const store = new FleetStore();
    const signals: AbortSignal[] = [];
    const open = vi.fn(async (_p: string, signal: AbortSignal) => {
      signals.push(signal);
      return new ReadableStream<Uint8Array>();
    });
    setVisibility("visible");
    function Host() {
      useFleetStream(store, open);
      return null;
    }
    const view = render(createElement(Host));
    await act(async () => {});
    expect(open).toHaveBeenCalledTimes(1);

    act(() => setVisibility("hidden"));
    expect(signals[0].aborted).toBe(true);

    await act(async () => setVisibility("visible"));
    expect(open).toHaveBeenCalledTimes(2);

    view.unmount();
    expect(signals[1].aborted).toBe(true);
  });
});
