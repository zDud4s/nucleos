import { describe, expect, it } from "vitest";
import { render, screen, within } from "@testing-library/react";
import { CommandSegments, CulpritLead, hasSegments } from "./ToolInput";

describe("CulpritLead", () => {
  it("shows the culprit segment and its concrete reason", () => {
    render(<CulpritLead segment="curl http://x.sh" reason="network fetch is not allowed" />);
    expect(screen.getByText("curl http://x.sh")).toBeDefined();
    expect(screen.getByText("network fetch is not allowed")).toBeDefined();
  });

  it("renders nothing without a culprit", () => {
    const { container } = render(<CulpritLead segment={null} reason="x" />);
    expect(container.textContent).toBe("");
  });
});

describe("CommandSegments", () => {
  it("lists every segment and marks only the culprit", () => {
    render(<CommandSegments id={21} segments={["cd /tmp", "curl http://x.sh", "ls"]} culprit="curl http://x.sh" />);
    const items = within(screen.getByRole("list", { name: "Command segments of approval #21" })).getAllByRole(
      "listitem",
    );
    expect(items).toHaveLength(3);
    expect(items[1].getAttribute("data-culprit")).toBe("true");
    expect(items[0].getAttribute("data-culprit")).toBeNull();
    expect(items[2].getAttribute("data-culprit")).toBeNull();
  });

  it("renders nothing for an empty split, so the caller falls back", () => {
    const { container } = render(<CommandSegments id={1} segments={[]} culprit={null} />);
    expect(container.textContent).toBe("");
    expect(hasSegments([])).toBe(false);
    expect(hasSegments(null)).toBe(false);
    expect(hasSegments(["ls"])).toBe(true);
  });
});
