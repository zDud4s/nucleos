import { describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import { LinkTabs } from "./LinkTabs";
import { renderWithRouter } from "../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

describe("LinkTabs", () => {
  it("is a named nav of links, with the current one marked", async () => {
    const { container } = await renderWithRouter(
      <LinkTabs
        label="Project modes"
        tabs={[
          { key: "state", to: "/teams", label: "State", current: true },
          { key: "work", to: "/runs", label: "Work", current: false, count: 0, quiet: true },
        ]}
      />,
    );

    expect(screen.getByRole("navigation", { name: "Project modes" }).className).toBe("ui-tab-list");
    const state = screen.getByRole("link", { name: "State" });
    expect(state.getAttribute("href")).toBe("/teams");
    expect(state.getAttribute("aria-current")).toBe("page");
    expect(state.getAttribute("data-state")).toBe("active");
    expect(state.className).toBe("ui-tab");

    const work = screen.getByRole("link", { name: /Work/ });
    expect(work.getAttribute("aria-current")).toBeNull();
    expect(work.getAttribute("data-state")).toBe("inactive");
    expect(work.hasAttribute("data-quiet")).toBe(true);
    // A zero is a reading, not an absence.
    expect(container.querySelector(".ui-count")?.textContent).toBe("0");
  });

  it("never lets the current tab recede", async () => {
    await renderWithRouter(
      <LinkTabs label="Modes" tabs={[{ key: "a", to: "/teams", label: "A", current: true, quiet: true }]} />,
    );
    expect(screen.getByRole("link", { name: "A" }).hasAttribute("data-quiet")).toBe(false);
  });
});
