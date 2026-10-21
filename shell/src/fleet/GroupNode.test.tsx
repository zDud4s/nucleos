import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import type { NodeProps } from "@xyflow/react";

vi.mock("@xyflow/react", async (original) => ({
  ...(await original<typeof import("@xyflow/react")>()),
  Handle: () => null,
}));

import { GroupNode, type FlowGroup } from "./GroupNode";
import { FleetStoreContext } from "./hooks";
import { FleetStore } from "./store";
import { node, snapshot } from "./test-fixtures";
import { FleetUiContext } from "./ui-context";

afterEach(cleanup);

const props = (group: FlowGroup["data"]["group"]) => ({ data: { group } }) as unknown as NodeProps<FlowGroup>;

function withParent(children: ReactNode) {
  const store = new FleetStore();
  store.apply(snapshot([node("r", { label: "nucleos-37" })]));
  return <FleetStoreContext.Provider value={store}>{children}</FleetStoreContext.Provider>;
}

describe("GroupNode", () => {
  it("says how many finished and toggles through the page", () => {
    const toggle = vi.fn();
    render(
      withParent(
        <FleetUiContext.Provider value={{ path: new Set(), openPanel: vi.fn(), toggleGroup: toggle, select: vi.fn() }}>
          <GroupNode {...props({ parentId: "r", count: 9, present: 9, omitted: 0, open: false })} />
        </FleetUiContext.Provider>,
      ),
    );
    expect(screen.getByText("✓ 9 finished")).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Expand the finished nodes under nucleos-37" }));
    expect(toggle).toHaveBeenCalledWith("r");
  });

  it("when open, says how many older ones were not loaded", () => {
    render(withParent(<GroupNode {...props({ parentId: "r", count: 9, present: 6, omitted: 3, open: true })} />));
    const toggle = screen.getByRole("button", { name: "Collapse the finished nodes under nucleos-37" });
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByText("+3 older not loaded")).toBeDefined();
  });
});
