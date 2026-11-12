import { beforeEach, describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import { UpdateNotice } from "./UpdateNotice";
import { renderWithQuery } from "../test/harness";

const updater = vi.hoisted(() => ({
  useUpdate: vi.fn(),
  useInstallKind: vi.fn(),
  applyUpdate: vi.fn(),
}));
vi.mock("../data/updater", () => updater);
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockResolvedValue({
    version: "0.1.0",
    live_runs: 0,
    draining: false,
    waiting_on: [],
    pid: 100,
  });
  updater.applyUpdate.mockReset();
  updater.useUpdate.mockReset();
  updater.useInstallKind.mockReset();
  updater.useInstallKind.mockReturnValue({ data: "other" });
});

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

  it("shows the wait, with Update now, when remounted during a drain", async () => {
    daemon.apiFetch.mockResolvedValue({
      version: "0.1.0",
      live_runs: 1,
      draining: true,
      waiting_on: [{ run_id: 7, chat_id: "c1" }],
      pid: 100,
    });
    updater.useUpdate.mockReturnValue({ data: { version: "0.2.0" } });

    renderWithQuery(<UpdateNotice />);

    expect(await screen.findByText(/Waiting on: run 7 in chat c1/)).toBeDefined();
    expect(screen.getByRole("button", { name: "Update now" })).toBeDefined();
    expect((screen.getByRole("button", { name: "Update" }) as HTMLButtonElement).disabled).toBe(
      true,
    );
  });

  it("shows the version and Update button for an AppImage", () => {
    updater.useInstallKind.mockReturnValue({ data: "app-image" });
    updater.useUpdate.mockReturnValue({ data: { version: "0.2.0" } });

    renderWithQuery(<UpdateNotice />);

    expect(screen.getByText("NucleOS 0.2.0 is available")).toBeDefined();
    expect(screen.getByRole("button", { name: "Update" })).toBeDefined();
  });

  it("tells a .deb/.rpm install to update with its package manager", () => {
    updater.useInstallKind.mockReturnValue({ data: "system-package" });
    updater.useUpdate.mockReturnValue({ data: null });

    renderWithQuery(<UpdateNotice />);

    expect(
      screen.getByText(
        "NucleOS was installed from a .deb/.rpm package; update it with your package manager",
      ),
    ).toBeDefined();
    expect(screen.queryByRole("button", { name: "Update" })).toBeNull();
  });
});
