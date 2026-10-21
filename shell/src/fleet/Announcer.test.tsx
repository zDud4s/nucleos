import { afterEach, describe, expect, it } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import { Announcer } from "./Announcer";
import { FleetStoreContext } from "./hooks";
import { FleetStore } from "./store";
import { node, snapshot } from "./test-fixtures";

afterEach(cleanup);

function setup() {
  const store = new FleetStore();
  const a = node("a", { label: "nucleos-37" });
  store.apply(snapshot([a]));
  const view = render(
    <FleetStoreContext.Provider value={store}>
      <Announcer />
    </FleetStoreContext.Provider>,
  );
  const region = () => view.container.querySelector("[aria-live]")!;
  return { store, a, region };
}

describe("Announcer", () => {
  it("is a polite region and says nothing at rest", () => {
    const { region } = setup();
    expect(region().getAttribute("aria-live")).toBe("polite");
    expect(region().textContent).toBe("");
  });

  it("announces finishing and waiting, but not busy flickers or lines", () => {
    const { store, a, region } = setup();
    act(() => store.apply({ t: "lines", node_id: "a", lines: [{ kind: "text", text: "x" }] }));
    expect(region().textContent).toBe("");
    act(() => store.apply({ t: "node", node: { ...a, state: "idle" } }));
    expect(region().textContent).toBe("nucleos-37 is waiting for you");
    act(() => store.apply({ t: "node", node: { ...a, state: "busy" } }));
    expect(region().textContent).toBe("nucleos-37 is waiting for you"); // going back to work says nothing new
    act(() => store.apply({ t: "node", node: { ...a, state: "done" } }));
    expect(region().textContent).toBe("nucleos-37 finished");
  });
});
