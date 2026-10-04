import { describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import { UpdateNotice } from "./UpdateNotice";
import { renderWithQuery } from "../test/harness";

const updater = vi.hoisted(() => ({ useUpdate: vi.fn(), applyUpdate: vi.fn() }));
vi.mock("../data/updater", () => updater);
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

describe("UpdateNotice", () => {
  it("renders nothing when no update is offered", () => {
    updater.useUpdate.mockReturnValue({ data: null });

    const { container } = renderWithQuery(<UpdateNotice />);

    expect(container.textContent).toBe("");
  });

  it("shows the offered version", () => {
    updater.useUpdate.mockReturnValue({ data: { version: "0.2.0" } });

    renderWithQuery(<UpdateNotice />);

    expect(screen.getByText("NucleOS 0.2.0 is available")).toBeDefined();
    expect(screen.getByRole("button", { name: "Update" })).toBeDefined();
  });
});
